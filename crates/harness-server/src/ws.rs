//! The WebSocket bus.
//!
//! One connection is one reader loop plus one writer task. The reader owns the
//! socket and the control handles for the session it started; the writer owns
//! the sink, drains a bounded queue, and pings. They end together: whichever
//! stops first gives the other a reason to stop, so no task outlives its socket.
//!
//! Events take one path only. The run task's bridge broadcasts an event onto the
//! session's fan-out, and every attached connection has a forwarder pulling from
//! it into that connection's own bounded queue. Putting the queue between the
//! broadcast and the socket is what stops a stalled client from stalling the
//! run — see [`crate::backpressure`] for what happens when the queue fills.
//!
//! A connection drives sessions, and a session runs one turn at a time. A
//! message for a session with a run in flight is queued on that session rather
//! than refused; when the run ends the session dequeues the next message and
//! starts it. A frame may name the session it addresses, so one connection can
//! drive several sessions — including ones another connection opened — and two
//! sessions run concurrently on their own histories.

use std::net::IpAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::{SinkExt, StreamExt};
use harness_agent::{ControlChannel, MemoryRecorder, ModelRouter};
use harness_core::{
    AgentEvent, AgentId, ClientEnvelope, ClientMessage, CompletionReason, Config, HarnessError,
    Message, PermissionMode, PlanNode, Result, RoutedEvent, ServerEnvelope, ServerMessage,
    SessionId, SubtaskStatus, PROTOCOL_VERSION,
};
use harness_orchestrator::{
    ExecutionReport, Executor, ExecutorConfig, Planner, TaskGraph, TaskNode,
};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

use crate::backpressure::{self, Delivery, Outbox};
use crate::state::{
    Admit, JobAdmit, NextRun, ResolvedProvider, ServerState, SessionRecord, WorkflowJob,
};

/// Ping period for the heartbeat.
///
/// An idle WebSocket through a proxy or NAT is a connection the middlebox is
/// entitled to reclaim; a ping every fifteen seconds keeps it visibly alive.
const HEARTBEAT: Duration = Duration::from_secs(15);

/// How long a finished connection waits for the writer to flush a close frame
/// before the task is cancelled. A peer that stopped reading must not pin this
/// task on a full socket buffer.
const WRITER_DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Ceiling on a socket-driven plan, mirroring `harness plan`: a plan that
/// explodes into dozens of nodes spends more tokens coordinating than working.
const MAX_PLAN_NODES: usize = 12;

/// The query parameters the upgrade understands.
///
/// `token` is the one a browser can use: the WebSocket API cannot set headers
/// on the handshake, so a page has to carry the shared secret in the URL.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
pub(crate) struct UpgradeParams {
    pub(crate) token: Option<String>,
}

/// Upgrades an HTTP request to the bus.
///
/// Two gates stand in front of the socket, and both exist for the same reason:
/// this bus can drive the `shell` tool, so anything that reaches it runs
/// commands on the machine. The `Origin` check keeps a page in the user's
/// browser from becoming the attacker's proxy; the token keeps a client off a
/// bus the operator deliberately exposed to a network. Either refusal happens
/// before `on_upgrade`, so a refused request never becomes a socket.
pub(crate) async fn upgrade(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    Query(params): Query<UpgradeParams>,
    ws: WebSocketUpgrade,
) -> Response {
    if let Err(reason) = origin_allowed(&headers) {
        tracing::warn!("refused a WebSocket upgrade: {reason}");
        return refusal(StatusCode::FORBIDDEN, &reason);
    }

    if let Err(reason) = token_allowed(&state, &headers, params.token.as_deref()) {
        tracing::warn!("refused a WebSocket upgrade: {reason}");
        return refusal(StatusCode::UNAUTHORIZED, &reason);
    }

    ws.on_upgrade(move |socket| connection(socket, state))
}

/// The refusal body a client sees.
///
/// Shared with the REST surface so a caller that is refused sees the same shape
/// whichever door it knocked on.
pub(crate) fn refusal(status: StatusCode, reason: &str) -> Response {
    (status, Json(serde_json::json!({ "error": reason }))).into_response()
}

/// Whether a WebSocket upgrade may proceed, judged by the request's headers.
///
/// A browser attaches `Origin` to every WebSocket handshake, and the page that
/// opens the socket can be any page the user visits — an ad, a link, a tab left
/// open. The browser is then the confused deputy: the attacker never has to
/// reach this machine, only to get a page in front of someone running the
/// server. So an `Origin` that is not this server's own is refused.
///
/// A missing `Origin` is allowed, deliberately. Non-browser clients — the CLI,
/// the tests, any script — do not send one, and a page in a browser cannot
/// suppress it, so the header's absence is not the attack; refusing it would
/// break every legitimate non-browser client while stopping nothing.
fn origin_allowed(headers: &HeaderMap) -> std::result::Result<(), String> {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return Ok(());
    };

    let origin = origin
        .to_str()
        .map_err(|_| "the Origin header is not valid text".to_string())?;
    let parsed = origin
        .parse::<Uri>()
        .map_err(|_| format!("`{origin}` is not a valid origin"))?;

    // `Origin: null` — a sandboxed frame, a `file://` page, a cross-origin
    // redirect — is exactly the shape a drive-by arrives in. `Uri` reads it as
    // an authority-form URI, so what marks it out is the missing scheme: no
    // page is served from an origin spelled that way.
    if parsed.scheme_str().is_none() {
        return Err(format!(
            "`{origin}` is not a full origin (it names no scheme)"
        ));
    }
    let Some(authority) = parsed.authority() else {
        return Err(format!("`{origin}` names no origin"));
    };
    let origin_host = bare_host(
        parsed
            .host()
            .ok_or_else(|| format!("`{origin}` names no host"))?,
    )
    .to_ascii_lowercase();
    let origin_port = parsed
        .port_u16()
        .or_else(|| default_port(parsed.scheme_str()));

    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return Err("the request names no Host to compare the Origin against".to_string());
    };
    let (host_name, host_port) = split_authority(host);
    let host_name = host_name.to_ascii_lowercase();

    // Same host, and the same port: the origin may leave implicit a port the
    // Host spells out, but a port the two disagree about is a different origin.
    if origin_host == host_name && (host_port.is_none() || host_port == origin_port) {
        return Ok(());
    }

    // Both sides on loopback: a page served from this machine talking to a
    // server on this machine. That is the local dev proxy — Vite on 5173
    // forwarding `/ws` to 8787 rewrites `Host` but not `Origin` — and it is not
    // the drive-by case, which always arrives with a remote origin. A rebinding
    // attack still fails here, because the Origin's *name* is what is compared
    // and never what that name resolves to.
    if is_loopback_host(&origin_host) && is_loopback_host(&host_name) {
        return Ok(());
    }

    Err(format!(
        "`{origin}` is not this server's origin (`{authority}`); open the UI from the address the server was started on"
    ))
}

