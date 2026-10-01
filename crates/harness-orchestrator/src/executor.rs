//! Running a task graph, layer by layer.
//!
//! Two rules shape everything here:
//!
//! * **A sub-agent starts clean.** Its history is built from its own objective,
//!   the four-field instruction, what its dependencies finished, and a recall
//!   summary — never from the planner's conversation. A worker that inherited the
//!   planner's reasoning would re-litigate decisions instead of executing them.
//! * **A failure is local.** A node that fails does not abort its siblings; the
//!   nodes that depended on it are reported as skipped. Only an explicit abort
//!   stops the run.
//! * **A sub-agent run is traceable.** When the caller names a parent session,
//!   each node's sub-agent runs on a session forked from it and its turns are
//!   persisted there, so a delegation can be opened and replayed afterwards
//!   rather than only watched as it streams. The fork point carries no turns, so
//!   "starts clean" still holds for the stored conversation.
//!
//! A node that never ran (because a dependency failed, or because an abort
//! arrived first) is reported as [`SubtaskStatus::Skipped`] in the report. It
//! emits no `SubtaskStart` — there is no worker lane to open — but it does emit
//! a terminal `SubtaskEnd` with `status: skipped`, emitted by `execute` once the
//! run is over. Without that frame a client that drew a row for every node from
//! the plan would leave the skipped rows pending forever.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use futures::stream::{FuturesUnordered, StreamExt};
use harness_agent::{
    AgentLoop, AgentRegistry, AgentRuntime, AgentSpec, ControlChannel, ControlHandle,
    ControlMessage, ModelRouter,
};
use harness_core::{
    AgentEvent, AgentId, CompletionReason, HarnessError, Memory, Message, NodeId, Result,
    RoutedEvent, SessionId, SubtaskId, SubtaskStatus, ToolsConfig,
};
use harness_llm::Provider;
use harness_memory::{node_stage, pin_stage, recall_for, RecallBudget, RecallScenario};
use harness_tools::{ToolContext, ToolRegistry};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Semaphore};

use crate::graph::{TaskGraph, TaskNode};
use crate::result::SubtaskResult;
use crate::verify::{Severity, VerificationReport, Verifier, VerifierConfig};
use crate::worktree::Worktree;

/// Longest previous reply quoted back to a retrying sub-agent.
const MAX_QUOTED_REPLY: usize = 2_000;

#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Reflexion attempts per node, after the first try.
    pub max_retries: u32,
    pub max_parallel: usize,
    /// `false` for tests and non-git workspaces.
    pub use_worktrees: bool,
    /// Whether a successful node's worktree outlives the node. The trade-off is
    /// real in both directions: keeping them costs disk and leaves a branch
    /// behind per node, while discarding them loses the only copy of that
    /// node's edits. Only a caller that genuinely wants a clean tree after every
    /// node should set this to `false`.
    pub keep_worktrees: bool,
    /// Budget for the shared-memory summary a sub-agent receives.
    pub recall_tokens: usize,
    /// Which provider the single provider instance this executor holds speaks
    /// for, e.g. `deepseek` or `opencode-go`.
    ///
    /// A node's model is resolved from its agent spec, and a spec writes it as
    /// `provider/model`. The executor can only ever send a request through the
    /// one provider it was given, so a node resolving to a *different* provider
    /// would have its bare model name sent to the wrong gateway and rejected
    /// there. The executor compares this id against each node's resolved
    /// `provider_id` and refuses a mismatch before the node spends a token.
    ///
    /// `None` asks the provider instance itself ([`Provider::id`]), which is the
    /// configured provider key for every provider built by `harness_llm` — set it
    /// only when the instance's id is not the key the router resolves against.
    pub provider_id: Option<String>,
    /// The session a run's sub-agents fork from.
    ///
    /// When set, each node's sub-agent gets a session of its own, branched from
    /// this one, and its turns are persisted there; the child's
    /// `forked_from_session` then names this session, which is what lets a UI
    /// group a delegation under the conversation that started it.
    ///
    /// `None` — the default, and what every caller that has no session of its
    /// own gets — keeps the older behaviour: the worker runs on an in-memory id
    /// that is never written to the store, and nothing about it can be opened
    /// afterwards.
    pub parent_session: Option<SessionId>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            max_retries: 1,
            max_parallel: 4,
            use_worktrees: false,
            keep_worktrees: true,
            recall_tokens: 1_000,
            provider_id: None,
            parent_session: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub nodes: Vec<NodeOutcome>,
    pub succeeded: usize,
    pub failed: usize,
}

impl ExecutionReport {
    pub fn get(&self, id: &str) -> Option<&NodeOutcome> {
        self.nodes.iter().find(|node| node.id == id)
    }

    /// Nodes that never ran because a dependency failed, or because the run was
    /// aborted before they were scheduled.
    pub fn skipped(&self) -> usize {
        self.nodes
            .iter()
            .filter(|node| node.status == SubtaskStatus::Skipped)
            .count()
    }
}

#[derive(Debug, Clone)]
pub struct NodeOutcome {
    pub id: String,
    pub status: SubtaskStatus,
    pub attempts: u32,
    pub result: Option<SubtaskResult>,
    pub verification: Option<VerificationReport>,
    pub tokens: u64,
    /// Where the node's edits live, when it succeeded under worktree isolation.
    /// The caller integrates from here; nothing merges automatically.
    pub worktree: Option<WorktreeRef>,
}

/// A kept checkout: the only copy of the edits a successful node made under
/// isolation, plus the branch those edits sit on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeRef {
    pub path: PathBuf,
    pub branch: String,
}

