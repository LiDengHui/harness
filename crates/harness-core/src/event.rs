//! The event vocabulary shared by the CLI and the WebSocket bus.
//!
//! Protocol-first: these types are defined once here so that the agent loop,
//! the CLI's JSON event stream and the browser client can never drift apart.
//!
//! Two layers exist on purpose:
//!   * [`AgentEvent`] is the internal, context-free stream produced by the loop.
//!   * [`ServerMessage`] is the wire form, carrying session/agent routing context
//!     in a [`ServerEnvelope`] so one socket can multiplex many agents.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::{AgentId, SessionId, SubtaskId};
use crate::tokens::TokenUsage;

/// Wire protocol version. Bump on breaking changes.
pub const PROTOCOL_VERSION: u32 = 1;

/// Relative importance of an inbound user instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

/// Terminal state of a single tool invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Ok,
    Error,
    Rejected,
    Timeout,
}

/// Lifecycle state of one node in the orchestration DAG.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubtaskStatus {
    #[default]
    Pending,
    Running,
    Succeeded,
    Failed,
    Skipped,
}

/// One node of the plan a run is executing, as the client draws it.
///
/// This is a checklist row. `id` is the key the progress frames
/// (`subtask_start` / `subtask_end`) carry as `node_id`, so a status can be
/// mapped onto the row it belongs to without the client having to guess.
///
/// It is a protocol type rather than a re-export of the orchestrator's
/// `TaskNode` because `harness-core` must not depend on `harness-orchestrator`;
/// the server maps one to the other when a run starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanNode {
    /// The `TaskGraph` node id, e.g. `add-parser`.
    pub id: String,
    /// What this node must achieve.
    pub objective: String,
    /// `.agent.md` id to run it, when the plan names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// Ids of the nodes that must finish first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

/// Why the agent loop stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionReason {
    /// The model produced a final answer with no pending tool calls.
    EndTurn,
    /// `harness.max_iterations` was reached.
    MaxIterations,
    /// The user sent an abort.
    Aborted,
    /// The agent's token budget was exhausted.
    BudgetExceeded,
    /// The run ended on an unrecoverable error.
    Error,
}

/// Events produced by the agent loop, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    AssistantChunk {
        text: String,
    },
    ThinkingChunk {
        text: String,
    },
    ToolCallStart {
        tool_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolCallProgress {
        tool_call_id: String,
        message: String,
    },
    ToolCallEnd {
        tool_call_id: String,
        name: String,
        status: ToolCallStatus,
        output: String,
        duration_ms: u64,
    },
    SubtaskStart {
        subtask_id: SubtaskId,
        /// The `TaskGraph` node this worker runs. `subtask_id` is a fresh id for
        /// the worker's lane and is *not* the node id, so this is what lets a
        /// client map the event onto a plan row.
        #[serde(default)]
        node_id: String,
        agent_id: AgentId,
        objective: String,
    },
    SubtaskEnd {
        subtask_id: SubtaskId,
        /// The `TaskGraph` node this terminal event closes. Present even for a
        /// node that never started — see `Skipped` — so no plan row stays
        /// invisible.
        #[serde(default)]
        node_id: String,
        status: SubtaskStatus,
        summary: String,
    },
    AgentHandoff {
        from: AgentId,
        to: AgentId,
        reason: String,
    },
    TokenUsage {
        usage: TokenUsage,
        budget_remaining: Option<u64>,
    },
    /// A guardrail inspected (and possibly blocked) some content.
    Guardrail {
        name: String,
        blocked: bool,
        detail: String,
    },
    /// A tool call is gated by the permission policy and is waiting for a human.
    ///
    /// The loop emits this and then blocks, so a client that never answers
    /// leaves the call to be refused when the configured timeout expires.
    ToolApprovalRequest {
        tool_call_id: String,
        name: String,
        arguments: Value,
        reason: String,
    },
    Error {
        message: String,
    },
    Done {
        reason: CompletionReason,
    },
}

