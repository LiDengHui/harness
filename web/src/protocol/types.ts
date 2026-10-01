/**
 * The wire protocol, mirrored field-for-field from `harness-core/src/event.rs`.
 *
 * Two rules keep this file honest:
 *
 *   1. Field names and tag literals are copied from serde's output, not
 *      paraphrased. `#[serde(tag = "type", rename_all = "snake_case")]` means
 *      every discriminant is the snake_case variant name, and `Option<T>` without
 *      `skip_serializing_if` becomes an explicit `null`.
 *   2. An envelope *is* its JSON: the Rust structs flatten the message into the
 *      envelope, so `ServerEnvelope` here is the routing fields intersected with
 *      a message variant. There is no nested `message` property on the wire.
 */

/** Bump-on-breaking-change protocol version; every frame must declare it. */
export const PROTOCOL_VERSION = 1;

/** A JSON value as `serde_json::Value` produces it (tool arguments, mostly). */
export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | { [key: string]: JsonValue };

/** Relative importance of an inbound user instruction. */
export const PRIORITIES = ['low', 'normal', 'high'] as const;
export type Priority = (typeof PRIORITIES)[number];

/**
 * How hard the model should think about one message.
 *
 * These are the wire's values and the only ones the server accepts: a level it
 * does not know is rejected rather than clamped, so the UI must never offer or
 * send one. Mirrors the `effort` field of `ClientMessage::UserMessage` in
 * `crates/harness-core/src/event.rs`.
 */
export const EFFORTS = ['low', 'high', 'max'] as const;
export type Effort = (typeof EFFORTS)[number];

/**
 * The level a message carries when the user has not chosen one.
 *
 * The server has the same configured default, so omitting `effort` and sending
 * this are the same run — which is what lets a client that has no opinion stay
 * silent without changing the outcome.
 */
export const DEFAULT_EFFORT: Effort = 'high';

/** Terminal state of a single tool invocation. */
export const TOOL_CALL_STATUSES = ['ok', 'error', 'rejected', 'timeout'] as const;
export type ToolCallStatus = (typeof TOOL_CALL_STATUSES)[number];

/** Lifecycle state of one node in the orchestration DAG. */
export const SUBTASK_STATUSES = ['pending', 'running', 'succeeded', 'failed', 'skipped'] as const;
export type SubtaskStatus = (typeof SUBTASK_STATUSES)[number];

/** Lifecycle state of one queued workflow job. */
export const WORKFLOW_JOB_STATES = ['queued', 'running', 'finished'] as const;
export type WorkflowJobState = (typeof WORKFLOW_JOB_STATES)[number];

/**
 * One node of a run's plan, as the `plan` frame carries it.
 *
 * Mirrors `harness_orchestrator::graph::TaskNode`: `agent` and `depends_on` are
 * `#[serde(default)]` on the Rust side, so a node the planner left them off is
 * read here as "no agent named" and "no dependencies" rather than failing the
 * frame. `files` is deliberately not mirrored — nothing in the UI reads it, and
 * a field this build does not model must not become a reason to refuse a frame.
 */
export interface PlanNode {
  id: string;
  objective: string;
  agent: string | null;
  depends_on: string[];
}

/** Why the agent loop stopped. */
export const COMPLETION_REASONS = [
  'end_turn',
  'max_iterations',
  'aborted',
  'budget_exceeded',
  'error',
] as const;
export type CompletionReason = (typeof COMPLETION_REASONS)[number];

/** Estimated and reported token counts for one model interaction. */
export interface TokenUsage {
  input_tokens: number;
  output_tokens: number;
}

// ---------------------------------------------------------------------------
// Server -> client
// ---------------------------------------------------------------------------