/// Whether the request carries the token this server requires, if any.
///
/// The token is only set when the operator asked for a non-loopback bind, so an
/// ordinary local run is unchanged. Three spellings are accepted because the
/// clients differ: a browser can only use the query string, a script usually
/// prefers a header, and `Authorization: Bearer` is what a generic HTTP client
/// reaches for first.
pub(crate) fn token_allowed(
    state: &ServerState,
    headers: &HeaderMap,
    query: Option<&str>,
) -> std::result::Result<(), String> {
    let Some(expected) = state.config().server.token.as_deref() else {
        return Ok(());
    };

    let presented = query
        .map(str::to_string)
        .or_else(|| header_value(headers, "x-harness-token"))
        .or_else(|| bearer(headers));

    match presented.as_deref() {
        Some(token) if token == expected => Ok(()),
        Some(_) => Err("the token presented is not the one this server requires".to_string()),
        None => Err(
            "this server requires a token; pass it as `?token=`, the `X-Harness-Token` header, \
             or `Authorization: Bearer`"
                .to_string(),
        ),
    }
}

fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = header_value(headers, header::AUTHORIZATION.as_str())?;
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))?;
    Some(token.trim().to_string())
}

fn default_port(scheme: Option<&str>) -> Option<u16> {
    match scheme {
        Some("http") | Some("ws") => Some(80),
        Some("https") | Some("wss") => Some(443),
        _ => None,
    }
}

/// Strips the brackets `Uri::host` keeps on an IPv6 literal, so both sides of
/// the comparison are spelled the same way.
fn bare_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// Splits `host[:port]`, keeping an IPv6 literal in brackets intact.
fn split_authority(authority: &str) -> (&str, Option<u16>) {
    let authority = authority.trim();
    if let Some(rest) = authority.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => (
                host,
                tail.strip_prefix(':').and_then(|port| port.parse().ok()),
            ),
            None => (authority, None),
        };
    }
    match authority.rsplit_once(':') {
        Some((host, port)) => (host, port.parse::<u16>().ok()),
        None => (authority, None),
    }
}

/// Whether a host names this machine. `localhost` counts because it is what a
/// browser on this machine is most likely to be using.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

async fn connection(socket: WebSocket, state: Arc<ServerState>) {
    let (sink, stream) = socket.split();
    let (tx, rx) = backpressure::channel(state.policy());
    let (close_tx, mut close_rx) = mpsc::channel::<()>(1);

    let mut writer = tokio::spawn(writer_loop(rx, sink, close_tx.clone()));

    let mut connection = Connection {
        state,
        tx,
        close: close_tx,
        forwarders: Vec::new(),
        attached: Vec::new(),
        active: None,
        aborted: Arc::new(AtomicBool::new(false)),
    };
    connection.run(stream, &mut close_rx).await;
    connection.shutdown();
    drop(connection);

    match tokio::time::timeout(WRITER_DRAIN_GRACE, &mut writer).await {
        Ok(Ok(())) => {}
        Ok(Err(err)) => tracing::debug!("writer task ended abnormally: {err}"),
        Err(_elapsed) => {
            writer.abort();
            let _ = writer.await;
        }
    }
}

struct Connection {
    state: Arc<ServerState>,
    /// The bounded queue the writer task drains — the backpressure point.
    tx: mpsc::Sender<ServerEnvelope>,
    /// Signals the reader that the connection has to end.
    close: mpsc::Sender<()>,
    forwarders: Vec<JoinHandle<()>>,
    attached: Vec<SessionId>,
    /// The session this connection opened; frames that name no session address
    /// it. A frame may still name another session to reach it.
    active: Option<Arc<SessionRecord>>,
    /// Set when an abort reaches this connection's plan. A plan's executor
    /// reports the interruption node by node and returns normally, so this is
    /// how the plan task learns which `Done` the client should see.
    aborted: Arc<AtomicBool>,
}

