//! Shared server state: one memory store, one agent registry, one session table.
//!
//! Everything a session needs is resolved from here, so the per-connection code
//! in [`crate::ws`] never touches configuration directly. The expensive pieces —
//! the SQLite store and the `.agent.md` registry — are opened once at startup
//! rather than per connection: two agents editing the same conversation would
//! otherwise fork the DAG, and a malformed agent file should fail at boot, not
//! halfway through a user's first message.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use harness_agent::{
    AgentLoop, AgentRegistry, AgentRuntime, AgentSpec, ControlChannel, ControlHandle, ModelRouter,
};
use harness_core::{
    AgentId, Config, GuardrailsConfig, HarnessError, Memory, Message, ProviderConfig, Result,
    ServerEnvelope, ServerMessage, SessionId,
};
use harness_guardrails::{
    BehaviorMonitor, ContentFence, GuardrailPipeline, PiiDetector, SecretScanner, ToolPolicy,
};
use harness_llm::Provider;
use harness_memory::SqliteMemory;
use harness_orchestrator::WorkflowSelector;
use harness_skills::SkillRegistry;
use harness_tools::{ToolContext, ToolRegistry};
use harness_workflows::WorkflowRegistry;
use parking_lot::{Mutex, RwLock};
use tokio::net::TcpListener;
use tokio::sync::broadcast;

use crate::backpressure::BackpressurePolicy;
use crate::ws;

/// Used only when a workspace has no `.agent.md` files at all: a server that
/// refuses to start because the directory is empty would be a worse default than
/// one that answers with a bare-bones agent.
const FALLBACK_SYSTEM_PROMPT: &str = "\
You are a coding agent working inside a project workspace.
Use the available tools to inspect and modify files. Prefer reading before writing.
Keep answers short and concrete.";

/// How many events one session's fan-out keeps for a subscriber that falls
/// behind. Beyond this a subscriber is told it lagged, which is what keeps a
/// paused browser tab from pinning every event ever emitted in memory.
const SESSION_FANOUT_CAPACITY: usize = 256;

/// How many user messages one session may hold waiting behind its running turn.
///
/// A queue is a promise to run everything it holds, so it is bounded: a client
/// that submits faster than the model runs is refused with an error naming this
/// limit rather than allowed to grow the session's memory without limit.
pub const MESSAGE_QUEUE_LIMIT: usize = 32;

/// How many workflow jobs one session may hold waiting behind its running turn.
///
/// Bounded for the same reason the message queue is: a job is a promise to run
/// it, and a client that submits faster than the model runs must be refused
/// rather than allowed to grow the session's memory without limit.
pub const WORKFLOW_QUEUE_LIMIT: usize = 16;

/// Builds the provider for one session.
///
/// This is the seam a test or an embedder uses to supply a different backend
/// without reimplementing session setup: everything else still runs through
/// [`ServerState::open_session`].
pub type ProviderFactory =
    Arc<dyn Fn(&str, &ProviderConfig, &str) -> Result<Arc<dyn Provider>> + Send + Sync>;

/// One live session: its fan-out, its conversation, its agent, and the run in
/// flight or the messages waiting for one.
pub struct SessionRecord {
    session_id: SessionId,
    agent_id: AgentId,
    model: String,
    /// The loop this session runs. Held here rather than by the connection that
    /// opened it, so any connection can address the session by id — a message
    /// naming a session is served on that session even if a different socket
    /// opened it.
    agent: Arc<AgentLoop>,
    /// The provider the session resolved to, so a `plan_task` on the session
    /// runs on exactly what a message would.
    resolved: ResolvedProvider,
    /// Fan-out to every connection watching this session. A broadcast rather
    /// than a queue per watcher so the run task never blocks on a slow reader.
    events: broadcast::Sender<ServerEnvelope>,
    /// The run in flight, if any, and the messages waiting behind it.
    ///
    /// One lock covers both on purpose: "is a run in flight?" and "hand the
    /// session to the next message" have to be decided together, or a message
    /// arriving exactly as a run ends could jump the queue.
    run: Mutex<RunState>,
    /// The conversation, held here rather than by the run task so a second
    /// `UserMessage` on the same connection continues the same session. A
    /// session loaded from the store starts from its stored turns, so the next
    /// run continues that conversation instead of beginning a new one.
    pub(crate) history: tokio::sync::Mutex<Vec<Message>>,
}