/// One event from the orchestrator's stream, with the sub-agent lane it belongs
/// to when a node's worker produced it.
///
/// The lane rides on every event rather than being inferred from the
/// `SubtaskStart` that opened it: a layer's nodes run concurrently, so two
/// workers' events interleave and a single "current lane" would attribute one
/// worker's output to another. A node's bracketing `SubtaskStart`/`SubtaskEnd`
/// carry its lane too; only the orchestrator's own events — a handoff, an error
/// that failed a node before it ran — carry none.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutedEvent {
    /// The worker's agent, when a node's sub-agent produced this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<AgentId>,
    /// The session that worker's conversation is persisted to, when it could be
    /// written to the store. `None` while a worker runs unpersisted, which is
    /// what happens when the caller gave the executor no parent session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<SessionId>,
    pub event: AgentEvent,
}

impl RoutedEvent {
    /// An event the orchestrator itself emitted; it belongs to no worker lane.
    pub fn orchestrator(event: AgentEvent) -> Self {
        Self {
            subagent_id: None,
            subagent_session_id: None,
            event,
        }
    }

    /// An event on one node's lane: its sub-agent's output, or the bracketing
    /// events the orchestrator emits for that node.
    pub fn from_subagent(
        subagent_id: AgentId,
        subagent_session_id: Option<SessionId>,
        event: AgentEvent,
    ) -> Self {
        Self {
            subagent_id: Some(subagent_id),
            subagent_session_id,
            event,
        }
    }
}

