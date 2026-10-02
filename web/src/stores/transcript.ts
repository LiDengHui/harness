/**
 * The transcript model, and the pure fold that builds it.
 *
 * The bus carries a flat, context-free event stream: there is no `turn_started`
 * on the wire (it is an internal `AgentEvent` the server does not forward) and
 * no user echo. So the timeline has to be *inferred*:
 *
 *   * a turn opens on the first frame that belongs to the model's output
 *     (an assistant chunk, a thinking chunk, or a tool call) and closes on
 *     `done` — the turn is identified by the assistant output it holds, which
 *     is what makes the cards group the way they do;
 *   * assistant text accumulates into one bubble per contiguous run inside that
 *     turn, and thinking into a separate collapsible block beside it;
 *   * tool cards are keyed by `tool_call_id`, so a progress or end frame that
 *     arrives after other output still updates the card it belongs to;
 *   * frames carrying a `subagent_id` are folded into that sub-agent's lane
 *     instead of the main one, so a delegation never interleaves the parent's
 *     transcript.
 *
 * The fold mutates in place. Everything here is held in `reactive()`/`ref()`
 * state, and replacing arrays wholesale on every delta would defeat the
 * granular updates that make a streaming transcript cheap to render.
 */

import { i18n } from '../i18n';
import type {
  CompletionReason,
  JsonValue,
  ServerEnvelope,
  SubtaskStatus,
  TokenUsage,
  ToolCallStatus,
  WorkflowJobState,
} from '../protocol';

/** How much of a tool output a card shows before it offers the full text. */
export const TOOL_OUTPUT_PREVIEW_CHARS = 2_000;

/** Bounded history for the diagnostics strip. */
const MAX_DIAGNOSTIC_SAMPLES = 50;

export interface ToolCall {
  toolCallId: string;
  name: string;
  arguments: JsonValue;
  /** Model-facing status; `running` until `tool_call_end` arrives. */
  status: ToolCallStatus | 'running';
  output: string;
  durationMs: number | null;
  progress: string[];
  startedAt: number;
  endedAt: number | null;
  /** The sub-agent lane the call ran on, if any. */
  lane: string | null;
}

export type Block =
  | { kind: 'text'; id: string; text: string }
  | { kind: 'thinking'; id: string; text: string }
  | { kind: 'tool'; id: string; call: ToolCall };

export interface TurnEntry {
  kind: 'turn';
  id: string;
  /** 1-based position among the turns of this transcript lane. */
  index: number;
  agentId: string;
  startedAt: number;
  endedAt: number | null;
  blocks: Block[];
  done: CompletionReason | null;
}

export interface UserEntry {
  kind: 'user';
  id: string;
  text: string;
  at: number;
}

export interface HandoffEntry {
  kind: 'handoff';
  id: string;
  from: string;
  to: string;
  reason: string;
  at: number;
}

export interface SubtaskEntry {
  kind: 'subtask';
  id: string;
  subtaskId: string;
  /**
   * The plan node this record belongs to, or `''` when the frame named none.
   *
   * `subtask_id` is the worker's own lane id; this is the key the plan's rows
   * use. The two are deliberately separate — a plan row is looked up by this,
   * never by the lane id — and an empty one means the record belongs to no
   * checklist row (a delegation outside a plan, or a frame from a server that
   * does not send the field yet).
   */
  nodeId: string;
  /**
   * The worker's agent, from the frame that announced this node.
   *
   * The lane key is the agent id, so every worker of the same agent shares one
   * lane; this is what lets a plan row name the agent that ran *it* rather than
   * whichever worker the lane happened to see first.
   */
  agentId: string | null;
  /**
   * The worker's own session, from the frame that announced this node.
   *
   * Recorded here rather than read off the lane for the same reason as
   * `agentId`: the lane keeps only the most recent worker's session, so a plan
   * row that used the lane's would open the wrong conversation when several
   * nodes ran under one agent.
   */
  subagentSessionId: string | null;
  objective: string;
  status: SubtaskStatus;
  summary: string;
  at: number;
  endedAt: number | null;
}

export interface ErrorEntry {
  kind: 'error';
  id: string;
  code: string;
  message: string;
  at: number;
  /**
   * Set when the frame is a tool waiting on a human: the fields the sentence is
   * built from, kept apart from `message` so it can be re-worded per locale.
   */
  approval?: { name: string; toolCallId: string; reason: string };
}

export type TimelineEntry = UserEntry | TurnEntry | HandoffEntry | SubtaskEntry | ErrorEntry;

/**
 * A tool call stopped until a human decides.
 *
 * Kept apart from the error-shaped entry that records it: the entry is the log
 * line, this is the live request the dialog answers. The arguments ride along
 * because the decision is made from them — the tool's name alone does not say
 * what the call is about to do.
 */
export interface PendingApproval {
  toolCallId: string;
  name: string;
  arguments: JsonValue;
  reason: string;
  at: number;
}

/**
 * One step of the plan the current run is executing.
 *
 * `id` is the plan node id, which is also the `subtask_id` its `subtask_start`
 * and `subtask_end` frames carry — that equality is what makes a checklist row
 * and a lane's record the same thing rather than two views that can disagree.
 */
export interface PlanNodeEntry {
  id: string;
  objective: string;
  /** The agent the plan named, or `null` when it named none. */
  agent: string | null;
  dependsOn: string[];
}

/**
 * The plan of the run the transcript is showing, or `null` when it has none.
 *
 * `source` records where it came from, and it is not decoration: a plan frame
 * carries the whole graph, while a plan assembled from `subtask_start` frames
 * only holds the steps that actually started. The view says which of the two it
 * is showing rather than presenting an inferred plan as the server's own.
 */