/// What a session is doing right now.
struct RunState {
    /// Present only while a run is in flight; the reader loop uses it to steer.
    control: Option<ControlHandle>,
    /// Messages accepted while a run was in flight, oldest first.
    ///
    /// A separate list from `jobs` on purpose: the two queues are numbered,
    /// reported and drained independently, so a client can show "2 messages
    /// waiting" and "1 workflow queued" as different things. They share only the
    /// one `control` slot, which is what enforces one turn at a time.
    queue: VecDeque<QueuedMessage>,
    /// Workflow jobs accepted while a run was in flight, oldest first.
    jobs: VecDeque<WorkflowJob>,
    /// The id handed to the next job. Monotonic per session, so a client can
    /// correlate `workflow_queued` / `workflow_started` / `workflow_finished`
    /// without a globally unique id.
    next_job_id: u64,
}

/// One queued workflow job: what to run and, when the client named one or the
/// selector chose one, which workflow it is.
#[derive(Debug, Clone)]
pub(crate) struct WorkflowJob {
    pub(crate) id: u64,
    pub(crate) task: String,
    pub(crate) workflow: Option<String>,
}

/// A message waiting for the session's current run to finish.
struct QueuedMessage {
    text: String,
    /// Reasoning level resolved when the message was admitted, carried with it
    /// so a queued message runs at the level it asked for rather than at
    /// whatever the session's config happens to say when its turn arrives.
    effort: Option<String>,
}

/// The outcome of handing a user message to a session.
pub(crate) enum Admit {
    /// The session was idle: start a run with this text and control channel.
    Started {
        text: String,
        effort: Option<String>,
        control: ControlChannel,
    },
    /// A run was in flight; the text waits at this 1-based queue position.
    Queued { text: String, position: usize },
    /// The queue is full; the text was not accepted.
    Full { limit: usize },
}

/// The outcome of handing a workflow job to a session.
pub(crate) enum JobAdmit {
    /// The session was idle: run this job now.
    Started {
        job: WorkflowJob,
        control: ControlChannel,
    },
    /// A run was in flight; the job waits at this 1-based workflow position.
    Queued { job: WorkflowJob, position: usize },
    /// The workflow queue is full; the job was not accepted.
    Full { limit: usize },
}

/// What a session should run once its current turn ends.
///
/// One type for both queues so the caller has a single place to dispatch from,
/// but the two are never merged: a message is popped from `queue` first, and a
/// job only when no message is waiting.
pub(crate) enum NextRun {
    Message {
        text: String,
        effort: Option<String>,
        control: ControlChannel,
    },
    Job {
        job: WorkflowJob,
        control: ControlChannel,
    },
}

impl SessionRecord {
    fn new(
        session_id: SessionId,
        agent_id: AgentId,
        model: String,
        agent: Arc<AgentLoop>,
        resolved: ResolvedProvider,
        history: Vec<Message>,
    ) -> Self {
        let (events, _) = broadcast::channel(SESSION_FANOUT_CAPACITY);
        Self {
            session_id,
            agent_id,
            model,
            agent,
            resolved,
            events,
            run: Mutex::new(RunState {
                control: None,
                queue: VecDeque::new(),
                jobs: VecDeque::new(),
                next_job_id: 0,
            }),
            history: tokio::sync::Mutex::new(history),
        }
    }

    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// The loop this session runs, cloned so a run task owns what it needs.
    pub(crate) fn agent(&self) -> Arc<AgentLoop> {
        Arc::clone(&self.agent)
    }

