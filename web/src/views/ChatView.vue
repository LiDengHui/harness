<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';

import { ApiError, getJson, requestFailureText } from '../api/http';
import ConnectionBadge from '../components/ConnectionBadge.vue';
import DiagnosticsStrip from '../components/DiagnosticsStrip.vue';
import HistoryView from '../components/HistoryView.vue';
import TranscriptView from '../components/TranscriptView.vue';
import { buildRail, declaredParent, type RailRoot } from '../components/rail/tree';
import { useNow } from '../composables/useClock';
import {
  DEFAULT_EFFORT,
  EFFORTS,
  type Effort,
  type SessionInfo,
  type SubtaskStatus,
  type WorkflowJobState,
} from '../protocol';
import { useAgentsStore } from '../stores/agents';
import { useSessionStore, type ComposerMode } from '../stores/session';
import {
  laneTarget,
  planNodeStatus,
  planNodeTarget,
  planProgress,
  type PlanNodeEntry,
  type SubagentLane,
  type SubagentTarget,
  type WorkflowJob,
} from '../stores/transcript';
import { useWorkflowsStore } from '../stores/workflows';

const { t, locale } = useI18n();

const sessions = useSessionStore();
const agents = useAgentsStore();
const workflows = useWorkflowsStore();

/** How many of a group's sessions the rail shows before it is expanded. */
const GROUP_PREVIEW = 3;

/**
 * The transports the rail can name, and the wording for each.
 *
 * A session label's prefix is the only place the wire records what opened the
 * session: `serve:` is the web UI, `run:` is the CLI. That is a transport, not
 * a project, which is what the two headings say.
 */
const TRANSPORTS: Record<string, { label: string; hint: string } | undefined> = {
  serve: { label: 'common.rail.group.serve', hint: 'common.rail.groupHint.serve' },
  run: { label: 'common.rail.group.run', hint: 'common.rail.groupHint.run' },
};

/**
 * The key of the group holding sessions whose label names no transport.
 *
 * A sentinel rather than the empty string, so a label that happens to start
 * with a colon cannot land in the same group as an absent one.
 */
const UNKNOWN = '\u0000unknown';

const draft = ref('');
const steering = ref('');
/** The text last injected into the running loop, shown back to the user. */
const steeringNote = ref<string | null>(null);
const chosenAgent = ref('');
/** Which frame the composer sends: an ordinary message, or a task to plan. */
const mode = ref<ComposerMode>('ask');
/**
 * The workflow chosen for the next task; empty means automatic selection.
 *
 * A chosen workflow changes what the send emits: the task is queued against that
 * workflow (`queue_workflow` with the id) rather than sent with none named, so
 * the plan comes from a recipe the reader picked instead of one the server chose
 * or made up on the spot.
 */
const chosenWorkflow = ref('');
/**
 * The picker option that opens the create panel instead of choosing a workflow.
 *
 * A sentinel rather than a real id, so a workflow whose id happens to be the
 * same string can never be mistaken for the control.
 */
const NEW_WORKFLOW = '\u0000new';
const newWorkflowOpen = ref(false);
const workflowDescription = ref('');
const creatingWorkflow = ref(false);
/** The failure behind the create panel's line, kept whole for its wording. */
const workflowCreateFailure = ref<unknown>(null);
/**
 * Whether the plan checklist is showing its rows.
 *
 * It opens by default — a plan that is running is the thing to watch — and the
 * reader can fold it away when they would rather have the height for the
 * transcript.
 */
const planOpen = ref(true);
/**
 * Whether the session rail is out as an overlay. Only the phone layout reads
 * it — above 640px the rail is a permanent column and the toggle is hidden, so
 * this can never leave the rail floating over the chat.
 */
const drawerOpen = ref(false);
/** The rail's filter text; empty means "every session". */
const search = ref('');
/** Whether every group shows all of its main conversations instead of the first few. */
const expanded = ref(false);
/**
 * The main conversations whose sub-agent sessions the reader has opened, by id.
 *
 * Per conversation rather than one flag for the whole rail: a branch belongs to
 * the session it was forked from, and opening one session's branches must not
 * spill another's into the list.
 */
const openRoots = ref<ReadonlySet<string>>(new Set<string>());
/**
 * The sub-agent a lane was opened from, held while its conversation is on screen.
 *
 * The lane is the only place that knows which worker a run was and what it was
 * asked to do, so those two travel here from the click. They are read only while
 * `sessionId` is the session the pane is showing, which is what stops a trace
 * from a lane the reader has left from labelling the next conversation.
 */
const subagentTrace = ref<SubagentTarget | null>(null);

/**
 * One message as `GET /api/sessions/{id}/history` returns it.
 *
 * The body is the server's own `Message` list, so the field names are the
 * wire's — `tool_call_id`, `tool_calls` — rather than this view's.
 */
interface HistoryMessage {
  role: string;
  content?: string | null;
  tool_calls?: unknown[];
  tool_call_id?: string | null;
  name?: string | null;
}

/** The history route's body: the session itself, and the messages it stores. */
interface HistoryResponse {
  session?: SessionInfo;
  messages?: HistoryMessage[];
}

/**
 * What the pane read back from a stored session, and how the read went.
 *
 * `unavailable` is kept apart from `failure` because the two need different
 * words: the server answers 404 both for a history route it has not been taught
 * yet and for a session that no longer exists, and neither is a failure the
 * reader caused — so both get the "no history here" sentence rather than the
 * path-and-status line a real failure earns.
 */
const history = ref<HistoryResponse | null>(null);
const historyLoading = ref(false);
const historyFailure = ref<unknown>(null);
const historyUnavailable = ref(false);
/** Guards against a slower read landing after a newer one. */
let historyToken = 0;

/** The page's shared ticker, so the rail's ages move without a frame arriving. */
const now = useNow();

const active = computed(() => sessions.active);
const showTranscript = computed(() => active.value !== null && active.value.entries.length > 0);

/** The session the composer addresses: whatever the pane is showing. */
const selectedId = computed(() => sessions.viewSessionId);

/**
 * Whether the selected session has a run in flight.
 *
 * The composer follows the selection rather than this connection's live session:
 * a message is addressed to the session on screen, so "running", the stop button
 * and the steering row all describe that session.
 */
const selectedRunning = computed(
  () => selectedId.value !== null && sessions.isSessionRunning(selectedId.value),
);

/** The selected session's waiting messages, in the order they will run. */
const queued = computed(() =>
  selectedId.value === null ? [] : sessions.queuedMessages(selectedId.value),
);

/**
 * The selected session's workflow jobs, oldest first.
 *
 * Kept beside the message queue rather than folded into it: the two wait on the
 * same session but are different things, and a reader has to be able to tell
 * "a message is waiting" from "a workflow is waiting" at a glance.
 */
const workflowJobs = computed(() =>
  selectedId.value === null ? [] : sessions.workflowJobs(selectedId.value),
);

/** The plan of the run on screen, or `null` when it has none. */
const plan = computed(() => active.value?.plan ?? null);

/** One checklist row: a plan step, how it reads now, and where it can be opened. */
interface PlanStep {
  node: PlanNodeEntry;
  status: SubtaskStatus;
  /** The step's own conversation, when the server has given it one. */
  target: SubagentTarget | null;
}

/**
 * The plan of the run on screen, as rows.
 *
 * Read from the transcript's plan and the subtask frames that update it, never
 * from a second copy: the checklist and the lanes are two views of one run, so
 * they cannot disagree. It is empty for a session this connection is not
 * driving, which is what keeps a stored conversation's pane from showing a plan
 * that belongs to a run it is not reading.
 */
const planSteps = computed<PlanStep[]>(() => {
  const transcript = active.value;
  if (transcript === null || transcript.plan === null) return [];
  return transcript.plan.nodes.map((node) => ({
    node,
    status: planNodeStatus(transcript, node),
    target: planNodeTarget(transcript, node.id),
  }));
});

/** How far the plan has got, for the checklist's own readout. */
const planDone = computed(() =>
  active.value === null ? { total: 0, finished: 0 } : planProgress(active.value),
);

/**
 * How one plan step reads, and the words for each status.
 *
 * `Record<SubtaskStatus, …>` is what keeps the table complete: a status the wire
 * gains cannot be rendered as a raw token until it is worded, and one that stops
 * existing cannot be left behind. The keys are spelled out here rather than built
 * from the status, which is what lets the i18n guard see them.
 */
const PLAN_STATUS_KEYS: Record<SubtaskStatus, string> = {
  pending: 'chat.plan.status.pending',
  running: 'chat.plan.status.running',
  succeeded: 'chat.plan.status.succeeded',
  failed: 'chat.plan.status.failed',
  skipped: 'chat.plan.status.skipped',
};

function planStatusLabel(status: SubtaskStatus): string {
  return t(PLAN_STATUS_KEYS[status]);
}

/** How one workflow job reads, worded like the plan table above. */
const JOB_STATE_KEYS: Record<WorkflowJobState, string> = {
  queued: 'common.workflowQueue.queued',
  running: 'common.workflowQueue.running',
  finished: 'common.workflowQueue.finished',
};

/** What a job that ended badly reads as, rather than as a plain "finished". */
const JOB_OUTCOME_KEYS: Record<'failed' | 'skipped', string> = {
  failed: 'common.workflowQueue.failed',
  skipped: 'common.workflowQueue.skipped',
};

function jobStateLabel(job: WorkflowJob): string {
  if (job.state === 'finished') {
    if (job.status === 'failed') return t(JOB_OUTCOME_KEYS.failed);
    if (job.status === 'skipped') return t(JOB_OUTCOME_KEYS.skipped);
  }
  return t(JOB_STATE_KEYS[job.state]);
}

