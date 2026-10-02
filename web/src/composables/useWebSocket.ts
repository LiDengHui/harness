/**
 * The transport: one shared socket, and the policy for keeping it alive.
 *
 * Everything the bus needs to survive a real network lives here — lazy opening,
 * exponential backoff with jitter, a heartbeat that can tell a dead peer from a
 * quiet one, and an outbound queue so a message typed before the socket is up is
 * still delivered in order. The stores above it only ever see whole envelopes.
 *
 * The constructor is injectable (`WebSocketImpl`) because reconnect behaviour is
 * the part most worth testing and the least pleasant to test against a real
 * socket.
 */

import { computed, ref, type ComputedRef, type Ref } from 'vue';

import { i18n } from '../i18n';
import { accessToken } from '../token';
import {
  type ClientMessage,
  type ClientRouting,
  type ServerEnvelope,
  clientEnvelope,
  parseServerFrame,
  serializeClientEnvelope,
} from '../protocol';

export type ConnectionState = 'connecting' | 'open' | 'closed' | 'reconnecting';

/** `WebSocket.OPEN`, spelled out so the injected fake does not have to copy it. */
const SOCKET_CONNECTING = 0;
const SOCKET_OPEN = 1;

/**
 * The slice of `WebSocket` this client uses.
 *
 * The DOM type differs only in its handler parameter types, so implementations
 * are swapped through `WebSocketImpl` rather than by subclassing.
 */
export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((event: unknown) => void) | null;
  onclose: ((event: unknown) => void) | null;
  onerror: ((event: unknown) => void) | null;
  onmessage: ((event: { data: unknown }) => void) | null;
}

export type WebSocketCtor = new (url: string) => WebSocketLike;

export type FrameHandler = (envelope: ServerEnvelope) => void;

export interface BackoffOptions {
  /** Delay before the first retry. */
  baseMs?: number;
  /** Multiplier applied to each consecutive failure. */
  factor?: number;
  /** Ceiling for the exponential term, applied before jitter. */
  maxMs?: number;
  /** Fraction of the delay that is randomised, e.g. `0.25` for ±25%. */
  jitter?: number;
}

export interface WebSocketClientOptions {
  /** Defaults to `<location>/ws`, so the proxy and the Rust binary both work. */
  url?: string;
  WebSocketImpl?: WebSocketCtor;
  heartbeatIntervalMs?: number;
  pongTimeoutMs?: number;
  backoff?: BackoffOptions;
  /** Events beyond this many are dropped, oldest first. */
  maxQueuedFrames?: number;
  /** Injected for deterministic tests. */
  random?: () => number;
}

/**
 * A transport failure, kept as a code plus the fields it was built from.
 *
 * Storing the sentence instead would freeze it in one locale and throw away
 * what a different rendering might need; `lastError` composes the wording on
 * read, so switching locale re-renders the failure that is already on screen.
 */
export type TransportErrorCode =
  | 'openFailed'
  | 'socketError'
  | 'reconnecting'
  | 'queueOverflow'
  | 'flushFailed'
  | 'sendFailed'
  | 'pongTimeout'
  | 'serverSilent'
  | 'heartbeatFailed'
  | 'nonTextFrame'
  | 'handlerThrew'
  | 'protocol';

export interface TransportError {
  code: TransportErrorCode;
  /** Interpolation values for the `errors.transport.<code>` message. */
  params?: Record<string, string | number>;
}

/** The sentence for one failure, in whichever locale is active now. */
export function transportErrorText(error: TransportError): string {
  return i18n.global.t(`errors.transport.${error.code}`, error.params ?? {});
}

export interface WebSocketClient {
  /** `connecting` on the first attempt, `reconnecting` after a failure. */
  readonly state: Ref<ConnectionState>;
  readonly isOpen: ComputedRef<boolean>;
  /** The most recent transport-level problem, cleared on a successful open. */
  readonly lastError: ComputedRef<string | null>;
  /** The failure behind `lastError`, with its code and fields intact. */
  readonly lastErrorDetails: Ref<TransportError | null>;
  /** Frames the parser refused, newest last. */
  readonly protocolErrors: Ref<string[]>;
  /** Consecutive failed attempts; resets to 0 on a successful open. */
  readonly attempts: Ref<number>;
  /** Outbound frames waiting for an open socket. */
  readonly queued: Ref<number>;