export type ServerMessage =
  | { type: 'session_started'; session_id: string; agent_id: string; model: string }
  | { type: 'assistant_chunk'; text: string }
  | { type: 'thinking_chunk'; text: string }
  | { type: 'tool_call_start'; tool_call_id: string; name: string; arguments: JsonValue }
  | { type: 'tool_call_progress'; tool_call_id: string; message: string }
  | {
      type: 'tool_call_end';
      tool_call_id: string;
      name: string;
      status: ToolCallStatus;
      output: string;
      duration_ms: number;
    }
  | { type: 'subtask_start'; subtask_id: string; node_id: string; objective: string }
  | {
      type: 'subtask_end';
      subtask_id: string;
      node_id: string;
      status: SubtaskStatus;
      summary: string;
    }
  | {
      /**
       * The plan a run is executing, sent once before any of its nodes start.
       *
       * It carries *every* node, not only the ones that will run, which is what
       * lets a checklist show the steps that never run at all — those end as
       * `skipped` rather than being absent. `workflow_id` names the workflow the
       * plan came from, when one was chosen; a free-form plan carries none.
       *
       * The per-node status arrives on `subtask_start` / `subtask_end`, keyed by
       * `node_id` — the same key this frame's node `id`s use — so the
       * checklist's progress and the lanes are drawn from one source.
       */
      type: 'plan_created';
      nodes: PlanNode[];
      workflow_id?: string | null;
    }
  | {
      /**
       * A workflow job was accepted into this session's workflow queue.
       *
       * `position` is 1-based within the *workflow* queue; queued user messages
       * are not counted, because the two queues number themselves separately.
       */
      type: 'workflow_queued';
      job_id: number;
      task: string;
      workflow_id?: string | null;
      position: number;
    }
  | {
      /** A queued workflow job became the running one. */
      type: 'workflow_started';
      job_id: number;
      task: string;
      workflow_id?: string | null;
    }
  | {
      /**
       * A workflow job reached a terminal state.
       *
       * It names only the job: the `task` and `workflow_id` it was queued with
       * are already known from `workflow_queued`, so the row is updated rather
       * than repeated. The job's own `done` frame follows this one.
       */
      type: 'workflow_finished';
      job_id: number;
      status: SubtaskStatus;
      summary: string;
    }
  | { type: 'agent_handoff'; from: string; to: string; reason: string }
  | { type: 'token_usage'; usage: TokenUsage; budget_remaining: number | null }
  | { type: 'guardrail'; name: string; blocked: boolean; detail: string }
  | {
      type: 'tool_approval_request';
      tool_call_id: string;
      name: string;
      arguments: JsonValue;
      reason: string;
    }
  | {
      /**
       * A message was accepted while its session had a run in flight.
       *
       * The server no longer refuses a busy session, so this is the frame that
       * says where the text landed instead of an `error`. `position` is 1-based
       * within that session's queue — the running turn is not part of it — which
       * is what lets the client keep each session's queue in order without
       * holding a global one. Mirrors `ServerMessage::MessageQueued` in
       * `crates/harness-core/src/event.rs`.
       */
      type: 'message_queued';
      text: string;
      position: number;
    }
  | { type: 'error'; code: string; message: string }
  | { type: 'done'; reason: CompletionReason }
  | { type: 'pong' };

/** Every `ServerMessage` variant's `type` literal. */
export type ServerMessageType = ServerMessage['type'];

export const SERVER_MESSAGE_TYPES = [
  'session_started',
  'assistant_chunk',
  'thinking_chunk',
  'tool_call_start',
  'tool_call_progress',
  'tool_call_end',
  'subtask_start',
  'subtask_end',
  'plan_created',
  'workflow_queued',
  'workflow_started',
  'workflow_finished',
  'agent_handoff',
  'token_usage',
  'guardrail',
  'tool_approval_request',
  'message_queued',
  'error',
  'done',
  'pong',
] as const satisfies readonly ServerMessageType[];

/**
 * Routing context carried alongside every server message.
 *
 * `agent_id` names the agent that owns the session; `subagent_id` is set when
 * the frame originates from a delegated sub-agent, which is what lets the UI
 * draw one lane per worker over a single socket.
 *
 * `subagent_session_id` is the sub-agent's *own* session: the server stores each
 * delegated run as a session of its own, forked from the session that delegated
 * it, so this is the id a lane's history is read back from
 * (`GET /api/sessions/{id}/history`). It is optional because the field is still
 * rolling out; a frame without it still draws its lane, the lane is just not
 * openable. See `SubagentLane` in `stores/transcript.ts` for how that reads.
 */
