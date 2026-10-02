/**
 * Per-session transcript and queue state.
 *
 * The store owns everything the chat screen needs: the timelines themselves,
 * which session the connection is driving, the REST-side session list, and — per
 * session — whether a run is in flight and what is waiting behind it.
 *
 * One wire fact shapes the design: a session runs one turn at a time, and a
 * message that arrives while it is running is *queued*, not refused. The server
 * says so with a `message_queued` frame naming the text and its 1-based place,
 * and the head of that session's queue becomes its active turn when the next run
 * starts. The queues are per session and independent, so two sessions can be
 * driven over one connection without their messages interleaving.
 *
 * A second wire fact shapes the failure path: an error's envelope is not
 * trustworthy about which session it concerns. The server stamps a refusal with
 * the session the connection is driving, which need not be the session the
 * refused frame addressed, so the store attributes an error to the send it
 * answers rather than to the envelope alone. That is what keeps one session's
 * failure from clearing another's run or landing in another's transcript.
 *
 * The one thing that outlives the page is the *selection*: the id of the session
 * the pane was showing, mirrored into `localStorage` and read back on the next
 * load so a reload lands in the same conversation. Nothing else is persisted —
 * the transcript comes from the server, and liveness belongs to the connection
 * that just died, so a reload re-reads history rather than pretending a run is
 * still in flight. See `restore` and `sessionPersistence`.
 */

import { defineStore } from 'pinia';
import { computed, ref, watch } from 'vue';

import { getJson, requestFailureText } from '../api/http';
import { useWebSocket } from '../composables/useWebSocket';
import type {
  ClientMessage,
  Effort,
  MemoryStats,
  Priority,
  ServerEnvelope,
  SessionInfo,
} from '../protocol';
import {
  readPersistedSession,
  writePersistedSession,
} from './sessionPersistence';
import {
  appendUserEntry,
  clearQueue,
  createTranscript,
  foldFrame,
  isNoRunError,
  markRunAbandoned,
  markRunStarted,
  removeApproval,
  removeUserEntry,
  type PendingApproval,
  type QueuedMessage,
  type Transcript,
  type WorkflowJob,
} from './transcript';

/**
 * A frame the store sent that the server has not answered yet.
 *
 * Errors used to be filed under whatever session the envelope named, and the
 * envelope is not always right: the server stamps a refusal with the session the
 * *connection* is driving when it has one, which need not be the session the
 * refused frame addressed. Keeping the outgoing frame's target is what lets a
 * refusal be attributed to the send it answers — and what lets the optimistic
 * bubble that send drew be taken back, instead of leaving a phantom run.
 *
 * Only a `submit` is recorded. Steering and aborting have no frame that says
 * they were accepted, so an entry for one could never be cleared and would only
 * misattribute a later error.
 *
 * The list needs no bound of its own: every send is answered by exactly one of
 * `session_started`, `message_queued` or `error`, so it holds only what is
 * genuinely in flight, and a deliberate close drops it with the socket.
 */
interface PendingSend {
  /** The session addressed, or `null` for a send that opened a new one. */
  sessionId: string | null;
  /** The optimistic bubble this send drew, if it drew one. */
  userEntryId: string | null;
}

/** Newest first, so the rail reads like a log. */
function newestFirst(sessions: SessionInfo[]): SessionInfo[] {
  return [...sessions].sort((left, right) => right.created_at.localeCompare(left.created_at));
}

/**
 * What the composer is for: an ordinary question, or a task to be planned and
 * delegated. It decides which client frame the text becomes.
 */
export type ComposerMode = 'ask' | 'plan';

/**
 * How freely a run may use tools, sent as `permission_mode` on each message.
 *
 * These are the wire's three tiers and the only values the server accepts, so
 * the UI must never offer or send another. The middle tier is the composer's
 * starting point: it asks only when a call needs a permission the run does not
 * already have, which is the least surprising default.
 */
export type PermissionMode = 'always_ask' | 'ask_when_needed' | 'full_auto';

/** The tier a message carries when the user has not chosen one. */
export const DEFAULT_PERMISSION_MODE: PermissionMode = 'ask_when_needed';

