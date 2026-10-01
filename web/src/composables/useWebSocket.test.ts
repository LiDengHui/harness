import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { i18n, messages } from '../i18n';
import {
  createWebSocketClient,
  defaultWebSocketUrl,
  type WebSocketClient,
  type WebSocketLike,
} from './useWebSocket';

/** A socket that does nothing until a test tells it to. */
class FakeSocket implements WebSocketLike {
  static readonly created: FakeSocket[] = [];

  readyState = 0;
  sent: string[] = [];
  closedWith: { code?: number; reason?: string } | null = null;

  onopen: ((event: unknown) => void) | null = null;
  onclose: ((event: unknown) => void) | null = null;
  onerror: ((event: unknown) => void) | null = null;
  onmessage: ((event: { data: unknown }) => void) | null = null;

  constructor(readonly url: string) {
    FakeSocket.created.push(this);
  }

  static last(): FakeSocket {
    const socket = FakeSocket.created[FakeSocket.created.length - 1];
    if (!socket) throw new Error('no socket was created');
    return socket;
  }

  static reset(): void {
    FakeSocket.created.length = 0;
  }

  send(data: string): void {
    if (this.readyState !== 1) throw new Error('the socket is not open');
    this.sent.push(data);
  }

  close(code?: number, reason?: string): void {
    this.closedWith = { code, reason };
    this.readyState = 3;
    this.onclose?.({});
  }

  /** The server accepted the upgrade. */
  accept(): void {
    this.readyState = 1;
    this.onopen?.({});
  }

  /** The connection went away without a close handshake. */
  drop(): void {
    this.readyState = 3;
    this.onclose?.({});
  }

  deliver(frame: Record<string, unknown>): void {
    this.onmessage?.({
      data: JSON.stringify({
        v: 1,
        session_id: 'sess_1',
        agent_id: 'default',
        ...frame,
      }),
    });
  }

  deliverRaw(data: unknown): void {
    this.onmessage?.({ data });
  }

  /** The `type` of every frame that went out, in order. */
  sentTypes(): string[] {
    return this.sent.map((frame) => String(JSON.parse(frame)['type']));
  }
}

const HEARTBEAT = 1_000;
const PONG_TIMEOUT = 200;

function makeClient(overrides: Partial<Parameters<typeof createWebSocketClient>[0]> = {}) {
  return createWebSocketClient({
    url: 'ws://example.test/ws',
    WebSocketImpl: FakeSocket,
    heartbeatIntervalMs: HEARTBEAT,
    pongTimeoutMs: PONG_TIMEOUT,
    backoff: { baseMs: 100, factor: 2, maxMs: 1_000, jitter: 0 },
    random: () => 0.5,
    ...overrides,
  });
}

/** Connects and returns the live client plus the socket it opened. */
function connected(overrides: Partial<Parameters<typeof createWebSocketClient>[0]> = {}) {
  const client = makeClient(overrides);
  client.connect();
  const socket = FakeSocket.last();
  socket.accept();
  return { client, socket };
}

/** The locale the suite found the app in, so a switch cannot leak between tests. */
const INITIAL_LOCALE = i18n.global.locale.value;

beforeEach(() => {
  vi.useFakeTimers();
  FakeSocket.reset();
});

afterEach(() => {
  vi.useRealTimers();
  i18n.global.locale.value = INITIAL_LOCALE;
});