export interface ServerEnvelopeRouting {
  v: number;
  session_id: string;
  agent_id: string;
  subagent_id?: string | null;
  subagent_session_id?: string | null;
}

type Distributed<M> = M extends ServerMessage ? ServerEnvelopeRouting & M : never;

/** `{ v, session_id, agent_id, subagent_id?, ...message }`, as sent. */
export type ServerEnvelope = Distributed<ServerMessage>;

// ---------------------------------------------------------------------------
// Client -> server
// ---------------------------------------------------------------------------

export type ClientMessage =
  | {
      type: 'user_message';
      text: string;
      /** Overrides the session's agent for this run only. */
      agent_id?: string | null;
      /**
       * How hard to think about this message, for this run only.
       *
       * Optional on the wire — a frame without it runs at the server's own
       * default — but the composer always names one, because the level is a
       * choice the reader made and silently dropping it would ignore that.
       */
      effort?: Effort | null;
    }
  | {
      /**
       * Ask for a plan rather than an answer: the agent works out what the task
       * needs and hands the pieces to sub-agents instead of doing it itself.
       *
       * The body field is `task`, not `text` — the server names it that way, and
       * the two messages are otherwise the same envelope.
       */
      type: 'plan_task';
      task: string;
      /** Overrides the session's agent for this run only. */
      agent_id?: string | null;
    }
  | {
      /**
       * Queue a task on this session's workflow queue.
       *
       * Distinct from `plan_task`, which asks the planner to make a plan up:
       * this enqueues a job, and the server answers with `workflow_queued` /
       * `workflow_started` / `workflow_finished` rather than a run of its own.
       * The queue is per session and separate from the message queue, so a
       * workflow waiting behind a run is visible apart from the messages
       * waiting behind it.
       *
       * `workflow` names the recipe to run. It is omitted when the caller wants
       * the server to choose one, which is also what the frame does when the
       * user left the picker automatic.
       */
      type: 'queue_workflow';
      task: string;
      workflow?: string | null;
      /** Overrides the session's agent for this job only. */
      agent_id?: string | null;
    }
  | { type: 'steering_message'; text: string; priority: Priority }
  | { type: 'abort'; reason?: string | null }
  | { type: 'tool_approval'; tool_call_id: string; approved: boolean; reason?: string | null }
  | { type: 'subscribe'; session_id: string }
  | { type: 'ping' };

export interface ClientEnvelopeRouting {
  v: number;
  /** Optional client-side correlation id; the server echoes nothing for it. */
  id?: string;
  session_id?: string;
}

type Distributable<M> = M extends ClientMessage ? ClientEnvelopeRouting & M : never;

/** `{ v, id?, session_id?, ...message }`, as accepted by the server. */
export type ClientEnvelope = Distributable<ClientMessage>;

export interface ClientRouting {
  sessionId?: string;
  id?: string;
}

/**
 * Builds a frame in the flattened shape the server parses.
 *
 * `v` is always written, matching `#[serde(default = "default_version")]` on a
 * field that is not skipped; the routing fields are omitted rather than sent as
 * `null`, matching their `skip_serializing_if`.
 */
export function clientEnvelope(message: ClientMessage, routing: ClientRouting = {}): ClientEnvelope {
  // The spread of a union-typed value widens to a single object type here; the
  // assertion only re-distributes it over the union, which the compiler cannot
  // do on its own.
  const envelope = { v: PROTOCOL_VERSION, ...message } as ClientEnvelopeRouting & ClientMessage;
  if (routing.id !== undefined) {
    envelope.id = routing.id;
  }
  if (routing.sessionId !== undefined) {
    envelope.session_id = routing.sessionId;
  }
  return envelope as ClientEnvelope;
}

/** Serializes a client envelope to the exact bytes the server expects. */
export function serializeClientEnvelope(envelope: ClientEnvelope): string {
  return JSON.stringify(envelope);
}