impl Connection {
    async fn run(
        &mut self,
        mut stream: futures::stream::SplitStream<WebSocket>,
        close_rx: &mut mpsc::Receiver<()>,
    ) {
        loop {
            tokio::select! {
                _ = close_rx.recv() => break,

                frame = stream.next() => match frame {
                    Some(Ok(WsMessage::Text(text))) => {
                        if !self.dispatch(text.as_str()).await {
                            break;
                        }
                    }
                    Some(Ok(WsMessage::Binary(_))) => {
                        // The protocol is text-only, and a binary frame cannot be
                        // parsed, so it is refused rather than silently ignored.
                        if !self.fail("bad_message", "this socket speaks text frames only") {
                            break;
                        }
                    }
                    // axum answers pings and close frames for us; anything else
                    // is a frame this connection has no use for.
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        tracing::debug!("websocket read failed: {err}");
                        break;
                    }
                    None => break,
                },
            }
        }
    }

    /// Returns `false` when the connection can no longer be served.
    async fn dispatch(&mut self, raw: &str) -> bool {
        let envelope: ClientEnvelope = match parse_client(raw) {
            Ok(envelope) => envelope,
            // The offending frame is not echoed: it is client-controlled text.
            // Its session id, if it had a readable one, is named in the message.
            Err(err) => return self.fail(err.code, &err.message),
        };

        match envelope.message {
            ClientMessage::UserMessage {
                text,
                agent_id,
                effort,
                permission_mode,
            } => {
                self.user_message(envelope.session_id, agent_id, text, effort, permission_mode)
                    .await
            }
            ClientMessage::PlanTask { task, agent_id } => self.plan_task(agent_id, task).await,
            ClientMessage::QueueWorkflow {
                task,
                workflow,
                agent_id,
            } => self.queue_workflow(agent_id, task, workflow).await,
            ClientMessage::SteeringMessage { text, priority } => {
                let record = match self.resolve_control(envelope.session_id) {
                    Ok(record) => record,
                    Err(alive) => return alive,
                };
                if record.steer(text, priority) {
                    true
                } else {
                    self.not_running()
                }
            }
            ClientMessage::Abort { reason } => {
                let record = match self.resolve_control(envelope.session_id) {
                    Ok(record) => record,
                    Err(alive) => return alive,
                };
                let is_active = self
                    .active
                    .as_ref()
                    .is_some_and(|active| active.session_id() == record.session_id());
                if record.abort(reason) {
                    if is_active {
                        self.aborted.store(true, Ordering::SeqCst);
                    }
                    true
                } else {
                    self.not_running()
                }
            }
            ClientMessage::ToolApproval {
                tool_call_id,
                approved,
                reason,
            } => {
                let record = match self.resolve_control(envelope.session_id) {
                    Ok(record) => record,
                    Err(alive) => return alive,
                };
                if record.approve(tool_call_id, approved, reason) {
                    true
                } else {
                    self.not_running()
                }
            }
            // Same lookup as a message: a stored session may be watched by a
            // connection that never opened it, and this process need not be the
            // one that did.
            ClientMessage::Subscribe { session_id } => {
                match self.state.session_or_load(session_id).await {
                    Ok(Some(record)) => {
                        self.attach(record);
                        true
                    }
                    Ok(None) => self.unknown_session(session_id),
                    Err(err) => self.fail_on(session_id, error_code(&err), &err.to_string()),
                }
            }
            ClientMessage::Ping => self.reply(ServerMessage::Pong, envelope.session_id),
        }
    }

    /// Resolves the session a control frame addresses, answering the client
    /// when there is none.
    ///
    /// `Err(alive)` carries the value the refusal returned, so the caller can
    /// pass it straight back: `false` means the reply could not be queued and
    /// the connection should end.
    /// Control frames stay on the in-memory map: steering, aborting or approving
    /// only means something for a run in flight, and a run in flight is always a
    /// session this process opened. A named session that is not in the map has
    /// nothing to control, and is reported as such under its own id.
    fn resolve_control(
        &self,
        named: Option<SessionId>,
    ) -> std::result::Result<Arc<SessionRecord>, bool> {
        let target = match named {
            Some(session_id) => self.state.session(session_id),
            None => self.active.clone(),
        };
        match target {
            Some(record) => Ok(record),
            None => match named {
                Some(session_id) => Err(self.unknown_session(session_id)),
                None => Err(self.not_running()),
            },
        }
    }

    /// Serves a user message, addressing the session it names when it names one.
    async fn user_message(
        &mut self,
        target: Option<SessionId>,
        agent_id: Option<AgentId>,
        text: String,
        effort: Option<String>,
        permission_mode: Option<String>,
    ) -> bool {
        let record = match target {
            // A message that names a session addresses that session, whether or
            // not this connection was the one that opened it. A session from an
            // earlier process is loaded from the store and continued, not forked.
            Some(session_id) => match self.state.session_or_load(session_id).await {
                Ok(Some(record)) => record,
                Ok(None) => return self.unknown_session(session_id),
                Err(err) => return self.fail_on(session_id, error_code(&err), &err.to_string()),
            },
            None => {
                if self.active.is_none() && !self.start_session(agent_id).await {
                    return false;
                }
                match self.active.clone() {
                    Some(record) => record,
                    // Unreachable: `start_session` either sets `active` or fails.
                    None => return false,
                }
            }
        };

        // Resolved before the message is admitted, so a queued message keeps
        // the level it asked for rather than reading the config again when its
        // turn comes. An unrecognised value is skipped by `resolve`, never sent.
        let effort = {
            let agent = record.agent();
            let declared = agent.config().reasoning_effort.as_deref();
            self.state
                .config()
                .thinking
                .resolve(effort.as_deref(), declared)
                .to_string()
        };

        // The tier for this run, resolved the same way: the message's request,
        // the agent's declaration, the configured default, then ask-when-needed,
        // which is this path's own default because a UI is attached and can
        // answer. An unrecognised request on the message is ignored rather than
        // sent, so a client the server does not understand cannot widen access.
        let permission = permission_mode
            .as_deref()
            .and_then(PermissionMode::parse)
            .unwrap_or(record.agent().config().permission_mode);

        // Attached before the message is admitted, so a run it starts cannot
        // emit an event this connection would miss.
        self.attach(Arc::clone(&record));

        match record.admit(text, Some(effort), Some(permission)) {
            Admit::Started {
                text,
                effort,
                permission,
                control,
            } => {
                spawn_run(
                    Arc::clone(&self.state),
                    record,
                    text,
                    effort,
                    permission,
                    control,
                    Arc::clone(&self.aborted),
                );
                true
            }
            Admit::Queued { text, position } => {
                self.reply_on(&record, ServerMessage::MessageQueued { text, position })
            }
            Admit::Full { limit } => self.reply_on(
                &record,
                ServerMessage::Error {
                    code: "queue_full".to_string(),
                    message: format!(
                        "this session already has {limit} messages waiting; wait for one to run"
                    ),
                },
            ),
        }
    }

    /// Decomposes a task into a DAG and runs it, streaming the orchestrator's
    /// events to this connection.
    ///
    /// A `plan_task` is refused while anything is in flight: it is not a queued
    /// user message, and queueing one would make the session's single `done`
    /// ambiguous about which unit of work finished.
    async fn plan_task(&mut self, agent_id: Option<AgentId>, task: String) -> bool {
        if self.active.is_none() && !self.start_session(agent_id).await {
            return false;
        }
        let Some(record) = self.active.clone() else {
            return false;
        };
        let Some(control) = record.begin_run() else {
            return self.fail("busy", "a run is already in flight on this session");
        };
        // Cleared so an abort of an earlier run cannot colour this one.
        self.aborted.store(false, Ordering::SeqCst);
        spawn_plan(
            Arc::clone(&self.state),
            record,
            task,
            control,
            Arc::clone(&self.aborted),
        );
        true
    }

    /// Enqueues a workflow job on this session's workflow queue.
    ///
    /// Unlike a `plan_task`, a job is never refused for being busy — waiting is
    /// the point of a queue. It starts at once when the session is idle, or is
    /// appended behind the running job when it is not; only a full workflow
    /// queue is refused. A queued *user message* does not affect this decision:
    /// the two queues are separate lists and each reports its own position.
    async fn queue_workflow(
        &mut self,
        agent_id: Option<AgentId>,
        task: String,
        workflow: Option<String>,
    ) -> bool {
        if self.active.is_none() && !self.start_session(agent_id).await {
            return false;
        }
        let Some(record) = self.active.clone() else {
            return false;
        };
        // Attached before the job is admitted, so a job that starts a run cannot
        // emit an event this connection would miss.
        self.attach(Arc::clone(&record));

        match record.enqueue_job(task, workflow) {
            JobAdmit::Started { job, control } => {
                // Cleared so an abort of an earlier run cannot colour this one.
                self.aborted.store(false, Ordering::SeqCst);
                spawn_workflow_job(
                    Arc::clone(&self.state),
                    record,
                    job,
                    control,
                    Arc::clone(&self.aborted),
                );
                true
            }
            JobAdmit::Queued { job, position } => self.reply_on(
                &record,
                ServerMessage::WorkflowQueued {
                    job_id: job.id,
                    task: job.task,
                    workflow_id: job.workflow,
                    position,
                },
            ),
            JobAdmit::Full { limit } => self.reply_on(
                &record,
                ServerMessage::Error {
                    code: "workflow_queue_full".to_string(),
                    message: format!(
                        "this session already has {limit} workflow jobs waiting; wait for one to run"
                    ),
                },
            ),
        }
    }

    /// Opens this connection's session and attaches to it.
    ///
    /// Returns `false` when the session could not be opened, the client having
    /// already been told why. `SessionStarted` is not sent here: it opens every
    /// run, so the run task emits it.
    async fn start_session(&mut self, agent_id: Option<AgentId>) -> bool {
        match self
            .state
            .open_session(agent_id.as_ref().map(AgentId::as_str))
            .await
        {
            Ok(opened) => {
                // One line per session, never per message: the operator can see
                // whether this session is screened without a transcript of it.
                if let Some(pipeline) = opened.agent.config().guardrails.as_deref() {
                    tracing::info!(
                        session = %opened.record.session_id(),
                        guards = ?pipeline.names(),
                        "guardrails active for this session"
                    );
                }
                let record = opened.record;
                self.attach(Arc::clone(&record));
                self.active = Some(record);
                true
            }
            Err(err) => self.fail(error_code(&err), &err.to_string()),
        }
    }

    /// Starts forwarding one session's events to this connection.
    fn attach(&mut self, record: Arc<SessionRecord>) {
        let session_id = record.session_id();
        if self.attached.contains(&session_id) {
            return;
        }
        self.attached.push(session_id);

        let events = record.subscribe();
        let outbox = Outbox::new(
            self.tx.clone(),
            ServerEnvelope::new(
                session_id,
                record.agent_id().clone(),
                ServerMessage::Error {
                    code: "backpressure".to_string(),
                    message: "this subscriber stopped reading".to_string(),
                },
            ),
            self.state.policy().clone(),
        );
        self.forwarders.push(tokio::spawn(forward_events(
            events,
            outbox,
            self.close.clone(),
        )));
    }

    fn shutdown(&mut self) {
        for handle in self.forwarders.drain(..) {
            handle.abort();
        }

        if let Some(record) = self.active.as_ref() {
            // A client that vanished must not leave the model burning tokens. The
            // abort is cooperative, so tools still get their cleanup grace, and
            // the run task is left to finish on its own: it owns the memory write
            // that makes the conversation outlive the socket. Both queues go with
            // the client: those messages and jobs were submitted by a connection
            // that is no longer there to receive their results. An explicit
            // `abort` frame is different — it leaves both queues to run.
            let dropped = record.abort_and_clear(Some("connection closed".to_string()));
            if dropped > 0 {
                tracing::debug!(
                    session = %record.session_id(),
                    dropped,
                    "dropped queued messages and jobs when the connection closed"
                );
            }
        }
    }

    fn not_running(&self) -> bool {
        self.fail("not_running", "no run is in flight on this connection")
    }

    fn fail(&self, code: &str, message: &str) -> bool {
        self.reply(
            ServerMessage::Error {
                code: code.to_string(),
                message: message.to_string(),
            },
            None,
        )
    }

    /// Refuses a frame that named a session which does not exist.
    ///
    /// Attributed to the session the client asked about, not to this
    /// connection's own: a client files an error under the conversation it
    /// addressed, so an error for session B must not arrive under session A.
    fn unknown_session(&self, session_id: SessionId) -> bool {
        self.fail_on(
            session_id,
            "unknown_session",
            &format!("no session `{session_id}` on this server"),
        )
    }

    /// A refusal attributed to the session the frame named.
    ///
    /// The agent is the server's own: the requested session is by definition not
    /// resolved here, so there is no agent to name.
    fn fail_on(&self, session_id: SessionId, code: &str, message: &str) -> bool {
        let envelope = ServerEnvelope::new(
            session_id,
            self.state.server_agent(),
            ServerMessage::Error {
                code: code.to_string(),
                message: message.to_string(),
            },
        );

        self.tx.try_send(envelope).is_ok()
    }

    /// Direct replies bypass the backpressure policy: they answer a frame the
    /// client just sent, so they are small and the client is demonstrably awake.
    /// A reply that cannot be queued therefore means the writer is gone.
    fn reply(&self, message: ServerMessage, session_hint: Option<SessionId>) -> bool {
        let envelope = match self.active.as_ref() {
            Some(active) => {
                ServerEnvelope::new(active.session_id(), active.agent_id().clone(), message)
            }
            // Before a session exists there is nothing to route, so the envelope
            // is attributed to the server itself.
            None => ServerEnvelope::new(
                session_hint.unwrap_or_default(),
                self.state.server_agent(),
                message,
            ),
        };

        self.tx.try_send(envelope).is_ok()
    }

    /// A reply attributed to the session it answers, which need not be the one
    /// this connection opened.
    fn reply_on(&self, record: &SessionRecord, message: ServerMessage) -> bool {
        let envelope = ServerEnvelope::new(record.session_id(), record.agent_id().clone(), message);

        self.tx.try_send(envelope).is_ok()
    }
}

