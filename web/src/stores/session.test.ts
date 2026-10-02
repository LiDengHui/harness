import { createPinia, setActivePinia } from 'pinia';
import { nextTick } from 'vue';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import {
  createWebSocketClient,
  setSharedWebSocketClient,
  type WebSocketLike,
} from '../composables/useWebSocket';
import { parseServerFrame } from '../protocol';
import { useSessionStore } from './session';
import {
  SESSION_STORAGE_KEY,
  setSessionStorage,
  type KeyValueStore,
} from './sessionPersistence';
import { MAX_QUEUED_MESSAGES } from './transcript';

/** The smallest socket the transport will accept. */
class SilentSocket implements WebSocketLike {
  readyState = 0;
  sent: string[] = [];

  onopen: ((event: unknown) => void) | null = null;
  onclose: ((event: unknown) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  send(data: string): void {
    this.sent.push(data);
  }

  close(): void {
    this.readyState = 3;
  }

  accept(): void {
    this.readyState = 1;
    this.onopen?.({});
  }

  /** Feeds the page a frame the server would have sent. */
  deliver(fields: Record<string, unknown>): void {
    this.onmessage?.({
      data: JSON.stringify({
        v: 1,
        session_id: 'sess_1',
        agent_id: 'default',
        ...fields,
      }),
    });
  }

  sentTypes(): string[] {
    return this.sent.map((frame) => String(JSON.parse(frame)['type']));
  }
}

/**
 * `localStorage` without a browser: the same three methods, in a map.
 *
 * `seed` writes the record a previous page would have left behind, so a test can
 * start from "the user was in this session" without driving a whole page.
 */
class MemoryStorage implements KeyValueStore {
  private values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }

  removeItem(key: string): void {
    this.values.delete(key);
  }

  seed(view: string | null, live: string | null): void {
    this.setItem(SESSION_STORAGE_KEY, JSON.stringify({ view, live }));
  }

  raw(value: string): void {
    this.setItem(SESSION_STORAGE_KEY, value);
  }
}

let socket: SilentSocket;
let storage: MemoryStorage;
let disconnect: () => void;

beforeEach(() => {
  setActivePinia(createPinia());
  socket = new SilentSocket();
  storage = new MemoryStorage();
  setSessionStorage(storage);
  const client = createWebSocketClient({
    url: 'ws://test/ws',
    WebSocketImpl: class {
      constructor() {
        return socket;
      }
    } as unknown as new (url: string) => WebSocketLike,
  });
  setSharedWebSocketClient(client);
  client.connect();
  socket.accept();
  disconnect = () => client.disconnect();
});

afterEach(() => {
  disconnect();
  setSharedWebSocketClient(null);
  setSessionStorage(null);
  vi.unstubAllGlobals();
});

/** A `fetch` that answers the session list and the memory stats route. */
function respondSessions(list: unknown[]): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: unknown) => ({
      ok: true,
      status: 200,
      json: async () =>
        String(path).includes('/api/memory/stats')
          ? { sessions: list.length, nodes: 0, blobs: 0, blob_bytes: 0, total_tokens: 0 }
          : list,
    })),
  );
}

/** One stored session, as `GET /api/sessions` would report it. */
function storedSession(id: string): Record<string, unknown> {
  return {
    id,
    label: 'serve:default',
    created_at: '2026-10-01T00:00:00Z',
    nodes: 1,
    tokens: 2,
    head: null,
    forked_from: null,
  };
}

/** The user bubbles a session's transcript holds, in order. */
function userTexts(sessions: SessionStore, sessionId: string): string[] {
  return (sessions.transcripts[sessionId]?.entries ?? [])
    .filter((entry) => entry.kind === 'user')
    .map((entry) => (entry.kind === 'user' ? entry.text : ''));
}

/** How many error entries a session's transcript holds. */
function errorEntries(sessions: SessionStore, sessionId: string): number {
  return (sessions.transcripts[sessionId]?.entries ?? []).filter((entry) => entry.kind === 'error')
    .length;
}