/** The workflows the picker can offer, as the server listed them. */
const workflowOptions = computed(() => workflows.list);

/** The workflow the picker has chosen, or `null` for automatic selection. */
const selectedWorkflow = computed(
  () => workflows.list.find((workflow) => workflow.id === chosenWorkflow.value) ?? null,
);

/**
 * One picker option's text: the name, and what the workflow is for.
 *
 * The `when` rides on the option itself rather than only on a tooltip, because
 * it is the whole basis for choosing: a name alone does not say whether this is
 * the workflow for the task in hand.
 */
function workflowOptionLabel(workflow: { name: string; when: string }): string {
  const when = workflow.when.trim();
  return when === '' ? workflow.name : `${workflow.name} — ${when}`;
}

/** The name a workflow id reads as, or `null` when the job named none. */
function workflowName(id: string | null): string | null {
  if (id === null || id === '') return null;
  return workflows.list.find((workflow) => workflow.id === id)?.name ?? id;
}

/** The create panel's failure, in whichever locale is active. */
const workflowCreateError = computed(() =>
  workflowCreateFailure.value === null ? '' : requestFailureText(workflowCreateFailure.value),
);

/**
 * Handles the picker's change, including the create option.
 *
 * The create sentinel is not a choice, so the selection is left as it was and
 * the panel opens instead — a `<select>` cannot hold a value that is an action,
 * and this is what keeps the action from looking like one of the workflows.
 */
function onWorkflowChange(event: Event): void {
  const value = (event.target as HTMLSelectElement).value;
  if (value === NEW_WORKFLOW) {
    openNewWorkflow();
    return;
  }
  chosenWorkflow.value = value;
}

function openNewWorkflow(): void {
  newWorkflowOpen.value = true;
  workflowDescription.value = '';
  workflowCreateFailure.value = null;
}

function closeNewWorkflow(): void {
  newWorkflowOpen.value = false;
}

/**
 * Asks the server to save a workflow made from the description, then picks it.
 *
 * Selecting it is the point: the reader described the workflow because they
 * want to run their task through it, so a created workflow that was not selected
 * would leave them to find it in the picker again.
 */
async function createWorkflow(): Promise<void> {
  const description = workflowDescription.value.trim();
  if (description === '' || creatingWorkflow.value) return;
  creatingWorkflow.value = true;
  workflowCreateFailure.value = null;
  try {
    const created = await workflows.create(description);
    chosenWorkflow.value = created.id;
    newWorkflowOpen.value = false;
  } catch (err) {
    workflowCreateFailure.value = err;
  } finally {
    creatingWorkflow.value = false;
  }
}

/** Opens the sub-agent conversation a plan step ran in. */
function openPlanNode(step: PlanStep): void {
  if (step.target === null) return;
  openSubagent(step.target);
}

/** Opens the session a workflow job ran in, where its plan is on screen. */
function openJobPlan(job: WorkflowJob): void {
  sessions.attach(job.sessionId);
  drawerOpen.value = false;
}

/**
 * The thinking levels the composer offers, and the words for each.
 *
 * `value` is the wire token the server accepts; `label` and `hint` say what the
 * level does in a person's words, because the token is not what the choice means
 * to the reader. `Record<Effort, …>` is what keeps the table complete: a level
 * the wire gains cannot be offered here until it is worded, and a level that
 * stops existing cannot be left behind.
 */
const EFFORT_WORDING: Record<Effort, { label: string; hint: string }> = {
  low: { label: 'common.effort.low', hint: 'common.effort.hint.low' },
  high: { label: 'common.effort.high', hint: 'common.effort.hint.high' },
  max: { label: 'common.effort.max', hint: 'common.effort.hint.max' },
};

/** The menu's options, shallowest first: the order the choice gets harder. */
const effortOptions = EFFORTS.map((value) => ({ value, ...EFFORT_WORDING[value] }));

/**
 * The key the choice is filed under while no session is on screen.
 *
 * A sentinel rather than the empty session id, so a level picked for a first
 * message is never mistaken for one picked for a session that names itself ''.
 */
const DRAFT_EFFORT_KEY = '';

/**
 * The thinking level chosen per session, for the life of the page.
 *
 * Keyed by session id, with the draft key standing for "no session yet". It is
 * deliberately not in `localStorage`: the level belongs to a conversation, not
 * to the browser, so a reload is allowed to forget it — and a choice made on one
 * conversation must not silently become the default for the next.
 */
const effortBySession = ref<Record<string, Effort>>({});

/** The session the level is chosen for: the one the composer addresses. */
const effortKey = computed(() => selectedId.value ?? DRAFT_EFFORT_KEY);

/** The level the composer will send, at the configured default until touched. */
const effort = computed<Effort>({
  get: () => effortBySession.value[effortKey.value] ?? DEFAULT_EFFORT,
  set: (value) => {
    effortBySession.value[effortKey.value] = value;
  },
});

/** What the chosen level does, for the control's own hover text. */
const effortHint = computed(() => EFFORT_WORDING[effort.value].hint);

/**
 * The level a first message went out at, held until the session it opens exists.
 *
 * A send with no session on screen is answered by a `session_started` naming one
 * that did not exist when the level was read, so the choice has no id to be
 * filed under yet. Holding it here lets that session inherit the level its first
 * message was sent at, instead of the control appearing to reset the moment the
 * session comes into being.
 */
const pendingEffort = ref<Effort | null>(null);

watch(
  () => sessions.liveSessionId,
  (id, previous) => {
    if (id === null || previous !== null || pendingEffort.value === null) return;
    const level = pendingEffort.value;
    pendingEffort.value = null;
    if (effortBySession.value[id] === undefined) effortBySession.value[id] = level;
    // The draft is spent: a session started later gets the configured default
    // rather than inheriting a choice made for a conversation that already ran.
    delete effortBySession.value[DRAFT_EFFORT_KEY];
  },
);

/**
 * Whether this connection is driving a session, so the pane shows its live
 * transcript rather than the stored history read back from the server.
 *
 * Sending into a session is what starts driving it: the message is addressed to
 * the selected session, so the pane continues that session instead of opening a
 * new one, and the history view gives way to the transcript.
 */
function drivenHere(sessionId: string): boolean {
  if (sessionId === sessions.liveSessionId) return true;
  if (sessions.isSessionRunning(sessionId)) return true;
  const transcript = sessions.transcripts[sessionId];
  return transcript !== undefined && transcript.entries.length > 0;
}

/**
 * The session the pane is reading as stored history, or `null` when it is
 * showing a session this connection is driving.
 */
const replayId = computed<string | null>(() => {
  const viewed = sessions.viewSessionId;
  if (viewed === null) return null;
  return drivenHere(viewed) ? null : viewed;
});

const replaying = computed(() => replayId.value !== null);

/**
 * The replayed session as it describes itself.
 *
 * Its own history response is preferred over the rail's list, so the header
 * speaks for the session on screen rather than for a row that may have been
 * filtered out of the rail.
 */
const replaySession = computed<SessionInfo | null>(() => {
  const declared = history.value?.session;
  if (declared !== undefined && typeof declared.id === 'string') return declared;
  const id = replayId.value;
  return id === null ? null : (sessions.sessions.find((item) => item.id === id) ?? null);
});

const replayName = computed(() =>
  replaySession.value === null ? (replayId.value ?? '') : sessionName(replaySession.value),
);

const replayHint = computed(() => (replaySession.value === null ? '' : rowHint(replaySession.value)));

/**
 * Whether the pane is showing a sub-agent's own conversation.
 *
 * True for any stored session the wire says was forked from another: the server
 * stores each delegated run that way, so a session with a parent *is* a
 * sub-agent's conversation. It is decided from the session's own declaration
 * rather than from having arrived here through a lane, because a reload lands
 * on the same session with no lane behind it.
 */
const traceDeclared = computed(() => {
  const item = replaySession.value;
  return item === null ? null : declaredParent(item);
});

/** The main conversation this sub-agent worked for, or `null` when it is one. */
const traceParentId = computed(() => {
  const item = replaySession.value;
  if (item === null) return null;
  return forkParentId(item, sessionsById.value);
});

/** What to call that conversation: its own title, or the id when it is gone. */
const traceParentName = computed(() => {
  const id = traceParentId.value ?? traceDeclared.value;
  if (id === null) return null;
  const parent = sessionsById.value.get(id);
  return parent === undefined ? shortId(id) : sessionName(parent);
});

/** Whether the back control can be offered: there is somewhere to go back to. */
const canGoBack = computed(() => traceParentId.value !== null);

/**
 * The sub-agent's id, for the trace's own line.
 *
 * The click that opened this pane is the first authority, and the parent's live
 * transcript is the second: a lane records the session the server gave its
 * worker, so a pane reopened after a reload can still be labelled from the
 * conversation that delegated the work. Nothing is invented when neither knows.
 */
const traceWorkerId = computed(() => {
  const id = replayId.value;
  if (id === null) return null;
  const trace = subagentTrace.value;
  if (trace !== null && trace.sessionId === id) return trace.subagentId;
  const lane = laneForSession(traceParentId.value, id);
  return lane === null ? null : lane.subagentId;
});

/**
 * What the sub-agent was asked to do.
 *
 * The objective exists only where the delegation was recorded — in the lane, or
 * in the click that opened the pane — so it is absent rather than guessed at
 * when neither holds one.
 */
const traceObjective = computed(() => {
  const id = replayId.value;
  if (id === null) return null;
  const trace = subagentTrace.value;
  if (trace !== null && trace.sessionId === id && trace.objective !== null) {
    return trace.objective;
  }
  const lane = laneForSession(traceParentId.value, id);
  if (lane === null) return null;
  return laneTarget(lane)?.objective ?? null;
});