/// The frame that opens a run.
///
/// Built from the record so every run reports the same routing context, whether
/// it was started by a message, dequeued from the queue, or is a plan.
fn session_started(record: &SessionRecord) -> ServerMessage {
    ServerMessage::SessionStarted {
        session_id: record.session_id(),
        agent_id: record.agent_id().clone(),
        model: record.model().to_string(),
    }
}

/// Runs one turn of an open session and persists the result.
///
/// The session has already admitted this message — its control handle is
/// registered and it counts as running — so the caller passes the matching
/// [`ControlChannel`]. Spawned rather than awaited: the reader loop has to keep
/// consuming inbound frames (steering, aborts) while the run is in flight.
///
/// When the turn ends the session's queues are drained, and the next waiting
/// unit — a message first, a workflow job only if no message waits — starts a
/// run of its own. That is what makes a queue a promise: a client submits
/// several messages or jobs and they run in order without polling.
pub(crate) fn spawn_run(
    state: Arc<ServerState>,
    record: Arc<SessionRecord>,
    text: String,
    effort: Option<String>,
    permission: Option<PermissionMode>,
    mut control: ControlChannel,
    aborted: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        // The sink is unbounded on purpose, as the kernel documents: the loop must
        // never block on its consumer. Backpressure is applied one hop later, in
        // the forwarder that owns the connection's bounded queue.
        let (events, receiver) = mpsc::unbounded_channel::<AgentEvent>();
        // The loop owns the end of this stream, so its `Done` is forwarded.
        let bridge = tokio::spawn(bridge_events(receiver, Arc::clone(&record), true));

        // Sent per run, not per session: a client with messages queued reads it
        // as "the next message is now the active turn".
        record.broadcast(session_started(&record));

        let session_id = record.session_id();
        let agent = record.agent();
        let mut history = record.history.lock().await;
        // Only the turns this run adds are persisted. A session continued from
        // the store already owns its earlier history, and appending the whole
        // vector again would duplicate the conversation in the DAG.
        let persisted = history.len();
        history.push(Message::user(text));

        // Persisted turn by turn rather than once at the end: an abort, a
        // provider error or a killed process must not lose the whole run. The
        // loop hands each turn to the recorder as it completes, so a crash can
        // cost at most the turn in flight. Nothing is appended afterwards — the
        // recorder already wrote these messages, and a second pass would
        // duplicate them in the DAG.
        let recorder = MemoryRecorder::new(Arc::clone(state.memory()), session_id);
        let outcome = agent
            .run_persisting_with_effort(
                &mut history,
                &events,
                &mut control,
                Some(&recorder),
                persisted,
                effort.as_deref(),
                permission,
            )
            .await;
        drop(events);
        if let Err(err) = bridge.await {
            tracing::warn!("event bridge stopped early: {err}");
        }

        if let Err(err) = outcome {
            // Every normal return already emitted `Done` from inside the loop, so
            // only the failure path has to close the stream itself.
            record.broadcast(ServerMessage::Error {
                code: "agent_error".to_string(),
                message: err.to_string(),
            });
            record.broadcast(ServerMessage::Done {
                reason: CompletionReason::Error,
            });
        }

        drop(history);
        // Ends this run and, atomically, admits the next waiting unit — so
        // anything arriving right now cannot jump a queue.
        spawn_next(&state, &record, aborted);
    });
}