export interface RunPlan {
  nodes: PlanNodeEntry[];
  /** The workflow this plan came from, when the plan frame named one. */
  workflowId: string | null;
  source: 'frame' | 'inferred';
}

/**
 * One workflow job waiting on, running in, or finished for this session.
 *
 * The row is keyed by `job_id` and every state change updates it in place, so a
 * job that goes queued → running → finished stays one row. `sessionId` is the
 * session the job runs in, which is what the row's "view plan" control opens.
 */
export interface WorkflowJob {
  /** The server's `job_id`, as a string: it is a u64 on the wire. */
  id: string;
  /** The workflow the job runs, or `null` when the server chose it. */
  workflowId: string | null;
  task: string;
  /** 1-based place in the workflow queue, as `workflow_queued` numbered it. */
  position: number | null;
  state: WorkflowJobState;
  /** The terminal outcome, once `workflow_finished` has arrived. */
  status: SubtaskStatus | null;
  /** What the finished job reported, or `''` while it has not. */
  summary: string;
  sessionId: string;
  /** When the frame last changed it. */
  at: number;
  /**
   * The bubble drawn for the job's own task once it started running.
   *
   * The user's words are not echoed by the server, so the bubble is placed when
   * the job stops waiting — the same moment a queued message's bubble is placed.
   * The id is kept so the job stays one row rather than drawing a bubble again
   * on every later state change.
   */
  userEntryId: string | null;
}

/**
 * What a workflow frame says about a job, before it is folded in.
 *
 * `workflow_finished` names only the job and its outcome, so every field here
 * but the id is optional: the fold keeps what the row already holds when a
 * frame does not repeat it.
 */
export interface WorkflowJobUpdate {
  id: string;
  task?: string;
  workflowId?: string | null;
  position?: number | null;
  state?: WorkflowJobState;
  status?: SubtaskStatus | null;
  summary?: string;
  sessionId: string;
  at: number;
}

/** One sub-agent's own transcript, keyed by `subagent_id`. */
export interface SubagentLane {
  subagentId: string;
  /**
   * The sub-agent's own session, when the server names one.
   *
   * The server stores each delegated run as a session of its own — forked from
   * the session that delegated it — and stamps every frame the worker produces
   * with that session's id. It is the id the lane's conversation is read back
   * from, and it is `null` for a lane whose frames carried no such hint, which
   * is what makes that lane readable but not openable. See `laneTarget`.
   */
  subagentSessionId: string | null;
  entries: TimelineEntry[];
  openTurnId: string | null;
}

/**
 * What the view needs to open a sub-agent's own conversation.
 *
 * The three things the detail view has to say are carried here, because two of
 * them — which sub-agent this was, and what it was asked to do — exist only in
 * the lane the reader clicked, and would be lost the moment the pane moved to
 * the sub-agent's own session.
 */
export interface SubagentTarget {
  /** The sub-agent's own session id: what its history is read from. */
  sessionId: string;
  /** The sub-agent's id, as the lane names it. */
  subagentId: string;
  /** What the sub-agent was asked to do, or `null` when none was recorded. */
  objective: string | null;
}

export interface TokenSample {
  at: number;
  usage: TokenUsage;
  budgetRemaining: number | null;
}

export interface GuardrailNote {
  at: number;
  name: string;
  blocked: boolean;
  detail: string;
}

export interface Diagnostics {
  tokens: TokenSample[];
  guardrails: GuardrailNote[];
}

export type SessionStatus = 'idle' | 'running' | 'done' | 'error';

/**
 * A message accepted while its session had a run in flight.
 *
 * One queue per session, never one global queue: two sessions run side by side,
 * so a message waiting on one must not be handed to the other. `position` is the
 * 1-based place the server assigned; the id is local, so a queue row can be keyed
 * without leaning on a position the server may reuse once a run drains.
 */
export interface QueuedMessage {
  id: string;
  text: string;
  /** 1-based position within this session's queue, as the server numbered it. */
  position: number;
}

/**
 * How many messages one session's queue will mirror.
 *
 * The queue is the client's copy of a server-side one, so it only has to be
 * large enough to render; the bound keeps a server that never drains from
 * growing this in memory without limit. Like the transport's outbound queue, the
 * oldest entries are dropped first — the newest are what the user just asked for.
 */
export const MAX_QUEUED_MESSAGES = 32;

/**
 * How many workflow jobs one session's queue will mirror.
 *
 * Bounded for the same reason the message queue is: it is the client's copy of
 * a server-side queue, so it only has to be large enough to render. The oldest
 * entries are dropped first, because the newest are what the user just asked
 * for.
 */
export const MAX_WORKFLOW_JOBS = 32;

export interface Transcript {
  sessionId: string;
  agentId: string;
  model: string;
  startedAt: number | null;
  status: SessionStatus;
  /** Why the most recent run stopped, so an abort before any output is visible. */
  lastCompletion: CompletionReason | null;
  entries: TimelineEntry[];
  lanes: Record<string, SubagentLane>;
  diagnostics: Diagnostics;
  /** The turn currently being written on the main lane. */
  openTurnId: string | null;
  /** Messages accepted for this session that have not become a run yet. */
  queue: QueuedMessage[];
  /** The plan of the run in flight, when it has one. */
  plan: RunPlan | null;
  /** Workflow jobs this session has queued, running or finished. */
  workflowQueue: WorkflowJob[];
  /** Tool calls stopped until the user decides; the dialog is driven by this. */
  approvals: PendingApproval[];
}