  connect(): void;
  /** Deliberate shutdown: no further reconnects, and the queue is dropped. */
  disconnect(): void;
  send(message: ClientMessage, routing?: ClientRouting): void;
  /** Receives *every* frame, sub-agent lanes included; returns an unsubscribe. */
  onFrame(handler: FrameHandler): () => void;
  onProtocolError(handler: (error: string) => void): () => void;
}

/** Where the bus lives, derived from the page so both serving modes work. */
export function defaultWebSocketUrl(): string {
  const location = globalThis.location;
  if (!location || !location.host) {
    return 'ws://127.0.0.1:8787/ws';
  }
  const scheme = location.protocol === 'https:' ? 'wss:' : 'ws:';
  // A server bound to the network requires the token it printed, and it arrives
  // in the page the server opened. It is taken from the capture rather than the
  // address bar, because a reconnect outlives a navigation: reading the current
  // URL here is how a socket that dropped after a menu click came back refused,
  // leaving the UI disconnected with no way for the reader to tell why.
  const token = accessToken(location.search);
  const query = token ? `?token=${encodeURIComponent(token)}` : '';
  return `${scheme}//${location.host}/ws${query}`;
}

function browserWebSocketCtor(): WebSocketCtor {
  // The DOM `WebSocket` and `WebSocketLike` agree on everything this client
  // uses; only the handler parameter types differ, so the cast stays here.
  return WebSocket as unknown as WebSocketCtor;
}