/// Runs a `plan_task` for one session and streams the orchestrator's events.
///
/// `plan_task` always plans freely — the workflow selector is not consulted —
/// which is what keeps it the free-form fallback it has always been.
pub(crate) fn spawn_plan(
    state: Arc<ServerState>,
    record: Arc<SessionRecord>,
    task: String,
    mut control: ControlChannel,
    aborted: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let (events, receiver) = mpsc::unbounded_channel::<RoutedEvent>();
        // A sub-agent's loop ends every turn with its own `Done`; those are
        // dropped, and the single terminal `Done` is broadcast below.
        let bridge = tokio::spawn(bridge_plan_events(receiver, Arc::clone(&record)));

        // A plan is a run too, so it opens with the same frame.
        record.broadcast(session_started(&record));

        let resolved = record.resolved();
        let outcome = run_plan(
            &state,
            &resolved,
            &record,
            &PlanSource::FreeForm,
            &task,
            &events,
            &mut control,
        )
        .await;

        drop(events);
        if let Err(err) = bridge.await {
            tracing::warn!("event bridge stopped early: {err}");
        }

        report_plan_outcome(&record, &outcome, &aborted);
        spawn_next(&state, &record, aborted);
    });
}

/// Runs one queued workflow job: resolves its plan, announces it, executes it,
/// reports the job's terminal state and starts whatever is next.
///
/// Spawned rather than awaited, like [`spawn_run`], so the reader loop keeps
/// consuming inbound frames: an abort or a steering message has to reach the
/// executor while it is working.
pub(crate) fn spawn_workflow_job(
    state: Arc<ServerState>,
    record: Arc<SessionRecord>,
    job: WorkflowJob,
    mut control: ControlChannel,
    aborted: Arc<AtomicBool>,
) {
    tokio::spawn(async move {
        let (events, receiver) = mpsc::unbounded_channel::<RoutedEvent>();
        let bridge = tokio::spawn(bridge_plan_events(receiver, Arc::clone(&record)));

        // A job is a run too, so it opens with the same frame; the job frames
        // then say which queued job became the active one.
        record.broadcast(session_started(&record));
        record.broadcast(ServerMessage::WorkflowStarted {
            job_id: job.id,
            task: job.task.clone(),
            workflow_id: job.workflow.clone(),
        });

        let source = match &job.workflow {
            Some(id) => PlanSource::Named(id.clone()),
            None => PlanSource::Select,
        };
        let resolved = record.resolved();
        let outcome = run_plan(
            &state,
            &resolved,
            &record,
            &source,
            &job.task,
            &events,
            &mut control,
        )
        .await;

        drop(events);
        if let Err(err) = bridge.await {
            tracing::warn!("event bridge stopped early: {err}");
        }

        let (status, summary) = job_outcome(&outcome, &aborted);
        record.broadcast(ServerMessage::WorkflowFinished {
            job_id: job.id,
            status,
            summary,
        });
        report_plan_outcome(&record, &outcome, &aborted);
        spawn_next(&state, &record, aborted);
    });
}

/// Starts whatever the session has waiting after a run ends.
///
/// A waiting message is taken first, and a job only when none waits; see
/// [`SessionRecord::finish_run`]. Nothing waiting leaves the session idle.
fn spawn_next(state: &Arc<ServerState>, record: &Arc<SessionRecord>, aborted: Arc<AtomicBool>) {
    match record.finish_run() {
        Some(NextRun::Message {
            text,
            effort,
            permission,
            control,
        }) => spawn_run(
            Arc::clone(state),
            Arc::clone(record),
            text,
            effort,
            permission,
            control,
            aborted,
        ),
        Some(NextRun::Job { job, control }) => {
            // Cleared here, before the next job starts, so an abort that ended
            // the previous run does not colour this one.
            aborted.store(false, Ordering::SeqCst);
            spawn_workflow_job(Arc::clone(state), Arc::clone(record), job, control, aborted);
        }
        None => {}
    }
}

/// Where a plan comes from.
enum PlanSource {
    /// Always plan freely; the selector is not consulted. This is `plan_task`.
    FreeForm,
    /// Ask the selector and, when it names none, fall back to a free-form plan.
    Select,
    /// Run this workflow by id. A missing one is an error, not a fallback: the
    /// client asked for it by name.
    Named(String),
}

/// Resolves, announces and executes one plan for a session.
///
/// The `plan_created` frame is broadcast before the executor starts, so a client
/// can draw every row — including rows for nodes that will be skipped — before
/// any node reports a status.
async fn run_plan(
    state: &ServerState,
    resolved: &ResolvedProvider,
    record: &SessionRecord,
    source: &PlanSource,
    task: &str,
    events: &mpsc::UnboundedSender<RoutedEvent>,
    control: &mut ControlChannel,
) -> Result<ExecutionReport> {
    let (graph, workflow_id) = resolve_graph(state, resolved, source, task).await?;
    // Layering validates first, so a malformed graph is refused before the
    // client is told a plan exists and before any node is scheduled.
    graph.layers()?;

    record.broadcast(ServerMessage::PlanCreated {
        nodes: graph.nodes.iter().map(plan_node).collect(),
        workflow_id,
    });

    let executor = Executor::new(
        ExecutorConfig {
            use_worktrees: worktrees_available(state.workspace_root()).await,
            // The session the plan runs on: each node's sub-agent forks its own
            // session from it, so a lane the client sees can be opened as a
            // stored conversation afterwards.
            parent_session: Some(record.session_id()),
            ..ExecutorConfig::default()
        },
        state.registry().clone(),
        ModelRouter::new(executor_router_config(state.config(), resolved)),
        Arc::clone(&resolved.provider),
        Arc::clone(state.memory()),
        state.workspace_root().to_path_buf(),
    );

    executor.execute(&graph, events, control).await
}

/// The graph a plan runs and, when one was chosen, the workflow it came from.
///
/// The selector is consulted only for [`PlanSource::Select`] and
/// [`PlanSource::Named`]; `plan_task`'s free-form path never touches it. A
/// selector that errors is reported rather than silently swallowed: the client
/// asked for a workflow, and a registry that cannot be read is not the same
/// answer as one that holds none.
async fn resolve_graph(
    state: &ServerState,
    resolved: &ResolvedProvider,
    source: &PlanSource,
    task: &str,
) -> Result<(TaskGraph, Option<String>)> {
    match source {
        PlanSource::FreeForm => Ok((plan_freely(resolved, task).await?, None)),
        PlanSource::Select => {
            if let Some(selector) = state.workflow_selector() {
                if let Some(workflow) = selector
                    .select(task, resolved.provider.as_ref(), &resolved.model, None)
                    .await?
                {
                    return Ok((workflow.graph, Some(workflow.id)));
                }
            }
            // No selector installed, or none fits the task: the free-form plan
            // is the documented fallback.
            Ok((plan_freely(resolved, task).await?, None))
        }
        PlanSource::Named(id) => {
            let Some(selector) = state.workflow_selector() else {
                return Err(HarnessError::Other(format!(
                    "no workflows are available, so `{id}` cannot be run"
                )));
            };
            match selector
                .select(task, resolved.provider.as_ref(), &resolved.model, Some(id))
                .await?
            {
                Some(workflow) => Ok((workflow.graph, Some(workflow.id))),
                None => Err(HarnessError::Other(format!(
                    "no workflow named `{id}` is available"
                ))),
            }
        }
    }
}