/** The lane a main conversation holds for one of its sub-agents, if it still does. */
function laneForSession(parentId: string | null, sessionId: string): SubagentLane | null {
  if (parentId === null) return null;
  const transcript = sessions.transcripts[parentId];
  if (transcript === undefined) return null;
  for (const lane of Object.values(transcript.lanes)) {
    if (lane.subagentSessionId === sessionId) return lane;
  }
  return null;
}

/**
 * The trace's standing line: which sub-agent this was, and whose work it was.
 *
 * Composed as one string rather than laid out as three spans, so a part that is
 * unknown drops out with its separator instead of leaving a dangling dot. The
 * last part is never dropped: a conversation whose parent is gone still has to
 * say so, or the reader is left wondering why there is no way back.
 */
const traceLine = computed(() => {
  const parts: string[] = [];
  const worker = traceWorkerId.value;
  if (worker !== null) parts.push(t('chat.trace.worker', { id: worker }));
  if (canGoBack.value) parts.push(t('chat.trace.belongs', { title: traceParentName.value ?? '' }));
  else parts.push(t('chat.trace.parentGone'));
  return parts.join(' · ');
});

const historyMessages = computed<HistoryMessage[]>(() => history.value?.messages ?? []);

const historyFailureText = computed(() =>
  historyFailure.value === null ? '' : requestFailureText(historyFailure.value),
);

/** One rail row: a session, and how far under its main conversation it sits. */
interface RailRow {
  session: SessionInfo;
  /** 0 for a main conversation, 1+ for a sub-agent's session nested under one. */
  depth: number;
  /** How many sessions were forked from this one, at any depth. */
  childCount: number;
  /** Whether this row's sub-agent sessions are on screen. Only roots carry it. */
  open: boolean;
}

/** One rail group: the main conversations one transport created. */
interface SessionGroup {
  /** The transport's label prefix, or `UNKNOWN`. */
  key: string;
  /** The heading the rail shows, or `null` when the group has none. */
  heading: string | null;
  /** What the heading means, for its tooltip. Empty when there is no heading. */
  hint: string;
  /** The rows to draw, each main conversation before the sessions under it. */
  rows: RailRow[];
  /** Sessions this group is holding back behind the preview. */
  hidden: number;
}

/** A group while the rail is still being filed into it. */
interface PendingGroup {
  key: string;
  heading: string | null;
  hint: string;
  roots: RailRoot[];
}

const statusClass = computed(() => {
  switch (active.value?.status) {
    case 'running':
      return 'tag-warn';
    case 'error':
      return 'tag-error';
    case 'done':
      return 'tag-ok';
    default:
      return '';
  }
});

function shortId(id: string): string {
  return id.length > 10 ? `${id.slice(0, 10)}…` : id;
}

/**
 * How old a session is, in the locale's own words.
 *
 * The wording is a message rather than a suffix glued onto a number, because
 * the two locales disagree about where the unit goes. Anything older than a
 * month is shown as a date instead: "37d" stops being easier to read than the
 * date it stands for.
 */