/// Messages sent from the client to the server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Start or continue a conversation.
    UserMessage {
        text: String,
        /// Overrides the session's agent for this run only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<AgentId>,
        /// Reasoning effort for this run only, outranking the agent's own
        /// declaration and the configured default.
        ///
        /// Free text rather than an enum so a value the UI does not know yet is
        /// read and then ignored by the server, instead of failing the frame;
        /// see `harness_core::resolve_thinking_effort` for the accepted set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
        /// Permission mode for this run only, outranking the agent's own
        /// declaration and the configured default.
        ///
        /// Free text for the same forward-compatibility reason as `effort`: an
        /// unrecognised value is read and ignored rather than failing the frame.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
    },
    /// Decompose a task into a DAG and run it through the orchestrator, which
    /// reports itself with `subtask_start` / `subtask_end` / `agent_handoff`.
    PlanTask {
        task: String,
        /// Overrides the session's agent for this plan only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<AgentId>,
    },
    /// Enqueue a job on this session's workflow queue.
    ///
    /// The workflow queue is a separate ordered list from the user-message
    /// queue: one job runs at a time and the next starts when it finishes, but a
    /// queued user message is never a job and vice versa. The client learns each
    /// job's state from `workflow_queued` / `workflow_started` /
    /// `workflow_finished`.
    ///
    /// A job with no workflow named asks the server to choose one and, when it
    /// chooses none, falls back to the free-form plan `plan_task` runs.
    QueueWorkflow {
        task: String,
        /// The workflow to run, when the client named one. Absent means "choose
        /// one for this task, or plan freely if nothing fits".
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow: Option<String>,
        /// Overrides the session's agent for this job only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_id: Option<AgentId>,
    },
    /// Inject an instruction into a run that is already in flight.
    SteeringMessage {
        text: String,
        #[serde(default)]
        priority: Priority,
    },
    /// Stop the current run; the session stays alive.
    Abort {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Answer a `tool_approval` request gated by the permission policy.
    ToolApproval {
        tool_call_id: String,
        approved: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Attach this connection to a session, replaying nothing but token usage.
    Subscribe {
        session_id: SessionId,
    },
    Ping,
}

/// Messages sent from the server to the client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// A run has begun on this session.
    ///
    /// Sent at the start of *every* run, not only when a session is opened. It
    /// is the client's cue that a new turn became the active one: a client that
    /// submitted messages while a run was in flight can promote the head of its
    /// queue each time this frame arrives, rather than inferring it from
    /// `done`. A run's own events always follow this frame.
    SessionStarted {
        session_id: SessionId,
        agent_id: AgentId,
        model: String,
    },
    /// A user message was accepted while a run was already in flight, and is
    /// waiting its turn.
    ///
    /// `position` is 1-based and counts the messages ahead of this one; the
    /// running turn is not part of the queue, so the first waiting message is
    /// at position 1. The message starts when the runs ahead of it finish, and
    /// a `session_started` frame marks the moment it does.
    MessageQueued {
        text: String,
        position: usize,
    },
    /// The plan a run is executing, sent once before any of its nodes start.
    ///
    /// Without this frame a client sees `subtask_start` / `subtask_end` for
    /// nodes it was never told about and cannot draw a checklist. It carries
    /// *every* node, not only the ones that will run, so a row exists for a node
    /// that is later skipped.
    PlanCreated {
        nodes: Vec<PlanNode>,
        /// The workflow the plan came from, when one was chosen; absent for a
        /// free-form plan.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_id: Option<String>,
    },
    /// A workflow job was accepted into this session's workflow queue and is
    /// waiting for the running job to finish.
    ///
    /// `position` is 1-based and counts the jobs ahead of this one in the
    /// *workflow* queue; the running job is not part of it. Queued user messages
    /// are not counted here — the two queues number themselves separately.
    WorkflowQueued {
        job_id: u64,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_id: Option<String>,
        position: usize,
    },
    /// A queued workflow job became the running one.
    WorkflowStarted {
        job_id: u64,
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workflow_id: Option<String>,
    },
    /// A workflow job reached a terminal state.
    WorkflowFinished {
        job_id: u64,
        /// `succeeded`, `failed` or `skipped`; the job's own `done` frame follows
        /// this one and closes the turn.
        status: SubtaskStatus,
        summary: String,
    },
    AssistantChunk {
        text: String,
    },
    ThinkingChunk {
        text: String,
    },
    ToolCallStart {
        tool_call_id: String,
        name: String,
        arguments: Value,
    },
    ToolCallProgress {
        tool_call_id: String,
        message: String,
    },
    ToolCallEnd {
        tool_call_id: String,
        name: String,
        status: ToolCallStatus,
        output: String,
        duration_ms: u64,
    },
    SubtaskStart {
        subtask_id: SubtaskId,
        /// The `TaskGraph` node this worker runs; the checklist row's key.
        #[serde(default)]
        node_id: String,
        objective: String,
    },
    SubtaskEnd {
        subtask_id: SubtaskId,
        /// The `TaskGraph` node this terminal frame closes.
        #[serde(default)]
        node_id: String,
        status: SubtaskStatus,
        summary: String,
    },
    AgentHandoff {
        from: AgentId,
        to: AgentId,
        reason: String,
    },
    TokenUsage {
        usage: TokenUsage,
        budget_remaining: Option<u64>,
    },
    Guardrail {
        name: String,
        blocked: bool,
        detail: String,
    },
    /// The server needs a human decision before running a tool.
    ToolApprovalRequest {
        tool_call_id: String,
        name: String,
        arguments: Value,
        reason: String,
    },
    Error {
        code: String,
        message: String,
    },
    Done {
        reason: CompletionReason,
    },
    Pong,
}