    /// The provider this session resolved to, for a plan on the same session.
    pub(crate) fn resolved(&self) -> ResolvedProvider {
        self.resolved.clone()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<ServerEnvelope> {
        self.events.subscribe()
    }

    /// Number of connections currently attached to this session.
    pub fn subscriber_count(&self) -> usize {
        self.events.receiver_count()
    }

    /// Whether a run is in flight. A run whose task was spawned a moment ago
    /// counts too: its control handle is registered before the task is.
    pub fn is_running(&self) -> bool {
        self.run.lock().control.is_some()
    }

    /// Hands the outbound stream to every subscriber. A send with no receivers
    /// left is not an error: nobody is watching this session.
    pub(crate) fn broadcast(&self, message: ServerMessage) {
        self.broadcast_envelope(ServerEnvelope::new(
            self.session_id,
            self.agent_id.clone(),
            message,
        ));
    }

    /// Hands one already-built envelope to every subscriber.
    ///
    /// Used where the frame's routing context has to travel whole: a sub-agent's
    /// frames name the worker's own session in `subagent_session_id`, which
    /// [`SessionRecord::broadcast`] has no way to express.
    pub(crate) fn broadcast_envelope(&self, envelope: ServerEnvelope) {
        let _ = self.events.send(envelope);
    }

    /// Hands a user message to the session: starts it when the session is idle,
    /// queues it behind the running turn otherwise.
    ///
    /// `effort` is the already-resolved reasoning level for this run (message >
    /// agent > config), carried through to the loop as a per-run override.
    ///
    /// The decision and the queue push share one lock, so a message cannot slip
    /// between a run finishing and its queue being drained.
    pub(crate) fn admit(&self, text: String, effort: Option<String>) -> Admit {
        let mut run = self.run.lock();
        if run.control.is_some() {
            if run.queue.len() >= MESSAGE_QUEUE_LIMIT {
                return Admit::Full {
                    limit: MESSAGE_QUEUE_LIMIT,
                };
            }
            let position = run.queue.len() + 1;
            run.queue.push_back(QueuedMessage {
                text: text.clone(),
                effort,
            });
            return Admit::Queued { text, position };
        }
        let (handle, control) = ControlHandle::channel();
        run.control = Some(handle);
        Admit::Started {
            text,
            effort,
            control,
        }
    }

    /// Claims the session for a run that is not a queued message (a plan).
    ///
    /// Returns `None` when a run is already in flight, which the caller reports
    /// as `busy`. A plan is never queued: it is a different shape of work, and
    /// putting one behind messages would make the session's single `done`
    /// ambiguous about which unit of work finished.
    pub(crate) fn begin_run(&self) -> Option<ControlChannel> {
        let mut run = self.run.lock();
        if run.control.is_some() {
            return None;
        }
        let (handle, control) = ControlHandle::channel();
        run.control = Some(handle);
        Some(control)
    }

    /// Hands a workflow job to the session: starts it when the session is idle,
    /// appends it to the workflow queue otherwise.
    ///
    /// This is a separate list from [`SessionRecord::admit`]'s message queue.
    /// `position` counts only jobs ahead of this one, so a client that queued a
    /// message first is not told its job is second — the two queues number
    /// themselves independently.
    pub(crate) fn enqueue_job(&self, task: String, workflow: Option<String>) -> JobAdmit {
        let mut run = self.run.lock();
        let id = run.next_job_id;
        run.next_job_id += 1;
        let job = WorkflowJob { id, task, workflow };

        if run.control.is_some() {
            if run.jobs.len() >= WORKFLOW_QUEUE_LIMIT {
                return JobAdmit::Full {
                    limit: WORKFLOW_QUEUE_LIMIT,
                };
            }
            let position = run.jobs.len() + 1;
            run.jobs.push_back(job.clone());
            return JobAdmit::Queued { job, position };
        }

        let (handle, control) = ControlHandle::channel();
        run.control = Some(handle);
        JobAdmit::Started { job, control }
    }

    /// Ends the run in flight and, if anything is waiting, admits the next unit
    /// of work — atomically, so a message arriving right now cannot jump a queue.
    ///
    /// A waiting user message outranks a waiting job: the message queue is
    /// drained first, and a job only starts when it is empty. The policy is
    /// fixed rather than configurable because a user who typed something is
    /// waiting on the result, while a queued job's submitter already accepted
    /// that it waits its turn.
    pub(crate) fn finish_run(&self) -> Option<NextRun> {
        let mut run = self.run.lock();
        run.control = None;

        if let Some(queued) = run.queue.pop_front() {
            let (handle, control) = ControlHandle::channel();
            run.control = Some(handle);
            return Some(NextRun::Message {
                text: queued.text,
                effort: queued.effort,
                control,
            });
        }

        let job = run.jobs.pop_front()?;
        let (handle, control) = ControlHandle::channel();
        run.control = Some(handle);
        Some(NextRun::Job { job, control })
    }

    pub(crate) fn steer(&self, text: String, priority: harness_core::Priority) -> bool {
        match self.run.lock().control.as_ref() {
            Some(handle) => handle.steer(text, priority),
            None => false,
        }
    }

    /// Cancels the run in flight.
    ///
    /// The queue is deliberately left alone: a message the client submitted is
    /// still owed a run, so cancelling one turn does not discard the messages
    /// behind it. They start as the aborted turn finishes.
    pub(crate) fn abort(&self, reason: Option<String>) -> bool {
        match self.run.lock().control.as_ref() {
            Some(handle) => handle.abort(reason),
            None => false,
        }
    }

    /// Aborts the run in flight and drops everything waiting behind it,
    /// returning how many units of work were dropped.
    ///
    /// Both queues are dropped: their submitters were the connection that just
    /// went away, and running either would spend tokens on nobody. An explicit
    /// `abort` frame does *not* do this — see [`SessionRecord::abort`].
    pub(crate) fn abort_and_clear(&self, reason: Option<String>) -> usize {
        let mut run = self.run.lock();
        let dropped = run.queue.len() + run.jobs.len();
        run.queue.clear();
        run.jobs.clear();
        if let Some(handle) = run.control.as_ref() {
            handle.abort(reason);
        }
        dropped
    }

    pub(crate) fn approve(
        &self,
        tool_call_id: String,
        approved: bool,
        reason: Option<String>,
    ) -> bool {
        match self.run.lock().control.as_ref() {
            Some(handle) => handle.approve(tool_call_id, approved, reason),
            None => false,
        }
    }
}

/// Everything one `UserMessage` needs to start a run.
///
/// The resolved provider lives on the [`SessionRecord`], so a `plan_task` can
/// reach it without the connection that opened the session.
pub(crate) struct OpenedSession {
    pub(crate) record: Arc<SessionRecord>,
    pub(crate) agent: Arc<AgentLoop>,
}

/// The provider a session resolved to.
///
/// Kept alongside the session so a `plan_task` on the same connection plans and
/// executes against exactly what a message run would use, rather than resolving
/// a second provider that could disagree with the first.
#[derive(Clone)]
pub(crate) struct ResolvedProvider {
    pub(crate) provider: Arc<dyn Provider>,
    pub(crate) provider_id: String,
    pub(crate) model: String,
}

pub struct ServerState {
    config: Config,
    workspace_root: PathBuf,
    memory: Arc<dyn Memory>,
    registry: Arc<AgentRegistry>,
    /// Loaded once, for the same reason the agent registry is: a malformed
    /// `SKILL.md` should fail at boot rather than turn one request into an
    /// error. `/api/extensions` only ever reads it.
    skills: SkillRegistry,
    router: ModelRouter,
    all_tools: ToolRegistry,
    /// Sessions are retained for the process lifetime so `subscribe` can join a
    /// session another connection started. The volume is the session count, and
    /// the same conversations already live in SQLite.
    sessions: RwLock<HashMap<SessionId, Arc<SessionRecord>>>,
    policy: BackpressurePolicy,
    provider_factory: ProviderFactory,
    /// Chooses a workflow for a task, when the workspace has any.
    ///
    /// `None` — the default, and what a build without `harness-workflows`
    /// produces — makes every `queue_workflow` with no workflow named fall back
    /// to the free-form planner. The seam exists so the registry can be wired in
    /// without the server depending on it.
    workflow_selector: RwLock<Option<Arc<dyn WorkflowSelector>>>,
    /// The workflows this workspace declares, when a caller installed them.
    ///
    /// Held separately from the selector because the REST surface reads the
    /// same registry the selector chooses from: `/api/workflows` lists it and
    /// `POST /api/workflows` saves into it, so the picker and the chooser can
    /// never disagree about what exists.
    workflow_registry: RwLock<Option<Arc<WorkflowRegistry>>>,
    /// Rebuilds the registry and its selector from disk.
    ///
    /// The registry is read once at boot, so a workflow created through
    /// `POST /api/workflows` would exist on disk and still be unchoosable until
    /// a restart. The CLI installs this because it owns the concrete registry
    /// and selector types; the server only knows the traits.
    workflow_loader: Option<WorkflowLoader>,
    /// Identity for connection-level messages that belong to no session.
    server_agent: AgentId,
}

/// Rebuilds the workflow registry and its selector from disk.
///
/// Named rather than spelled inline so the field and its setter cannot drift,
/// and because the inline form trips `clippy::type_complexity`.
pub type WorkflowLoader =
    Arc<dyn Fn() -> Result<(Arc<WorkflowRegistry>, Arc<dyn WorkflowSelector>)> + Send + Sync>;

impl ServerState {
    /// Opens the memory database and loads the agent registry once, so every
    /// session shares one store and one registry.
    pub async fn new(config: Config, workspace_root: PathBuf) -> Result<Self> {
        let memory: Arc<dyn Memory> =
            Arc::new(SqliteMemory::open(config.db_path(&workspace_root)).await?);
        let registry = Arc::new(AgentRegistry::load(&workspace_root)?);
        let skills = SkillRegistry::load(&workspace_root)?;

        let timeout = Duration::from_secs(config.provider.request_timeout_secs);
        let max_retries = config.provider.max_retries;
        let provider_factory: ProviderFactory = Arc::new(move |id, provider, model| {
            harness_llm::from_config(id, provider, model, timeout, max_retries)
        });

        Ok(Self {
            router: ModelRouter::new(config.clone()),
            config,
            workspace_root,
            memory,
            registry,
            skills,
            all_tools: ToolRegistry::with_builtins(),
            sessions: RwLock::new(HashMap::new()),
            policy: BackpressurePolicy::default(),
            provider_factory,
            workflow_selector: RwLock::new(None),
            workflow_registry: RwLock::new(None),
            workflow_loader: None,
            // Built here rather than at the call site so the invalid-id path is
            // handled once, at startup, instead of at every reply.
            server_agent: AgentId::new("server")?,
        })
    }