/** A transcript container: the main timeline and every lane are both targets. */
interface FoldTarget {
  entries: TimelineEntry[];
  openTurnId: string | null;
}

let sequence = 0;

function nextId(prefix: string): string {
  sequence += 1;
  return `${prefix}_${sequence}`;
}

export function createTranscript(sessionId: string): Transcript {
  return {
    sessionId,
    agentId: '',
    model: '',
    startedAt: null,
    status: 'idle',
    lastCompletion: null,
    entries: [],
    lanes: {},
    diagnostics: { tokens: [], guardrails: [] },
    openTurnId: null,
    queue: [],
    plan: null,
    workflowQueue: [],
    approvals: [],
  };
}

export function appendUserEntry(transcript: Transcript, text: string, at = Date.now()): UserEntry {
  const entry: UserEntry = { kind: 'user', id: nextId('user'), text, at };
  transcript.entries.push(entry);
  return entry;
}

/**
 * Takes back a user bubble the store drew before the server answered.
 *
 * A send is drawn the moment it goes out, so a refusal has to be able to remove
 * the bubble it belongs to: leaving it would show a message that nothing is
 * going to answer. Only the entry the send itself drew is removed — by its id,
 * so a bubble from another send is never taken with it.
 */
export function removeUserEntry(transcript: Transcript, id: string): boolean {
  const index = transcript.entries.findIndex((entry) => entry.kind === 'user' && entry.id === id);
  if (index < 0) return false;
  transcript.entries.splice(index, 1);
  return true;
}

/**
 * Clears a pending approval once it has been answered.
 *
 * The error-shaped entry that recorded the request stays in the log — the
 * decision is part of the run's history — but the live request goes, so the
 * dialog stops offering a choice that has already been made.
 */
export function removeApproval(transcript: Transcript, toolCallId: string): boolean {
  const index = transcript.approvals.findIndex((item) => item.toolCallId === toolCallId);
  if (index < 0) return false;
  transcript.approvals.splice(index, 1);
  return true;
}

/**
 * Marks a run as in flight.
 *
 * The server only sends `session_started` when it *opens* a session, so a second
 * run on the same session produces no frame that says "running" — the client has
 * to infer it from having sent a message the server accepted.
 *
 * The previous run's plan goes with it: a checklist belongs to one run, and
 * leaving the last one on screen while the next is starting would show a plan
 * that is no longer executing. A plan frame for this run replaces it.
 */
export function markRunStarted(transcript: Transcript): void {
  beginRun(transcript);
}

/** Starts a run: in flight, and holding no plan from the run before it. */
function beginRun(transcript: Transcript): void {
  transcript.status = 'running';
  transcript.plan = null;
}

/**
 * Undoes `markRunStarted` when the run turned out not to exist.
 *
 * Either the frame was refused (the server answered `not_running`) or the socket
 * that was driving the run went away, which aborts the run server-side.
 */
export function markRunAbandoned(transcript: Transcript): void {
  if (transcript.status !== 'running') return;
  transcript.status = transcript.lastCompletion === 'error' ? 'error' : transcript.lastCompletion ? 'done' : 'idle';
}

/**
 * Adds a message the server accepted to this session's queue.
 *
 * A position the queue already holds is the same message announced twice, so the
 * text is refreshed in place rather than a duplicate row appearing.
 */
export function enqueueQueued(
  transcript: Transcript,
  text: string,
  position: number,
): QueuedMessage {
  const existing = transcript.queue.find((entry) => entry.position === position);
  if (existing) {
    existing.text = text;
    return existing;
  }

  const entry: QueuedMessage = { id: nextId('queued'), text, position };
  transcript.queue.push(entry);
  if (transcript.queue.length > MAX_QUEUED_MESSAGES) {
    transcript.queue.splice(0, transcript.queue.length - MAX_QUEUED_MESSAGES);
  }
  return entry;
}

/**
 * Turns the head of the queue into the session's active turn.
 *
 * The user's own text is not echoed by the server, so the bubble is placed here,
 * at the moment the message stops waiting and starts running. Returns the entry
 * that was promoted, or `null` when nothing was waiting.
 */
export function promoteQueued(transcript: Transcript, at: number = Date.now()): QueuedMessage | null {
  const entry = transcript.queue.shift();
  if (!entry) return null;
  appendUserEntry(transcript, entry.text, at);
  beginRun(transcript);
  return entry;
}

/** Drops every waiting message; used when the connection driving them is gone. */
export function clearQueue(transcript: Transcript): void {
  transcript.queue = [];
}

/**
 * Whether a frame is the first output of a run that has not been marked running.
 *
 * A run opens on the same three frames the fold treats as model output, and only
 * on the main lane: a sub-agent frame belongs to a run already in flight. The
 * status guard is what keeps a message queued behind a *running* session from
 * being promoted by that run's own first chunk.
 */
function startsQueuedRun(transcript: Transcript, envelope: ServerEnvelope): boolean {
  if (envelope.subagent_id != null) return false;
  if (transcript.status === 'running' || transcript.queue.length === 0) return false;
  if (transcript.openTurnId !== null) return false;
  return (
    envelope.type === 'assistant_chunk' ||
    envelope.type === 'thinking_chunk' ||
    envelope.type === 'tool_call_start'
  );
}

/** Error codes that mean no run was started, so "running" was a wrong guess. */
const NO_RUN_ERROR_CODES = new Set([
  'not_running',
  'agent_not_found',
  'unknown_session',
  'bad_message',
  'config',
  'session_error',
]);