impl From<AgentEvent> for ServerMessage {
    fn from(event: AgentEvent) -> Self {
        match event {
            AgentEvent::TurnStarted { .. } => ServerMessage::Pong,
            AgentEvent::AssistantChunk { text } => ServerMessage::AssistantChunk { text },
            AgentEvent::ThinkingChunk { text } => ServerMessage::ThinkingChunk { text },
            AgentEvent::ToolCallStart {
                tool_call_id,
                name,
                arguments,
            } => ServerMessage::ToolCallStart {
                tool_call_id,
                name,
                arguments,
            },
            AgentEvent::ToolCallProgress {
                tool_call_id,
                message,
            } => ServerMessage::ToolCallProgress {
                tool_call_id,
                message,
            },
            AgentEvent::ToolCallEnd {
                tool_call_id,
                name,
                status,
                output,
                duration_ms,
            } => ServerMessage::ToolCallEnd {
                tool_call_id,
                name,
                status,
                output,
                duration_ms,
            },
            AgentEvent::SubtaskStart {
                subtask_id,
                node_id,
                objective,
                ..
            } => ServerMessage::SubtaskStart {
                subtask_id,
                node_id,
                objective,
            },
            AgentEvent::SubtaskEnd {
                subtask_id,
                node_id,
                status,
                summary,
            } => ServerMessage::SubtaskEnd {
                subtask_id,
                node_id,
                status,
                summary,
            },
            AgentEvent::AgentHandoff { from, to, reason } => {
                ServerMessage::AgentHandoff { from, to, reason }
            }
            AgentEvent::TokenUsage {
                usage,
                budget_remaining,
            } => ServerMessage::TokenUsage {
                usage,
                budget_remaining,
            },
            AgentEvent::Guardrail {
                name,
                blocked,
                detail,
            } => ServerMessage::Guardrail {
                name,
                blocked,
                detail,
            },
            AgentEvent::ToolApprovalRequest {
                tool_call_id,
                name,
                arguments,
                reason,
            } => ServerMessage::ToolApprovalRequest {
                tool_call_id,
                name,
                arguments,
                reason,
            },
            AgentEvent::Error { message } => ServerMessage::Error {
                code: "agent_error".to_string(),
                message,
            },
            AgentEvent::Done { reason } => ServerMessage::Done { reason },
        }
    }
}

/// Wire wrapper carrying the routing context for a server message.
///
/// `agent_id` names the agent that owns the session; `subagent_id` is set when
/// the message originates from a delegated sub-agent, which is what lets the UI
/// draw one lane per worker over a single socket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerEnvelope {
    pub v: u32,
    pub session_id: SessionId,
    pub agent_id: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_id: Option<AgentId>,
    /// The session a sub-agent's conversation is stored under, set alongside
    /// `subagent_id` on every frame a worker produced.
    ///
    /// `subagent_id` says which worker a frame belongs to; this says where its
    /// transcript lives. It is the id a client fetches with
    /// `GET /api/sessions/{id}/history` to open the worker's own conversation,
    /// which is what makes a delegation traceable rather than only visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_session_id: Option<SessionId>,
    #[serde(flatten)]
    pub message: ServerMessage,
}

impl ServerEnvelope {
    pub fn new(session_id: SessionId, agent_id: AgentId, message: ServerMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            session_id,
            agent_id,
            subagent_id: None,
            subagent_session_id: None,
            message,
        }
    }

    pub fn from_subagent(mut self, subagent: AgentId) -> Self {
        self.subagent_id = Some(subagent);
        self
    }

    /// Names both the worker's agent and the session its turns are persisted to.
    ///
    /// The session is optional because a worker may run unpersisted — when the
    /// caller gave the executor no parent session, or when the store could not
    /// be written — in which case there is nothing to open afterwards.
    pub fn from_subagent_session(mut self, subagent: AgentId, session: Option<SessionId>) -> Self {
        self.subagent_id = Some(subagent);
        self.subagent_session_id = session;
        self
    }
}

/// Wire wrapper for an inbound client message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClientEnvelope {
    #[serde(default = "default_version")]
    pub v: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
    #[serde(flatten)]
    pub message: ClientMessage,
}

fn default_version() -> u32 {
    PROTOCOL_VERSION
}

impl ClientEnvelope {
    pub fn new(message: ClientMessage) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id: None,
            session_id: None,
            message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::SubtaskId;
    use serde_json::json;