function store() {
  return useSessionStore();
}

type SessionStore = ReturnType<typeof useSessionStore>;

/** Folds a frame the way `App.vue` does when the bus delivers one. */
function deliver(fields: Record<string, unknown>): void {
  const parsed = parseServerFrame(
    JSON.stringify({ v: 1, session_id: 'sess_1', agent_id: 'default', ...fields }),
  );
  if (!parsed.ok) throw new Error(`test frame is invalid: ${parsed.error}`);
  store().ingest(parsed.envelope);
}

describe('session store', () => {
  it('sends the first message without a session and adopts the one it gets back', () => {
    const sessions = store();

    expect(sessions.sendMessage('read the manifest')).toBe(true);
    expect(socket.sentTypes()).toEqual(['user_message']);
    expect(JSON.parse(socket.sent[0] as string)).toEqual({
      v: 1,
      type: 'user_message',
      text: 'read the manifest',
    });
    expect(sessions.liveSessionId).toBeNull();

    deliver({ type: 'session_started', model: 'mock-1' });

    expect(sessions.liveSessionId).toBe('sess_1');
    expect(sessions.viewSessionId).toBe('sess_1');
    // The user's own bubble is placed once the session it belongs to exists.
    expect(sessions.active?.entries[0]).toMatchObject({ kind: 'user', text: 'read the manifest' });
  });

  it('carries the session id and the chosen agent on later messages', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('  review it  ', 'code-reviewer');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'review it',
      agent_id: 'code-reviewer',
    });
  });

  it('carries the chosen thinking level on the message it applies to', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('think this one through', null, undefined, 'max');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'think this one through',
      effort: 'max',
    });
  });

  it('writes the level beside the agent override when both were chosen', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('a quick one', 'code-reviewer', undefined, 'low');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'a quick one',
      agent_id: 'code-reviewer',
      effort: 'low',
    });
  });

  it('leaves the level off a message sent with no opinion, so the server default stands', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('no opinion on the level');

    expect(socket.sent[socket.sent.length - 1]).not.toContain('effort');
  });

  it('carries the chosen permission tier on the message it applies to', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('be careful with this one', null, undefined, undefined, 'always_ask');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'user_message',
      text: 'be careful with this one',
      permission_mode: 'always_ask',
    });
  });

  it('leaves the permission tier off a message sent with no opinion', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.sendMessage('no opinion on permissions');

    expect(socket.sent[socket.sent.length - 1]).not.toContain('permission_mode');
  });

  it('keeps a tool approval request as a pending item the dialog can read', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    deliver({
      type: 'tool_approval_request',
      tool_call_id: 'call_7',
      name: 'shell',
      arguments: { command: 'rm -rf build' },
      reason: 'the command writes outside the workspace',
    });

    expect(sessions.approvals('sess_1')).toEqual([
      {
        toolCallId: 'call_7',
        name: 'shell',
        arguments: { command: 'rm -rf build' },
        reason: 'the command writes outside the workspace',
        at: expect.any(Number),
      },
    ]);
    // The log entry stays too, so the request is not lost from the transcript.
    expect(errorEntries(sessions, 'sess_1')).toBe(1);
  });

  it('answers a pending approval and clears it', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({
      type: 'tool_approval_request',
      tool_call_id: 'call_7',
      name: 'shell',
      arguments: {},
      reason: 'needs a decision',
    });

    sessions.respondToApproval('call_7', true);

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'tool_approval',
      tool_call_id: 'call_7',
      approved: true,
    });
    expect(sessions.approvals('sess_1')).toEqual([]);
  });

  it('sends a denial with its reason', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({
      type: 'tool_approval_request',
      tool_call_id: 'call_8',
      name: 'write_file',
      arguments: {},
      reason: 'needs a decision',
    });

    sessions.respondToApproval('call_8', false, 'not this file');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'tool_approval',
      tool_call_id: 'call_8',
      approved: false,
      reason: 'not this file',
    });
    expect(sessions.approvals('sess_1')).toEqual([]);
  });

  it('refuses an empty message instead of sending one', () => {
    const sessions = store();
    expect(sessions.sendMessage('   ')).toBe(false);
    expect(socket.sent).toEqual([]);
  });

  it('sends steering with its priority and the live session', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    expect(sessions.steer('prefer the short path', 'high')).toBe(true);

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'steering_message',
      text: 'prefer the short path',
      priority: 'high',
    });
  });

  it('sends an abort with a reason', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'working' });

    expect(sessions.isRunning).toBe(true);
    sessions.abort('stopped from the console');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'abort',
      reason: 'stopped from the console',
    });

    deliver({ type: 'done', reason: 'aborted' });
    expect(sessions.isRunning).toBe(false);
  });

  it('keeps a sub-agent frame out of the main transcript', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'main' });
    deliver({ type: 'assistant_chunk', text: 'worker', subagent_id: 'test-writer' });

    const active = sessions.active;
    expect(active).not.toBeNull();
    expect(active?.lanes['test-writer']?.entries).toHaveLength(1);
    expect(active?.entries.filter((entry) => entry.kind === 'turn')).toHaveLength(1);
  });

  it('addresses a send to the session on screen and continues it', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'live work' });
    deliver({ type: 'done', reason: 'end_turn' });

    // Reading a stored session points the pane at it, and it is where a send
    // must land: the reply belongs beside what the user is looking at.
    sessions.attach('sess_stored');
    expect(sessions.viewSessionId).toBe('sess_stored');

    expect(sessions.sendMessage('continue this one')).toBe(true);

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_stored',
      type: 'user_message',
      text: 'continue this one',
    });
    // The send continues the stored session rather than opening a new one...
    expect(sessions.isSessionRunning('sess_stored')).toBe(true);
    expect(sessions.viewSessionId).toBe('sess_stored');
    // ...and the session this connection last drove is left alone.
    expect(sessions.isSessionRunning('sess_1')).toBe(false);
    expect(sessions.canSend).toBe(true);
  });

  it('accepts a send into a running session instead of refusing it', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });
    expect(sessions.canSend).toBe(true);

    expect(sessions.sendMessage('while it runs')).toBe(true);

    // It is not drawn as a run of its own: the server confirms it into the queue,
    // and only that frame puts it there.
    expect(sessions.queuedCount('sess_1')).toBe(0);
    expect(sessions.transcripts['sess_1']?.entries.filter((entry) => entry.kind === 'user')).toHaveLength(0);

    deliver({ type: 'message_queued', text: 'while it runs', position: 1 });

    expect(sessions.queuedCount('sess_1')).toBe(1);
    expect(sessions.queuedMessages('sess_1')[0]?.text).toBe('while it runs');
    expect(sessions.transcripts['sess_1']?.entries.filter((entry) => entry.kind === 'error')).toHaveLength(0);
  });

  it('keeps a separate queue per session', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'working' });

    deliver({ type: 'message_queued', text: 'second', position: 1 });
    deliver({ session_id: 'sess_2', type: 'message_queued', text: 'other', position: 1 });

    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual(['second']);
    expect(sessions.queuedMessages('sess_2').map((message) => message.text)).toEqual(['other']);
    expect(sessions.queuedCount('sess_1')).toBe(1);
    expect(sessions.queuedCount('sess_2')).toBe(1);
  });

  it('promotes a queued message when its own session runs, and not the other', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'first run' });
    deliver({ type: 'message_queued', text: 'a second', position: 1 });
    deliver({ session_id: 'sess_2', type: 'message_queued', text: 'b first', position: 1 });

    // The first run ends: the queue is still waiting, nothing is active yet.
    deliver({ type: 'done', reason: 'end_turn' });
    expect(sessions.queuedCount('sess_1')).toBe(1);

    // The next output on sess_1 is the queued run starting.
    deliver({ type: 'assistant_chunk', text: 'answering the second' });

    expect(sessions.queuedCount('sess_1')).toBe(0);
    expect(sessions.isSessionRunning('sess_1')).toBe(true);
    // sess_2's queue is untouched by the other session's run.
    expect(sessions.queuedCount('sess_2')).toBe(1);
    expect(sessions.isSessionRunning('sess_2')).toBe(false);

    const userTexts = sessions.transcripts['sess_1']?.entries
      .filter((entry) => entry.kind === 'user')
      .map((entry) => (entry.kind === 'user' ? entry.text : ''));
    expect(userTexts).toEqual(['a second']);
  });

  it('preserves the order messages were queued in', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });
    deliver({ type: 'message_queued', text: 'one', position: 1 });
    deliver({ type: 'message_queued', text: 'two', position: 2 });
    deliver({ type: 'message_queued', text: 'three', position: 3 });

    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual([
      'one',
      'two',
      'three',
    ]);

    deliver({ type: 'done', reason: 'end_turn' });
    deliver({ type: 'assistant_chunk', text: 'running one' });

    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual(['two', 'three']);
  });

  it('keeps a session queue bounded, dropping the oldest', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });

    const overflow = MAX_QUEUED_MESSAGES + 3;
    for (let position = 1; position <= overflow; position += 1) {
      deliver({ type: 'message_queued', text: `msg ${position}`, position });
    }

    const queue = sessions.queuedMessages('sess_1');
    expect(queue).toHaveLength(MAX_QUEUED_MESSAGES);
    expect(queue[0]?.text).toBe('msg 4');
    expect(queue[queue.length - 1]?.text).toBe(`msg ${overflow}`);
  });

  it('marks a second run on the same session as running, with no session_started to say so', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'first' });
    deliver({ type: 'done', reason: 'end_turn' });
    expect(sessions.isRunning).toBe(false);

    sessions.sendMessage('again');

    // The server sends session_started only when it opens a session, so a second
    // run has to be inferred from the accepted message.
    expect(sessions.isRunning).toBe(true);

    deliver({ type: 'done', reason: 'end_turn' });
    expect(sessions.isRunning).toBe(false);
  });

  it('takes back the optimistic running state when the frame was refused', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'done', reason: 'end_turn' });

    sessions.sendMessage('too late');
    expect(sessions.isRunning).toBe(true);

    deliver({ type: 'error', code: 'not_running', message: 'no run is in flight' });

    expect(sessions.isRunning).toBe(false);
    expect(sessions.active?.status).toBe('done');
    expect(sessions.active?.entries.some((entry) => entry.kind === 'error')).toBe(true);
  });

  it('stays running when the refusal means another run really is in flight', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    sessions.sendMessage('one');

    deliver({ type: 'error', code: 'busy', message: 'a run is already in flight' });

    expect(sessions.isRunning).toBe(true);
  });

  it('takes back the optimistic run and bubble when the send was refused', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'done', reason: 'end_turn' });

    sessions.sendMessage('into a session the server no longer has');
    expect(sessions.isSessionRunning('sess_1')).toBe(true);
    expect(userTexts(sessions, 'sess_1')).toEqual(['into a session the server no longer has']);

    deliver({
      type: 'error',
      code: 'unknown_session',
      message: 'no session with that id on this server',
    });

    // The run was a guess and nothing is going to answer it, so both halves of
    // the guess go: no phantom "running", no bubble waiting for a reply.
    expect(sessions.isSessionRunning('sess_1')).toBe(false);
    expect(userTexts(sessions, 'sess_1')).toEqual([]);
    // The failure itself stays visible, so the send is not silently swallowed.
    expect(errorEntries(sessions, 'sess_1')).toBe(1);
  });

  it('files an error under the send it answers, not the session the envelope names', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });

    // A second session, addressed explicitly: its send is the one refused.
    sessions.attach('sess_2');
    expect(sessions.sendMessage('into the other session')).toBe(true);
    expect(sessions.isSessionRunning('sess_2')).toBe(true);

    // The server answers with the session the connection is driving, which is
    // not the session the refused frame addressed.
    deliver({
      session_id: 'sess_1',
      type: 'error',
      code: 'unknown_session',
      message: 'no session with that id on this server',
    });

    // The refusal belongs to sess_2 — the session the store actually sent to.
    expect(sessions.isSessionRunning('sess_2')).toBe(false);
    expect(userTexts(sessions, 'sess_2')).toEqual([]);
    expect(errorEntries(sessions, 'sess_2')).toBe(1);

    // And the session the envelope named is left exactly as it was.
    expect(sessions.isSessionRunning('sess_1')).toBe(true);
    expect(errorEntries(sessions, 'sess_1')).toBe(0);
  });

  it('drops the pending text when a send that would open a session is refused', () => {
    const sessions = store();
    expect(sessions.sendMessage('the very first message')).toBe(true);
    expect(sessions.viewSessionId).toBeNull();

    // No session was opened, so the envelope names none.
    deliver({ session_id: '', type: 'error', code: 'session_error', message: 'could not open it' });

    // The text is not left behind to be placed under the next session that opens.
    deliver({ type: 'session_started', model: 'mock-1' });
    expect(sessions.viewSessionId).toBe('sess_1');
    expect(userTexts(sessions, 'sess_1')).toEqual([]);
  });

  it('keeps a queued message when an unrelated error arrives', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });

    sessions.sendMessage('waiting its turn');
    deliver({ type: 'message_queued', text: 'waiting its turn', position: 1 });
    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual([
      'waiting its turn',
    ]);

    // An error about the run itself: the queue behind it is still going to run.
    deliver({ type: 'error', code: 'agent_error', message: 'the provider refused' });

    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual([
      'waiting its turn',
    ]);
    expect(sessions.isSessionRunning('sess_1')).toBe(true);
    expect(errorEntries(sessions, 'sess_1')).toBe(1);
  });

  it('leaves the queue unchanged when a send is refused', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });
    deliver({ type: 'message_queued', text: 'already waiting', position: 1 });

    expect(sessions.sendMessage('one too many')).toBe(true);

    deliver({
      type: 'error',
      code: 'queue_full',
      message: 'this session already has 32 messages waiting',
    });

    // The refused message never became a queue entry, the run it was aimed at is
    // untouched, and the refusal is visible rather than swallowed.
    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual([
      'already waiting',
    ]);
    expect(sessions.isSessionRunning('sess_1')).toBe(true);
    expect(errorEntries(sessions, 'sess_1')).toBe(1);
  });

  it('gives up on a run when the connection driving it goes away', async () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    sessions.sendMessage('long job');
    expect(sessions.isRunning).toBe(true);

    disconnect();
    await nextTick();

    expect(sessions.isRunning).toBe(false);
    expect(sessions.liveSessionId).toBeNull();
  });

  it('starts a new session on a fresh connection', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    sessions.sendMessage('a turn on the live session');

    sessions.startNewSession();

    expect(sessions.liveSessionId).toBeNull();
    expect(sessions.viewSessionId).toBeNull();
    expect(sessions.active).toBeNull();
  });

  it('queues a planning request as a workflow job with automatic selection', () => {
    const sessions = store();

    expect(sessions.planTask('split the refactor')).toBe(true);

    // No workflow is named, which is the server's cue to choose one for the
    // task — or to plan freely when nothing fits.
    expect(JSON.parse(socket.sent[0] as string)).toEqual({
      v: 1,
      type: 'queue_workflow',
      task: 'split the refactor',
    });
    expect(sessions.liveSessionId).toBeNull();
    // Nothing is drawn optimistically: the job's own frames say when it starts.
    expect(sessions.isRunning).toBe(false);
  });

  it('carries the session and the chosen agent on a planning request', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    expect(sessions.planTask('  split the refactor  ', 'backend-architect')).toBe(true);

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'queue_workflow',
      task: 'split the refactor',
      agent_id: 'backend-architect',
    });
  });

  it('refuses an empty planning request instead of sending one', () => {
    const sessions = store();
    expect(sessions.planTask('   ')).toBe(false);
    expect(socket.sent).toEqual([]);
  });

  it('renders a planned run in the sub-agent lanes the frames announce', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    expect(sessions.planTask('ship the endpoint')).toBe(true);
    // The job's own frame is what says the run is under way.
    deliver({ type: 'workflow_started', job_id: 0, task: 'ship the endpoint' });
    expect(sessions.isRunning).toBe(true);

    // The scripted plan: a handoff opens the lane, the worker's own frames land
    // in it, and the main lane keeps only the user's task and the delegation.
    deliver({
      type: 'agent_handoff',
      from: 'default',
      to: 'backend-architect',
      reason: 'the plan splits here',
    });
    deliver({
      type: 'subtask_start',
      subtask_id: 'sub_1',
      objective: 'sketch the API',
      subagent_id: 'backend-architect',
    });
    deliver({
      type: 'assistant_chunk',
      text: 'drafted the contract',
      subagent_id: 'backend-architect',
    });
    deliver({
      type: 'subtask_end',
      subtask_id: 'sub_1',
      status: 'succeeded',
      summary: 'three routes',
      subagent_id: 'backend-architect',
    });
    deliver({ type: 'done', reason: 'end_turn' });

    const active = sessions.active;
    if (!active) throw new Error('no transcript was opened');

    const lane = active.lanes['backend-architect'];
    if (!lane) throw new Error('the handoff announced no lane');
    expect(lane.entries.map((entry) => entry.kind)).toEqual(['subtask', 'turn']);

    const subtask = lane.entries[0];
    if (!subtask || subtask.kind !== 'subtask') throw new Error('no subtask in the lane');
    expect(subtask.objective).toBe('sketch the API');
    expect(subtask.status).toBe('succeeded');
    expect(subtask.summary).toBe('three routes');

    expect(active.entries.map((entry) => entry.kind)).toEqual(['user', 'handoff']);
    expect(sessions.isRunning).toBe(false);
  });
});