describe('useWebSocket', () => {
  it('does not open a socket until it is connected', () => {
    const client = makeClient();
    expect(FakeSocket.created).toHaveLength(0);

    client.connect();
    expect(FakeSocket.created).toHaveLength(1);
    expect(client.state.value).toBe('connecting');
    expect(FakeSocket.last().url).toBe('ws://example.test/ws');

    FakeSocket.last().accept();
    expect(client.state.value).toBe('open');
    expect(client.isOpen.value).toBe(true);
  });

  it('derives the URL from the page, so https pages get wss', () => {
    const g = globalThis as { location?: unknown };
    const original = g.location;
    try {
      g.location = { protocol: 'https:', host: 'harness.local:8787' };
      expect(defaultWebSocketUrl()).toBe('wss://harness.local:8787/ws');

      g.location = { protocol: 'http:', host: '127.0.0.1:5173' };
      expect(defaultWebSocketUrl()).toBe('ws://127.0.0.1:5173/ws');
    } finally {
      g.location = original;
    }
  });

  it('queues outbound frames and flushes them in order on open', () => {
    const client = makeClient();
    client.connect();
    const socket = FakeSocket.last();

    client.send({ type: 'user_message', text: 'first' });
    client.send({ type: 'steering_message', text: 'second', priority: 'high' });
    expect(client.queued.value).toBe(2);
    expect(socket.sent).toHaveLength(0);

    socket.accept();

    expect(client.queued.value).toBe(0);
    expect(socket.sentTypes()).toEqual(['user_message', 'steering_message']);
    expect(JSON.parse(socket.sent[1] as string)).toMatchObject({
      v: 1,
      type: 'steering_message',
      text: 'second',
      priority: 'high',
    });
  });

  it('sends straight to the socket while it is open', () => {
    const { client, socket } = connected();

    client.send({ type: 'abort', reason: 'stop' });

    expect(client.queued.value).toBe(0);
    expect(socket.sentTypes()).toEqual(['abort']);
  });

  it('keeps the queue across an unexpected disconnect', () => {
    const { client, socket } = connected();
    socket.drop();

    client.send({ type: 'ping' });
    expect(client.queued.value).toBe(1);

    vi.advanceTimersByTime(100);
    const reconnected = FakeSocket.last();
    expect(reconnected).not.toBe(socket);
    reconnected.accept();

    expect(reconnected.sentTypes()).toEqual(['ping']);
  });

  it('reconnects with exponential backoff and resets it once the socket is back', () => {
    const client = makeClient();
    client.connect();
    FakeSocket.last().accept();

    FakeSocket.last().drop();
    expect(client.state.value).toBe('reconnecting');
    expect(client.attempts.value).toBe(1);

    // First retry is due after the base delay.
    vi.advanceTimersByTime(99);
    expect(FakeSocket.created).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.created).toHaveLength(2);

    // Second failure doubles it.
    FakeSocket.last().drop();
    expect(client.attempts.value).toBe(2);
    vi.advanceTimersByTime(199);
    expect(FakeSocket.created).toHaveLength(2);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.created).toHaveLength(3);

    // A successful open clears the streak, so the next failure starts over.
    FakeSocket.last().accept();
    expect(client.attempts.value).toBe(0);
    FakeSocket.last().drop();
    vi.advanceTimersByTime(100);
    expect(FakeSocket.created).toHaveLength(4);
  });

  it('caps the backoff delay', () => {
    const client = makeClient({ backoff: { baseMs: 100, factor: 10, maxMs: 400, jitter: 0 } });
    client.connect();

    for (let attempt = 0; attempt < 4; attempt += 1) {
      FakeSocket.last().drop();
      vi.advanceTimersByTime(3_000);
    }
    const before = FakeSocket.created.length;
    FakeSocket.last().drop();
    vi.advanceTimersByTime(400);
    expect(FakeSocket.created.length).toBe(before + 1);
  });

  it('spreads retries with jitter instead of retrying in lockstep', () => {
    const jittered = { baseMs: 100, factor: 2, maxMs: 1_000, jitter: 0.25 };
    const late = makeClient({ backoff: jittered, random: () => 1 });
    late.connect();
    FakeSocket.last().drop();
    vi.advanceTimersByTime(124);
    expect(FakeSocket.created).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.created).toHaveLength(2);

    FakeSocket.reset();

    const early = makeClient({ backoff: jittered, random: () => 0 });
    early.connect();
    FakeSocket.last().drop();
    vi.advanceTimersByTime(74);
    expect(FakeSocket.created).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeSocket.created).toHaveLength(2);
  });

  it('pings on an interval and reconnects when no pong comes back', () => {
    const { client, socket } = connected();

    vi.advanceTimersByTime(HEARTBEAT);
    expect(socket.sentTypes()).toEqual(['ping']);

    // The peer never answered: the socket is treated as dead.
    vi.advanceTimersByTime(PONG_TIMEOUT);
    expect(socket.closedWith).not.toBeNull();
    expect(client.state.value).toBe('reconnecting');
    // Asserted by code: the sentence belongs to the locale, not to this test.
    expect(client.lastErrorDetails.value).toEqual({ code: 'serverSilent' });

    vi.advanceTimersByTime(100);
    expect(FakeSocket.created).toHaveLength(2);
  });

  it('treats a pong as proof of life', () => {
    const { client, socket } = connected();

    vi.advanceTimersByTime(HEARTBEAT);
    socket.deliver({ type: 'pong' });

    vi.advanceTimersByTime(PONG_TIMEOUT + 1);
    expect(client.state.value).toBe('open');
    expect(FakeSocket.created).toHaveLength(1);
    expect(socket.closedWith).toBeNull();
  });

  it('stops reconnecting after an explicit disconnect', () => {
    const client = makeClient();
    client.connect();
    FakeSocket.last().accept();
    client.send({ type: 'ping' });

    client.disconnect();
    expect(client.state.value).toBe('closed');
    expect(client.queued.value).toBe(0);

    vi.advanceTimersByTime(60_000);
    expect(FakeSocket.created).toHaveLength(1);
  });

  it('surfaces a frame it cannot parse without dropping the connection', () => {
    const { client, socket } = connected();
    const seen: string[] = [];
    client.onProtocolError((error) => seen.push(error));

    socket.onmessage?.({ data: '{"v":1,"type":"nonsense"}' });
    expect(client.protocolErrors.value).toHaveLength(1);
    expect(seen).toHaveLength(1);
    // The parser owns that wording; the transport only carries it through, so
    // the code and the parser's detail are what this test pins.
    expect(client.lastErrorDetails.value?.code).toBe('protocol');
    expect(client.lastErrorDetails.value?.params?.['detail']).toContain('unknown message type');
    expect(client.state.value).toBe('open');

    // The connection is still usable afterwards.
    const texts: string[] = [];
    client.onFrame((envelope) => {
      if (envelope.type === 'assistant_chunk') texts.push(envelope.text);
    });
    socket.deliver({ type: 'assistant_chunk', text: 'still here' });
    expect(texts).toEqual(['still here']);
  });

  it('refuses a binary frame instead of guessing at it', () => {
    const { client, socket } = connected();

    socket.deliverRaw(new Uint8Array([1, 2, 3]));

    expect(client.lastErrorDetails.value).toEqual({
      code: 'nonTextFrame',
      params: { kind: 'binary' },
    });
    expect(client.protocolErrors.value[0]).toBe(
      i18n.global.t('errors.transport.nonTextFrame', { kind: 'binary' }),
    );
  });

  it('delivers every frame, sub-agent lanes included, to every subscriber', () => {
    const { client, socket } = connected();
    const first: string[] = [];
    const second: string[] = [];
    const unsubscribe = client.onFrame((envelope) => first.push(envelope.type));
    client.onFrame((envelope) => second.push(envelope.type));

    socket.deliver({ type: 'assistant_chunk', text: 'main lane' });
    socket.deliver({ type: 'assistant_chunk', text: 'worker lane', subagent_id: 'code-reviewer' });

    expect(first).toEqual(['assistant_chunk', 'assistant_chunk']);
    expect(second).toEqual(first);

    unsubscribe();
    socket.deliver({ type: 'done', reason: 'end_turn' });
    expect(first).toEqual(['assistant_chunk', 'assistant_chunk']);
    expect(second).toHaveLength(3);
  });

  it('hands the sub-agent id to the subscriber', () => {
    const { client, socket } = connected();
    let lane: string | null | undefined;
    client.onFrame((envelope) => {
      lane = envelope.subagent_id;
    });

    socket.deliver({ type: 'assistant_chunk', text: 'x', subagent_id: 'test-writer' });

    expect(lane).toBe('test-writer');
  });

  it('survives a handler that throws', () => {
    const { client, socket } = connected();
    const reached: string[] = [];
    client.onFrame(() => {
      throw new Error('handler bug');
    });
    client.onFrame((envelope) => reached.push(envelope.type));

    socket.deliver({ type: 'assistant_chunk', text: 'x' });

    expect(reached).toEqual(['assistant_chunk']);
    expect(client.lastErrorDetails.value).toEqual({
      code: 'handlerThrew',
      params: { reason: 'handler bug' },
    });
  });

  it('exposes a client that can be built without a DOM', () => {
    const client: WebSocketClient = makeClient();
    expect(client.queued.value).toBe(0);
    expect(client.protocolErrors.value).toEqual([]);
  });
});