    /// Replaces provider construction. Tests use it to run a scripted provider
    /// without a socket or a key; an embedder can point a provider id at its own
    /// backend.
    pub fn with_provider_factory(mut self, factory: ProviderFactory) -> Self {
        self.provider_factory = factory;
        self
    }

    /// Installs the workflow selector.
    ///
    /// Left unset by default, which is what makes `queue_workflow` with no
    /// workflow named fall back to the planner: a build that does not include a
    /// workflow registry still serves the message.
    pub fn with_workflow_selector(self, selector: Arc<dyn WorkflowSelector>) -> Self {
        *self.workflow_selector.write() = Some(selector);
        self
    }

    /// The installed selector, if any. `None` means every job plans freely.
    pub fn workflow_selector(&self) -> Option<Arc<dyn WorkflowSelector>> {
        self.workflow_selector.read().clone()
    }

    /// Installs the workflow registry the REST surface reads and writes.
    ///
    /// Installed alongside the selector rather than derived from it: the two are
    /// backed by one registry at the call site, so the list a client picks from
    /// is exactly the set the selector chooses among.
    pub fn with_workflow_registry(self, registry: Arc<WorkflowRegistry>) -> Self {
        *self.workflow_registry.write() = Some(registry);
        self
    }

    /// Installs the loader `reload_workflows` calls after a workflow is created.
    ///
    /// The CLI owns this because it owns the concrete registry and selector
    /// types; without it a created workflow is saved but unchoosable until the
    /// next restart.
    pub fn with_workflow_loader(mut self, loader: WorkflowLoader) -> Self {
        self.workflow_loader = Some(loader);
        self
    }