/// The free-form plan `plan_task` has always run.
async fn plan_freely(resolved: &ResolvedProvider, task: &str) -> Result<TaskGraph> {
    let planner = Planner::new(
        Arc::clone(&resolved.provider),
        resolved.model.clone(),
        MAX_PLAN_NODES,
    );
    planner.plan(task, None).await
}

fn plan_node(node: &TaskNode) -> PlanNode {
    PlanNode {
        id: node.id.clone(),
        objective: node.objective.clone(),
        agent: node.agent.clone(),
        depends_on: node.depends_on.clone(),
    }
}

/// The terminal state a workflow job reports, and the one-line summary that
/// travels with it.
fn job_outcome(outcome: &Result<ExecutionReport>, aborted: &AtomicBool) -> (SubtaskStatus, String) {
    if aborted.load(Ordering::SeqCst) {
        return (
            SubtaskStatus::Failed,
            "the job was aborted before it finished".to_string(),
        );
    }
    match outcome {
        Ok(report) if report.failed > 0 => (
            SubtaskStatus::Failed,
            format!("{} of {} node(s) failed", report.failed, report.nodes.len()),
        ),
        Ok(report) => (
            SubtaskStatus::Succeeded,
            format!("{} node(s) succeeded", report.succeeded),
        ),
        Err(err) => (SubtaskStatus::Failed, err.to_string()),
    }
}

/// Closes a plan's stream with the one terminal `Done` the client sees.
///
/// An abort outranks a node's failure: the executor reports the interrupted node
/// as failed and then returns normally, so the flag the reader loop set is what
/// names the interruption.
fn report_plan_outcome(
    record: &SessionRecord,
    outcome: &Result<ExecutionReport>,
    aborted: &AtomicBool,
) {
    if aborted.load(Ordering::SeqCst) {
        record.broadcast(ServerMessage::Done {
            reason: CompletionReason::Aborted,
        });
        return;
    }

    match outcome {
        Ok(report) if report.failed > 0 => {
            // Mirrors `harness plan`, which fails the command when any node
            // failed; each node's `subtask_end` frame carries the detail.
            record.broadcast(ServerMessage::Error {
                code: "plan_failed".to_string(),
                message: format!("{} of {} node(s) failed", report.failed, report.nodes.len()),
            });
            record.broadcast(ServerMessage::Done {
                reason: CompletionReason::Error,
            });
        }
        Ok(_) => record.broadcast(ServerMessage::Done {
            reason: CompletionReason::EndTurn,
        }),
        Err(err) => {
            record.broadcast(ServerMessage::Error {
                code: "plan_error".to_string(),
                message: err.to_string(),
            });
            record.broadcast(ServerMessage::Done {
                reason: CompletionReason::Error,
            });
        }
    }
}

/// The config the executor's router resolves node models against.
///
/// `Executor` has no model-override hook: it re-resolves every node's model from
/// its spec, falling back to the *configured default provider's* model. The
/// session resolved its own provider, so the config the executor sees is
/// rewritten to name it — the same adjustment `harness plan` makes.
fn executor_router_config(config: &Config, resolved: &ResolvedProvider) -> Config {
    let mut config = config.clone();
    config.provider.default = resolved.provider_id.clone();
    if let Some(provider) = config.providers.get_mut(&resolved.provider_id) {
        provider.default_model = Some(resolved.model.clone());
    }
    config
}

/// Isolation is only possible where `git worktree add` would succeed, which is
/// the same two facts `harness plan` checks: a repository, and a commit to base
/// a checkout on.
async fn worktrees_available(root: &Path) -> bool {
    git_succeeds(root, &["rev-parse", "--git-dir"]).await
        && git_succeeds(root, &["rev-parse", "--verify", "--quiet", "HEAD"]).await
}

async fn git_succeeds(root: &Path, args: &[&str]) -> bool {
    tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .await
        .map(|output| output.status.success())
        .unwrap_or(false)
}

/// Converts the loop's internal events into wire messages.
///
/// `forward_done` says whether the loop's own `Done` closes the client's stream.
/// A user-message run's loop owns that end; a plan's does not, because every
/// sub-agent emits one too and the plan task broadcasts the single terminal one.
async fn bridge_events(
    mut events: mpsc::UnboundedReceiver<AgentEvent>,
    record: Arc<SessionRecord>,
    forward_done: bool,
) {
    while let Some(event) = events.recv().await {
        // `TurnStarted` is an internal boundary marker whose wire conversion is a
        // `Pong`, which on a socket would be indistinguishable from an answer to a
        // client heartbeat. It carries no client meaning, so it is not forwarded.
        if matches!(event, AgentEvent::TurnStarted { .. }) {
            continue;
        }
        if !forward_done && matches!(event, AgentEvent::Done { .. }) {
            continue;
        }
        record.broadcast(event.into());
    }
}

/// Converts the executor's routed events into wire frames.
///
/// The plan's own events stay on the session's lane; a node's sub-agent events
/// carry their worker's lane, so the client can draw one lane per worker and
/// fetch that worker's stored conversation by its `subagent_session_id`. Both
/// internal-only frames are dropped: `TurnStarted` has no client meaning, and
/// every sub-agent ends with its own `Done`, of which the plan task broadcasts
/// the single terminal one.
async fn bridge_plan_events(
    mut events: mpsc::UnboundedReceiver<RoutedEvent>,
    record: Arc<SessionRecord>,
) {
    while let Some(routed) = events.recv().await {
        if matches!(
            &routed.event,
            AgentEvent::TurnStarted { .. } | AgentEvent::Done { .. }
        ) {
            continue;
        }
        let envelope = ServerEnvelope::new(
            record.session_id(),
            record.agent_id().clone(),
            routed.event.into(),
        );
        let envelope = match routed.subagent_id {
            Some(subagent) => envelope.from_subagent_session(subagent, routed.subagent_session_id),
            None => envelope,
        };
        record.broadcast_envelope(envelope);
    }
}