function relativeAge(iso: string, at: number): string {
  const parsed = new Date(iso);
  if (Number.isNaN(parsed.getTime())) return iso;
  const seconds = Math.max(0, Math.round((at - parsed.getTime()) / 1000));
  if (seconds < 60) return t('common.time.justNow');
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return t('common.time.minutes', { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t('common.time.hours', { count: hours });
  const days = Math.floor(hours / 24);
  if (days < 30) return t('common.time.days', { count: days });
  return parsed.toLocaleDateString(locale.value);
}

/**
 * The transport that opened a session: the part of its label before the first
 * colon, or `null` when the label names none.
 *
 * `serve:` is the web UI and `run:` is the CLI; a label with no colon, or one
 * whose prefix this rail does not know, is left unnamed rather than guessed at.
 */
function transportOf(item: SessionInfo): string | null {
  const label = item.label ?? '';
  const colon = label.indexOf(':');
  return colon <= 0 ? null : label.slice(0, colon);
}

/** The agent a session ran under: the part of its label after the prefix. */
function sessionAgent(item: SessionInfo): string | null {
  const label = item.label ?? '';
  const colon = label.indexOf(':');
  if (colon <= 0) return null;
  const name = label.slice(colon + 1).trim();
  return name === '' ? null : name;
}

/**
 * The session's own words: a preview of its first user message, as the server
 * sends it.
 *
 * Read off the wire rather than through `SessionInfo`, which does not declare
 * the field yet: the guarded read is what lets this view render against both a
 * server that sends `title` and one that does not.
 */
function sessionTitle(item: SessionInfo): string | null {
  const { title } = item as { title?: string | null };
  if (typeof title !== 'string') return null;
  const trimmed = title.trim();
  return trimmed === '' ? null : trimmed;
}

/**
 * What a row reads first: the user's own first words, else a plain statement
 * that the session has none yet.
 *
 * The title is `null` exactly until the user has said something, so a session
 * opened a moment ago has nothing of its own to show. Naming the agent there
 * instead would read as a title — and would repeat, one line down, the agent
 * the row already lists — so the row says what is actually true of it and
 * leaves the agent and the id to the line beneath.
 */
function sessionName(item: SessionInfo): string {
  return sessionTitle(item) ?? t('common.rail.untitled');
}

/**
 * A row's hover text: everything a person does not read, kept one gesture away.
 * The id in full, the agent, what the session was forked from, and the two
 * counts that describe the stored session rather than the conversation.
 */
function rowHint(item: SessionInfo): string {
  const parts = [item.id];
  const agent = sessionAgent(item);
  if (agent !== null) parts.push(t('common.rail.agent', { agent }));
  const parent = forkParentName(item);
  if (parent !== null) parts.push(t('common.rail.forkedFrom', { title: parent }));
  parts.push(t('chat.rail.nodes', { count: item.nodes }));
  parts.push(t('chat.rail.tokens', { count: item.tokens }));
  return parts.join(' · ');
}

/** Every stored session by id, so a fork parent can be named even mid-search. */
const sessionsById = computed(() => new Map(sessions.sessions.map((item) => [item.id, item])));

/**
 * The id of the session a stored session was forked from, or `null` when this
 * rail cannot name one.
 *
 * A parent this rail cannot name is no parent at all: a session whose
 * `forked_from_session` points at something the list does not hold — a session
 * since removed, an id from another store — is a main conversation rather than
 * a row dropped from the rail.
 */
function forkParentId(item: SessionInfo, known: Map<string, SessionInfo>): string | null {
  const declared = declaredParent(item);
  if (declared === null || declared === item.id) return null;
  return known.has(declared) ? declared : null;
}

/** The title of the session a stored session was forked from, or `null`. */
function forkParentName(item: SessionInfo): string | null {
  const parentId = forkParentId(item, sessionsById.value);
  if (parentId === null) return null;
  const parent = sessionsById.value.get(parentId);
  return parent === undefined ? null : sessionName(parent);
}

/**
 * The rail's rows: main conversations, each with the sub-agent sessions the
 * reader has opened under it.
 *
 * The rule itself lives in `components/rail/tree.ts`, where it can be tested
 * without a browser: a session the server forked from another is a sub-agent's
 * conversation and is never a row of its own, and a search that matches one
 * pulls its main conversation in as context rather than reporting a hit the
 * reader cannot reach.
 */
const railRoots = computed<RailRoot[]>(() =>
  buildRail({
    sessions: sessions.sessions,
    search: search.value,
    nameOf: sessionName,
    open: openRoots.value,
    reveal: sessions.viewSessionId,
  }),
);

/** Opens or closes one main conversation's sub-agent sessions. */
function toggleRoot(id: string): void {
  const next = new Set(openRoots.value);
  if (next.has(id)) next.delete(id);
  else next.add(id);
  openRoots.value = next;
}

/**
 * The rail's groups, each holding main conversations.
 *
 * A group is a transport, not a project: the two the rail can name appear in
 * the order of their newest session, which is the order the store already
 * sorted into. Sessions whose label names no transport are collected into one
 * last group with no heading at all — a heading invented for them would say
 * less than the rows themselves.
 *
 * A sub-agent's session follows the conversation it was forked from into that
 * conversation's group, which is why the tree is built before the grouping: a
 * branch belongs to the conversation that delegated it, not to a transport of
 * its own.
 *
 * While the rail is collapsed — and nothing is being searched for — each group
 * is cut to its first few main conversations, which is what the show-more
 * control below the list expands. A conversation is never cut away from the
 * sessions under it: the tree is a unit, so showing a parent means keeping its
 * children reachable.
 */
const groups = computed<SessionGroup[]>(() => {
  const previewing = !expanded.value && search.value.trim() === '';
  const pending: PendingGroup[] = [];
  const byTransport = new Map<string, PendingGroup>();
  const unnamed: PendingGroup = { key: UNKNOWN, heading: null, hint: '', roots: [] };

  for (const root of railRoots.value) {
    const transport = transportOf(root.session);
    const named = transport === null ? undefined : TRANSPORTS[transport];
    let group: PendingGroup;
    if (transport === null || named === undefined) {
      group = unnamed;
    } else {
      const existing = byTransport.get(transport);
      if (existing !== undefined) {
        group = existing;
      } else {
        group = {
          key: transport,
          heading: t(named.label),
          hint: t(named.hint),
          roots: [],
        };
        byTransport.set(transport, group);
        pending.push(group);
      }
    }
    group.roots.push(root);
  }
  if (unnamed.roots.length > 0) pending.push(unnamed);

  return pending.map((group) => {
    const shown = previewing ? previewRoots(group.roots) : group.roots;
    const cut = group.roots.filter((root) => !shown.includes(root));
    const rows: RailRow[] = [];
    for (const root of shown) {
      rows.push({
        session: root.session,
        depth: 0,
        childCount: root.childCount,
        open: root.open,
      });
      for (const child of root.children) {
        rows.push({
          session: child.session,
          depth: child.depth,
          childCount: child.childCount,
          open: false,
        });
      }
    }
    // Only what the preview cut away counts as held back. A conversation the
    // reader has left closed is not hidden — its own control is what shows it.
    const hidden = cut.reduce((total, root) => total + 1 + root.childCount, 0);
    return { key: group.key, heading: group.heading, hint: group.hint, rows, hidden };
  });
});

/**
 * The conversations the collapsed rail lists: its first few, plus wherever the
 * reader is.
 *
 * A preview is a convenience, and the reader's own place is not: a conversation
 * the reader is inside — a sub-agent's, opened from a lane or restored by a
 * reload — is kept in the list even when it sorts past the cut, so the rail can
 * never hide the row that says where they are.
 */
function previewRoots(roots: RailRoot[]): RailRoot[] {
  const shown = roots.slice(0, GROUP_PREVIEW);
  const here = roots.slice(GROUP_PREVIEW).find((root) => root.holdsView);
  return here === undefined ? shown : [...shown, here];
}

/** Sessions the collapsed rail is holding back, across every group. */
const hiddenCount = computed(() =>
  groups.value.reduce((total, group) => total + group.hidden, 0),
);

/**
 * Whether the filter let nothing through.
 *
 * A search matches a session by its id, its label or its title, and a match on
 * a sub-agent's session is reached through the conversation it was forked from
 * — so an empty rail is exactly a search that matched no session anywhere.
 */
const noMatches = computed(
  () => search.value.trim() !== '' && railRoots.value.length === 0,
);

/** The toggle only appears when it would do something, and never mid-search. */
const canToggleGroups = computed(() => search.value.trim() === '' && hiddenCount.value > 0);

/**
 * Turns written on the session on screen so far.
 *
 * A bare count, not `N/M`: the iteration ceiling lives in the server's config
 * (`harness.max_iterations`) and no frame or route publishes it, so a
 * denominator here would be invented. `session_started` carries no such field.
 */
const turnCount = computed(
  () => active.value?.entries.filter((entry) => entry.kind === 'turn').length ?? 0,
);

/**
 * The model the session on screen resolved to, as `session_started` reported
 * it. Before a session exists there is nothing truthful to show: the
 * configured default is the provider's, and no route publishes it.
 */
const resolvedModel = computed(() => active.value?.model ?? '');

/** A wire status reads as its translation when one exists, and as it came otherwise. */
function sessionStatusLabel(status: string): string {
  switch (status) {
    case 'idle':
      return t('chat.sessionStatus.idle');
    case 'running':
      return t('chat.sessionStatus.running');
    case 'done':
      return t('chat.sessionStatus.done');
    case 'error':
      return t('chat.sessionStatus.error');
    default:
      return status;
  }
}

/**
 * What the pane says when it has no transcript to show.
 *
 * The wording names *this connection*, never the rail: a stored session is not
 * this connection's session, so an empty pane beside a rail holding dozens of
 * them must not read as "there are no sessions".
 */
const idleReason = computed(() =>
  active.value === null ? t('common.session.noSessionHere') : t('chat.empty.attached'),
);

const steeringNoteText = computed(() =>
  steeringNote.value === null ? '' : t('chat.composer.steeringSent', { text: steeringNote.value }),
);

/**
 * Sends the composer's text, addressed to the session on screen.
 *
 * The target is what makes a message land in the session being read rather than
 * in whichever session this connection last opened: sending into a session
 * opened for reading continues that session instead of starting a new one, and a
 * message sent while it is running goes into its queue.
 */
function send(): void {
  const text = draft.value.trim();
  if (text === '') return;
  const agent = chosenAgent.value === '' ? null : chosenAgent.value;
  const level = effort.value;
  // A chosen workflow decides what the send is: the task is queued against that
  // recipe, and the plan comes from it rather than from the planner.
  if (chosenWorkflow.value !== '') {
    if (!sessions.runWorkflow(text, chosenWorkflow.value, agent, selectedId.value)) return;
    draft.value = '';
    return;
  }
  // Planning is queued as a workflow job with no workflow named, which is the
  // server's automatic selection: it picks a workflow for the task, or plans
  // freely when none fits.
  if (mode.value === 'plan') {
    if (!sessions.planTask(text, agent, selectedId.value)) return;
    draft.value = '';
    return;
  }
  if (!sessions.sendMessage(text, agent, selectedId.value, level)) return;
  // A first message's level has no session to be filed under yet, so it waits
  // for the session the send opens. See `pendingEffort`.
  if (selectedId.value === null) pendingEffort.value = level;
  draft.value = '';
}

function onDraftKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' || event.shiftKey) return;
  event.preventDefault();
  send();
}

function sendSteering(): void {
  const text = steering.value.trim();
  if (text === '') return;
  sessions.steer(text, 'normal', selectedId.value);
  steering.value = '';
  steeringNote.value = text;
}

/** Stops the selected session's run, so the control follows the pane. */
function abortSelected(): void {
  sessions.abort(undefined, selectedId.value);
}

function startNewSession(): void {
  sessions.startNewSession();
  steeringNote.value = null;
  drawerOpen.value = false;
}

/** The rail is an overlay on phone, so choosing from it also dismisses it. */
function pickSession(id: string): void {
  sessions.attach(id);
  drawerOpen.value = false;
}

/**
 * Opens a sub-agent's own conversation, in place of the one it worked for.
 *
 * The pane is pointed at the sub-agent's session, which is what makes this a
 * *view of that session* rather than a second rendering of the parent's lane:
 * the history route, the composer's target and the reload restore all follow
 * `viewSessionId`, so reusing the ordinary selection keeps every one of them
 * working and leaves the reader's place in the parent untouched. Going back is
 * the same move in reverse — see `backToParent`.
 *
 * Nothing else has to be opened by hand: the rail draws the conversation
 * holding the session on screen open, so the row the reader has just arrived in
 * is listed from the moment it arrives. See `reveal` in `rail/tree.ts`.
 */
function openSubagent(target: SubagentTarget): void {
  subagentTrace.value = target;
  sessions.attach(target.sessionId);
  drawerOpen.value = false;
}

/** Returns to the conversation the sub-agent worked for. */
function backToParent(): void {
  const id = traceParentId.value;
  if (id === null) return;
  subagentTrace.value = null;
  sessions.attach(id);
}

/**
 * Reads one stored session's history into the pane.
 *
 * The route is read once per selection rather than followed: a stored session
 * is not being written to by this connection, so there is nothing to stream.
 * A newer selection bumps the token, which is what stops a slow read from
 * landing under the session the reader has already moved on to.
 */
async function loadHistory(id: string): Promise<void> {
  const token = (historyToken += 1);
  historyLoading.value = true;
  historyFailure.value = null;
  historyUnavailable.value = false;
  history.value = null;
  try {
    const body = await getJson<HistoryResponse>(
      `/api/sessions/${encodeURIComponent(id)}/history`,
    );
    if (token !== historyToken) return;
    history.value = body;
  } catch (err) {
    if (token !== historyToken) return;
    // 404 is both "no such route yet" and "no such session": either way there is
    // no history to read, which is not the same as a read that failed.
    if (err instanceof ApiError && err.isMissing) historyUnavailable.value = true;
    else historyFailure.value = err;
  } finally {
    if (token === historyToken) historyLoading.value = false;
  }
}

/** The pane follows the selection: a replay is read, a live session is not. */
watch(
  replayId,
  (id) => {
    if (id === null) {
      history.value = null;
      historyFailure.value = null;
      historyUnavailable.value = false;
      historyLoading.value = false;
      return;
    }
    void loadHistory(id);
  },
  { immediate: true },
);

onMounted(() => {
  // Reattach before the list is read: the stored id points the pane at a
  // session, and the list is what later confirms that session still exists.
  sessions.restore();
  void sessions.refreshList();
  void agents.load();
  void workflows.load();
});
</script>

<template>
  <div class="chat">
    <!-- Phone only: the scrim the drawer slides over. Tapping it dismisses. -->
    <div v-if="drawerOpen" class="rail-backdrop" @click="drawerOpen = false" />

    <aside id="session-rail" class="rail" :class="{ 'rail-open': drawerOpen }">
      <div class="rail-head">
        <h2>{{ t('chat.rail.title') }}</h2>
        <div class="rail-head-actions">
          <button
            class="btn btn-small"
            type="button"
            :title="t('chat.rail.newSessionTitle')"
            @click="startNewSession"
          >
            {{ t('chat.rail.newSession') }}
          </button>
          <button
            class="btn btn-small rail-close"
            type="button"
            :title="t('common.actions.close')"
            :aria-label="t('common.actions.close')"
            @click="drawerOpen = false"
          >
            ✕
          </button>
        </div>
      </div>

      <input
        v-model="search"
        class="input rail-search"
        type="search"
        :placeholder="t('common.rail.search')"
        :aria-label="t('common.rail.search')"
      />

      <p v-if="sessions.listLoading && sessions.sessions.length === 0" class="muted pad">
        {{ t('chat.rail.loading') }}
      </p>
      <p v-else-if="sessions.listError" class="pad error-text">{{ sessions.listError }}</p>
      <p v-else-if="sessions.sessions.length === 0" class="muted pad">{{ t('chat.rail.empty') }}</p>
      <p v-else-if="noMatches" class="muted pad">{{ t('common.rail.noMatch') }}</p>

      <div v-else class="rail-scroll">
        <section v-for="group in groups" :key="group.key" class="session-group">
          <h3 v-if="group.heading" class="group-head muted" :title="group.hint">
            {{ group.heading }}
          </h3>
          <ul class="session-list">
            <li
              v-for="row in group.rows"
              :key="row.session.id"
              class="session-item"
              :class="{
                selected: row.session.id === sessions.viewSessionId,
                forked: row.depth > 0,
              }"
              :style="{ '--depth': row.depth }"
              :title="rowHint(row.session)"
              @click="pickSession(row.session.id)"
            >
              <div class="session-row">
                <span v-if="row.depth > 0" class="fork-mark" aria-hidden="true">↳</span>
                <span class="session-name">{{ sessionName(row.session) }}</span>
                <span class="session-age mono muted">
                  {{ relativeAge(row.session.created_at, now) }}
                </span>
              </div>
              <div class="session-row session-meta mono muted">
                <span v-if="row.session.id === sessions.liveSessionId" class="tag tag-ok">
                  {{ t('chat.rail.live') }}
                </span>
                <span
                  v-if="sessions.isSessionRunning(row.session.id)"
                  class="tag tag-warn"
                  :title="t('common.rail.runningTitle')"
                >
                  {{ t('common.rail.running') }}
                </span>
                <span
                  v-if="sessions.queuedCount(row.session.id) > 0"
                  class="tag tag-queue"
                  :title="t('common.rail.queuedTitle', { count: sessions.queuedCount(row.session.id) })"
                >
                  {{ t('common.rail.queued', { count: sessions.queuedCount(row.session.id) }) }}
                </span>
                <span v-if="row.depth > 0" class="tag tag-lane">{{ t('common.rail.subagentTag') }}</span>
                <span v-if="sessionAgent(row.session)" class="session-agent">
                  {{ sessionAgent(row.session) }}
                </span>
                <span>{{ shortId(row.session.id) }}</span>
              </div>
              <!--
                The main conversation's own count of the work it delegated. The
                sub-agent sessions are not rows of their own — the rail is a list
                of main conversations — so this control is how they stay
                reachable, and the count is how the rail stays honest about what
                the store holds.
              -->
              <button
                v-if="row.depth === 0 && row.childCount > 0"
                class="rail-expander"
                type="button"
                :aria-expanded="row.open"
                :title="row.open ? t('common.rail.subagentCloseTitle') : t('common.rail.subagentOpenTitle')"
                @click.stop="toggleRoot(row.session.id)"
              >
                <span class="rail-caret" aria-hidden="true">{{ row.open ? '▾' : '▸' }}</span>
                <span :title="t('common.rail.subagentCountTitle')">
                  {{ t('common.rail.subagentCount', { count: row.childCount }) }}
                </span>
              </button>
            </li>
          </ul>
        </section>

        <button
          v-if="canToggleGroups"
          class="btn btn-small rail-more"
          type="button"
          :aria-expanded="expanded"
          @click="expanded = !expanded"
        >
          {{ expanded ? t('common.rail.showLess') : t('common.rail.showMore') }}
        </button>
      </div>

      <footer v-if="sessions.stats" class="rail-foot mono muted" :title="t('common.rail.statsHint')">
        <div>
          {{ t('chat.rail.statsSessions', { sessions: sessions.stats.sessions, nodes: sessions.stats.nodes }) }}
        </div>
        <div>
          {{ t('chat.rail.statsTokens', { tokens: sessions.stats.total_tokens, blobs: sessions.stats.blobs }) }}
        </div>
      </footer>
    </aside>

    <main class="chat-main">
      <header class="chat-head">
        <button
          class="btn btn-small rail-toggle"
          type="button"
          aria-controls="session-rail"
          :aria-expanded="drawerOpen"
          :title="t('chat.rail.openTitle')"
          @click="drawerOpen = !drawerOpen"
        >
          {{ t('chat.rail.open') }}
        </button>
        <ConnectionBadge />
        <div class="chat-head-right mono muted">
          <span v-if="sessions.liveSessionId">
            <i18n-t keypath="chat.head.liveSession" scope="global">
              <template #session>
                <strong>{{ shortId(sessions.liveSessionId) }}</strong>
              </template>
            </i18n-t>
          </span>
          <span v-else>{{ t('common.session.notAttached') }}</span>
          <span v-if="active" class="tag" :class="statusClass">
            {{ sessionStatusLabel(active.status) }}
          </span>
        </div>
      </header>

      <!--
        The plan checklist: the steps the run on screen is executing.

        It is a status list, not a second transcript — one row per plan node,
        carrying the node's objective, its agent and how it reads right now. The
        status is the node's own subtask frame, so a row and the lane it opens
        can never disagree, and a step the run passed by ends as skipped rather
        than staying blank. Clicking a row goes to that step's own sub-agent
        conversation, which is where the detail lives; a step with no
        conversation yet stays a plain status line.
      -->
      <section
        v-if="planSteps.length > 0 && !replaying"
        class="plan"
        :aria-label="t('chat.plan.heading')"
      >
        <header class="plan-head">
          <h3 class="plan-title">{{ t('chat.plan.heading') }}</h3>
          <span class="tag plan-count" :title="t('chat.plan.heading')">
            {{ t('chat.plan.progress', { finished: planDone.finished, total: planDone.total }) }}
          </span>
          <span
            v-if="plan && plan.workflowId"
            class="tag tag-lane plan-workflow"
            :title="t('chat.plan.fromWorkflowTitle')"
          >
            {{ t('chat.plan.fromWorkflow', { id: plan.workflowId }) }}
          </span>
          <button
            class="btn btn-small plan-toggle"
            type="button"
            :aria-expanded="planOpen"
            @click="planOpen = !planOpen"
          >
            {{ planOpen ? t('chat.plan.collapse') : t('chat.plan.expand') }}
          </button>
        </header>

        <p v-if="plan && plan.source === 'inferred'" class="plan-note muted tiny">
          {{ t('chat.plan.inferred') }}
        </p>

        <ol v-if="planOpen" class="plan-list">
          <li
            v-for="step in planSteps"
            :key="step.node.id"
            class="plan-step"
            :class="`plan-step-${step.status}`"
          >
            <span class="tag plan-status" :class="`tag-${step.status}`">
              {{ planStatusLabel(step.status) }}
            </span>
            <button
              v-if="step.target"
              class="plan-objective plan-open"
              type="button"
              :title="t('chat.plan.openTitle')"
              @click="openPlanNode(step)"
            >
              {{ step.node.objective }}
            </button>
            <span v-else class="plan-objective">{{ step.node.objective }}</span>
            <span v-if="step.node.agent" class="plan-agent muted">
              {{ t('chat.plan.agent', { agent: step.node.agent }) }}
            </span>
          </li>
        </ol>
      </section>

      <!--
        The replay pane: a stored session's own history, read back from the
        server. The header speaks for the session on screen — its title, its id,
        and the session it was forked from — so nothing here reads as shared with
        the live session. Sending from the composer addresses this session, which
        is what ends the replay: the pane switches to the session's own
        transcript and continues it live.

        A session the wire says was forked from another is a sub-agent's own
        conversation: the server stores each delegated run as a session of its
        own. The pane then says so — which worker it was, what it was asked to
        do, and the conversation the work belonged to — and carries the way back
        to that conversation in its header, so opening a sub-agent is a step the
        reader can always retrace.
      -->
      <section v-if="replaying" class="replay">
        <header class="replay-head">
          <span class="tag replay-badge">{{ t('common.replay.badge') }}</span>
          <span v-if="traceDeclared" class="tag tag-lane">{{ t('chat.trace.badge') }}</span>
          <strong class="replay-name" :title="replayHint">{{ replayName }}</strong>
          <span class="mono muted">{{ shortId(replayId ?? '') }}</span>
          <button
            v-if="canGoBack"
            class="btn btn-small trace-back"
            type="button"
            :title="t('chat.trace.backTitle')"
            @click="backToParent()"
          >
            {{ t('chat.trace.back', { title: traceParentName ?? '' }) }}
          </button>
        </header>

        <!--
          The trace: what this conversation is and whose work it holds. It sits
          above the messages so the reader knows what they are about to read
          before they read it, and the objective is shown only when one was
          actually recorded — a delegation with no recorded objective says
          nothing rather than borrowing a plausible-sounding one.
        -->
        <p v-if="traceDeclared" class="trace-line mono muted">{{ traceLine }}</p>
        <p v-if="traceObjective" class="trace-line trace-objective">
          {{ t('chat.trace.objective', { objective: traceObjective }) }}
        </p>

        <p v-if="historyLoading" class="muted pad">{{ t('common.replay.loading') }}</p>
        <p v-else-if="historyUnavailable" class="muted pad">{{ t('common.replay.unavailable') }}</p>
        <p v-else-if="historyFailureText" class="pad error-text">{{ historyFailureText }}</p>
        <!-- The renderer owns the empty case: it is the one reading the array. -->
        <HistoryView v-else :messages="historyMessages" />
      </section>

      <TranscriptView
        v-else-if="showTranscript && active"
        :transcript="active"
        @open-subagent="openSubagent"
      />
      <div v-else class="empty pad">
        <div class="empty-note">
          <!--
            A reload remembers the session it was in. When the server no longer
            has it, the pane has fallen back to a fresh session, and saying so is
            what keeps the empty pane from reading as "your conversation was
            never here".
          -->
          <p v-if="sessions.restoreNotice === 'missing'" class="restore-note">
            {{ t('common.session.restoreMissing') }}
          </p>
          <p class="muted">{{ idleReason }}</p>
        </div>
      </div>

      <DiagnosticsStrip v-if="active && !replaying" :transcript="active" />

      <!--
        The workflow queue: the jobs this session has queued against a workflow.
        It is deliberately a different strip from the message queue below — its
        own heading, its own badge, its own state words — because "a workflow is
        waiting" and "a message is waiting" are different things, and a reader
        must be able to tell them apart without reading twice. A job that has
        started offers the way to the plan it produced.
      -->
      <section
        v-if="workflowJobs.length > 0"
        class="queue queue-workflow"
        :aria-label="t('common.workflowQueue.heading', { count: workflowJobs.length })"
      >
        <h3 class="queue-head">
          <span class="tag tag-workflow">{{ t('common.workflowQueue.tag') }}</span>
          <span class="muted">{{ t('common.workflowQueue.heading', { count: workflowJobs.length }) }}</span>
        </h3>
        <ol class="queue-list">
          <li v-for="job in workflowJobs" :key="job.id" class="queue-item">
            <span
              class="tag job-state"
              :class="[`job-${job.state}`, job.status === 'failed' ? 'job-failed' : '']"
            >
              {{ jobStateLabel(job) }}
            </span>
            <span class="queue-text">{{ job.task }}</span>
            <span
              v-if="workflowName(job.workflowId)"
              class="job-workflow mono muted"
              :title="job.workflowId ?? ''"
            >
              {{ workflowName(job.workflowId) }}
            </span>
            <button
              v-if="job.state !== 'queued'"
              class="btn btn-small job-open"
              type="button"
              :title="t('common.workflowQueue.openPlanTitle')"
              @click="openJobPlan(job)"
            >
              {{ t('common.workflowQueue.openPlan') }}
            </button>
          </li>
        </ol>
      </section>

      <!--
        The waiting strip: the messages this session has accepted but not
        started. It sits between the transcript and the composer, apart from the
        active turn, and a message leaves it the moment the server starts it —
        the head of the list is always the one that runs next.
      -->
      <section
        v-if="queued.length > 0"
        class="queue"
        :aria-label="t('common.queue.heading', { count: queued.length })"
      >
        <h3 class="queue-head muted">{{ t('common.queue.heading', { count: queued.length }) }}</h3>
        <ol class="queue-list">
          <li v-for="(item, index) in queued" :key="item.id" class="queue-item">
            <span class="queue-pos mono" aria-hidden="true">{{ index + 1 }}</span>
            <span class="queue-text">{{ item.text }}</span>
            <span class="queue-ahead muted">
              {{
                index === 0
                  ? t('common.queue.next')
                  : t('common.queue.ahead', { count: index })
              }}
            </span>
          </li>
        </ol>
      </section>

      <footer class="composer">
        <!-- The toolbar: the run's small labelled controls, above the input. -->
        <div class="composer-toolbar">
          <span class="muted tiny">{{ t('chat.composer.mode') }}</span>
          <button
            class="btn btn-small"
            :class="{ 'btn-primary': mode === 'ask' }"
            type="button"
            :title="t('chat.composer.askTitle')"
            @click="mode = 'ask'"
          >
            {{ t('chat.composer.ask') }}
          </button>
          <button
            class="btn btn-small"
            :class="{ 'btn-primary': mode === 'plan' }"
            type="button"
            :title="t('chat.composer.planTitle')"
            @click="mode = 'plan'"
          >
            {{ t('chat.composer.plan') }}
          </button>
          <span v-if="mode === 'plan'" class="muted tiny plan-hint">
            {{ t('chat.composer.planHint') }}
          </span>
          <!--
            What the chosen workflow is for, in the reader's own words. The
            option in the picker carries the same `when`, but it is only visible
            while the menu is open — this is what keeps the choice readable once
            it has been made. When the list could not be read, the same place
            says so, so the picker is never silently automatic for no reason.
          -->
          <span
            v-if="selectedWorkflow && selectedWorkflow.when"
            class="muted tiny workflow-hint"
          >
            {{ t('chat.composer.workflowWhen', { when: selectedWorkflow.when }) }}
          </span>
          <span
            v-else-if="workflows.error"
            class="muted tiny workflow-hint"
            :title="workflows.error"
          >
            {{ t('chat.composer.workflowUnavailable') }}
          </span>
          <span v-if="turnCount > 0" class="tag composer-progress">
            {{ t('common.composer.turns', { count: turnCount }) }}
          </span>
        </div>

        <!-- The input, with its own bottom row: new session, agent, model, send. -->
        <div class="composer-input">
          <textarea
            id="draft"
            name="draft"
            v-model="draft"
            class="draft"
            rows="3"
            :placeholder="
              mode === 'plan' ? t('chat.composer.placeholderPlan') : t('chat.composer.placeholderAsk')
            "
            @keydown="onDraftKeydown"
          />

          <div class="composer-bar">
            <button
              class="btn btn-small composer-plus"
              type="button"
              :title="t('chat.rail.newSessionTitle')"
              :aria-label="t('chat.rail.newSession')"
              @click="startNewSession"
            >
              +
            </button>

            <select
              id="agent"
              name="agent"
              v-model="chosenAgent"
              class="input mono composer-agent"
            >
              <option value="">{{ t('chat.composer.agentDefault') }}</option>
              <option v-for="agent in agents.list" :key="agent.id" :value="agent.id">
                {{ agent.id }}
              </option>
            </select>

            <!--
              The workflow picker. It sits beside the agent because both choose
              how the next task runs, and it is one control: automatic selection,
              one of the server's workflows, or the option that opens the create
              panel. Each workflow's option carries its `when` — that is what
              tells a reader which one their task needs.
            -->
            <select
              id="workflow"
              name="workflow"
              class="input mono composer-workflow"
              :title="t('common.workflow.pickTitle')"
              :aria-label="t('chat.composer.workflowLabel')"
              :value="chosenWorkflow"
              @change="onWorkflowChange"
            >
              <option value="">{{ t('chat.composer.workflowAuto') }}</option>
              <option v-for="workflow in workflowOptions" :key="workflow.id" :value="workflow.id">
                {{ workflowOptionLabel(workflow) }}
              </option>
              <option :value="NEW_WORKFLOW">{{ t('chat.composer.workflowNew') }}</option>
            </select>

            <span class="composer-gap" />

            <span v-if="resolvedModel" class="composer-model mono muted" :title="t('common.composer.modelHint')">
              {{ resolvedModel }}
            </span>

            <!--
              The level sits beside the model because the two are chosen
              together: it says how hard that model should think about the next
              message, and it rides on that message. The dot joins them into one
              readout — "model · level" — and only appears when there is a model
              to be joined to.
            -->
            <span v-if="resolvedModel" class="composer-sep muted" aria-hidden="true">·</span>
            <select
              id="effort"
              name="effort"
              v-model="effort"
              class="input composer-effort"
              :title="t(effortHint)"
              :aria-label="t('common.effort.label')"
            >
              <option v-for="option in effortOptions" :key="option.value" :value="option.value">
                {{ t(option.label) }}
              </option>
            </select>

            <button
              class="btn btn-primary composer-send"
              type="button"
              :disabled="draft.trim() === ''"
              @click="send()"
            >
              {{ mode === 'plan' ? t('chat.composer.planSend') : t('chat.composer.send') }}
            </button>
            <button
              class="btn btn-danger composer-stop"
              type="button"
              :disabled="!selectedRunning"
              @click="abortSelected()"
            >
              {{ t('chat.composer.stop') }}
            </button>
          </div>
        </div>

        <div class="composer-row steering">
          <input
            id="steering"
            name="steering"
            v-model="steering"
            class="input mono"
            type="text"
            :placeholder="t('chat.composer.steeringPlaceholder')"
            :disabled="!selectedRunning"
            @keydown.enter.prevent="sendSteering()"
          />
          <button
            class="btn"
            type="button"
            :disabled="!selectedRunning || steering.trim() === ''"
            @click="sendSteering()"
          >
            {{ t('chat.composer.steer') }}
          </button>
          <span v-if="!selectedRunning" class="muted tiny">{{ t('chat.composer.steeringIdle') }}</span>
          <span v-else-if="steeringNote" class="muted tiny">{{ steeringNoteText }}</span>
        </div>
      </footer>

      <!--
        The create panel: a description in, a saved workflow out. It is a small
        overlay rather than a permanent row in the composer, which is what keeps
        the composer a place to type a task instead of a control panel. The
        description is the only input — the server writes the workflow from it —
        and a workflow it returns is selected straight away, because describing
        one is how the reader said they want to run through it.
      -->
      <div v-if="newWorkflowOpen" class="workflow-backdrop" @click="closeNewWorkflow" />
      <section
        v-if="newWorkflowOpen"
        class="workflow-modal"
        role="dialog"
        aria-modal="true"
        :aria-label="t('chat.composer.workflowCreateTitle')"
      >
        <h3 class="workflow-modal-title">{{ t('chat.composer.workflowCreateTitle') }}</h3>
        <p class="muted tiny">{{ t('chat.composer.workflowCreateHint') }}</p>
        <textarea
          v-model="workflowDescription"
          class="input workflow-description"
          rows="3"
          :placeholder="t('chat.composer.workflowDescribePlaceholder')"
          :aria-label="t('chat.composer.workflowCreateTitle')"
        />
        <p v-if="workflowCreateError" class="error-text tiny">{{ workflowCreateError }}</p>
        <div class="workflow-modal-actions">
          <button class="btn btn-small" type="button" @click="closeNewWorkflow">
            {{ t('chat.composer.workflowCancel') }}
          </button>
          <button
            class="btn btn-primary btn-small"
            type="button"
            :disabled="workflowDescription.trim() === '' || creatingWorkflow"
            @click="createWorkflow()"
          >
            {{
              creatingWorkflow
                ? t('chat.composer.workflowCreating')
                : t('chat.composer.workflowCreate')
            }}
          </button>
        </div>
      </section>
    </main>
  </div>