    /// Re-reads the workflow registry from disk and swaps it in.
    ///
    /// Best effort: a reload that fails leaves the previously loaded registry in
    /// place, because a broken new file must not take down a working server.
    /// Returns whether the swap happened.
    pub fn reload_workflows(&self) -> bool {
        let Some(loader) = self.workflow_loader.as_ref() else {
            return false;
        };
        match loader() {
            Ok((registry, selector)) => {
                *self.workflow_registry.write() = Some(registry);
                *self.workflow_selector.write() = Some(selector);
                true
            }
            Err(err) => {
                tracing::warn!("workflow registry reload failed, keeping the loaded one: {err}");
                false
            }
        }
    }

    /// The installed workflow registry, if any. `None` makes `/api/workflows`
    /// answer with an empty list rather than an error: a server that has none is
    /// a server with nothing to show, not a broken one.
    pub fn workflow_registry(&self) -> Option<Arc<WorkflowRegistry>> {
        self.workflow_registry.read().clone()
    }

    /// The provider and model a server-level model call runs on.
    ///
    /// Authoring a workflow is not a session's work, so there is no agent to
    /// resolve: the configured default provider and its default model are used,
    /// which is what a bare `harness run` with no agent would use. The same
    /// [`ProviderFactory`] builds it, so a test or an embedder swaps the backend
    /// here exactly as it does for a session.
    pub(crate) fn authoring_provider(&self) -> Result<(Arc<dyn Provider>, String)> {
        let (provider_id, provider_config) = self.config.provider(None)?;
        let model = self.config.model_for(provider_config, None)?;
        let provider = (self.provider_factory)(&provider_id, provider_config, &model)?;
        Ok((provider, model))
    }

    /// Binds, prints the listening URL, and serves until Ctrl-C.
    ///
    /// A method rather than a free function so a caller can build the state
    /// first — which is what installing a workflow registry requires, because
    /// the registry lives in a crate the server does not otherwise need. The
    /// banner and shutdown behaviour mirror `harness_server::serve`, which is
    /// kept for callers that have no state to prepare.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        let listener = TcpListener::bind(self.listen_address())
            .await
            .map_err(HarnessError::Io)?;
        let bound = listener.local_addr().map_err(HarnessError::Io)?;