/// Pumps one session's fan-out into one connection's queue.
async fn forward_events(
    mut events: broadcast::Receiver<ServerEnvelope>,
    mut outbox: Outbox,
    close: mpsc::Sender<()>,
) {
    loop {
        match events.recv().await {
            Ok(envelope) => match outbox.send(envelope).await {
                // A dropped frame is the policy's choice: this subscriber is
                // behind and everything else keeps streaming.
                Delivery::Sent | Delivery::Dropped => {}
                Delivery::Closed => {
                    // Ask the reader to end the connection; dropping the queue
                    // then closes the writer.
                    let _ = close.send(()).await;
                    return;
                }
            },
            // A subscriber that fell behind misses events instead of stalling the
            // sender; the conversation stays readable through the REST API.
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::debug!("subscriber missed {missed} events");
            }
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

async fn writer_loop(
    mut rx: mpsc::Receiver<ServerEnvelope>,
    mut sink: futures::stream::SplitSink<WebSocket, WsMessage>,
    close: mpsc::Sender<()>,
) {
    let mut heartbeat = tokio::time::interval(HEARTBEAT);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // `interval` fires immediately; the first ping should be one period out.
    heartbeat.tick().await;

    loop {
        tokio::select! {
            outbound = rx.recv() => match outbound {
                Some(envelope) => match encode(&envelope) {
                    Ok(text) => {
                        if sink.send(WsMessage::text(text)).await.is_err() {
                            break;
                        }
                    }
                    Err(err) => tracing::error!("failed to encode a server message: {err}"),
                },
                None => break,
            },

            _ = heartbeat.tick() => {
                // A ping that fails means the peer is gone. The read side usually
                // notices too, but not when the socket is half-open.
                if sink.send(WsMessage::Ping(Bytes::new())).await.is_err() {
                    break;
                }
            }
        }
    }

    let _ = sink.close().await;
    let _ = close.try_send(());
}

fn error_code(err: &HarnessError) -> &'static str {
    match err {
        HarnessError::AgentNotFound(_) => "agent_not_found",
        HarnessError::Provider { .. } => "provider",
        HarnessError::Config(_) => "config",
        HarnessError::Memory(_) => "memory",
        HarnessError::ToolNotFound(_) => "tool_not_found",
        _ => "session_error",
    }
}

/// Encodes an outbound envelope.
///
/// The two steps are not redundant. `ServerEnvelope` flattens the message, and
/// `ServerMessage::SessionStarted` carries `session_id` and `agent_id` under the
/// same names as the envelope itself, so a direct `to_string` writes those keys
/// twice — JSON that a strict parser rejects and a lenient one silently halves.
/// Routing through `Value` collapses each pair onto the single value the two
/// copies already agree on.
fn encode(envelope: &ServerEnvelope) -> std::result::Result<String, serde_json::Error> {
    serde_json::to_string(&serde_json::to_value(envelope)?)
}

/// Why a frame could not be read, in the shape the refusal frame needs.
///
/// The session id is parsed by hand rather than by the derived `Deserialize`
/// for two reasons. The first is the mirror image of the collision [`encode`]
/// documents: `ClientEnvelope` flattens `ClientMessage`, and
/// `ClientMessage::Subscribe` carries a `session_id` under the same name as the
/// envelope's routing field. serde's flatten deserializer gives that key to the
/// envelope and then reports it *missing* on the message, so the derived
/// `Deserialize` cannot read a subscribe frame at all. The second is that a
/// malformed id must be *named* in the refusal — a `SessionId` that fails to
/// parse cannot carry the string the client sent — so the raw value is kept
/// here, where it can still be put into the error message. Both types belong to
/// `harness-core`, which is why the workaround lives here.
#[derive(Debug)]
struct FrameError {
    code: &'static str,
    message: String,
}

impl FrameError {
    fn bad(message: impl Into<String>) -> Self {
        Self {
            code: "bad_message",
            message: message.into(),
        }
    }
}

fn parse_client(raw: &str) -> std::result::Result<ClientEnvelope, FrameError> {
    let mut value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| FrameError::bad(format!("unparsable envelope: {err}")))?;

    // Taken out before the typed parse so a malformed id is reported rather than
    // collapsing into a generic serde error that does not name it.
    let session_id = match value
        .as_object_mut()
        .and_then(|object| object.remove("session_id"))
    {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(raw)) => Some(
            raw.parse::<SessionId>()
                .map_err(|_| FrameError::bad(format!("`{raw}` is not a session id")))?,
        ),
        Some(other) => {
            return Err(FrameError::bad(format!(
                "session_id must be a string, not {other}"
            )))
        }
    };

    // A subscribe frame is rebuilt rather than handed to the derived
    // `Deserialize`, which cannot read one for the reason above. It carries the
    // id inside the message as well as on the envelope, and the two name the
    // same session, so the value already parsed completes it.
    if value.get("type").and_then(serde_json::Value::as_str) == Some("subscribe") {
        let session_id =
            session_id.ok_or_else(|| FrameError::bad("a subscribe frame needs a session_id"))?;
        return Ok(ClientEnvelope {
            v: value
                .get("v")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(PROTOCOL_VERSION as u64) as u32,
            id: value
                .get("id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string),
            session_id: Some(session_id),
            message: ClientMessage::Subscribe { session_id },
        });
    }

    let mut envelope: ClientEnvelope = serde_json::from_value(value)
        .map_err(|err| FrameError::bad(format!("unparsable envelope: {err}")))?;
    envelope.session_id = session_id;

    Ok(envelope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use serde_json::json;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    fn ids() -> (SessionId, AgentId) {
        (SessionId::new(), AgentId::new("test").expect("a valid id"))
    }

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).expect("a header name"),
                HeaderValue::from_str(value).expect("a header value"),
            );
        }
        map
    }

    #[test]
    fn an_origin_from_another_site_is_refused() {
        let refused = origin_allowed(&headers(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "http://evil.example"),
        ]));
        let reason = refused.expect_err("a page on another site must not open the bus");
        assert!(reason.contains("evil.example"), "{reason}");
    }

    #[test]
    fn the_servers_own_origin_is_allowed() {
        assert!(origin_allowed(&headers(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "http://127.0.0.1:8787"),
        ]))
        .is_ok());
        assert!(origin_allowed(&headers(&[
            ("host", "192.168.1.7:8787"),
            ("origin", "http://192.168.1.7:8787"),
        ]))
        .is_ok());
        // The origin may leave the port implicit when it is the scheme default.
        assert!(origin_allowed(&headers(&[
            ("host", "example.test:80"),
            ("origin", "http://example.test"),
        ]))
        .is_ok());
    }

    #[test]
    fn a_missing_origin_is_allowed_because_no_browser_can_suppress_it() {
        assert!(origin_allowed(&headers(&[("host", "127.0.0.1:8787")])).is_ok());
    }

    #[test]
    fn a_null_origin_is_refused() {
        let refused = origin_allowed(&headers(&[("host", "127.0.0.1:8787"), ("origin", "null")]));
        assert!(refused.is_err(), "`Origin: null` names no page to trust");
    }

    #[test]
    fn a_remote_origin_on_another_port_is_refused() {
        let refused = origin_allowed(&headers(&[
            ("host", "192.168.1.7:8787"),
            ("origin", "http://192.168.1.7:9999"),
        ]));
        assert!(refused.is_err(), "a different port is a different origin");
    }

    /// The local dev proxy rewrites `Host` but not `Origin`, so `pnpm dev`
    /// arrives as a loopback origin on another port. Both ends are on this
    /// machine, which is not the drive-by case: that one always arrives with a
    /// remote origin, and a rebinding attack still fails because the Origin's
    /// name is compared and never what it resolves to. The cost is that a page
    /// served by another local web server can also open the bus; that is the
    /// price of not breaking local development, and it is bounded to this
    /// machine.
    #[test]
    fn a_loopback_page_on_another_port_is_allowed_as_local() {
        assert!(origin_allowed(&headers(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "http://localhost:5173"),
        ]))
        .is_ok());
        assert!(origin_allowed(&headers(&[
            ("host", "127.0.0.1:8787"),
            ("origin", "http://127.0.0.1:9999"),
        ]))
        .is_ok());
        assert!(origin_allowed(&headers(&[
            ("host", "[::1]:8787"),
            ("origin", "http://[::1]:5173"),
        ]))
        .is_ok());
    }

    #[test]
    fn an_ipv6_authority_is_split_without_its_brackets() {
        assert_eq!(split_authority("[::1]:8787"), ("::1", Some(8787)));
        assert_eq!(split_authority("[::1]"), ("::1", None));
        assert_eq!(
            split_authority("example.test:80"),
            ("example.test", Some(80))
        );
        assert_eq!(split_authority("example.test"), ("example.test", None));
    }

    /// A state whose memory store is a temporary file, so a test can serve it.
    async fn bound_server(config: Config) -> (std::net::SocketAddr, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("a temporary workspace");
        let mut config = config;
        config.memory.db_path = dir.path().join("harness.db");

        let state = Arc::new(
            ServerState::new(config, dir.path().to_path_buf())
                .await
                .expect("the state builds"),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("a loopback port is available");
        let addr = listener.local_addr().expect("the socket is bound");
        tokio::spawn(async move {
            let _ = axum::serve(listener, Arc::clone(&state).router()).await;
        });
        (addr, dir)
    }

    /// Asserts the handshake was refused with `status` rather than upgraded or
    /// silently closed.
    fn assert_refused<T>(
        outcome: std::result::Result<T, tokio_tungstenite::tungstenite::Error>,
        status: StatusCode,
    ) {
        match outcome {
            Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
                assert_eq!(response.status(), status, "the refusal names its reason");
            }
            Err(other) => panic!("expected an HTTP refusal, got {other}"),
            Ok(_) => panic!("the handshake was accepted"),
        }
    }

    #[tokio::test]
    async fn a_foreign_origin_never_reaches_the_bus() {
        let (addr, _dir) = bound_server(Config::default()).await;
        let url = format!("ws://{addr}/ws");

        let mut request = url
            .as_str()
            .into_client_request()
            .expect("a client request");
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("http://evil.example"),
        );

        assert_refused(
            tokio_tungstenite::connect_async(request).await,
            StatusCode::FORBIDDEN,
        );
    }

    #[tokio::test]
    async fn a_client_with_no_origin_is_still_served() {
        let (addr, _dir) = bound_server(Config::default()).await;
        let url = format!("ws://{addr}/ws");

        let (socket, response) = tokio_tungstenite::connect_async(url.as_str())
            .await
            .expect("a client that sends no Origin is not a browser and is allowed");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        drop(socket);
    }

    #[tokio::test]
    async fn a_client_on_the_servers_own_origin_is_served() {
        let (addr, _dir) = bound_server(Config::default()).await;
        let url = format!("ws://{addr}/ws");

        let mut request = url
            .as_str()
            .into_client_request()
            .expect("a client request");
        request.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_str(&format!("http://{addr}")).expect("an origin"),
        );

        let (socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("the server's own origin is the page it serves");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        drop(socket);
    }

    #[tokio::test]
    async fn a_configured_token_is_required_and_accepted() {
        let mut config = Config::default();
        config.server.token = Some("s3cret".to_string());
        let (addr, _dir) = bound_server(config).await;
        let url = format!("ws://{addr}/ws");

        assert_refused(
            tokio_tungstenite::connect_async(url.as_str()).await,
            StatusCode::UNAUTHORIZED,
        );

        let (socket, response) = tokio_tungstenite::connect_async(format!("{url}?token=s3cret"))
            .await
            .expect("the token in the query is accepted");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        drop(socket);

        let mut request = url
            .as_str()
            .into_client_request()
            .expect("a client request");
        request
            .headers_mut()
            .insert("x-harness-token", HeaderValue::from_static("s3cret"));
        let (socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("the token in the header is accepted");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        drop(socket);

        let mut request = url
            .as_str()
            .into_client_request()
            .expect("a client request");
        request.headers_mut().insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer s3cret"),
        );
        let (socket, response) = tokio_tungstenite::connect_async(request)
            .await
            .expect("the bearer token is accepted");
        assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
        drop(socket);
    }

    #[test]
    fn a_session_started_frame_is_encoded_with_each_key_once() {
        let (session_id, agent_id) = ids();
        let envelope = ServerEnvelope::new(
            session_id,
            agent_id.clone(),
            ServerMessage::SessionStarted {
                session_id,
                agent_id,
                model: "mock-1".to_string(),
            },
        );

        let encoded = encode(&envelope).expect("encodable");
        assert_eq!(encoded.matches("\"session_id\"").count(), 1, "{encoded}");
        assert_eq!(encoded.matches("\"agent_id\"").count(), 1, "{encoded}");

        let value: serde_json::Value = serde_json::from_str(&encoded).expect("valid JSON");
        assert_eq!(value["type"], "session_started");
        assert_eq!(value["session_id"], session_id.to_string());
        assert_eq!(value["agent_id"], "test");
        assert_eq!(value["v"], PROTOCOL_VERSION);
        assert_eq!(value["model"], "mock-1");
    }

    #[test]
    fn ordinary_envelopes_round_trip_through_the_wire_types() {
        let (session_id, agent_id) = ids();
        let messages = vec![
            ServerMessage::AssistantChunk {
                text: "hi".to_string(),
            },
            ServerMessage::ToolCallEnd {
                tool_call_id: "call_1".to_string(),
                name: "read_file".to_string(),
                status: harness_core::ToolCallStatus::Ok,
                output: "contents".to_string(),
                duration_ms: 3,
            },
            ServerMessage::Error {
                code: "busy".to_string(),
                message: "slow down".to_string(),
            },
            ServerMessage::Done {
                reason: CompletionReason::EndTurn,
            },
            ServerMessage::Pong,
        ];

        for message in messages {
            let envelope = ServerEnvelope::new(session_id, agent_id.clone(), message);
            let encoded = encode(&envelope).expect("encodable");
            let decoded: ServerEnvelope = serde_json::from_str(&encoded).expect("parsable");
            assert_eq!(decoded, envelope);
        }
    }

    #[test]
    fn a_subscribe_frame_is_read_from_the_envelopes_routing_field() {
        let (session_id, _) = ids();
        let raw = json!({
            "v": PROTOCOL_VERSION,
            "session_id": session_id.to_string(),
            "type": "subscribe",
        })
        .to_string();

        let envelope = parse_client(&raw).expect("readable");
        assert_eq!(envelope.session_id, Some(session_id));
        assert_eq!(envelope.message, ClientMessage::Subscribe { session_id });
    }

    #[test]
    fn ordinary_client_frames_still_go_through_the_wire_types() {
        let ping = ClientEnvelope::new(ClientMessage::Ping);
        assert_eq!(
            parse_client(&serde_json::to_string(&ping).expect("encodable"))
                .expect("readable")
                .message,
            ClientMessage::Ping
        );

        let steering = ClientEnvelope::new(ClientMessage::SteeringMessage {
            text: "short path".to_string(),
            priority: harness_core::Priority::High,
        });
        assert_eq!(
            parse_client(&serde_json::to_string(&steering).expect("encodable"))
                .expect("readable")
                .message,
            steering.message
        );
    }

    #[test]
    fn frames_that_are_not_client_messages_are_refused() {
        assert!(parse_client("{\"nope\":true}").is_err());
        assert!(parse_client("not json at all").is_err());
        // A subscribe frame with nothing to subscribe to.
        assert!(parse_client("{\"type\":\"subscribe\"}").is_err());
    }
}