/**
 * Whether an error means the run the client optimistically marked never started.
 *
 * The fold uses this to take back the inferred `running`; the store uses it to
 * take back the optimistic bubble beside it, so the two halves of the same guess
 * are undone together. An error that arrives *during* a run (`agent_error`, a
 * guardrail) is not one of these: that run really is in flight.
 */
export function isNoRunError(code: string): boolean {
  return NO_RUN_ERROR_CODES.has(code);
}

/** The turn a lane is currently writing, opening one if needed. */
function openTurn(target: FoldTarget, agentId: string, at: number): TurnEntry {
  if (target.openTurnId !== null) {
    const existing = target.entries.find(
      (entry): entry is TurnEntry => entry.kind === 'turn' && entry.id === target.openTurnId,
    );
    if (existing && existing.endedAt === null) return existing;
  }

  const turn: TurnEntry = {
    kind: 'turn',
    id: nextId('turn'),
    index: target.entries.filter((entry) => entry.kind === 'turn').length + 1,
    agentId,
    startedAt: at,
    endedAt: null,
    blocks: [],
    done: null,
  };
  target.entries.push(turn);
  target.openTurnId = turn.id;
  return turn;
}

function closeOpenTurn(target: FoldTarget, reason: CompletionReason, at: number): TurnEntry | null {
  if (target.openTurnId === null) return null;
  const turn = target.entries.find(
    (entry): entry is TurnEntry => entry.kind === 'turn' && entry.id === target.openTurnId,
  );
  target.openTurnId = null;
  if (!turn) return null;
  turn.endedAt = at;
  turn.done = reason;
  return turn;
}

function appendText(turn: TurnEntry, text: string): void {
  const last = turn.blocks[turn.blocks.length - 1];
  if (last && last.kind === 'text') {
    last.text += text;
    return;
  }
  turn.blocks.push({ kind: 'text', id: nextId('text'), text });
}

function appendThinking(turn: TurnEntry, text: string): void {
  const last = turn.blocks[turn.blocks.length - 1];
  if (last && last.kind === 'thinking') {
    last.text += text;
    return;
  }
  turn.blocks.push({ kind: 'thinking', id: nextId('thinking'), text });
}

function findTool(transcript: Transcript, toolCallId: string): ToolCall | null {
  for (const entries of entryLists(transcript)) {
    for (const entry of entries) {
      if (entry.kind !== 'turn') continue;
      for (const block of entry.blocks) {
        if (block.kind === 'tool' && block.call.toolCallId === toolCallId) return block.call;
      }
    }
  }
  return null;
}

/** Every list of entries the transcript holds: the main lane and each sub-agent. */
function entryLists(transcript: Transcript): TimelineEntry[][] {
  return [transcript.entries, ...Object.values(transcript.lanes).map((lane) => lane.entries)];
}

/**
 * The subtask a frame refers to, wherever it was announced.
 *
 * A `subtask_end` has to find a start that may have landed in a lane, and the
 * two frames are not guaranteed to carry the same routing context, so the lookup
 * spans the main lane and every sub-agent lane.
 */
function findSubtask(transcript: Transcript, subtaskId: string): SubtaskEntry | null {
  for (const entries of entryLists(transcript)) {
    for (const entry of entries) {
      if (entry.kind === 'subtask' && entry.subtaskId === subtaskId) return entry;
    }
  }
  return null;
}

/**
 * The key a plan row is looked up by.
 *
 * The server names the plan node on `subtask_start` / `subtask_end` as
 * `node_id`; a frame from a server that does not send it yet falls back to the
 * subtask's own id, which is the best key available and keeps the checklist
 * working through the rollout.
 */
function nodeKey(nodeId: string, subtaskId: string): string {
  return nodeId === '' ? subtaskId : nodeId;
}

/**
 * The record of the plan node with this id, wherever it was announced.
 *
 * The newest record wins: node ids are the planner's slugs, so two runs on one
 * session can both have a `review` node, and the row has to read the run on
 * screen rather than the first one that ever used the name. Ordering by the
 * frame's own time is what does it — the records live in different lists (a
 * worker's lane, the main timeline), so "last in the list" would compare the
 * wrong things.
 */
function findSubtaskByNode(transcript: Transcript, nodeId: string): SubtaskEntry | null {
  let found: SubtaskEntry | null = null;
  for (const entries of entryLists(transcript)) {
    for (const entry of entries) {
      if (entry.kind !== 'subtask') continue;
      if (nodeKey(entry.nodeId, entry.subtaskId) !== nodeId) continue;
      if (found === null || entry.at >= found.at) found = entry;
    }
  }
  return found;
}

function ensureLane(transcript: Transcript, subagentId: string): SubagentLane {
  const existing = transcript.lanes[subagentId];
  if (existing) return existing;
  const lane: SubagentLane = { subagentId, subagentSessionId: null, entries: [], openTurnId: null };
  transcript.lanes[subagentId] = lane;
  return lane;
}

/**
 * Adds a step to the checklist, opening a plan for it if there is none.
 *
 * A plan frame is authoritative and arrives first, so this usually only appends
 * a node the frame did not list — which keeps a running step visible rather than
 * silently dropping it. When no frame ever arrives, the plan is assembled from
 * the steps that actually started and is marked `inferred`, so the view can say
 * that it is not the server's full plan.
 */
function ensurePlanNode(transcript: Transcript, node: PlanNodeEntry): void {
  const plan = transcript.plan;
  if (plan === null) {
    transcript.plan = { nodes: [node], workflowId: null, source: 'inferred' };
    return;
  }
  if (plan.nodes.some((existing) => existing.id === node.id)) return;
  plan.nodes.push(node);
}