        eprintln!("harness server listening on http://{bound}");
        eprintln!("  REST  http://{bound}/api/health");
        eprintln!("  bus   ws://{bound}/ws");

        axum::serve(listener, Arc::clone(&self).router())
            .with_graceful_shutdown(async {
                if let Err(err) = tokio::signal::ctrl_c().await {
                    tracing::error!("failed to listen for Ctrl-C: {err}");
                }
                tracing::info!("shutting down");
            })
            .await
            .map_err(|err| HarnessError::Other(format!("server stopped: {err}")))
    }

    /// Replaces the outbound queue policy, mostly so its limits can be reached
    /// in a test rather than only under real load.
    pub fn with_policy(mut self, policy: BackpressurePolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The full axum router. `Arc<ServerState>` because axum state must be cheap
    /// to clone.
    pub fn router(self: Arc<Self>) -> axum::Router {
        crate::api::routes()
            .route("/ws", axum::routing::get(ws::upgrade))
            .layer(tower_http::trace::TraceLayer::new_for_http())
            .with_state(self)
    }

    /// `host:port` the server binds, as configured.
    pub fn listen_address(&self) -> String {
        format!("{}:{}", self.config.server.host, self.config.server.port)
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    pub fn memory(&self) -> &Arc<dyn Memory> {
        &self.memory
    }

    pub fn registry(&self) -> &AgentRegistry {
        &self.registry
    }

    /// The skills discovered for this workspace, with their bodies still on disk.
    pub fn skills(&self) -> &SkillRegistry {
        &self.skills
    }

    pub fn policy(&self) -> &BackpressurePolicy {
        &self.policy
    }

    /// Identity used for connection-level messages that precede any session.
    pub fn server_agent(&self) -> AgentId {
        self.server_agent.clone()
    }

    /// Number of sessions this process knows about.
    pub fn session_count(&self) -> usize {
        self.sessions.read().len()
    }

    pub fn session(&self, session_id: SessionId) -> Option<Arc<SessionRecord>> {
        self.sessions.read().get(&session_id).cloned()
    }

    /// The session a frame addresses: the live one, or the stored one opened on
    /// demand.
    ///
    /// A session id outlives the process that created it — the conversation is
    /// in the store, not in memory — so a frame may name a session this process
    /// has never opened. Loading it continues that conversation under its own
    /// id rather than forking it: the client already knows the session, and a
    /// fork would leave the reply in a conversation it never asked about.
    ///
    /// `Ok(None)` means the id names nothing in either place, which the caller
    /// reports as `unknown_session`; an `Err` is a store failure and is not the
    /// client's fault.
    pub(crate) async fn session_or_load(
        &self,
        session_id: SessionId,
    ) -> Result<Option<Arc<SessionRecord>>> {
        if let Some(record) = self.session(session_id) {
            return Ok(Some(record));
        }

        let Some(info) = self.memory.session(session_id).await? else {
            return Ok(None);
        };

        // The label is the only record of which agent the conversation belongs
        // to; the spec is reloaded from the registry so the model and tool
        // whitelist come from the agent file, exactly as for a fresh session.
        let spec = select_stored_spec(
            &self.registry,
            info.label.as_deref(),
            &self.config.harness.default_agent,
        );
        let history = self.memory.history(session_id, None).await?;
        let opened = self
            .build_session(spec.as_ref(), session_id, history)
            .await?;
        Ok(Some(opened.record))
    }

    /// Resolves everything a run needs and registers a new session.
    ///
    /// This mirrors `harness run`: the spec decides the tools and the system
    /// prompt, the router decides the model, and the memory session is created
    /// before the loop so `SessionStarted` can name a real id.
    pub(crate) async fn open_session(
        &self,
        requested_agent: Option<&str>,
    ) -> Result<OpenedSession> {
        let spec = select_spec(
            &self.registry,
            requested_agent,
            &self.config.harness.default_agent,
        )?;
        let session_id = self
            .memory
            .create_session(Some(&format!("serve:{}", spec.id)))
            .await?;

        self.build_session(spec.as_ref(), session_id, Vec::new())
            .await
    }

    /// Builds the loop, provider and record for one session and registers it.
    ///
    /// Shared by the two entry points so a session continued from the store runs
    /// exactly what a freshly opened one would: same provider resolution, same
    /// guardrails, same tool whitelist. The only difference is the session id
    /// and the history it starts from.
    async fn build_session(
        &self,
        spec: &AgentSpec,
        session_id: SessionId,
        history: Vec<Message>,
    ) -> Result<OpenedSession> {
        // Nothing to override from a socket: `--provider` moved the default
        // already, and an explicit `model:` in the agent file outranks it.
        let resolved = self.router.resolve(Some(spec), None, &[])?;
        let (provider_id, provider_config) = self.config.provider(Some(&resolved.provider_id))?;
        let provider = (self.provider_factory)(&provider_id, provider_config, &resolved.model)?;

        let runtime = AgentRuntime::build_with_overrides(
            spec,
            &self.router,
            Arc::clone(&provider),
            &self.all_tools,
            Arc::clone(&self.memory),
            &[],
        )?;

        let tool_ctx = ToolContext::new(self.workspace_root.clone(), self.config.tools.clone());
        let mut agent_config = runtime.agent_config(session_id);
        // The in-flight trim budget comes from the config, so `dhs serve` and
        // `dhs web` trim at the same threshold `dhs run` does.
        agent_config.context_trim_threshold = self.config.context.trim_threshold_chars;
        // Every session gets the pipeline the config asks for, or none at all.
        if let Some(pipeline) = guardrail_pipeline(&self.config.guardrails)? {
            agent_config = agent_config.with_guardrails(pipeline);
        }
        let agent_id = agent_config.agent_id.clone();
        let agent = Arc::new(AgentLoop::new(
            agent_config,
            Arc::clone(&provider),
            runtime.tools.clone(),
            tool_ctx,
        ));

        let resolved_provider = ResolvedProvider {
            provider: Arc::clone(&provider),
            provider_id: resolved.provider_id.clone(),
            model: resolved.model.clone(),
        };
        let record = Arc::new(SessionRecord::new(
            session_id,
            agent_id,
            resolved.model.clone(),
            Arc::clone(&agent),
            resolved_provider.clone(),
            history,
        ));

        // Two connections may resolve the same stored session at the same
        // moment. The first to register wins and the second adopts its record,
        // so one conversation never has two loops racing to append to it.
        let mut sessions = self.sessions.write();
        if let Some(existing) = sessions.get(&session_id) {
            return Ok(OpenedSession {
                record: Arc::clone(existing),
                agent: existing.agent(),
            });
        }
        sessions.insert(session_id, Arc::clone(&record));

        Ok(OpenedSession { record, agent })
    }
}

/// The agent to run: the one asked for, else the configured default, else a
/// built-in fallback so a workspace with no agent files still serves.
///
/// A requested agent that is not in the registry is an error here, because a
/// client naming one just now asked for something that does not exist. A label
/// on a stored session is different — see [`select_stored_spec`].
fn select_spec<'a>(
    registry: &'a AgentRegistry,
    requested: Option<&str>,
    configured_default: &str,
) -> Result<Cow<'a, AgentSpec>> {
    if let Some(id) = requested {
        return registry.get(id).map(Cow::Borrowed).ok_or_else(|| {
            let ids: Vec<&str> = registry
                .list()
                .iter()
                .map(|spec| spec.id.as_str())
                .collect();
            let available = if ids.is_empty() {
                "(none)".to_string()
            } else {
                ids.join(", ")
            };
            HarnessError::AgentNotFound(format!("{id}; available: {available}"))
        });
    }