describe('transport error wording', () => {
  /** The raw message for a dotted key, straight from the locale tree. */
  function rawMessage(locale: string, path: string): string {
    const found = path
      .split('.')
      .reduce<unknown>(
        (node, part) => (node as Record<string, unknown> | undefined)?.[part],
        messages[locale],
      );
    if (typeof found !== 'string') throw new Error(`${path} is missing from ${locale}`);
    return found;
  }

  it('explains a reconnect it has no other reason for, and clears it on open', () => {
    const { client, socket } = connected();
    socket.drop();

    expect(client.state.value).toBe('reconnecting');
    expect(client.lastErrorDetails.value).toEqual({ code: 'reconnecting', params: { attempt: 1 } });

    vi.advanceTimersByTime(100);
    FakeSocket.last().accept();

    expect(client.lastErrorDetails.value).toBeNull();
    expect(client.lastError.value).toBeNull();
  });

  it('keeps the specific failure instead of overwriting it with the retry note', () => {
    const client = makeClient({ heartbeatIntervalMs: 100, pongTimeoutMs: 300 });
    client.connect();
    FakeSocket.last().accept();

    vi.advanceTimersByTime(100);
    vi.advanceTimersByTime(100);

    // The tick found the peer still silent: that reason outlives the retry it
    // schedules.
    expect(client.lastErrorDetails.value).toEqual({
      code: 'pongTimeout',
      params: { timeout: 300 },
    });

    vi.advanceTimersByTime(100);
    expect(client.lastErrorDetails.value).toEqual({
      code: 'pongTimeout',
      params: { timeout: 300 },
    });
  });

  it('renders one failure differently per locale and follows a switch', () => {
    const { client } = connected();
    vi.advanceTimersByTime(HEARTBEAT + PONG_TIMEOUT);
    expect(client.lastErrorDetails.value).toEqual({ code: 'serverSilent' });

    i18n.global.locale.value = 'en';
    const english = client.lastError.value;
    i18n.global.locale.value = 'zh-CN';
    const chinese = client.lastError.value;

    expect(english).toBe(rawMessage('en', 'errors.transport.serverSilent'));
    expect(chinese).toBe(rawMessage('zh-CN', 'errors.transport.serverSilent'));
    expect(english).not.toBe(chinese);
  });
});