/**
 * Adds a workflow job or refreshes the one already on the queue.
 *
 * A frame carries only what changed: `workflow_finished` names the job and its
 * outcome, not the task it was queued with, so a field the frame leaves out
 * keeps what the row already holds. The job's own task becomes the user's
 * message the moment it starts running — the server never echoes the user's
 * text, so this is the only place the words they typed can be placed. It is
 * drawn once, which is why the id is remembered.
 */
export function upsertWorkflowJob(transcript: Transcript, update: WorkflowJobUpdate): WorkflowJob {
  const existing = transcript.workflowQueue.find((job) => job.id === update.id);
  if (existing) {
    const starting = existing.state !== 'running' && update.state === 'running';
    if (update.task !== undefined) existing.task = update.task;
    if (update.workflowId !== undefined) existing.workflowId = update.workflowId;
    if (update.position !== undefined) existing.position = update.position;
    if (update.state !== undefined) existing.state = update.state;
    if (update.status !== undefined) existing.status = update.status;
    if (update.summary !== undefined) existing.summary = update.summary;
    existing.sessionId = update.sessionId;
    existing.at = update.at;
    if (starting) drawJobTask(transcript, existing, update.at);
    return existing;
  }

  const job: WorkflowJob = {
    id: update.id,
    workflowId: update.workflowId ?? null,
    task: update.task ?? '',
    position: update.position ?? null,
    state: update.state ?? 'queued',
    status: update.status ?? null,
    summary: update.summary ?? '',
    sessionId: update.sessionId,
    at: update.at,
    userEntryId: null,
  };
  transcript.workflowQueue.push(job);
  if (transcript.workflowQueue.length > MAX_WORKFLOW_JOBS) {
    transcript.workflowQueue.splice(0, transcript.workflowQueue.length - MAX_WORKFLOW_JOBS);
  }
  if (job.state === 'running') drawJobTask(transcript, job, update.at);
  return job;
}

function drawJobTask(transcript: Transcript, job: WorkflowJob, at: number): void {
  if (job.userEntryId !== null) return;
  job.userEntryId = appendUserEntry(transcript, job.task, at).id;
}

function pushDiagnostic<T>(samples: T[], sample: T): void {
  samples.push(sample);
  if (samples.length > MAX_DIAGNOSTIC_SAMPLES) {
    samples.splice(0, samples.length - MAX_DIAGNOSTIC_SAMPLES);
  }
}

/**
 * Folds one server frame into the transcript, in place.
 *
 * Unknown-but-valid frames (`pong`) are ignored; the caller has already dealt
 * with frames the parser refused.
 */