    Ok(default_spec(registry, configured_default))
}

/// The agent a stored session was created with, from the label it carries.
///
/// `harness run` writes `run:<agent>` and this server writes `serve:<agent>`
/// (see `harness-cli`'s run command and [`ServerState::open_session`]), so the
/// label is the only record of which `.agent.md` the conversation belongs to.
/// An unrecognised label names no agent and falls back to the default.
fn agent_from_label(label: Option<&str>) -> Option<&str> {
    let (prefix, agent) = label?.split_once(':')?;
    matches!(prefix, "run" | "serve")
        .then_some(agent)
        .filter(|agent| !agent.is_empty())
}

/// The agent to run a stored session on.
///
/// Unlike [`select_spec`], a label naming an agent that is no longer in the
/// registry does not fail the session: the conversation is still readable, and
/// refusing to continue it would make it unreachable. It runs on the default
/// agent instead, logged because the model and tool whitelist silently changed.
fn select_stored_spec<'a>(
    registry: &'a AgentRegistry,
    label: Option<&str>,
    configured_default: &str,
) -> Cow<'a, AgentSpec> {
    if let Some(agent) = agent_from_label(label) {
        if let Some(spec) = registry.get(agent) {
            return Cow::Borrowed(spec);
        }
        tracing::warn!(
            agent,
            "the agent this session was created with is no longer in the registry; using the default"
        );
    }

    default_spec(registry, configured_default)
}