describe('session restore', () => {
  it('restores the session the last page was showing, and only its id', () => {
    storage.seed('sess_9', 'sess_9');
    const sessions = store();

    sessions.restore();

    expect(sessions.viewSessionId).toBe('sess_9');
    // A reload is a new connection: nothing is being driven, so no session is
    // "live" and the rail must not put its live tag on one.
    expect(sessions.liveSessionId).toBeNull();
    // The transcript is not stored, so the restored session starts empty and
    // idle — no phantom run, no queue nothing will ever drain.
    expect(sessions.isSessionRunning('sess_9')).toBe(false);
    expect(sessions.queuedCount('sess_9')).toBe(0);
    expect(sessions.transcripts['sess_9']).toBeUndefined();
  });

  it('falls back to the id the connection was driving when no viewed id was stored', () => {
    storage.seed(null, 'sess_5');
    const sessions = store();

    sessions.restore();

    expect(sessions.viewSessionId).toBe('sess_5');
    expect(sessions.liveSessionId).toBeNull();
  });

  it('leaves the pane unattached when storage holds no session', () => {
    const sessions = store();

    sessions.restore();

    expect(sessions.viewSessionId).toBeNull();
    expect(sessions.liveSessionId).toBeNull();
    expect(sessions.restoreNotice).toBeNull();
  });

  it('ignores a stored record it cannot read', () => {
    storage.raw('{ not json');
    const sessions = store();

    sessions.restore();

    expect(sessions.viewSessionId).toBeNull();
  });

  it('does not reattach over a selection this page already has', () => {
    storage.seed('sess_old', 'sess_old');
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.restore();

    // The page opened a session of its own; the stored one must not clobber it.
    expect(sessions.viewSessionId).toBe('sess_1');
    expect(sessions.liveSessionId).toBe('sess_1');
  });

  it('keeps the restored selection once the server list confirms it', async () => {
    storage.seed('sess_kept', 'sess_kept');
    const sessions = store();
    sessions.restore();
    respondSessions([storedSession('sess_kept')]);

    await sessions.refreshList();

    expect(sessions.viewSessionId).toBe('sess_kept');
    expect(sessions.restoreNotice).toBeNull();
  });

  it('drops a restored session the server no longer has, and says so', async () => {
    storage.seed('sess_gone', 'sess_gone');
    const sessions = store();
    sessions.restore();
    respondSessions([storedSession('sess_other')]);

    await sessions.refreshList();

    // The pane falls back to a fresh session rather than showing one that
    // cannot be opened, and the reason is there to be read.
    expect(sessions.viewSessionId).toBeNull();
    expect(sessions.liveSessionId).toBeNull();
    expect(sessions.restoreNotice).toBe('missing');
  });

  it('keeps the restored selection when the list could not be read', async () => {
    storage.seed('sess_maybe', null);
    const sessions = store();
    sessions.restore();
    // A failed list proves nothing about the id, so it is not evidence of a
    // missing session and must not drop the selection.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch');
      }),
    );

    await sessions.refreshList();

    expect(sessions.viewSessionId).toBe('sess_maybe');
    expect(sessions.restoreNotice).toBeNull();
  });

  it('keeps the id but drops the run when the page is reloaded', async () => {
    const before = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'working' });
    deliver({ type: 'message_queued', text: 'waiting its turn', position: 1 });
    expect(before.isSessionRunning('sess_1')).toBe(true);
    expect(before.queuedCount('sess_1')).toBe(1);
    await nextTick();

    // A reload: a new store over the same storage, with a new connection.
    setActivePinia(createPinia());
    const after = store();
    after.restore();

    // The conversation is found again...
    expect(after.viewSessionId).toBe('sess_1');
    // ...but the run and its queue belonged to the old connection and are gone.
    expect(after.isSessionRunning('sess_1')).toBe(false);
    expect(after.queuedCount('sess_1')).toBe(0);
    expect(after.transcripts['sess_1']).toBeUndefined();
  });

  it('stores the selection as it changes and forgets it when a new session starts', async () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    await nextTick();

    expect(JSON.parse(storage.getItem(SESSION_STORAGE_KEY) as string)).toEqual({
      view: 'sess_1',
      live: 'sess_1',
    });

    sessions.startNewSession();
    await nextTick();

    expect(storage.getItem(SESSION_STORAGE_KEY)).toBeNull();
  });

  it('stores ids only, never the transcript', async () => {
    store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'a long answer' });
    await nextTick();

    const raw = storage.getItem(SESSION_STORAGE_KEY) as string;
    expect(JSON.parse(raw)).toEqual({ view: 'sess_1', live: 'sess_1' });
    expect(raw).not.toContain('a long answer');
  });
});