impl NodeOutcome {
    fn skipped(id: &str) -> Self {
        Self {
            id: id.to_string(),
            status: SubtaskStatus::Skipped,
            attempts: 0,
            result: None,
            verification: None,
            tokens: 0,
            worktree: None,
        }
    }
}

pub struct Executor {
    config: ExecutorConfig,
    registry: AgentRegistry,
    router: ModelRouter,
    provider: Arc<dyn Provider>,
    /// The provider identity a node's resolved `provider_id` is compared
    /// against: `config.provider_id`, else the provider instance's own id.
    provider_id: String,
    memory: Arc<dyn Memory>,
    workspace_root: PathBuf,
    verifier: Verifier,
    tools: ToolRegistry,
    tools_config: ToolsConfig,
    /// `git worktree add` writes into the repository's shared `.git/worktrees`
    /// directory, so two concurrent creations against one repository race. The
    /// lock is held only for the synchronous git call, never across an await, so
    /// the cost is a brief block of one worker thread rather than a stall.
    worktree_lock: Mutex<()>,
}

impl Executor {
    /// Builds an executor around one provider instance.
    ///
    /// Every node is served through that single instance, so
    /// [`ExecutorConfig::provider_id`] (or, failing that, the provider's own
    /// [`Provider::id`]) is recorded as the executor's identity and a node whose
    /// spec resolves to another provider is refused rather than misrouted.
    pub fn new(
        config: ExecutorConfig,
        registry: AgentRegistry,
        router: ModelRouter,
        provider: Arc<dyn Provider>,
        memory: Arc<dyn Memory>,
        workspace_root: PathBuf,
    ) -> Self {
        let provider_id = config
            .provider_id
            .clone()
            .unwrap_or_else(|| provider.id().to_string());
        Self {
            config,
            registry,
            router,
            provider,
            provider_id,
            memory,
            workspace_root,
            verifier: Verifier::new(VerifierConfig::default()),
            tools: ToolRegistry::with_builtins(),
            tools_config: ToolsConfig::default(),
            worktree_lock: Mutex::new(()),
        }
    }

    /// Replaces the default (no-check, no-adversary) verifier.
    pub fn with_verifier(mut self, verifier: Verifier) -> Self {
        self.verifier = verifier;
        self
    }

    /// Replaces the builtin tool registry, e.g. to keep a test hermetic.
    pub fn with_tools(mut self, tools: ToolRegistry) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_tools_config(mut self, tools_config: ToolsConfig) -> Self {
        self.tools_config = tools_config;
        self
    }

    /// Runs the graph layer by layer. Nodes in a layer run concurrently.
    pub async fn execute(
        &self,
        graph: &TaskGraph,
        events: &mpsc::UnboundedSender<RoutedEvent>,
        control: &mut ControlChannel,
    ) -> Result<ExecutionReport> {
        graph.validate()?;
        let layers = graph.layers()?;
        let entry_agent = self.entry_agent();
        let semaphore = Arc::new(Semaphore::new(self.config.max_parallel.max(1)));
        // Pinned once for the whole run, so every node's session forks from the
        // same point: a parent written to while the run is in flight must not
        // move the fork under a node that has not started yet.
        let fork_point = match self.config.parent_session {
            Some(parent) => self.fork_point(parent).await,
            None => None,
        };

        let mut outcomes: HashMap<String, NodeOutcome> = HashMap::new();
        // Nodes whose `SubtaskStart` was announced. Every other node needs a
        // terminal frame of its own at the end, or a client that drew a row per
        // plan node would never learn how it ended.
        let mut announced: BTreeSet<String> = BTreeSet::new();
        let mut aborted = false;
        let mut control_open = true;
        // Shared with the node futures so a node still queued for a worker slot
        // learns about the abort without having to run.
        let abort_flag = Arc::new(AtomicBool::new(false));

        for layer in layers {
            if aborted {
                break;
            }

            // The layer loop only watches the control channel while nodes are in
            // flight, so anything queued between layers is drained here.
            for message in control.try_drain() {
                if message.is_abort() {
                    aborted = true;
                    abort_flag.store(true, Ordering::SeqCst);
                }
            }
            if aborted {
                break;
            }

            let mut runnable: Vec<&TaskNode> = Vec::new();
            for node in layer.iter().copied() {
                if self.dependencies_satisfied(node, &outcomes) {
                    runnable.push(node);
                } else {
                    outcomes.insert(node.id.clone(), NodeOutcome::skipped(&node.id));
                }
            }

            let mut pending = FuturesUnordered::new();
            let mut handles: Vec<ControlHandle> = Vec::new();
            let mut remaining = runnable.len();

            for node in runnable {
                let (handle, channel) = ControlHandle::channel();
                handles.push(handle);
                let dependencies = self.dependency_context(graph, node, &outcomes);
                pending.push(self.run_node(
                    node,
                    dependencies,
                    entry_agent.clone(),
                    channel,
                    Arc::clone(&semaphore),
                    Arc::clone(&abort_flag),
                    events,
                    fork_point,
                    graph.guidance.as_deref(),
                ));
            }

            while remaining > 0 {
                tokio::select! {
                    biased;

                    message = control.recv(), if control_open => match message {
                        Some(ControlMessage::Abort { .. }) => {
                            aborted = true;
                            abort_flag.store(true, Ordering::SeqCst);
                            for handle in &handles {
                                handle.abort(Some("the orchestrator was aborted".into()));
                            }
                        }
                        Some(ControlMessage::Steering { text, priority }) => {
                            for handle in &handles {
                                handle.steer(&text, priority);
                            }
                        }
                        Some(ControlMessage::ToolApproval { .. }) => {}
                        None => control_open = false,
                    },

                    Some(run) = pending.next() => {
                        remaining -= 1;
                        if run.announced {
                            announced.insert(run.outcome.id.clone());
                        }
                        outcomes.insert(run.outcome.id.clone(), run.outcome);
                    }
                }
            }
        }

        // Nodes an abort kept from ever being scheduled still need a status.
        for node in &graph.nodes {
            if !outcomes.contains_key(&node.id) {
                outcomes.insert(node.id.clone(), NodeOutcome::skipped(&node.id));
            }
        }

        let mut nodes = Vec::with_capacity(graph.nodes.len());
        let mut succeeded = 0usize;
        let mut failed = 0usize;
        for node in &graph.nodes {
            let outcome = outcomes
                .remove(&node.id)
                .unwrap_or_else(|| NodeOutcome::skipped(&node.id));
            match outcome.status {
                SubtaskStatus::Succeeded => succeeded += 1,
                SubtaskStatus::Failed => failed += 1,
                _ => {}
            }
            nodes.push(outcome);
        }

        // A node that was never announced — one skipped because a dependency
        // failed, one an abort kept from starting, or one whose agent could not
        // be resolved before it had a lane — still needs a terminal frame. The
        // plan carried every node, so every row must reach a state; a row that
        // stayed `pending` would be indistinguishable from one still running.
        for outcome in &nodes {
            if announced.contains(&outcome.id) {
                continue;
            }
            let summary = if aborted {
                "the run was aborted before this node started".to_string()
            } else if outcome.status == SubtaskStatus::Skipped {
                "a dependency did not succeed, so this node was skipped".to_string()
            } else {
                "the node ended before its worker could start".to_string()
            };
            emit(
                events,
                AgentEvent::SubtaskEnd {
                    subtask_id: SubtaskId::new(),
                    node_id: outcome.id.clone(),
                    status: outcome.status,
                    summary,
                },
            );
        }

        Ok(ExecutionReport {
            nodes,
            succeeded,
            failed,
        })
    }