export function foldFrame(
  transcript: Transcript,
  envelope: ServerEnvelope,
  at: number = Date.now(),
): void {
  const lane = envelope.subagent_id ? ensureLane(transcript, envelope.subagent_id) : null;
  // The lane's own session, learned from whichever frame names it first: a
  // sub-agent's session exists for as long as its run does, and every frame the
  // run produces carries the same id.
  if (lane !== null && envelope.subagent_session_id != null) {
    lane.subagentSessionId = envelope.subagent_session_id;
  }
  const target: FoldTarget = lane ?? transcript;

  // A run can start without a `session_started` (the server only sends that when
  // it opens a session), so the queued head is promoted on whichever frame
  // announces the run — the frame itself, or the first output after the last one
  // closed. Promoting before the switch keeps the user's bubble ahead of the
  // turn it belongs to.
  if (envelope.type === 'session_started' || startsQueuedRun(transcript, envelope)) {
    promoteQueued(transcript, at);
  }

  // Output on the main lane means a run is under way. `session_started` only
  // announces the session, so the flag comes from here and from the store's own
  // optimistic send; this is also what recovers it for a run nobody sent for.
  // A run that was not already running begins here, which is what retires the
  // plan the previous run left on screen.
  if (
    lane === null &&
    transcript.status !== 'running' &&
    (envelope.type === 'assistant_chunk' ||
      envelope.type === 'thinking_chunk' ||
      envelope.type === 'tool_call_start')
  ) {
    beginRun(transcript);
  }

  switch (envelope.type) {
    case 'session_started': {
      transcript.sessionId = envelope.session_id;
      transcript.agentId = envelope.agent_id;
      transcript.model = envelope.model;
      transcript.startedAt = at;
      return;
    }

    case 'assistant_chunk':
      appendText(openTurn(target, envelope.agent_id, at), envelope.text);
      return;

    case 'thinking_chunk':
      appendThinking(openTurn(target, envelope.agent_id, at), envelope.text);
      return;

    case 'tool_call_start': {
      const turn = openTurn(target, envelope.agent_id, at);
      const call: ToolCall = {
        toolCallId: envelope.tool_call_id,
        name: envelope.name,
        arguments: envelope.arguments,
        status: 'running',
        output: '',
        durationMs: null,
        progress: [],
        startedAt: at,
        endedAt: null,
        lane: envelope.subagent_id ?? null,
      };
      turn.blocks.push({ kind: 'tool', id: nextId('tool'), call });
      return;
    }

    case 'tool_call_progress': {
      const call = findTool(transcript, envelope.tool_call_id);
      if (call) call.progress.push(envelope.message);
      return;
    }

    case 'tool_call_end': {
      const call = findTool(transcript, envelope.tool_call_id);
      if (!call) return;
      call.status = envelope.status;
      call.output = envelope.output;
      call.durationMs = envelope.duration_ms;
      call.endedAt = at;
      return;
    }

    case 'subtask_start': {
      const entry: SubtaskEntry = {
        kind: 'subtask',
        id: nextId('subtask'),
        subtaskId: envelope.subtask_id,
        nodeId: envelope.node_id,
        agentId: envelope.subagent_id ?? null,
        subagentSessionId: envelope.subagent_session_id ?? null,
        objective: envelope.objective,
        status: 'running',
        summary: '',
        at,
        endedAt: null,
      };
      // A frame that names a sub-agent belongs in that worker's lane; only the
      // session's own frames stay on the main timeline.
      target.entries.push(entry);
      // The node joins the checklist even when no plan frame announced it: the
      // plan may still be rolling out, and a step that is visibly running must
      // not be missing from the list of steps.
      ensurePlanNode(transcript, {
        id: nodeKey(envelope.node_id, envelope.subtask_id),
        objective: envelope.objective,
        agent: envelope.subagent_id ?? null,
        dependsOn: [],
      });
      return;
    }

    case 'plan_created': {
      // The whole graph, once. An empty plan is not a plan: it would replace a
      // real checklist with a blank panel, so it is ignored.
      if (envelope.nodes.length === 0) return;
      // A plan frame is output of a run, so it is also evidence one is in
      // flight — which is what keeps a client that subscribed mid-run from
      // reading every step as skipped before the first chunk arrives.
      if (transcript.status !== 'running') transcript.status = 'running';
      transcript.plan = {
        nodes: envelope.nodes.map((node) => ({
          id: node.id,
          objective: node.objective,
          agent: node.agent,
          dependsOn: [...node.depends_on],
        })),
        workflowId: envelope.workflow_id ?? null,
        source: 'frame',
      };
      return;
    }

    case 'workflow_queued': {
      upsertWorkflowJob(transcript, {
        id: String(envelope.job_id),
        task: envelope.task,
        workflowId: envelope.workflow_id ?? null,
        position: envelope.position,
        state: 'queued',
        sessionId: envelope.session_id,
        at,
      });
      return;
    }

    case 'workflow_started': {
      // A job is a run too, so the session is in flight from this frame — there
      // is no `session_started`-then-output delay to wait for, and the stop
      // control has to be live while the job's plan is being made.
      if (transcript.status !== 'running') transcript.status = 'running';
      upsertWorkflowJob(transcript, {
        id: String(envelope.job_id),
        task: envelope.task,
        workflowId: envelope.workflow_id ?? null,
        state: 'running',
        sessionId: envelope.session_id,
        at,
      });
      return;
    }

    case 'workflow_finished': {
      // The frame names only the job: its task and workflow are already on the
      // row, so they are left as they are rather than blanked.
      upsertWorkflowJob(transcript, {
        id: String(envelope.job_id),
        state: 'finished',
        status: envelope.status,
        summary: envelope.summary,
        sessionId: envelope.session_id,
        at,
      });
      return;
    }

    case 'subtask_end': {
      const entry = findSubtask(transcript, envelope.subtask_id);
      if (!entry) return;
      entry.status = envelope.status;
      entry.summary = envelope.summary;
      entry.endedAt = at;
      return;
    }

    case 'agent_handoff': {
      // The lane is created here rather than when the first frame arrives, so
      // the delegation is visible from the moment it is announced.
      ensureLane(transcript, envelope.to);
      const entry: HandoffEntry = {
        kind: 'handoff',
        id: nextId('handoff'),
        from: envelope.from,
        to: envelope.to,
        reason: envelope.reason,
        at,
      };
      transcript.entries.push(entry);
      return;
    }

    case 'token_usage':
      pushDiagnostic(transcript.diagnostics.tokens, {
        at,
        usage: envelope.usage,
        budgetRemaining: envelope.budget_remaining,
      });
      return;

    case 'guardrail':
      pushDiagnostic(transcript.diagnostics.guardrails, {
        at,
        name: envelope.name,
        blocked: envelope.blocked,
        detail: envelope.detail,
      });
      return;

    case 'tool_approval_request': {
      // The log keeps an error-shaped entry, so the transcript does not silently
      // omit a tool waiting on a human.
      const entry: ErrorEntry = {
        kind: 'error',
        id: nextId('approval'),
        code: 'tool_approval_request',
        // The reason is the raw material; `describeError` composes the sentence.
        message: envelope.reason,
        at,
        approval: {
          name: envelope.name,
          toolCallId: envelope.tool_call_id,
          reason: envelope.reason,
        },
      };
      transcript.entries.push(entry);
      // The dialog reads this list rather than scanning the entries above, so a
      // request and its log line can be worded independently. A repeated request
      // for the same call refreshes the item instead of stacking a second one.
      const existing = transcript.approvals.find(
        (item) => item.toolCallId === envelope.tool_call_id,
      );
      if (existing) {
        existing.name = envelope.name;
        existing.arguments = envelope.arguments;
        existing.reason = envelope.reason;
        existing.at = at;
      } else {
        transcript.approvals.push({
          toolCallId: envelope.tool_call_id,
          name: envelope.name,
          arguments: envelope.arguments,
          reason: envelope.reason,
          at,
        });
      }
      return;
    }

    case 'message_queued':
      // The session's own queue, never a global one: the message waits behind
      // *this* session's run and is promoted by it alone.
      enqueueQueued(transcript, envelope.text, envelope.position);
      return;

    case 'error': {
      const entry: ErrorEntry = {
        kind: 'error',
        id: nextId('error'),
        code: envelope.code,
        message: envelope.message,
        at,
      };
      target.entries.push(entry);
      // A run that never started is not a run: the optimistic "running" has to
      // be taken back. A busy session no longer produces an error frame at all
      // (the message is queued), so nothing here treats `busy` as a failure.
      if (!envelope.subagent_id && isNoRunError(envelope.code)) {
        markRunAbandoned(transcript);
      }
      return;
    }

    case 'done': {
      closeOpenTurn(target, envelope.reason, at);
      if (envelope.subagent_id === undefined || envelope.subagent_id === null) {
        transcript.status = envelope.reason === 'error' ? 'error' : 'done';
        transcript.lastCompletion = envelope.reason;
      }
      return;
    }

    case 'pong':
      return;
  }
}