describe('workflows', () => {
  it('queues a task against a chosen workflow instead of asking for a plan', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    expect(sessions.runWorkflow('ship the endpoint', 'ship-it')).toBe(true);

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'queue_workflow',
      task: 'ship the endpoint',
      workflow: 'ship-it',
    });
  });

  it('carries the chosen agent on the job', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    sessions.runWorkflow('ship it', 'ship-it', 'backend-architect');

    expect(JSON.parse(socket.sent[socket.sent.length - 1] as string)).toEqual({
      v: 1,
      session_id: 'sess_1',
      type: 'queue_workflow',
      task: 'ship it',
      workflow: 'ship-it',
      agent_id: 'backend-architect',
    });
  });

  it('sends the job before a session exists, so the send opens one', () => {
    const sessions = store();

    expect(sessions.runWorkflow('ship it', 'ship-it')).toBe(true);

    expect(JSON.parse(socket.sent[0] as string)).toEqual({
      v: 1,
      type: 'queue_workflow',
      task: 'ship it',
      workflow: 'ship-it',
    });
  });

  it('refuses an empty task, and reads an empty workflow id as automatic selection', () => {
    const sessions = store();

    expect(sessions.runWorkflow('   ', 'ship-it')).toBe(false);
    expect(socket.sent).toEqual([]);

    // An empty id is not a workflow named: it is the automatic choice, which
    // the frame carries by leaving the field off.
    expect(sessions.runWorkflow('ship it', '')).toBe(true);
    expect(JSON.parse(socket.sent[0] as string)).toEqual({
      v: 1,
      type: 'queue_workflow',
      task: 'ship it',
    });
  });

  it('shows the jobs a session has queued, running and finished', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    deliver({
      type: 'workflow_queued',
      job_id: 7,
      task: 'ship the endpoint',
      workflow_id: 'ship-it',
      position: 1,
    });
    expect(sessions.workflowJobCount('sess_1')).toBe(1);
    expect(sessions.workflowJobs('sess_1')[0]).toMatchObject({
      id: '7',
      workflowId: 'ship-it',
      task: 'ship the endpoint',
      state: 'queued',
      position: 1,
      sessionId: 'sess_1',
    });

    // The state change updates the one row rather than adding another.
    deliver({ type: 'workflow_started', job_id: 7, task: 'ship the endpoint', workflow_id: 'ship-it' });
    expect(sessions.workflowJobCount('sess_1')).toBe(1);
    expect(sessions.workflowJobs('sess_1')[0]?.state).toBe('running');
    // The task becomes the user's own message at that moment.
    expect(userTexts(sessions, 'sess_1')).toEqual(['ship the endpoint']);

    deliver({
      type: 'workflow_finished',
      job_id: 7,
      status: 'succeeded',
      summary: '1 of 1 node(s) succeeded',
    });
    expect(sessions.workflowJobs('sess_1')[0]?.state).toBe('finished');
    expect(sessions.workflowJobs('sess_1')[0]?.status).toBe('succeeded');
  });

  it('keeps the message queue and the workflow queue apart', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    deliver({ type: 'assistant_chunk', text: 'busy' });

    deliver({ type: 'message_queued', text: 'a message waiting', position: 1 });
    deliver({
      type: 'workflow_queued',
      job_id: 7,
      task: 'a workflow waiting',
      workflow_id: 'ship-it',
      position: 1,
    });

    expect(sessions.queuedMessages('sess_1').map((message) => message.text)).toEqual([
      'a message waiting',
    ]);
    expect(sessions.workflowJobs('sess_1').map((job) => job.task)).toEqual(['a workflow waiting']);
  });

  it('attributes a refused job to the session it addressed', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });
    sessions.attach('sess_2');

    expect(sessions.runWorkflow('ship it', 'no-such-workflow', null, 'sess_2')).toBe(true);
    // The server stamps the refusal with the session the connection drives,
    // which is not the session the job addressed.
    deliver({
      session_id: 'sess_1',
      type: 'error',
      code: 'unknown_workflow',
      message: 'no such workflow',
    });

    expect(errorEntries(sessions, 'sess_2')).toBe(1);
    expect(errorEntries(sessions, 'sess_1')).toBe(0);
  });

  it('carries a run’s plan onto the transcript the checklist reads', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    deliver({
      type: 'plan_created',
      nodes: [
        { id: 'sketch', objective: 'sketch the API', agent: 'backend-architect', depends_on: [] },
        { id: 'review', objective: 'review it', depends_on: ['sketch'] },
      ],
      workflow_id: 'ship-it',
    });

    expect(sessions.active?.plan?.nodes.map((node) => node.id)).toEqual(['sketch', 'review']);
    expect(sessions.active?.plan?.workflowId).toBe('ship-it');
    expect(sessions.active?.plan?.source).toBe('frame');
  });

  it('keeps each session’s workflow queue to itself', () => {
    const sessions = store();
    deliver({ type: 'session_started', model: 'mock-1' });

    deliver({
      type: 'workflow_queued',
      job_id: 7,
      task: 'ship it',
      workflow_id: 'ship-it',
      position: 1,
    });
    deliver({
      session_id: 'sess_2',
      type: 'workflow_queued',
      job_id: 8,
      task: 'other work',
      workflow_id: 'other',
      position: 1,
    });

    expect(sessions.workflowJobs('sess_1').map((job) => job.id)).toEqual(['7']);
    expect(sessions.workflowJobs('sess_2').map((job) => job.id)).toEqual(['8']);
  });
});