    /// The agent a run is considered to have started under, for `AgentHandoff`.
    fn entry_agent(&self) -> Option<AgentId> {
        self.registry
            .default_spec("default")
            .and_then(|spec| AgentId::new(spec.id.clone()).ok())
    }

    fn resolve_spec(&self, node: &TaskNode) -> Result<&AgentSpec> {
        match node.agent.as_deref() {
            Some(id) => self
                .registry
                .get(id)
                .ok_or_else(|| HarnessError::AgentNotFound(id.to_string())),
            None => self
                .registry
                .default_spec("default")
                .ok_or_else(|| HarnessError::AgentNotFound("default".into())),
        }
    }

    /// Refuses a node whose spec resolves its model to a provider other than the
    /// one this executor holds.
    ///
    /// The message names the node, the model it asked for, the provider that
    /// would have served it and the provider the executor holds, because the
    /// fix is a command-line or spec change the user has to make.
    fn node_provider_matches(&self, node: &TaskNode, spec: &AgentSpec) -> Result<()> {
        let resolved = self.router.resolve(Some(spec), None, &[])?;
        let held = self.provider_id.as_str();
        if resolved.provider_id == held {
            return Ok(());
        }

        let asked = spec.model.as_deref().unwrap_or(resolved.model.as_str());
        Err(HarnessError::Other(format!(
            "node `{}` asks for model `{asked}`, which provider `{}` would serve, but this \
             executor holds a `{held}` provider: the request would reach the wrong gateway. \
             Run the graph under `--provider {}`, or drop the provider prefix from the model \
             in the node's agent spec.",
            node.id, resolved.provider_id, resolved.provider_id
        )))
    }

    fn dependencies_satisfied(
        &self,
        node: &TaskNode,
        outcomes: &HashMap<String, NodeOutcome>,
    ) -> bool {
        node.depends_on.iter().all(|dep| {
            outcomes
                .get(dep)
                .map(|outcome| outcome.status == SubtaskStatus::Succeeded)
                .unwrap_or(false)
        })
    }