// ---------------------------------------------------------------------------
// Derived views
// ---------------------------------------------------------------------------

/**
 * What a lane opens, or `null` when it cannot be opened.
 *
 * A lane is openable exactly when the server has named the sub-agent's own
 * session. Until that field lands every lane reads as `null`, which is what
 * keeps the rail from offering a control that would fetch nothing — the view
 * says so in words instead of drawing a dead button.
 *
 * The objective comes from the lane's own subtask record. A lane opened by a
 * handoff may hold none, and the honest answer there is to say nothing about the
 * objective rather than to borrow one from a sibling.
 */
export function laneTarget(lane: SubagentLane): SubagentTarget | null {
  const sessionId = lane.subagentSessionId;
  if (sessionId === null) return null;
  const subtask = lane.entries.find(
    (entry): entry is SubtaskEntry => entry.kind === 'subtask',
  );
  const objective = subtask === undefined ? '' : subtask.objective.trim();
  return {
    sessionId,
    subagentId: lane.subagentId,
    objective: objective === '' ? null : objective,
  };
}

/**
 * How one plan step reads right now.
 *
 * The status is the node's own `subtask_end` when one arrived — that is the
 * same frame the lane is drawn from, so a checklist row and the lane it opens
 * can never disagree. A step with no record is `pending` while the run is in
 * flight and `skipped` once it is over: a plan lists work that may never run,
 * and a step the run passed by has to read as skipped rather than stay blank.
 */
export function planNodeStatus(transcript: Transcript, node: PlanNodeEntry): SubtaskStatus {
  const entry = findSubtaskByNode(transcript, node.id);
  if (entry !== null) {
    // A step whose worker never closed stays "running" on its own record. Once
    // the run itself is over nothing is running, so the row reads as skipped
    // rather than claiming a step is still in flight.
    if (entry.status === 'running' && transcript.status !== 'running') return 'skipped';
    return entry.status;
  }
  return transcript.status === 'running' ? 'pending' : 'skipped';
}

/**
 * Where a plan step's own conversation can be opened, or `null`.
 *
 * The target is built from the step's *own* record, never from the lane: the
 * lane key is the agent id, so every worker of the same agent shares one lane,
 * and the lane keeps only the last worker's session. A row that used the lane
 * would open a sibling's conversation — or, worse, a previous run's.
 *
 * A step is openable exactly when its own record names a session. A step that
 * never ran has no record at all, so the row stays a status line rather than
 * offering a control that would fetch nothing.
 */
export function planNodeTarget(transcript: Transcript, nodeId: string): SubagentTarget | null {
  const entry = findSubtaskByNode(transcript, nodeId);
  if (entry === null || entry.subagentSessionId === null) return null;
  const objective = entry.objective.trim();
  return {
    sessionId: entry.subagentSessionId,
    subagentId: entry.agentId ?? '',
    objective: objective === '' ? null : objective,
  };
}

/** How far a plan has got, for the checklist's own readout. */
export interface PlanProgress {
  total: number;
  /** Steps that will not run again: succeeded, failed or skipped. */
  finished: number;
}

export function planProgress(transcript: Transcript): PlanProgress {
  const plan = transcript.plan;
  if (plan === null) return { total: 0, finished: 0 };
  let finished = 0;
  for (const node of plan.nodes) {
    const status = planNodeStatus(transcript, node);
    if (status !== 'pending' && status !== 'running') finished += 1;
  }
  return { total: plan.nodes.length, finished };
}

export function latestUsage(transcript: Transcript): TokenSample | null {
  return transcript.diagnostics.tokens[transcript.diagnostics.tokens.length - 1] ?? null;
}

/** Total tokens reported so far, summed across every sample. */
export function totalTokens(transcript: Transcript): TokenUsage {
  return transcript.diagnostics.tokens.reduce<TokenUsage>(
    (total, sample) => ({
      input_tokens: total.input_tokens + sample.usage.input_tokens,
      output_tokens: total.output_tokens + sample.usage.output_tokens,
    }),
    { input_tokens: 0, output_tokens: 0 },
  );
}

export function toolCalls(transcript: Transcript): ToolCall[] {
  const calls: ToolCall[] = [];
  for (const entry of transcript.entries) {
    if (entry.kind !== 'turn') continue;
    for (const block of entry.blocks) {
      if (block.kind === 'tool') calls.push(block.call);
    }
  }
  return calls;
}

/** Tools whose successful calls change a file on disk. */
const FILE_WRITING_TOOLS = new Set(['write_file', 'edit_file']);

/** One file a turn touched, with the line counts the call itself implies. */
export interface FileChange {
  path: string;
  /** Lines the call added, counted from the arguments it carried. */
  added: number;
  /**
   * Lines the call removed. An `edit_file` knows its own replaced text; a
   * `write_file` replaces the whole file, so what it displaced is not on the
   * wire and its removals stay at zero rather than being guessed at.
   */
  removed: number;
}