</template>

<style scoped>
.chat {
  display: grid;
  grid-template-columns: 16rem 1fr;
  height: 100%;
  min-height: 0;
}

.rail {
  display: flex;
  flex-direction: column;
  border-right: 1px solid var(--border);
  background: var(--panel);
  min-height: 0;
}

.rail-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0.5rem 0.75rem;
  border-bottom: 1px solid var(--border);
}

.rail-head h2 {
  font-size: 0.8rem;
  text-transform: uppercase;
  letter-spacing: 0.05em;
  margin: 0;
  color: var(--muted);
}

.rail-head-actions {
  display: flex;
  align-items: center;
  gap: 0.35rem;
}

/*
 * The drawer and its scrim belong to the phone layout, which is the only one
 * that has them: above 640px the rail is a permanent column, the toggle is
 * hidden, and the backdrop never renders. Keeping them out of the flow here
 * rather than only inside the media query is what makes "not openable on
 * desktop" true even if the state is left set.
 */
.rail-toggle,
.rail-close,
.rail-backdrop {
  display: none;
}

.rail-search {
  margin: 0.5rem 0.75rem;
  flex: none;
  font-size: 0.78rem;
}

/* The list scrolls; the head, the search box and the footer stay put. */
.rail-scroll {
  flex: 1;
  min-height: 0;
  overflow-y: auto;
  padding-bottom: 0.35rem;
}