    fn dependency_context(
        &self,
        graph: &TaskGraph,
        node: &TaskNode,
        outcomes: &HashMap<String, NodeOutcome>,
    ) -> Vec<(String, String, String)> {
        node.depends_on
            .iter()
            .filter_map(|dep| {
                let objective = graph.get(dep)?.objective.clone();
                let state = outcomes
                    .get(dep)
                    .and_then(|outcome| outcome.result.as_ref())
                    .map(|result| result.state.clone())
                    .unwrap_or_else(|| "succeeded".to_string());
                Some((dep.clone(), objective, state))
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_node(
        &self,
        node: &TaskNode,
        dependencies: Vec<(String, String, String)>,
        entry_agent: Option<AgentId>,
        control: ControlChannel,
        semaphore: Arc<Semaphore>,
        aborted: Arc<AtomicBool>,
        events: &mpsc::UnboundedSender<RoutedEvent>,
        fork_point: Option<ForkPoint>,
        guidance: Option<&str>,
    ) -> NodeRun {
        let subtask_id = SubtaskId::new();
        // The node's own events travel on a channel of their own and are tagged
        // with the node's lane on the way out. A layer's nodes run concurrently,
        // so by the time a chunk arrives the `SubtaskStart` that opened its lane
        // is long past; a single "current lane" on the shared stream would
        // attribute one worker's output to another.
        let (node_events, node_rx) = mpsc::unbounded_channel::<AgentEvent>();
        let lane: Arc<OnceLock<SubagentLane>> = Arc::new(OnceLock::new());
        let forwarder = tokio::spawn(forward_node_events(
            node_rx,
            Arc::clone(&lane),
            events.clone(),
        ));

        let run = self
            .run_node_inner(
                node,
                dependencies,
                entry_agent,
                subtask_id,
                control,
                semaphore,
                aborted,
                &node_events,
                events,
                &lane,
                fork_point,
                guidance,
            )
            .await;

        // A node that was announced always gets a terminal event, on every path
        // out of `run_node_inner` — a provider error or a setup failure included.
        // A node that never started was never announced, so it has no lane to
        // close here; `execute` closes it with a terminal frame once the run is
        // over, so no plan row is left without a state.
        if run.announced {
            send_node(
                &node_events,
                AgentEvent::SubtaskEnd {
                    subtask_id,
                    node_id: node.id.clone(),
                    status: run.outcome.status,
                    summary: run.summary.clone(),
                },
            );
        }
        // The channel is drained before returning, so a caller that reads the
        // stream as soon as `execute` resolves sees every event the node produced.
        drop(node_events);
        if forwarder.await.is_err() {
            tracing::debug!("the event forwarder for node `{}` stopped early", node.id);
        }
        run
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_node_inner(
        &self,
        node: &TaskNode,
        dependencies: Vec<(String, String, String)>,
        entry_agent: Option<AgentId>,
        subtask_id: SubtaskId,
        mut control: ControlChannel,
        semaphore: Arc<Semaphore>,
        aborted: Arc<AtomicBool>,
        node_events: &mpsc::UnboundedSender<AgentEvent>,
        events: &mpsc::UnboundedSender<RoutedEvent>,
        lane: &Arc<OnceLock<SubagentLane>>,
        fork_point: Option<ForkPoint>,
        guidance: Option<&str>,
    ) -> NodeRun {
        // The start event carries the agent id, so the id is resolved before it
        // is announced. A node naming an unknown agent is still announced with
        // the id it asked for, then fails.
        let requested = node
            .agent
            .clone()
            .or_else(|| {
                self.registry
                    .default_spec("default")
                    .map(|spec| spec.id.clone())
            })
            .and_then(|id| AgentId::new(id).ok());

        let spec = match self.resolve_spec(node) {
            Ok(spec) => spec,
            Err(err) => {
                if let Some(agent_id) = requested.clone() {
                    // Announced so the lane exists and can be closed; no session
                    // was created for a node whose agent could not be resolved.
                    set_lane(lane, agent_id.clone(), None);
                    send_node(
                        node_events,
                        AgentEvent::SubtaskStart {
                            subtask_id,
                            node_id: node.id.clone(),
                            agent_id,
                            objective: node.objective.clone(),
                        },
                    );
                }
                emit(
                    events,
                    AgentEvent::Error {
                        message: err.to_string(),
                    },
                );
                return failed_run(node, 0, 0, err.to_string(), requested.is_some());
            }
        };

        // Neither failure below can announce a start (there is no usable agent
        // id), so each reports itself with an error event instead of a subtask
        // pair the UI could not attach to a lane.
        let agent_id = match AgentId::new(spec.id.clone()) {
            Ok(id) => id,
            Err(err) => {
                emit(
                    events,
                    AgentEvent::Error {
                        message: err.to_string(),
                    },
                );
                return failed_run(node, 0, 0, err.to_string(), false);
            }
        };

        // Waiting for a slot before announcing the start keeps the UI honest: a
        // node that has not been given a worker is not "running".
        let _permit = match semaphore.acquire_owned().await {
            Ok(permit) => permit,
            Err(_) => {
                let message = "the executor was shut down before this node started";
                emit(
                    events,
                    AgentEvent::Error {
                        message: message.to_string(),
                    },
                );
                return failed_run(node, 0, 0, message, false);
            }
        };

        // An abort that landed while this node was queued must not let it start:
        // the point of the abort is to stop spending tokens, not to spend them
        // one node later.
        if aborted.load(Ordering::SeqCst) {
            return skipped_run(node, "the run was aborted before this node started");
        }

        // Created before the lane is announced, so the `SubtaskStart` a client
        // receives already names the session it can open the worker's own
        // conversation from.
        let session = self.subagent_session(fork_point, &node.id).await;
        // The start boundary is pinned before the worker runs, so a stage can be
        // forked from the point where this node began rather than only from the
        // run's end.
        if let Some(session) = session {
            self.pin_node_stage(session, &node.id, "start").await;
        }
        set_lane(lane, agent_id.clone(), session);

        send_node(
            node_events,
            AgentEvent::SubtaskStart {
                subtask_id,
                node_id: node.id.clone(),
                agent_id: agent_id.clone(),
                objective: node.objective.clone(),
            },
        );

        if let Some(entry) = &entry_agent {
            if entry != &agent_id {
                emit(
                    events,
                    AgentEvent::AgentHandoff {
                        from: entry.clone(),
                        to: agent_id.clone(),
                        reason: format!("node `{}` is run by `{agent_id}`", node.id),
                    },
                );
            }
        }

        // The executor holds one provider instance, so a node whose spec names a
        // different provider could only be sent to the wrong gateway, where the
        // bare model name would be rejected with a message about a model that
        // gateway has never heard of. Refusing here costs no tokens and names
        // both providers.
        if let Err(err) = self.node_provider_matches(node, spec) {
            let message = err.to_string();
            emit(
                events,
                AgentEvent::Error {
                    message: message.clone(),
                },
            );
            return failed_run(node, 0, 0, message, true);
        }

        let worktree = if self.config.use_worktrees {
            match self.create_worktree(node) {
                Ok(worktree) => Some(worktree),
                Err(err) => return failed_run(node, 0, 0, err.to_string(), true),
            }
        } else {
            None
        };
        let root = worktree
            .as_ref()
            .map(|worktree| worktree.path().to_path_buf())
            .unwrap_or_else(|| self.workspace_root.clone());

        let runtime = match AgentRuntime::build_with_overrides(
            spec,
            &self.router,
            Arc::clone(&self.provider),
            &self.tools,
            Arc::clone(&self.memory),
            &[],
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                if let Some(worktree) = worktree {
                    worktree.remove();
                }
                return failed_run(node, 0, 0, err.to_string(), true);
            }
        };

        // The worker runs on its own forked session when one could be created;
        // the in-memory id keeps the loop's contract satisfied when it could not.
        let agent = AgentLoop::new(
            runtime.agent_config(session.unwrap_or_else(SessionId::new)),
            Arc::clone(&self.provider),
            runtime.tools.clone(),
            ToolContext::new(root.clone(), self.tools_config.clone()),
        );

        // The sub-agent starts clean: its own objective, the shared contract,
        // what its dependencies produced, and a recall summary. The planner's
        // conversation is deliberately absent.
        let recall = self.recall_summary(node).await;
        let mut history = vec![Message::user(build_prompt(
            node,
            &dependencies,
            &recall,
            guidance,
        ))];

        let mut outcome = NodeOutcome {
            id: node.id.clone(),
            status: SubtaskStatus::Pending,
            attempts: 0,
            result: None,
            verification: None,
            tokens: 0,
            worktree: None,
        };
        let mut feedback: Option<String> = None;

        // Every exit from the attempt loop carries the node's terminal status
        // and the summary its `SubtaskEnd` will show.
        let (status, mut summary) = loop {
            outcome.attempts += 1;

            if let Some(feedback) = feedback.take() {
                history.push(Message::user(feedback));
            }

            let run = match agent.run(&mut history, node_events, &mut control).await {
                Ok(run) => run,
                Err(err) => {
                    if self.retries_exhausted(outcome.attempts) {
                        break (SubtaskStatus::Failed, err.to_string());
                    }
                    feedback = Some(retry_message(
                        &format!("the previous attempt failed: {err}"),
                        "",
                        "",
                    ));
                    continue;
                }
            };

            outcome.tokens += run.usage.total();

            if run.reason == CompletionReason::Aborted {
                break (
                    SubtaskStatus::Failed,
                    "aborted before it finished".to_string(),
                );
            }

            match SubtaskResult::parse(&run.final_text) {
                Err(err) => {
                    if self.retries_exhausted(outcome.attempts) {
                        break (SubtaskStatus::Failed, err.to_string());
                    }
                    feedback = Some(retry_message(
                        &format!("the previous reply was rejected: {err}"),
                        &run.final_text,
                        "",
                    ));
                }
                Ok(result) => {
                    // A worker that reports itself blocked or only partly done
                    // has not finished the work, whatever shape the reply has.
                    // Marking such a node succeeded would let its dependents run
                    // on incomplete work and would report a run as green when it
                    // is not. The report is terminal rather than retried:
                    // re-asking the same worker the same question would spend
                    // tokens to hear the same answer.
                    if let Some(reason) = blocked_state(&result.state) {
                        let summary = format!("the node reported itself blocked: {reason}");
                        outcome.result = Some(result);
                        break (SubtaskStatus::Failed, summary);
                    }

                    match self.verifier.verify(node, &result, &root).await {
                        Err(err) => {
                            if self.retries_exhausted(outcome.attempts) {
                                break (SubtaskStatus::Failed, err.to_string());
                            }
                            feedback = Some(retry_message(
                                &format!("the previous result could not be verified: {err}"),
                                &run.final_text,
                                "",
                            ));
                        }
                        Ok(report) => {
                            let valid = report.valid;
                            let issues = render_issues(&report);
                            let rendered = serde_json::to_string(&result)
                                .unwrap_or_else(|_| run.final_text.clone());
                            outcome.result = Some(result);
                            outcome.verification = Some(report);

                            if valid {
                                let state = outcome
                                    .result
                                    .as_ref()
                                    .map(|result| result.state.clone())
                                    .unwrap_or_else(|| "done".to_string());
                                break (SubtaskStatus::Succeeded, state);
                            }
                            if self.retries_exhausted(outcome.attempts) {
                                break (
                                    SubtaskStatus::Failed,
                                    "the verifier rejected the result".to_string(),
                                );
                            }
                            feedback = Some(retry_message(
                                "the verifier rejected the previous result",
                                &rendered,
                                &issues,
                            ));
                        }
                    }
                }
            }
        };

        outcome.status = status;

        // The sub-agent's own conversation is persisted turn by turn, so its
        // session can be opened and replayed long after the run. A write that
        // fails only degrades the trace: the node's verdict is already decided,
        // and a missing record must not turn a success into a failure.
        if let Some(session) = session {
            for message in &history {
                if let Err(err) = self.memory.append(session, message).await {
                    tracing::warn!(
                        "failed to persist a turn of node `{}`'s session: {err}",
                        node.id
                    );
                }
            }
            // Pinned after the turns are durable, so the marker's ancestors
            // include everything the node produced: forking from it replays the
            // stage, not an empty session.
            self.pin_node_stage(session, &node.id, "end").await;
        }

        // A successful node's worktree is the only copy of its edits, so it is
        // kept for the caller to integrate from. A failed node has nothing worth
        // integrating, and its checkout and branch are cleaned up as before —
        // otherwise every failure would leave a checkout and a branch behind.
        if let Some(worktree) = worktree {
            if outcome.status == SubtaskStatus::Succeeded && self.config.keep_worktrees {
                let path = worktree.path().to_path_buf();
                let branch = worktree.branch().to_string();
                tracing::info!(
                    "node `{}` kept its worktree at `{}` on branch `{branch}`",
                    node.id,
                    path.display()
                );
                // The `SubtaskEnd` summary is what the CLI prints, so the location
                // of the work has to travel with it — a user must not have to
                // guess where their edits went.
                summary = format!(
                    "{summary} — kept at `{}` on branch `{branch}` (nothing merged automatically)",
                    path.display()
                );
                outcome.worktree = Some(WorktreeRef { path, branch });
            } else {
                worktree.remove();
            }
        }

        NodeRun {
            outcome,
            summary,
            announced: true,
        }
    }

    /// True once a node has used its initial attempt plus `max_retries` retries.
    fn retries_exhausted(&self, attempts: u32) -> bool {
        attempts > self.config.max_retries
    }

    fn create_worktree(&self, node: &TaskNode) -> Result<Worktree> {
        let _guard = self
            .worktree_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Worktree::create(&self.workspace_root, &node.id)
    }

    /// The node a run's sub-agent sessions fork from.
    ///
    /// The parent's head is used when it has one. A parent that has not been
    /// written to yet has no head, so a snapshot marker is pinned at its empty
    /// root: the marker belongs to the parent session — every child's
    /// `forked_from_session` therefore names the parent — but it has no turn
    /// ancestors, so the children inherit no conversation and a sub-agent's
    /// stored history holds its own turns alone.
    ///
    /// A parent that *has* been written to forks from its head instead, and the
    /// child inherits those turns: that is `branch`'s contract, and it is why the
    /// plan callers keep their session empty until the run is over.
    ///
    /// A memory that cannot be read or written degrades the run to unpersisted
    /// sub-agents; it never fails the run, because the node's verdict does not
    /// depend on its trace.
    async fn fork_point(&self, parent: SessionId) -> Option<ForkPoint> {
        let node = match self.memory.head(parent).await {
            Ok(Some(node)) => node,
            Ok(None) => match self.memory.snapshot(parent, "plan").await {
                Ok(node) => node,
                Err(err) => {
                    tracing::warn!("could not pin a fork point in session {parent}: {err}");
                    return None;
                }
            },
            Err(err) => {
                tracing::warn!("could not read the head of session {parent}: {err}");
                return None;
            }
        };
        Some(ForkPoint {
            session: parent,
            node,
        })
    }

    /// Forks the session one node's sub-agent runs on, labelled with the node id
    /// so the child is identifiable in the store.
    ///
    /// `None` means the sub-agent runs unpersisted — no parent session, or a
    /// store that refused the fork — which is reported by the node's events
    /// carrying no `subagent_session_id`.
    async fn subagent_session(
        &self,
        fork_point: Option<ForkPoint>,
        label: &str,
    ) -> Option<SessionId> {
        let fork_point = fork_point?;
        match self
            .memory
            .branch(fork_point.session, fork_point.node, Some(label))
            .await
        {
            Ok(session) => Some(session),
            Err(err) => {
                tracing::warn!("could not persist the session for node `{label}`: {err}");
                None
            }
        }
    }

    async fn recall_summary(&self, node: &TaskNode) -> String {
        // The scenario shapes the query: a sub-agent needs its own objective
        // *and* its node id, because prior attempts at this node are labelled
        // with the id while the objective reaches the surrounding work. A query
        // built from the raw objective alone can surface a sibling node's
        // material as if it were this one's.
        let scenario = RecallScenario::SubAgent {
            objective: node.objective.clone(),
            node_id: node.id.clone(),
        };
        let budget = RecallBudget {
            limit: 8,
            max_tokens: self.config.recall_tokens,
        };

        // A memory that cannot be read degrades the prompt; it must not fail the
        // node, because the node's own objective is the contract it is held to.
        match recall_for(self.memory.as_ref(), scenario, budget).await {
            Ok(hits) => hits
                .iter()
                .map(|hit| format!("- {}", hit.snippet))
                .collect::<Vec<_>>()
                .join("\n"),
            Err(err) => {
                tracing::warn!("recall for node `{}` failed: {err}", node.id);
                String::new()
            }
        }
    }

    /// Pins a stage marker at one boundary of a node, logging a failure.
    ///
    /// The label carries the node id and the boundary, so a run is traceable
    /// stage by stage and a later branch can fork from a stage rather than only
    /// from the run's end. A memory that cannot be written degrades the trace;
    /// it must not fail the node, because the node's verdict does not depend on
    /// it.
    async fn pin_node_stage(&self, session: SessionId, node_id: &str, boundary: &str) {
        let stage = node_stage(node_id, boundary);
        if let Err(err) = pin_stage(self.memory.as_ref(), session, &stage).await {
            tracing::warn!("could not pin the {boundary} stage of node `{node_id}`: {err}");
        }
    }
}

/// The node every sub-agent session of one run forks from.
#[derive(Debug, Clone, Copy)]
struct ForkPoint {
    session: SessionId,
    node: NodeId,
}

/// The lane one node's events belong to.
///
/// Filled in once the node's agent and session are known and before any of its
/// events can be sent, so the forwarder always finds it.
struct SubagentLane {
    agent_id: AgentId,
    session_id: Option<SessionId>,
}

/// A finished node plus the one-line summary its `SubtaskEnd` carries.
struct NodeRun {
    outcome: NodeOutcome,
    summary: String,
    /// True once `SubtaskStart` was announced for this node.
    announced: bool,
}

fn failed_run(
    node: &TaskNode,
    attempts: u32,
    tokens: u64,
    summary: impl Into<String>,
    announced: bool,
) -> NodeRun {
    NodeRun {
        outcome: NodeOutcome {
            id: node.id.clone(),
            status: SubtaskStatus::Failed,
            attempts,
            result: None,
            verification: None,
            tokens,
            worktree: None,
        },
        summary: summary.into(),
        announced,
    }
}

fn skipped_run(node: &TaskNode, summary: impl Into<String>) -> NodeRun {
    NodeRun {
        outcome: NodeOutcome::skipped(&node.id),
        summary: summary.into(),
        announced: false,
    }
}

fn build_prompt(
    node: &TaskNode,
    dependencies: &[(String, String, String)],
    recall: &str,
    guidance: Option<&str>,
) -> String {
    let mut prompt = format!(
        "Objective:\n{}\n\n{}",
        node.objective,
        SubtaskResult::INSTRUCTION
    );

    // The workflow's own preamble, when this graph came from one. It is
    // guidance for the run as a whole, so every node of that run sees it; a
    // free-form plan has none and adds nothing.
    if let Some(guidance) = guidance.map(str::trim).filter(|text| !text.is_empty()) {
        prompt.push_str("\n\nHow this run must be carried out:\n");
        prompt.push_str(guidance);
        prompt.push('\n');
    }

    if !dependencies.is_empty() {
        prompt.push_str("\n\nFinished work this task depends on:\n");
        for (id, objective, state) in dependencies {
            prompt.push_str(&format!("- `{id}`: {objective}\n  state: {state}\n"));
        }
    }

    if !node.files.is_empty() {
        prompt.push_str(&format!(
            "\nFiles this task claims: {}\n",
            node.files.join(", ")
        ));
    }

    if !recall.trim().is_empty() {
        prompt.push_str("\nRelevant memory from earlier sessions:\n");
        prompt.push_str(recall);
        prompt.push('\n');
    }

    prompt
}

/// The reason a worker's `state` field reports the node as not finished.
///
/// [`SubtaskResult::INSTRUCTION`] spells the answers out as `done`, `partly
/// done` or `blocked`, so the first word of the field is the verdict and the
/// rest is the explanation. Only that first word is read: a `done` whose prose
/// mentions something being blocked is still done, and a summary that starts
/// with a different word is left to the verifier.
fn blocked_state(state: &str) -> Option<String> {
    let trimmed = state.trim();
    let verdict = trimmed
        .split(|c: char| {
            c.is_whitespace() || matches!(c, ',' | ';' | ':' | '.' | '!' | '?' | '—' | '-')
        })
        .find(|word| !word.is_empty())
        .unwrap_or("")
        .to_ascii_lowercase();

    let blocked = matches!(
        verdict.as_str(),
        "blocked"
            | "block"
            | "stuck"
            | "partial"
            | "partially"
            | "partly"
            | "incomplete"
            | "unfinished"
            | "cannot"
            | "can't"
            | "cant"
    );
    blocked.then(|| trimmed.to_string())
}

/// The structured issues a rejected result is retried against.
fn render_issues(report: &VerificationReport) -> String {
    let mut lines = Vec::new();
    for check in &report.checks {
        if !check.passed {
            lines.push(format!("- check `{}` failed: {}", check.name, check.detail));
        }
    }
    for issue in &report.issues {
        lines.push(format!(
            "- [{}] {}: {}",
            severity_label(issue.severity),
            issue.summary,
            issue.evidence
        ));
    }
    if lines.is_empty() {
        lines.push("- the verifier gave no detail".to_string());
    }
    lines.join("\n")
}

fn severity_label(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Warning => "warning",
        Severity::Error => "error",
    }
}

fn retry_message(reason: &str, quoted: &str, issues: &str) -> String {
    let quoted = if quoted.chars().count() > MAX_QUOTED_REPLY {
        let head: String = quoted.chars().take(MAX_QUOTED_REPLY).collect();
        format!("{head}\n… (truncated)")
    } else {
        quoted.to_string()
    };

    let mut message = format!("{reason}.");
    if !quoted.trim().is_empty() {
        message.push_str(&format!("\n\nPrevious reply:\n{quoted}"));
    }
    if !issues.trim().is_empty() {
        message.push_str(&format!("\n\nIssues:\n{issues}"));
    }
    message.push_str(&format!(
        "\n\n{}\n\nReply again with a corrected JSON object only.",
        SubtaskResult::INSTRUCTION
    ));
    message
}

/// Records the lane a node's events carry. The first call wins; there is only
/// ever one per node.
fn set_lane(lane: &Arc<OnceLock<SubagentLane>>, agent_id: AgentId, session_id: Option<SessionId>) {
    let _ = lane.set(SubagentLane {
        agent_id,
        session_id,
    });
}

/// Tags one node's events with the node's lane and forwards them.
async fn forward_node_events(
    mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    lane: Arc<OnceLock<SubagentLane>>,
    out: mpsc::UnboundedSender<RoutedEvent>,
) {
    while let Some(event) = rx.recv().await {
        let routed = match lane.get() {
            Some(lane) => RoutedEvent::from_subagent(lane.agent_id.clone(), lane.session_id, event),
            // Unreachable in practice — the lane is set before the first event
            // is sent — but the orchestrator's lane is the safe default.
            None => RoutedEvent::orchestrator(event),
        };
        if out.send(routed).is_err() {
            return;
        }
    }
}

/// Sends one event on a node's own channel, to be tagged on the way out.
fn send_node(events: &mpsc::UnboundedSender<AgentEvent>, event: AgentEvent) {
    // A vanished consumer must not bring the run down with it.
    let _ = events.send(event);
}

/// Sends one event the orchestrator itself produced, on the shared stream.
fn emit(events: &mpsc::UnboundedSender<RoutedEvent>, event: AgentEvent) {
    let _ = events.send(RoutedEvent::orchestrator(event));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{CheckOutcome, Issue};

    fn node(id: &str, objective: &str) -> TaskNode {
        TaskNode {
            id: id.into(),
            objective: objective.into(),
            agent: None,
            depends_on: Vec::new(),
            files: Vec::new(),
            verify: Vec::new(),
        }
    }

    #[test]
    fn the_prompt_carries_the_objective_the_contract_and_the_dependencies() {
        let mut node = node("b", "wire the parser in");
        node.depends_on = vec!["a".into()];
        node.files = vec!["src/lib.rs".into()];
        let dependencies = vec![(
            "a".to_string(),
            "write the parser".to_string(),
            "done".to_string(),
        )];

        let prompt = build_prompt(&node, &dependencies, "- an earlier decision", None);

        assert!(prompt.contains("wire the parser in"));
        assert!(prompt.contains("\"objective\""));
        assert!(prompt.contains("\"boundary\""));
        assert!(prompt.contains("write the parser"));
        assert!(prompt.contains("state: done"));
        assert!(prompt.contains("src/lib.rs"));
        assert!(prompt.contains("an earlier decision"));
    }

    #[test]
    fn a_workflows_guidance_reaches_every_nodes_prompt() {
        let with = build_prompt(&node("a", "do a"), &[], "", Some("Verify before you edit."));
        assert!(with.contains("How this run must be carried out:"), "{with}");
        assert!(with.contains("Verify before you edit."), "{with}");

        // A free-form plan carries no guidance, and blank guidance is nothing.
        let without = build_prompt(&node("a", "do a"), &[], "", None);
        assert!(!without.contains("How this run"), "{without}");
        let blank = build_prompt(&node("a", "do a"), &[], "", Some("   "));
        assert!(!blank.contains("How this run"), "{blank}");
    }

    #[test]
    fn a_blocked_or_partly_done_state_is_not_success() {
        assert!(blocked_state("blocked").is_some());
        assert!(blocked_state("Blocked: the API key is missing").is_some());
        assert!(blocked_state("partly done — the parser compiles").is_some());
        assert!(blocked_state("stuck on the fixture").is_some());
        assert!(blocked_state("cannot proceed without network").is_some());

        // A finished node whose prose mentions a blocker is still finished.
        assert!(blocked_state("done; nothing was blocked").is_none());
        assert!(blocked_state("done").is_none());
        assert!(blocked_state("succeeded").is_none());
        assert!(blocked_state("   ").is_none());
    }

    #[test]
    fn an_empty_recall_adds_nothing_to_the_prompt() {
        let prompt = build_prompt(&node("a", "do a"), &[], "   ", None);
        assert!(!prompt.contains("Relevant memory"));
        assert!(!prompt.contains("Finished work"));
    }

    #[test]
    fn the_retry_message_quotes_the_previous_reply_the_issues_and_the_reason() {
        let message = retry_message(
            "the verifier rejected the previous result",
            "{\"state\":\"x\"}",
            "- [error] no test covers the new branch: src/parse.rs",
        );
        assert!(message.contains("the verifier rejected the previous result"));
        assert!(message.contains("{\"state\":\"x\"}"));
        assert!(message.contains("no test covers the new branch"));
        assert!(message.contains("\"boundary\""));
    }

    #[test]
    fn a_very_long_previous_reply_is_truncated() {
        let long = "x".repeat(MAX_QUOTED_REPLY + 500);
        let message = retry_message("rejected", &long, "");
        assert!(message.contains("truncated"));
        assert!(
            !message.contains(&"x".repeat(MAX_QUOTED_REPLY + 1)),
            "the quoted reply must be cut short"
        );
    }

    #[test]
    fn a_report_with_no_detail_still_produces_feedback() {
        let report = VerificationReport {
            valid: false,
            checks: Vec::new(),
            issues: Vec::new(),
        };
        assert_eq!(render_issues(&report), "- the verifier gave no detail");
    }

    #[test]
    fn failed_checks_and_issues_are_both_rendered() {
        let report = VerificationReport {
            valid: false,
            checks: vec![CheckOutcome {
                name: "cargo check".into(),
                passed: false,
                detail: "exit 101; 1 error(s), 0 warning(s)".into(),
            }],
            issues: vec![Issue {
                severity: Severity::Error,
                summary: "`cargo check` failed".into(),
                evidence: "error[E0425]: cannot find value `x`".into(),
            }],
        };
        let rendered = render_issues(&report);
        assert!(rendered.contains("cargo check"), "{rendered}");
        assert!(rendered.contains("[error]"), "{rendered}");
        assert!(rendered.contains("E0425"), "{rendered}");
    }
}