/** What a turn changed, summed over every file it wrote or edited. */
export interface ChangeSummary {
  files: FileChange[];
  added: number;
  removed: number;
}

/**
 * Lines in a block of text, with a trailing newline not opening a further line.
 *
 * The wire carries no line counts for a tool result, so the only honest source
 * is the text the call was made with: a `write_file`'s `content` and an
 * `edit_file`'s `old_string` / `new_string`.
 */
function lineCount(text: string): number {
  if (text === '') return 0;
  const body = text.endsWith('\n') ? text.slice(0, -1) : text;
  return body.split('\n').length;
}

/** A string field of a tool call's arguments, or `null` when it is not one. */
function argumentString(args: JsonValue, field: string): string | null {
  if (typeof args !== 'object' || args === null || Array.isArray(args)) return null;
  const value = (args as { [key: string]: JsonValue })[field];
  return typeof value === 'string' ? value : null;
}

/**
 * How many replacements an `edit_file` result reports.
 *
 * The count is in the result text (`path: N replacement(s)`) rather than in the
 * arguments, so it is read back out of it; a result that does not say falls back
 * to the single replacement an unqualified edit performs.
 */
function replacementCount(output: string): number {
  const match = /(\d+)\s+replacement/.exec(output);
  if (!match) return 1;
  const count = Number(match[1]);
  return Number.isFinite(count) && count > 0 ? count : 1;
}

/**
 * What a turn changed on disk, derived from its own tool calls.
 *
 * Only successful calls count: a rejected or failed edit changed nothing, so
 * including it would overstate the work. Edits to the same path are merged, and
 * the summary is `null` for a turn that wrote nothing, which is what the card
 * keys its `v-if` off.
 */
export function turnChanges(turn: TurnEntry): ChangeSummary | null {
  const byPath = new Map<string, FileChange>();

  for (const block of turn.blocks) {
    if (block.kind !== 'tool') continue;
    const { call } = block;
    if (call.status !== 'ok' || !FILE_WRITING_TOOLS.has(call.name)) continue;

    const path = argumentString(call.arguments, 'path');
    if (path === null || path === '') continue;

    let added = 0;
    let removed = 0;

    if (call.name === 'write_file') {
      added = lineCount(argumentString(call.arguments, 'content') ?? '');
    } else {
      const replacements = replacementCount(call.output);
      added = lineCount(argumentString(call.arguments, 'new_string') ?? '') * replacements;
      removed = lineCount(argumentString(call.arguments, 'old_string') ?? '') * replacements;
    }

    const existing = byPath.get(path);
    if (existing) {
      existing.added += added;
      existing.removed += removed;
    } else {
      byPath.set(path, { path, added, removed });
    }
  }

  if (byPath.size === 0) return null;

  const files = [...byPath.values()];
  return {
    files,
    added: files.reduce((total, file) => total + file.added, 0),
    removed: files.reduce((total, file) => total + file.removed, 0),
  };
}

/** The text of a tool output as the card should show it, before any toggle. */
export function previewOf(text: string, limit = TOOL_OUTPUT_PREVIEW_CHARS): string {
  if (text.length <= limit) return text;
  const hidden = text.length - limit;
  return `${text.slice(0, limit)}\n${i18n.global.t('errors.transcript.previewMore', { count: hidden })}`;
}

/**
 * How an error frame should read on screen.
 *
 * `tone` is the whole point: a `notice` is the transport explaining itself and
 * gets muted styling, while an `error` is something the user has to act on — a
 * run that failed, or a send that was refused.
 */
export interface ErrorPresentation {
  /** Short label for the entry's badge. */
  label: string;
  /** The sentence to show the user. */
  message: string;
  tone: 'notice' | 'error';
}

/**
 * Turns an error frame into the line the transcript shows.
 *
 * `backpressure` and `queue_full` are not the run failing: the server closed the
 * connection because this client stopped reading, or refused a send because the
 * session's queue was already at its limit. Both are explanations the user can
 * act on rather than alarms about the agent.
 *
 * The wording is resolved here rather than at the fold, so what the user reads
 * is the sentence for the locale that is active now.
 */
export function describeError(entry: ErrorEntry): ErrorPresentation {
  switch (entry.code) {
    case 'backpressure':
      return {
        label: i18n.global.t('errors.transcript.backpressureLabel'),
        message: i18n.global.t('errors.transcript.backpressure'),
        tone: 'notice',
      };
    case 'queue_full':
      // The queue's cap, not the run failing: the send was refused because the
      // session already had its limit waiting, so the user is told what to do.
      return {
        label: i18n.global.t('errors.transcript.queueFullLabel'),
        message: i18n.global.t('errors.transcript.queueFull', { detail: entry.message }),
        tone: 'error',
      };
    case 'tool_approval_request': {
      const approval = entry.approval;
      if (!approval) return { label: entry.code, message: entry.message, tone: 'error' };
      return {
        label: i18n.global.t('errors.transcript.approvalLabel'),
        message: i18n.global.t('errors.transcript.approvalRequest', {
          name: approval.name,
          toolCallId: approval.toolCallId,
          reason: approval.reason,
        }),
        tone: 'error',
      };
    }
    default:
      return { label: entry.code, message: entry.message, tone: 'error' };
  }
}

/**
 * How a finished run reads.
 *
 * The reason stays a `CompletionReason` everywhere it is stored; this is the one
 * place that turns it into words — for the tag on a closed turn, and for the
 * note left by a run that ended without producing any output at all.
 */
export function completionReasonText(reason: CompletionReason): string {
  return i18n.global.t(`errors.completion.${reason}`);
}