/// The configured default agent, or a built-in fallback so a workspace with no
/// agent files still serves.
fn default_spec<'a>(registry: &'a AgentRegistry, configured_default: &str) -> Cow<'a, AgentSpec> {
    match registry.default_spec(configured_default) {
        Some(spec) => Cow::Borrowed(spec),
        None => {
            tracing::debug!("no .agent.md files found, using the built-in agent");
            Cow::Owned(AgentSpec {
                id: "default".to_string(),
                name: "Default".to_string(),
                description: "Built-in fallback for workspaces with no agent files.".to_string(),
                system_prompt: FALLBACK_SYSTEM_PROMPT.to_string(),
                source_path: PathBuf::from("<built-in>"),
                ..Default::default()
            })
        }
    }
}

/// The guard pipeline the config asks for, or `None` when it is switched off.
///
/// Mirrors `harness run`'s builder: the same deterministic guard set, with the
/// loop detector's threshold taken from the configuration rather than the
/// library default.
fn guardrail_pipeline(config: &GuardrailsConfig) -> Result<Option<Arc<GuardrailPipeline>>> {
    if !config.enabled {
        return Ok(None);
    }
    Ok(Some(Arc::new(GuardrailPipeline::new(vec![
        Arc::new(SecretScanner::new()?),
        Arc::new(PiiDetector::new()?),
        Arc::new(ContentFence::new()?),
        Arc::new(ToolPolicy::destructive_defaults()?),
        Arc::new(BehaviorMonitor::new(config.max_identical_tool_calls)),
    ]))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::ProviderKind;
    use harness_llm::{MockProvider, ScriptedTurn};

    /// A server whose only provider is the mock, so no key or network is needed.
    async fn state(guardrails_enabled: bool) -> (tempfile::TempDir, ServerState) {
        state_with(guardrails_enabled, None).await
    }

    /// [`state`] with an explicit in-flight trim budget, so a test can tell a
    /// configured value apart from the loop's own default.
    async fn state_with(
        guardrails_enabled: bool,
        trim_threshold_chars: Option<usize>,
    ) -> (tempfile::TempDir, ServerState) {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        std::fs::create_dir_all(dir.path().join("agents")).expect("the agents directory");
        std::fs::write(
            dir.path().join("agents/test.agent.md"),
            "---\nname: Test Agent\ndescription: A fixture agent.\n---\nYou are a test agent.\n",
        )
        .expect("the agent file");

        let mut config = Config::default();
        config.harness.default_agent = "test".to_string();
        config.provider.default = "mock".to_string();
        config.providers.insert(
            "mock".to_string(),
            ProviderConfig {
                kind: ProviderKind::Mock,
                default_model: Some("mock-1".to_string()),
                ..Default::default()
            },
        );
        config.memory.db_path = dir.path().join("harness.db");
        config.guardrails.enabled = guardrails_enabled;
        if let Some(threshold) = trim_threshold_chars {
            config.context.trim_threshold_chars = threshold;
        }

        let factory: ProviderFactory = Arc::new(|id, _config, model| {
            Ok(Arc::new(MockProvider::new(
                id,
                model,
                vec![ScriptedTurn::Text("done".to_string())],
            )) as Arc<dyn Provider>)
        });
        let state = ServerState::new(config, dir.path().to_path_buf())
            .await
            .expect("the state builds")
            .with_provider_factory(factory);
        (dir, state)
    }

    #[tokio::test]
    async fn a_session_gets_the_pipeline_when_guardrails_are_enabled() {
        let (_dir, state) = state(true).await;
        let opened = state.open_session(None).await.expect("a session opens");
        let pipeline = opened
            .agent
            .config()
            .guardrails
            .as_deref()
            .expect("guardrails are attached");
        assert_eq!(
            pipeline.names(),
            vec![
                "secret_scanner",
                "pii_detector",
                "content_fence",
                "tool_policy",
                "behavior_monitor",
            ]
        );
    }

    #[tokio::test]
    async fn a_session_gets_no_pipeline_when_guardrails_are_disabled() {
        let (_dir, state) = state(false).await;
        let opened = state.open_session(None).await.expect("a session opens");
        assert!(opened.agent.config().guardrails.is_none());
    }

    #[tokio::test]
    async fn a_session_trims_at_the_configured_threshold() {
        // `0` is a real setting (trimming off), not an absent one, so the
        // session must carry the configured value rather than the loop default.
        let (_dir, state) = state_with(false, Some(0)).await;
        let opened = state.open_session(None).await.expect("a session opens");
        assert_eq!(opened.agent.config().context_trim_threshold, 0);
    }
}