.session-group {
  padding: 0.2rem 0;
}

.group-head {
  margin: 0;
  padding: 0.35rem 0.75rem 0.1rem;
  font-size: 0.68rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.session-list {
  list-style: none;
  margin: 0;
  padding: 0;
}

.session-item {
  /* A root sits at the rail's own padding; a fork is indented by its depth. */
  padding: 0.35rem 0.75rem;
  padding-left: calc(0.75rem + var(--depth, 0) * 0.7rem);
  border-left: 2px solid transparent;
  cursor: pointer;
  font-size: 0.78rem;
}

/*
 * A forked session is drawn under the session it came from: a connector runs
 * down its parent's column while the row itself is indented, so the nesting
 * reads as a tree rather than as a row that happens to be inset.
 */
.session-item.forked {
  position: relative;
}

.session-item.forked::before {
  content: '';
  position: absolute;
  left: calc(0.75rem + (var(--depth, 1) - 1) * 0.7rem);
  top: 0;
  bottom: 0;
  border-left: 1px solid var(--border);
}

/* The glyph names the direction, so an indented row cannot read as a stray one. */
.fork-mark {
  flex: none;
  color: var(--muted);
}

/*
 * The count of a main conversation's sub-agent sessions, and the control that
 * lists them. It is a button because it does something the row itself does not:
 * clicking the row opens the conversation, clicking this shows the work that
 * was delegated out of it.
 */
.rail-expander {
  display: flex;
  align-items: center;
  gap: 0.3rem;
  margin-top: 0.15rem;
  padding: 0;
  border: none;
  background: none;
  font: inherit;
  font-size: 0.72rem;
  color: var(--lane);
  cursor: pointer;
  text-align: left;
  min-width: 0;
}

.rail-expander:hover {
  text-decoration: underline;
}

.rail-caret {
  flex: none;
}

.session-item:hover {
  background: var(--panel-2);
}

.session-item.selected {
  border-left-color: var(--accent);
  background: var(--panel-2);
}

.session-row {
  display: flex;
  gap: 0.5rem;
  justify-content: space-between;
  align-items: baseline;
  min-width: 0;
}

/*
 * The title takes the room; its age keeps its own column at the end. `min-width:
 * 0` is what lets a long title ellipsise instead of pushing the age off the row.
 */
.session-name {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.session-age {
  flex: none;
  font-size: 0.72rem;
}

/* The second line: the agent and the id, both secondary to the title above. */
.session-meta {
  flex-wrap: wrap;
  gap: 0.4rem;
  margin-top: 0.1rem;
  font-size: 0.72rem;
}

.session-agent {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.rail-more {
  margin: 0.35rem 0.75rem 0;
}

.rail-foot {
  border-top: 1px solid var(--border);
  padding: 0.4rem 0.75rem;
  font-size: 0.72rem;
  flex: none;
}

.chat-main {
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.chat-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 1rem;
  padding: 0.4rem 0.75rem;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
  font-size: 0.78rem;
}

.chat-head-right {
  display: flex;
  align-items: center;
  gap: 0.75rem;
}

.empty {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
}

/*
 * The empty pane's words, stacked: the restore notice, when a reload could not
 * reopen the session it remembered, sits above the standing explanation. Both
 * are centred and allowed to wrap, so the pair cannot widen a phone viewport.
 */
.empty-note {
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
  align-items: center;
  text-align: center;
  min-width: 0;
}

/* A restore that fell through is a warning about the session, not a failure. */
.restore-note {
  margin: 0;
  color: var(--warn);
  overflow-wrap: anywhere;
}

/*
 * The replay: a stored conversation read back into the pane. It is a column
 * like the transcript it replaces — a header that names the session, a standing
 * note that it is read only, and the messages themselves filling what is left.
 */
.replay {
  flex: 1;
  min-height: 0;
  /* Without this a flex item defaults to min-width:auto, so the header's widest
     child (a long fork title) sets the min-content width and pushes the whole
     pane wider than a phone viewport. */
  min-width: 0;
  display: flex;
  flex-direction: column;
  overflow-y: auto;
}

.replay-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
  min-width: 0;
  padding: 0.45rem 0.75rem;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
  font-size: 0.78rem;
  position: sticky;
  top: 0;
  z-index: 1;
}

.replay-badge {
  flex: none;
}

/* The title takes the room; a long one ellipsises rather than pushing the rest
   of the header off the row. */
.replay-name {
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/*
 * The trace: the two lines that say what a sub-agent's conversation is. They
 * sit between the header and the messages, muted like every other standing
 * note, and the objective is marked apart because it is the one line that
 * quotes the delegation rather than describing the view.
 */
.trace-line {
  margin: 0;
  padding: 0.4rem 0.75rem 0;
  font-size: 0.74rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

.trace-objective {
  padding-top: 0.2rem;
  color: var(--fg);
}

/* The way back sits at the far end of the header, past the id. */
.trace-back {
  margin-left: auto;
  flex: none;
}

/*
 * The waiting strip: the messages a session has accepted but not started. It is
 * a sibling of the transcript rather than part of it, which is what keeps a
 * waiting message distinct from the active turn — and it scrolls on its own when
 * a queue is longer than the space it is given.
 */
.queue {
  flex: none;
  max-height: 30vh;
  overflow-y: auto;
  border-top: 1px solid var(--border);
  background: var(--panel-2);
}

.queue-head {
  margin: 0;
  padding: 0.35rem 0.75rem 0.15rem;
  font-size: 0.68rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.queue-list {
  list-style: none;
  margin: 0;
  padding: 0 0 0.3rem;
}

.queue-item {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
  padding: 0.15rem 0.75rem;
  font-size: 0.78rem;
}

/* The place in the queue, so the order reads without counting rows. */
.queue-pos {
  flex: none;
  font-size: 0.72rem;
  color: var(--muted);
}

.queue-text {
  flex: 1 1 auto;
  min-width: 0;
  overflow-wrap: anywhere;
}

.queue-ahead {
  flex: none;
  font-size: 0.72rem;
}

/* The rail's waiting count: the accent marks it apart from the run tags. */
.tag-queue {
  color: var(--accent);
  border-color: var(--accent);
}

/*
 * The plan checklist: the steps of the run on screen. It sits above the
 * transcript as a panel of its own — a status list, not part of the thread —
 * and is bounded so a long plan scrolls inside it rather than pushing the
 * transcript off the screen.
 */
.plan {
  flex: none;
  min-width: 0;
  max-height: 40vh;
  display: flex;
  flex-direction: column;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
}

.plan-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
  min-width: 0;
  padding: 0.4rem 0.75rem 0.3rem;
}

.plan-title {
  margin: 0;
  font-size: 0.68rem;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.06em;
  color: var(--muted);
}

/* The progress readout and the workflow badge keep their size; the toggle is
   pushed to the far end so it never crowds them. */
.plan-count,
.plan-workflow {
  flex: none;
}

.plan-toggle {
  margin-left: auto;
  flex: none;
}

/* The inferred note is a caveat about the list below it, so it sits between the
   head and the rows rather than being folded into either. */
.plan-note {
  margin: 0;
  padding: 0 0.75rem 0.3rem;
  overflow-wrap: anywhere;
}

.plan-list {
  list-style: none;
  margin: 0;
  padding: 0 0 0.35rem;
  overflow-y: auto;
  min-height: 0;
}

.plan-step {
  display: flex;
  align-items: baseline;
  gap: 0.5rem;
  flex-wrap: wrap;
  padding: 0.2rem 0.75rem;
  font-size: 0.78rem;
  min-width: 0;
}

/* The status is a fixed-width tag, so the objectives line up as a column. */
.plan-status {
  flex: none;
  min-width: 4.2rem;
  text-align: center;
}

/*
 * The objective takes the room. As a button it is the way into the step's own
 * conversation; as a span — a step that never ran — it is only text.
 */
.plan-objective {
  flex: 1 1 8rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

.plan-open {
  padding: 0;
  border: none;
  background: none;
  font: inherit;
  color: var(--fg);
  text-align: left;
  cursor: pointer;
}

.plan-open:hover {
  text-decoration: underline;
}

/* A step that is running is the one to watch, so its objective is marked. */
.plan-step-running .plan-objective {
  color: var(--warn);
}

.plan-agent {
  flex: none;
  font-size: 0.72rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

/*
 * The workflow queue. It is the message queue's sibling, not a second copy of
 * it: the accent border and the badge are what make "a workflow is waiting"
 * readable apart from "a message is waiting".
 */
.queue-workflow {
  border-top: 2px solid var(--lane);
}

.tag-workflow {
  color: var(--lane);
  border-color: var(--lane);
}

/* The heading pairs the badge with the count, so the strip names itself. */
.queue-workflow .queue-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.job-state {
  flex: none;
  min-width: 3.6rem;
  text-align: center;
}

/* A running job is the one that matters; a finished one is history. */
.job-running {
  color: var(--warn);
  border-color: var(--warn);
}

.job-finished {
  color: var(--ok);
  border-color: var(--ok);
}

/* A job that failed is not a job that finished well. */
.job-failed {
  color: var(--error);
  border-color: var(--error);
}

.job-workflow {
  flex: none;
  font-size: 0.72rem;
  max-width: 8rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.job-open {
  flex: none;
}

/* The picker is one short choice beside the agent, sized like it rather than
   like a form field, so it cannot push the run buttons off the row. */
.composer-workflow {
  flex: 0 1 auto;
  max-width: 14rem;
  min-width: 0;
}

/* The hint describes the choice made above; it wraps rather than widening. */
.workflow-hint {
  min-width: 0;
  overflow-wrap: anywhere;
}

/*
 * The create panel. A small centred overlay, never wider than the viewport, so
 * it cannot push the page past a phone's width; the backdrop is what makes it a
 * decision to make rather than a form sitting in the way.
 */
.workflow-backdrop {
  position: fixed;
  inset: 0;
  z-index: 40;
  background: rgba(0, 0, 0, 0.55);
}

.workflow-modal {
  position: fixed;
  z-index: 41;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  width: min(26rem, 92vw);
  max-height: 80vh;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.9rem;
  border: 1px solid var(--border);
  border-radius: 6px;
  background: var(--panel);
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
}

.workflow-modal-title {
  margin: 0;
  font-size: 0.85rem;
}

.workflow-description {
  font: inherit;
  font-size: 0.82rem;
  resize: vertical;
  min-height: 3rem;
  width: 100%;
}

.workflow-modal-actions {
  display: flex;
  justify-content: flex-end;
  gap: 0.5rem;
}


.composer {
  border-top: 1px solid var(--border);
  padding: 0.5rem 0.75rem 0.75rem;
  background: var(--panel);
  display: flex;
  flex-direction: column;
  gap: 0.4rem;
}

.composer-row {
  display: flex;
  gap: 0.5rem;
  align-items: flex-start;
}

/* The small labelled controls that sit above the input. */
.composer-toolbar {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  flex-wrap: wrap;
}

/* The readout is the row's last item, so it is pushed to the far end. */
.composer-progress {
  margin-left: auto;
}

/*
 * The input and its bottom row are one box, which is what makes the row read
 * as belonging to the field rather than floating under it. The field itself is
 * borderless inside, and the wrapper takes the focus ring on its behalf.
 */
.composer-input {
  display: flex;
  flex-direction: column;
  gap: 0.3rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--code);
  padding: 0.35rem 0.4rem;
}

.composer-input:focus-within {
  border-color: var(--accent);
}

.draft {
  font: inherit;
  font-size: 0.82rem;
  color: var(--fg);
  background: transparent;
  border: none;
  outline: none;
  resize: vertical;
  width: 100%;
  min-height: 3rem;
  padding: 0.15rem 0.25rem;
}

.draft:disabled {
  opacity: 0.5;
}

.composer-bar {
  display: flex;
  align-items: center;
  gap: 0.35rem;
  flex-wrap: wrap;
}

.composer-plus {
  min-width: 1.9rem;
  font-weight: 600;
}

.composer-agent {
  flex: 0 1 auto;
  max-width: 12rem;
  min-width: 0;
}

/* Pushes the model and the run buttons to the right end of the row. */
.composer-gap {
  flex: 1 1 auto;
}

.composer-model {
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  font-size: 0.72rem;
}

/* The dot that joins the model and its level into one readout. */
.composer-sep {
  flex: none;
  font-size: 0.72rem;
}

/*
 * The level, sized like the readout it sits beside rather than like a form
 * field: it is one short word, and it must not push the run buttons off a
 * phone's bottom row.
 */
.composer-effort {
  flex: 0 1 auto;
  max-width: 8rem;
  min-width: 0;
  font-size: 0.72rem;
  padding: 0.05rem 0.3rem;
}

.steering {
  align-items: center;
}

.steering .input {
  flex: 1;
}

.tiny {
  font-size: 0.72rem;
}

/*
 * Phone: ≤640px. Everything below is inside this one query, so the desktop
 * grid above is left exactly as it was.
 *
 * Two things shape the layout. The rail stops being a column and becomes a
 * slide-over, which is what frees the full width for the chat. The composer
 * stops being a row of controls and becomes a stacked bar that holds the
 * bottom of the column, so the run controls stay put while the transcript
 * scrolls behind them.
 */
@media (max-width: 640px) {
  .chat {
    grid-template-columns: 1fr;
  }

  /* ---- the rail as a drawer ---- */

  .rail {
    position: fixed;
    inset: 0 auto 0 0;
    width: min(18rem, 86vw);
    z-index: 30;
    transform: translateX(-100%);
    transition: transform 0.18s ease;
    padding-bottom: env(safe-area-inset-bottom, 0px);
  }

  .rail-open {
    transform: translateX(0);
    box-shadow: 4px 0 24px rgba(0, 0, 0, 0.55);
  }

  .rail-backdrop {
    display: block;
    position: fixed;
    inset: 0;
    z-index: 29;
    background: rgba(0, 0, 0, 0.55);
  }

  .rail-close {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 44px;
    min-height: 44px;
  }

  /* A session row is a tap target, not a text line. The sides are named one at
     a time so the base rule's depth-based `padding-left` survives here. */
  .session-item {
    padding-top: 0.55rem;
    padding-right: 0.75rem;
    padding-bottom: 0.55rem;
    min-height: 44px;
  }

  /* The expander is a control of its own inside that row, so it gets its own
     hit area rather than borrowing the row's. */
  .rail-expander {
    min-height: 36px;
  }

  /* ---- the header, with the drawer's handle ---- */

  .rail-toggle {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    min-width: 44px;
    min-height: 44px;
  }

  .chat-head {
    flex-wrap: wrap;
    gap: 0.4rem 0.5rem;
    padding: 0.35rem 0.6rem;
    font-size: 0.74rem;
  }

  .chat-head-right {
    flex-wrap: wrap;
    gap: 0.4rem 0.6rem;
    min-width: 0;
  }

  /* ---- the composer as a bottom bar ---- */

  .composer {
    /* Never shrink: on a short viewport the transcript gives up its height
       first, which is what keeps the run controls on screen. */
    flex: none;
    position: sticky;
    bottom: 0;
    z-index: 2;
    gap: 0.3rem;
    padding: 0.4rem 0.6rem;
    /*
     * The bar itself is cleared by the shell, which pads `.app-body` with
     * `--tabbar-height` — and that value already includes the bottom safe-area
     * inset. Counting it here as well would leave 56px of dead space between
     * the steering row and the tab bar, so this is only the composer's own
     * breathing room.
     */
    padding-bottom: 0.4rem;
  }

  .composer-toolbar {
    gap: 0.3rem;
  }

  /* The mode pair is a control, not a run button: 36px is enough to hit. */
  .composer-toolbar .btn {
    min-height: 36px;
  }

  .composer-row {
    flex-wrap: wrap;
    gap: 0.35rem;
  }

  .composer-row > .input,
  .composer-row > select {
    min-width: 0;
  }

  .draft {
    /* `rows="3"` is the desktop height; on a phone the composer is competing
       with the transcript for the viewport, so the box is held to two lines and
       scrolls inside. */
    height: 3rem;
    min-height: 3rem;
    resize: none;
  }

  /*
   * The input's bottom row wraps rather than shrinks: the model name and the
   * run buttons drop to a second line when the agent picker needs the width,
   * which is what keeps every control at its full size on a 360px screen.
   */
  .composer-bar {
    gap: 0.3rem;
  }

  .composer-agent {
    flex: 1 1 6rem;
    max-width: none;
  }

  /* The workflow picker shares the row with the agent rather than being
     squeezed: on a phone it wraps to its own line before it gets too narrow to
     read the workflow names. */
  .composer-workflow {
    flex: 1 1 8rem;
    max-width: none;
  }

  .composer-plus {
    min-width: 44px;
  }

  .composer-bar .btn,
  .steering .btn {
    min-height: 44px;
  }

  .steering {
    align-items: center;
  }

  .steering .input {
    flex: 1 1 60%;
  }

  /* ---- the plan checklist and the workflow queue ---- */

  /* A phone has less height to give, so the checklist keeps a smaller share of
     it; a long plan still scrolls inside the panel rather than moving the
     transcript off the screen. */
  .plan {
    max-height: 34vh;
  }

  /* The objective is a tap target when it opens a conversation, not a text
     line. */
  .plan-open {
    min-height: 36px;
  }

  .plan-status {
    min-width: 3.6rem;
  }

  .job-open {
    min-height: 36px;
  }

  .workflow-modal {
    width: min(26rem, 94vw);
  }
}
</style>