    #[test]
    fn client_message_round_trips_through_json() {
        let envelope = ClientEnvelope::new(ClientMessage::SteeringMessage {
            text: "stop refactoring, just fix the test".into(),
            priority: Priority::High,
        });

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "steering_message");
        assert_eq!(json["priority"], "high");

        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, envelope.message);
    }

    #[test]
    fn a_plan_task_round_trips_through_json() {
        let envelope = ClientEnvelope::new(ClientMessage::PlanTask {
            task: "split the parser into modules".into(),
            agent_id: None,
        });

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "plan_task");
        assert_eq!(json["task"], "split the parser into modules");
        assert!(json.get("agent_id").is_none());

        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, envelope.message);

        // The agent override is optional, so a frame naming one is still read.
        let with_agent = json!({
            "v": PROTOCOL_VERSION,
            "type": "plan_task",
            "task": "review the diff",
            "agent_id": "code-reviewer",
        });
        let envelope: ClientEnvelope = serde_json::from_value(with_agent).unwrap();
        assert_eq!(
            envelope.message,
            ClientMessage::PlanTask {
                task: "review the diff".into(),
                agent_id: Some(AgentId::new("code-reviewer").unwrap()),
            }
        );
    }

    #[test]
    fn a_user_message_carries_an_optional_effort_and_round_trips() {
        let envelope = ClientEnvelope::new(ClientMessage::UserMessage {
            text: "think hard".into(),
            agent_id: None,
            effort: Some("max".into()),
            permission_mode: Some("always_ask".into()),
        });

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["type"], "user_message");
        assert_eq!(json["effort"], "max");
        assert_eq!(json["permission_mode"], "always_ask");
        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, envelope.message);

        // The field is optional: a frame from a client that predates it is
        // still read, and an absent value stays absent on the way out.
        let bare = ClientEnvelope::new(ClientMessage::UserMessage {
            text: "plain".into(),
            agent_id: None,
            effort: None,
            permission_mode: None,
        });
        let json = serde_json::to_value(&bare).unwrap();
        assert!(json.get("effort").is_none());
        assert!(json.get("permission_mode").is_none());
        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, bare.message);
    }

    #[test]
    fn server_envelope_flattens_with_routing_context() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new("backend-architect").unwrap();
        let envelope = ServerEnvelope::new(
            session_id,
            agent_id.clone(),
            ServerMessage::AssistantChunk { text: "hi".into() },
        );

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["v"], PROTOCOL_VERSION);
        assert_eq!(json["type"], "assistant_chunk");
        assert_eq!(json["agent_id"], "backend-architect");
        assert_eq!(json["session_id"], session_id.to_string());
        assert!(json.get("subagent_id").is_none());

        let back: ServerEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back, envelope);
    }

    #[test]
    fn a_subagent_envelope_names_the_workers_session_and_round_trips() {
        let session_id = SessionId::new();
        let agent_id = AgentId::new("test").unwrap();
        let worker_session = SessionId::new();
        let envelope = ServerEnvelope::new(
            session_id,
            agent_id.clone(),
            ServerMessage::AssistantChunk { text: "hi".into() },
        )
        .from_subagent_session(agent_id.clone(), Some(worker_session));

        let json = serde_json::to_value(&envelope).unwrap();
        assert_eq!(json["subagent_id"], "test");
        assert_eq!(json["subagent_session_id"], worker_session.to_string());

        let back: ServerEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back, envelope);
        assert_eq!(back.subagent_session_id, Some(worker_session));

        // A worker that could not be persisted still names its agent, with no
        // session to open; the field is optional on the wire as well.
        let unpinned = ServerEnvelope::new(
            session_id,
            agent_id.clone(),
            ServerMessage::AssistantChunk { text: "hi".into() },
        )
        .from_subagent_session(agent_id, None);
        let json = serde_json::to_value(&unpinned).unwrap();
        assert_eq!(json["subagent_id"], "test");
        assert!(json.get("subagent_session_id").is_none());
        let back: ServerEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back, unpinned);
    }

    #[test]
    fn a_routed_event_carries_the_lane_and_round_trips() {
        let routed = RoutedEvent::from_subagent(
            AgentId::new("code-reviewer").unwrap(),
            Some(SessionId::new()),
            AgentEvent::AssistantChunk { text: "hi".into() },
        );
        let json = serde_json::to_value(&routed).unwrap();
        assert_eq!(json["subagent_id"], "code-reviewer");
        let back: RoutedEvent = serde_json::from_value(json).unwrap();
        assert_eq!(back, routed);

        // The orchestrator's own events belong to no lane.
        let planner = RoutedEvent::orchestrator(AgentEvent::Done {
            reason: CompletionReason::EndTurn,
        });
        let json = serde_json::to_value(&planner).unwrap();
        assert!(json.get("subagent_id").is_none());
        assert!(json.get("subagent_session_id").is_none());
        assert_eq!(
            serde_json::from_value::<RoutedEvent>(json).unwrap(),
            planner
        );
    }

    #[test]
    fn tool_events_carry_arguments_and_status() {
        let event = AgentEvent::ToolCallEnd {
            tool_call_id: "call_1".into(),
            name: "read_file".into(),
            status: ToolCallStatus::Ok,
            output: "contents".into(),
            duration_ms: 12,
        };
        let message: ServerMessage = event.into();
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["duration_ms"], 12);
    }

    #[test]
    fn subtask_events_survive_the_internal_to_wire_conversion() {
        let subtask_id = SubtaskId::new();
        let event = AgentEvent::SubtaskStart {
            subtask_id,
            node_id: "review-diff".into(),
            agent_id: AgentId::new("code-reviewer").unwrap(),
            objective: "review the diff".into(),
        };
        let json = serde_json::to_value(ServerMessage::from(event)).unwrap();
        assert_eq!(json["type"], "subtask_start");
        assert_eq!(json["objective"], "review the diff");
        // The node id travels with the frame so the client can key a checklist
        // row on it; `subtask_id` is the worker's lane, not the node.
        assert_eq!(json["node_id"], "review-diff");
        assert_ne!(json["subtask_id"], "review-diff");
    }

    #[test]
    fn a_plan_created_frame_carries_every_node_and_round_trips() {
        let message = ServerMessage::PlanCreated {
            nodes: vec![
                PlanNode {
                    id: "write-parser".into(),
                    objective: "write the parser".into(),
                    agent: None,
                    depends_on: Vec::new(),
                },
                PlanNode {
                    id: "wire-parser".into(),
                    objective: "wire it in".into(),
                    agent: Some("backend-architect".into()),
                    depends_on: vec!["write-parser".into()],
                },
            ],
            workflow_id: Some("feature-build".into()),
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "plan_created");
        assert_eq!(json["workflow_id"], "feature-build");
        let nodes = json["nodes"].as_array().expect("the nodes are an array");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["id"], "write-parser");
        assert_eq!(nodes[1]["depends_on"][0], "write-parser");
        assert!(nodes[0].get("agent").is_none(), "{json}");

        let back: ServerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, message);

        // A free-form plan names no workflow, and the field stays absent.
        let free_form = ServerMessage::PlanCreated {
            nodes: vec![PlanNode {
                id: "only".into(),
                objective: "do the thing".into(),
                agent: None,
                depends_on: Vec::new(),
            }],
            workflow_id: None,
        };
        let json = serde_json::to_value(&free_form).unwrap();
        assert!(json.get("workflow_id").is_none());
        assert_eq!(
            serde_json::from_value::<ServerMessage>(json).unwrap(),
            free_form
        );
    }

    #[test]
    fn a_node_that_never_runs_reports_skipped_with_its_node_id() {
        let message = ServerMessage::SubtaskEnd {
            subtask_id: SubtaskId::new(),
            node_id: "dependent".into(),
            status: SubtaskStatus::Skipped,
            summary: "a dependency did not succeed".into(),
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "subtask_end");
        assert_eq!(json["status"], "skipped");
        assert_eq!(json["node_id"], "dependent");
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, message);
    }

    #[test]
    fn the_workflow_queue_frames_round_trip() {
        let queued = ServerMessage::WorkflowQueued {
            job_id: 7,
            task: "ship the feature".into(),
            workflow_id: Some("feature-build".into()),
            position: 1,
        };
        let json = serde_json::to_value(&queued).unwrap();
        assert_eq!(json["type"], "workflow_queued");
        assert_eq!(json["job_id"], 7);
        assert_eq!(json["position"], 1);
        assert_eq!(
            serde_json::from_value::<ServerMessage>(json).unwrap(),
            queued
        );

        let started = ServerMessage::WorkflowStarted {
            job_id: 7,
            task: "ship the feature".into(),
            workflow_id: None,
        };
        let json = serde_json::to_value(&started).unwrap();
        assert_eq!(json["type"], "workflow_started");
        assert!(json.get("workflow_id").is_none());
        assert_eq!(
            serde_json::from_value::<ServerMessage>(json).unwrap(),
            started
        );

        let finished = ServerMessage::WorkflowFinished {
            job_id: 7,
            status: SubtaskStatus::Succeeded,
            summary: "3 of 3 node(s) succeeded".into(),
        };
        let json = serde_json::to_value(&finished).unwrap();
        assert_eq!(json["type"], "workflow_finished");
        assert_eq!(json["status"], "succeeded");
        assert_eq!(
            serde_json::from_value::<ServerMessage>(json).unwrap(),
            finished
        );
    }

    #[test]
    fn a_queue_workflow_frame_is_read_with_and_without_a_named_workflow() {
        let named = ClientEnvelope::new(ClientMessage::QueueWorkflow {
            task: "ship it".into(),
            workflow: Some("feature-build".into()),
            agent_id: None,
        });
        let json = serde_json::to_value(&named).unwrap();
        assert_eq!(json["type"], "queue_workflow");
        assert_eq!(json["workflow"], "feature-build");
        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, named.message);

        let chosen = ClientEnvelope::new(ClientMessage::QueueWorkflow {
            task: "ship it".into(),
            workflow: None,
            agent_id: None,
        });
        let json = serde_json::to_value(&chosen).unwrap();
        assert!(json.get("workflow").is_none());
        let back: ClientEnvelope = serde_json::from_value(json).unwrap();
        assert_eq!(back.message, chosen.message);
    }

    #[test]
    fn tool_approval_requests_are_representable() {
        let message = ServerMessage::ToolApprovalRequest {
            tool_call_id: "call_9".into(),
            name: "shell".into(),
            arguments: json!({ "command": "rm -rf /" }),
            reason: "destructive command".into(),
        };
        let json = serde_json::to_string(&message).unwrap();
        assert!(json.contains("\"type\":\"tool_approval_request\""));

        // The loop only holds the internal stream, so the request has to survive
        // the conversion the server's event bridge performs.
        let event = AgentEvent::ToolApprovalRequest {
            tool_call_id: "call_9".into(),
            name: "shell".into(),
            arguments: json!({ "command": "rm -rf /" }),
            reason: "destructive command".into(),
        };
        let json = serde_json::to_value(ServerMessage::from(event)).unwrap();
        assert_eq!(json["type"], "tool_approval_request");
        assert_eq!(json["tool_call_id"], "call_9");
        assert_eq!(json["reason"], "destructive command");
    }

    #[test]
    fn a_queued_message_reports_its_text_and_position() {
        let message = ServerMessage::MessageQueued {
            text: "run the tests".into(),
            position: 2,
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "message_queued");
        assert_eq!(json["text"], "run the tests");
        assert_eq!(json["position"], 2);

        let back: ServerMessage = serde_json::from_value(json).unwrap();
        assert_eq!(back, message);
    }
}