export function createWebSocketClient(options: WebSocketClientOptions = {}): WebSocketClient {
  const heartbeatIntervalMs = options.heartbeatIntervalMs ?? 15_000;
  const pongTimeoutMs = options.pongTimeoutMs ?? 5_000;
  const baseMs = options.backoff?.baseMs ?? 500;
  const factor = options.backoff?.factor ?? 2;
  const maxMs = options.backoff?.maxMs ?? 15_000;
  const jitter = options.backoff?.jitter ?? 0.25;
  const maxQueuedFrames = options.maxQueuedFrames ?? 256;
  const random = options.random ?? Math.random;

  const state = ref<ConnectionState>('closed');
  const lastErrorDetails = ref<TransportError | null>(null);
  const protocolErrors = ref<string[]>([]);
  const attempts = ref(0);
  const queued = ref(0);
  const isOpen = computed(() => state.value === 'open');
  /** Composed on read, so a locale switch re-renders the failure in place. */
  const lastError = computed<string | null>(() =>
    lastErrorDetails.value === null ? null : transportErrorText(lastErrorDetails.value),
  );

  const frameHandlers = new Set<FrameHandler>();
  const errorHandlers = new Set<(error: string) => void>();

  let socket: WebSocketLike | null = null;
  let outbound: string[] = [];
  /** Whether the client intends to hold a connection open at all. */
  let started = false;
  let attempt = 0;
  let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  let heartbeatTimer: ReturnType<typeof setTimeout> | null = null;
  let pongTimer: ReturnType<typeof setTimeout> | null = null;
  let awaitingPong = false;

  function isCurrent(candidate: WebSocketLike): boolean {
    return candidate === socket;
  }

  function noteError(error: TransportError): void {
    lastErrorDetails.value = error;
  }

  function connect(): void {
    started = true;
    if (socket && (socket.readyState === SOCKET_CONNECTING || socket.readyState === SOCKET_OPEN)) {
      return;
    }
    openSocket();
  }

  function disconnect(): void {
    started = false;
    clearReconnect();
    stopHeartbeat();
    const closing = socket;
    socket = null;
    if (closing) {
      try {
        closing.close(1000, 'client closing');
      } catch {
        // Already gone; nothing to release.
      }
    }
    outbound = [];
    queued.value = 0;
    state.value = 'closed';
  }

  function openSocket(): void {
    clearReconnect();
    const url = options.url ?? defaultWebSocketUrl();
    let created: WebSocketLike;
    state.value = attempt === 0 ? 'connecting' : 'reconnecting';
    try {
      const ctor = options.WebSocketImpl ?? browserWebSocketCtor();
      created = new ctor(url);
    } catch (err) {
      noteError({ code: 'openFailed', params: { url, reason: errorMessage(err) } });
      scheduleReconnect();
      return;
    }

    socket = created;
    created.onopen = () => {
      if (isCurrent(created)) handleOpen(created);
    };
    created.onclose = () => {
      if (isCurrent(created)) handleClose(created);
    };
    created.onerror = () => {
      if (isCurrent(created)) noteError({ code: 'socketError' });
    };
    created.onmessage = (event) => {
      if (isCurrent(created)) handleInbound(event.data);
    };
  }

  function handleOpen(opened: WebSocketLike): void {
    attempt = 0;
    attempts.value = 0;
    lastErrorDetails.value = null;
    state.value = 'open';
    flushQueue(opened);
    startHeartbeat(opened);
  }

  function handleClose(closed: WebSocketLike): void {
    if (!isCurrent(closed)) return;
    socket = null;
    stopHeartbeat();
    if (!started) {
      state.value = 'closed';
      return;
    }
    state.value = 'reconnecting';
    scheduleReconnect();
  }

  /**
   * Gives up on a socket the transport still believes in — a half-open one
   * whose missing pong never produced a close event.
   */
  function dropSocket(dead: WebSocketLike): void {
    if (!isCurrent(dead)) return;
    socket = null;
    stopHeartbeat();
    try {
      dead.close();
    } catch {
      // Nothing to close.
    }
    if (!started) {
      state.value = 'closed';
      return;
    }
    state.value = 'reconnecting';
    scheduleReconnect();
  }

  function clearReconnect(): void {
    if (reconnectTimer !== null) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
  }

  function scheduleReconnect(): void {
    if (!started || reconnectTimer !== null) return;
    attempt += 1;
    attempts.value = attempt;
    // A close that carried no reason of its own still deserves one: without
    // this the badge would show a retry with nothing to explain it.
    if (lastErrorDetails.value === null) {
      noteError({ code: 'reconnecting', params: { attempt } });
    }

    const exponential = Math.min(maxMs, baseMs * factor ** (attempt - 1));
    // Jitter is centred on the exponential term: without it every client that a
    // single restart disconnected would come back in lockstep.
    const spread = exponential * jitter;
    const delay = Math.max(0, Math.round(exponential - spread + random() * spread * 2));

    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      if (started) openSocket();
    }, delay);
  }

  function enqueue(frame: string): void {
    outbound.push(frame);
    if (outbound.length > maxQueuedFrames) {
      outbound.shift();
      // The newest frames describe what the user just asked for; the oldest are
      // the ones worth losing.
      noteError({ code: 'queueOverflow', params: { limit: maxQueuedFrames } });
    }
    queued.value = outbound.length;
  }

  function flushQueue(opened: WebSocketLike): void {
    const pending = outbound;
    outbound = [];
    queued.value = 0;
    for (let index = 0; index < pending.length; index += 1) {
      const frame = pending[index] as string;
      try {
        opened.send(frame);
      } catch (err) {
        // The socket died mid-flush: keep the unsent remainder, in order.
        outbound = pending.slice(index);
        queued.value = outbound.length;
        noteError({ code: 'flushFailed', params: { reason: errorMessage(err) } });
        return;
      }
    }
  }

  function send(message: ClientMessage, routing: ClientRouting = {}): void {
    const frame = serializeClientEnvelope(clientEnvelope(message, routing));
    const current = socket;
    if (current && current.readyState === SOCKET_OPEN) {
      try {
        current.send(frame);
        return;
      } catch (err) {
        noteError({ code: 'sendFailed', params: { reason: errorMessage(err) } });
      }
    }
    enqueue(frame);
  }

  function startHeartbeat(alive: WebSocketLike): void {
    stopHeartbeat();
    heartbeatTimer = setTimeout(tick, heartbeatIntervalMs);

    function tick(): void {
      heartbeatTimer = null;
      if (!isCurrent(alive)) return;
      if (awaitingPong) {
        noteError({ code: 'pongTimeout', params: { timeout: pongTimeoutMs } });
        dropSocket(alive);
        return;
      }
      awaitingPong = true;
      pongTimer = setTimeout(() => {
        pongTimer = null;
        if (awaitingPong && isCurrent(alive)) {
          noteError({ code: 'serverSilent' });
          dropSocket(alive);
        }
      }, pongTimeoutMs);
      try {
        alive.send(serializeClientEnvelope(clientEnvelope({ type: 'ping' })));
      } catch (err) {
        noteError({ code: 'heartbeatFailed', params: { reason: errorMessage(err) } });
      }
      heartbeatTimer = setTimeout(tick, heartbeatIntervalMs);
    }
  }

  function stopHeartbeat(): void {
    if (heartbeatTimer !== null) {
      clearTimeout(heartbeatTimer);
      heartbeatTimer = null;
    }
    if (pongTimer !== null) {
      clearTimeout(pongTimer);
      pongTimer = null;
    }
    awaitingPong = false;
  }

  function handleInbound(data: unknown): void {
    if (typeof data !== 'string') {
      reportProtocolError({ code: 'nonTextFrame', params: { kind: describe(data) } });
      return;
    }

    const parsed = parseServerFrame(data);
    if (!parsed.ok) {
      // The parser owns the wording of a malformed frame; this carries it
      // through unchanged, so a version mismatch still reads as it described it.
      reportProtocolError({ code: 'protocol', params: { detail: parsed.error } });
      return;
    }

    if (parsed.envelope.type === 'pong') {
      awaitingPong = false;
      if (pongTimer !== null) {
        clearTimeout(pongTimer);
        pongTimer = null;
      }
    }

    // Every frame goes to every handler; routing (including `subagent_id`) is in
    // the envelope, and filtering is the stores' business.
    for (const handler of frameHandlers) {
      try {
        handler(parsed.envelope);
      } catch (err) {
        noteError({ code: 'handlerThrew', params: { reason: errorMessage(err) } });
      }
    }
  }

  function reportProtocolError(error: TransportError): void {
    const text = transportErrorText(error);
    protocolErrors.value = [...protocolErrors.value, text].slice(-50);
    lastErrorDetails.value = error;
    for (const handler of errorHandlers) {
      try {
        handler(text);
      } catch {
        // A reporting handler that throws is not worth a second failure.
      }
    }
  }

  return {
    state,
    isOpen,
    lastError,
    lastErrorDetails,
    protocolErrors,
    attempts,
    queued,
    connect,
    disconnect,
    send,
    onFrame(handler: FrameHandler): () => void {
      frameHandlers.add(handler);
      return () => {
        frameHandlers.delete(handler);
      };
    },
    onProtocolError(handler: (error: string) => void): () => void {
      errorHandlers.add(handler);
      return () => {
        errorHandlers.delete(handler);
      };
    },
  };
}

/** The client the app shares: one socket per page, opened on first use. */
let shared: WebSocketClient | null = null;

export function useWebSocket(): WebSocketClient {
  if (shared === null) {
    shared = createWebSocketClient();
  }
  return shared;
}

/**
 * Replaces the shared client.
 *
 * The app never calls this: it exists so a test can drive the stores through a
 * fake transport — with a fake socket — and assert the frames they emit.
 */
export function setSharedWebSocketClient(client: WebSocketClient | null): void {
  shared = client;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function describe(value: unknown): string {
  if (value === null) return 'null';
  if (typeof value === 'string') return 'string';
  if (typeof Blob !== 'undefined' && value instanceof Blob) return 'blob';
  if (value instanceof ArrayBuffer || ArrayBuffer.isView(value)) return 'binary';
  return typeof value;
}