export const useSessionStore = defineStore('session', () => {
  const transcripts = ref<Record<string, Transcript>>({});
  /** The session this connection most recently drove; set when the server says so. */
  const liveSessionId = ref<string | null>(null);
  /** The session the transcript pane is showing. */
  const viewSessionId = ref<string | null>(null);
  /** A message typed before the server had handed out a session id. */
  const pendingUserText = ref<string | null>(null);
  /** Sends still waiting for an answer, oldest first. See `PendingSend`. */
  const pendingSends: PendingSend[] = [];
  /**
   * The id a reload asked us to reattach to, until the session list can confirm
   * it still exists. See `restore` and `settleRestore`.
   */
  const restoredSessionId = ref<string | null>(null);
  /**
   * Why the restored selection was dropped, in a word, or `null` when there is
   * nothing to explain. The wording lives in the view, like every other code.
   */
  const restoreNotice = ref<'missing' | null>(null);

  const sessions = ref<SessionInfo[]>([]);
  const stats = ref<MemoryStats | null>(null);
  const listLoading = ref(false);
  /** The failure behind `listError`, kept whole so its wording follows the locale. */
  const listFailure = ref<{ cause: unknown } | null>(null);

  /** Composed on read, so a locale switch re-words what is already on screen. */
  const listError = computed<string | null>(() =>
    listFailure.value === null ? null : requestFailureText(listFailure.value.cause),
  );

  const active = computed<Transcript | null>(() => {
    const id = viewSessionId.value;
    return id === null ? null : (transcripts.value[id] ?? null);
  });

  const live = computed<Transcript | null>(() => {
    const id = liveSessionId.value;
    return id === null ? null : (transcripts.value[id] ?? null);
  });

  const isRunning = computed(() => live.value?.status === 'running');

  /**
   * Whether a session has a run in flight, by its own id.
   *
   * Read from the session's transcript rather than from the one the pane shows:
   * with independent sessions, "is anything running" is a per-session question.
   */
  function isSessionRunning(sessionId: string): boolean {
    return transcripts.value[sessionId]?.status === 'running';
  }

  /** How many messages are waiting behind a session's current run. */
  function queuedCount(sessionId: string): number {
    return transcripts.value[sessionId]?.queue.length ?? 0;
  }

  /** The messages waiting behind a session's current run, oldest first. */
  function queuedMessages(sessionId: string): QueuedMessage[] {
    return transcripts.value[sessionId]?.queue ?? [];
  }

  /** The workflow jobs this session has queued, running or finished. */
  function workflowJobs(sessionId: string): WorkflowJob[] {
    return transcripts.value[sessionId]?.workflowQueue ?? [];
  }

  /** How many workflow jobs a session is showing. */
  function workflowJobCount(sessionId: string): number {
    return transcripts.value[sessionId]?.workflowQueue.length ?? 0;
  }

  /** The tool calls a session is waiting on a human decision for. */
  function approvals(sessionId: string): PendingApproval[] {
    return transcripts.value[sessionId]?.approvals ?? [];
  }

  /**
   * The composer may always send.
   *
   * A message to a session with a run in flight is queued, never refused, so
   * there is no state left in which the store holds it back. The computed is
   * kept while the view migrates off the old one-session gate.
   */
  const canSend = computed(() => true);

  function ensureTranscript(sessionId: string): Transcript {
    const existing = transcripts.value[sessionId];
    if (existing) return existing;
    const created = createTranscript(sessionId);
    transcripts.value[sessionId] = created;
    return created;
  }

  /** The session an envelope's routing names, or `null` when it names none. */
  function namedSession(envelope: ServerEnvelope): string | null {
    return envelope.session_id === '' ? null : envelope.session_id;
  }

  /**
   * Records a send the server has not answered yet.
   *
   * Called by `submit` the moment the frame goes out, so a refusal can be traced
   * back to the frame it answers rather than to whatever session the server's
   * envelope happens to name.
   */
  function noteSend(sessionId: string | null, userEntryId: string | null): void {
    pendingSends.push({ sessionId, userEntryId });
  }

  /**
   * Clears the send a frame answers, if it answers one.
   *
   * `session_started` opens a run, `message_queued` accepts a message, and
   * `workflow_queued` accepts a workflow job, so any of the three is the answer
   * to the send that asked for it, and the envelope's session names which send.
   * A frame about another session leaves every send alone: sessions are
   * independent, so one session's traffic must not consume another's
   * outstanding send.
   */
  function acknowledgeSend(envelope: ServerEnvelope): void {
    if (
      envelope.type !== 'session_started' &&
      envelope.type !== 'message_queued' &&
      envelope.type !== 'workflow_queued'
    ) {
      return;
    }
    const index = pendingSends.findIndex(
      (send) =>
        send.sessionId === envelope.session_id ||
        // A send with no target is answered by whichever session it opened.
        (send.sessionId === null && envelope.type === 'session_started'),
    );
    if (index >= 0) pendingSends.splice(index, 1);
  }

  /**
   * Takes the outstanding send an error answers, if there is one.
   *
   * A send the envelope names is taken first: when the server is right, that is
   * the send the error is about. When it names nothing, the error answers
   * whichever send is still waiting — a targeted send whose refusal was stamped
   * blank, or the send that was to open a session. When it names a session none
   * of our sends addressed, a send that addressed another session is the better
   * authority: the server stamps a refusal with the session the *connection* is
   * driving, which is how an error about A used to land in B. A send that
   * addressed no session cannot be what a named error is about, so it is left
   * alone and the envelope stands.
   */
  function claimPendingSend(named: string | null): PendingSend | null {
    const index = pendingSends.findIndex((send) => send.sessionId === named);
    if (index >= 0) return pendingSends.splice(index, 1)[0] ?? null;
    if (named === null) return pendingSends.shift() ?? null;

    const targeted = pendingSends.findIndex((send) => send.sessionId !== null);
    if (targeted >= 0) return pendingSends.splice(targeted, 1)[0] ?? null;
    return null;
  }

  /**
   * Files an error under the session it is about, and takes back the optimistic
   * state that session's send set up.
   *
   * The session is the one the refused send addressed, not the one the envelope
   * names: the two disagree exactly when the server stamps a refusal with the
   * connection's own session. When no send is outstanding the envelope is all
   * there is to go on — an error about a run (`agent_error`) arrives that way.
   *
   * Only a refusal that means no run started takes the bubble back. An error
   * that lands while a run is in flight leaves both the run and the queue alone:
   * a message waiting behind that run is still going to run.
   */
  function ingestError(envelope: ServerEnvelope & { type: 'error' }): void {
    const named = namedSession(envelope);
    const owner = claimPendingSend(named);
    const target = owner === null ? named : owner.sessionId;

    if (target === null) {
      // The refused frame was the one that would have opened a session, and no
      // session came back for it to belong to. The optimistic text goes with it
      // rather than being placed under a session that never existed.
      pendingUserText.value = null;
      return;
    }

    const transcript = ensureTranscript(target);
    if (owner !== null && owner.userEntryId !== null && isNoRunError(envelope.code)) {
      removeUserEntry(transcript, owner.userEntryId);
    }
    foldFrame(transcript, envelope);
  }

  /** Folds one frame. Registered as a bus subscriber by `App.vue`. */
  function ingest(envelope: ServerEnvelope): void {
    if (envelope.type === 'error') {
      ingestError(envelope);
      return;
    }

    const transcript = ensureTranscript(envelope.session_id);

    if (envelope.type === 'session_started') {
      // The user's own text is never echoed by the server, so the optimistic
      // bubble is placed now that the session it belongs to exists — and the run
      // it started is in flight from this moment.
      if (pendingUserText.value !== null) {
        appendUserEntry(transcript, pendingUserText.value);
        pendingUserText.value = null;
        markRunStarted(transcript);
      }
      liveSessionId.value = envelope.session_id;
      if (viewSessionId.value === null) {
        viewSessionId.value = envelope.session_id;
      }
      // A session this page opened is the answer the restore notice was waiting
      // for, and it supersedes an id that has not settled yet.
      restoredSessionId.value = null;
      restoreNotice.value = null;
    }

    acknowledgeSend(envelope);
    foldFrame(transcript, envelope);

    if (envelope.type === 'done') {
      void refreshList();
    }
  }

  /**
   * Sends the composer's text as an ordinary question.
   *
   * `effort` is the thinking level and `permissionMode` is the tool-permission
   * tier for this run. Both ride on `user_message` only: a workflow job is
   * decomposed before any model sees it, so there is no single run for either to
   * apply to, and the server's `QueueWorkflow` carries no such fields. A value
   * left out is left off the frame, which the server reads as its own configured
   * default.
   *
   * The target defaults to the session on screen and then to the live one, so
   * sending into a session opened for reading continues it instead of starting a
   * new one. A target that is already running takes the message into its queue:
   * the server confirms with `message_queued`, which is the only thing that puts
   * it there, so the bubble is not drawn twice.
   */
  function sendMessage(
    rawText: string,
    agentId: string | null = null,
    targetSessionId?: string | null,
    effort?: Effort | null,
    permissionMode?: PermissionMode | null,
  ): boolean {
    const text = rawText.trim();
    if (text === '') return false;

    const message: ClientMessage = { type: 'user_message', text };
    if (agentId !== null) message.agent_id = agentId;
    if (effort != null) message.effort = effort;
    if (permissionMode != null) message.permission_mode = permissionMode;
    const ws = useWebSocket();
    const target = targetSessionId ?? viewSessionId.value ?? liveSessionId.value;

    if (target === null) {
      pendingUserText.value = text;
      noteSend(null, null);
      ws.send(message);
      return true;
    }

    const transcript = ensureTranscript(target);
    let userEntryId: string | null = null;
    if (transcript.status !== 'running' && transcript.queue.length === 0) {
      userEntryId = appendUserEntry(transcript, text).id;
      // No frame announces a second run on an open session, so the run is marked
      // in flight here and cleared by `done` (or by an error that means it never
      // started). A session with something already queued is left alone: the run
      // that drains the queue will promote it, in order.
      markRunStarted(transcript);
    }
    // Recorded before the frame goes out, so an answer that arrives first still
    // finds the send it belongs to.
    noteSend(target, userEntryId);
    ws.send(message, { sessionId: target });
    return true;
  }

  /**
   * Asks for a plan rather than an answer, by queueing a workflow job.
   *
   * No workflow is named, which the server reads as "choose one for this task,
   * or plan freely when nothing fits" — the automatic selection the picker's
   * default stands for. Routing it through the workflow queue rather than the
   * older `plan_task` frame is what makes the run visible while it waits: the
   * job appears in this session's workflow queue, and the plan it produces
   * arrives as ordinary `plan_created` and subtask frames.
   */
  function planTask(
    rawText: string,
    agentId: string | null = null,
    targetSessionId?: string | null,
  ): boolean {
    return runWorkflow(rawText, null, agentId, targetSessionId);
  }

  /**
   * Queues a task on this session's workflow queue.
   *
   * `workflowId` names the recipe to run; `null` asks the server to choose one,
   * which is also how a task planned without a chosen workflow is sent.
   *
   * This is not a send: nothing is drawn optimistically and the session is not
   * marked running here, because the server's `workflow_queued` /
   * `workflow_started` frames are what say the job was accepted and when it
   * starts. The task becomes the user's message at that moment (see
   * `upsertWorkflowJob`), and the run the job produces arrives as ordinary plan
   * and subtask frames on this session.
   *
   * Addressed like a send, so a workflow queued while reading a stored session
   * runs there rather than in whichever session this connection last opened.
   */
  function runWorkflow(
    rawText: string,
    workflowId: string | null,
    agentId: string | null = null,
    targetSessionId?: string | null,
  ): boolean {
    const text = rawText.trim();
    if (text === '') return false;
    const message: ClientMessage = {
      type: 'queue_workflow',
      task: text,
      ...(workflowId === null || workflowId === '' ? {} : { workflow: workflowId }),
      ...(agentId === null ? {} : { agent_id: agentId }),
    };
    const target = targetSessionId ?? viewSessionId.value ?? liveSessionId.value;
    // Recorded so a refusal is attributed to the session this job was aimed at,
    // and cleared by the `workflow_queued` frame that accepts it.
    noteSend(target, null);
    useWebSocket().send(message, target === null ? {} : { sessionId: target });
    return true;
  }

  /**
   * Injects an instruction into a run that is already in flight.
   *
   * Addressed like a send: the session on screen, then the live one, so steering
   * follows the session the user is watching.
   */
  function steer(
    rawText: string,
    priority: Priority = 'normal',
    targetSessionId?: string | null,
  ): boolean {
    const text = rawText.trim();
    if (text === '') return false;
    const target = targetSessionId ?? viewSessionId.value ?? liveSessionId.value;
    useWebSocket().send(
      { type: 'steering_message', text, priority },
      target === null ? {} : { sessionId: target },
    );
    return true;
  }

  /** Stops a session's run, addressed like a send. */
  function abort(
    reason: string | null = 'stopped from the console',
    targetSessionId?: string | null,
  ): void {
    const target = targetSessionId ?? viewSessionId.value ?? liveSessionId.value;
    useWebSocket().send(
      { type: 'abort', reason },
      target === null ? {} : { sessionId: target },
    );
  }

  /**
   * Answers a tool call that is waiting on a human decision.
   *
   * Addressed like a send, so the reply reaches the session whose run is blocked
   * rather than whichever session this connection last drove. The pending
   * request is cleared once the frame is out, because the dialog must stop
   * offering a choice the user has already made.
   */
  function respondToApproval(
    toolCallId: string,
    approved: boolean,
    reason: string | null = null,
    targetSessionId?: string | null,
  ): void {
    const target = targetSessionId ?? viewSessionId.value ?? liveSessionId.value;
    const message: ClientMessage = { type: 'tool_approval', tool_call_id: toolCallId, approved };
    if (reason != null) message.reason = reason;
    useWebSocket().send(message, target === null ? {} : { sessionId: target });
    const transcript = target === null ? null : transcripts.value[target];
    if (transcript) removeApproval(transcript, toolCallId);
  }

  /**
   * Reattaches the pane to the conversation the last page left behind.
   *
   * Only the *selection* comes back. A reload is a new connection: no run
   * survived it, nothing is queued behind it, and the transcript is not stored
   * here — only the id is — so the session is read back through the ordinary
   * replay path (`/api/sessions/{id}/history`) rather than being dressed up as a
   * live one. Restoring `liveSessionId` would be exactly the lie the UI must not
   * tell: it would put the `live` tag on a session this connection has never
   * driven, and hide the stored history behind an empty live transcript.
   *
   * The stored id is not trusted yet — it may name a session this server no
   * longer has — so it is held in `restoredSessionId` until the session list
   * arrives and `settleRestore` can confirm it.
   */
  function restore(): void {
    // A store that already has a selection is not a cold page; re-reading
    // storage here would only clobber what the user is looking at.
    if (viewSessionId.value !== null || liveSessionId.value !== null) return;

    const record = readPersistedSession();
    if (record === null) return;
    const id = record.view ?? record.live;
    if (id === null) return;

    restoredSessionId.value = id;
    restoreNotice.value = null;
    viewSessionId.value = id;
  }

  /**
   * Checks the restored id against the server's own list, once it has arrived.
   *
   * A stored id can outlive the session it names: the store may have been
   * cleared, or the id may have come from a different server behind the same
   * origin. A name that is not in the list is a dead selection, so the pane
   * drops it and falls back to a fresh session, saying so rather than showing a
   * session that cannot be opened.
   *
   * A list that failed to load proves nothing about the id, so the selection is
   * left alone in that case — the pane's own history read is then the only
   * authority, and a 404 there already has words of its own.
   */
  function settleRestore(): void {
    const pending = restoredSessionId.value;
    if (pending === null || listFailure.value !== null) return;
    restoredSessionId.value = null;
    if (sessions.value.some((item) => item.id === pending)) return;

    if (viewSessionId.value === pending) viewSessionId.value = null;
    if (liveSessionId.value === pending) liveSessionId.value = null;
    restoreNotice.value = 'missing';
  }

  /**
   * Points the pane at a session and attaches to its live updates.
   *
   * Subscribing replays nothing (the bus only fans out what happens next), so a
   * historical session shows an empty transcript until something arrives.
   */
  function attach(sessionId: string): void {
    // Picking a session by hand is the answer the notice was waiting for, and it
    // supersedes whatever the reload was still trying to reattach to.
    restoreNotice.value = null;
    restoredSessionId.value = null;
    ensureTranscript(sessionId);
    viewSessionId.value = sessionId;
    if (sessionId !== liveSessionId.value) {
      useWebSocket().send({ type: 'subscribe', session_id: sessionId });
    }
  }

  function viewLive(): void {
    viewSessionId.value = liveSessionId.value;
  }

  /**
   * Starts a new session.
   *
   * The server opens a session when a message arrives without one, so a fresh
   * session is a fresh connection: this drops the socket (which abandons the
   * runs it was driving and clears their queues) and opens another.
   */
  function startNewSession(): void {
    const ws = useWebSocket();
    ws.disconnect();
    liveSessionId.value = null;
    viewSessionId.value = null;
    pendingUserText.value = null;
    // A deliberate fresh start cancels a restore that has not settled yet.
    restoredSessionId.value = null;
    restoreNotice.value = null;
    ws.connect();
  }

  async function refreshList(): Promise<void> {
    listLoading.value = true;
    listFailure.value = null;
    try {
      const [list, memory] = await Promise.all([
        getJson<SessionInfo[]>('/api/sessions'),
        getJson<MemoryStats>('/api/memory/stats'),
      ]);
      const known = Array.isArray(list) ? list : null;
      sessions.value = newestFirst(known ?? []);
      stats.value = memory;
      // The list is what says whether the id a reload restored still names a
      // session, so the decision waits for it rather than guessing. A body that
      // is not a list is not an answer either, so it settles nothing.
      if (known !== null) settleRestore();
    } catch (err) {
      listFailure.value = { cause: err };
    } finally {
      listLoading.value = false;
    }
  }

  /**
   * A connection that goes away takes its runs with it: the server aborts every
   * run it was driving and the messages waiting behind them will never run, so
   * the local mirrors are dropped. Without this, a dropped socket would leave a
   * session looking busy forever, with a queue nothing will ever drain.
   *
   * A deliberate close also drops the outstanding sends: the transport throws
   * its outbound queue away with the socket, so no answer is coming. A reconnect
   * keeps them — the queued frames are flushed on the new socket, and their
   * answers are still what says whether they were accepted.
   */
  watch(
    () => useWebSocket().state.value,
    (state, previous) => {
      if (previous !== 'open' || state === 'open') return;
      for (const transcript of Object.values(transcripts.value)) {
        if (transcript.status === 'running') markRunAbandoned(transcript);
        if (transcript.queue.length > 0) clearQueue(transcript);
        // The run that was waiting on a decision is aborted server-side, so no
        // answer can be delivered any more: the pending requests go with it.
        if (transcript.approvals.length > 0) transcript.approvals = [];
      }
      liveSessionId.value = null;
      if (state === 'closed') pendingSends.length = 0;
    },
  );

  /**
   * Mirrors the two ids into storage as the selection changes.
   *
   * The ids alone, never the transcript: a reload re-reads the conversation from
   * the server, so a stored copy would only be a stale second owner of it. The
   * `view` id is the one a reload needs — a connection that has gone away leaves
   * `live` null while the pane may still be showing a stored session, and that
   * selection is exactly what must survive. When neither id names anything the
   * entry is removed, so a browser with no session stays a blank slate.
   */
  watch([viewSessionId, liveSessionId], ([view, live]) => {
    writePersistedSession(view, live);
  });

  return {
    transcripts,
    liveSessionId,
    viewSessionId,
    sessions,
    stats,
    listLoading,
    listError,
    active,
    live,
    isRunning,
    isSessionRunning,
    queuedCount,
    queuedMessages,
    workflowJobs,
    workflowJobCount,
    approvals,
    canSend,
    ensureTranscript,
    ingest,
    sendMessage,
    planTask,
    runWorkflow,
    steer,
    abort,
    respondToApproval,
    attach,
    viewLive,
    startNewSession,
    restore,
    restoreNotice,
    refreshList,
  };
});
