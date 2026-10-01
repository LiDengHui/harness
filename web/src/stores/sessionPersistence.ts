/**
 * What survives a reload, and nothing else.
 *
 * The active session used to live only in memory, so a refresh — or reopening
 * the page on a phone — silently dropped the user into a fresh conversation with
 * no sign that the one they were in was still on the server. The fix is to keep
 * the two ids that name it: the session the connection was driving, and the
 * session the pane was showing.
 *
 * Only ids are stored. The transcript is the server's record, addressable at
 * `/api/sessions/{id}/history`; a copy here would be a second, disagreeing owner
 * of the same conversation, and would go stale the moment a run produced a frame
 * the stored copy missed. The id is enough to find the conversation again.
 *
 * Storage is injectable for the same reason the transport is: the tests run in
 * the `node` environment, which has no `localStorage`, and a browser that refuses
 * storage access (private mode, a hardened profile) must get a working page
 * rather than a boot failure — so every read and write is best-effort.
 */

/** Namespaced, so the harness never collides with another app on the origin. */
export const SESSION_STORAGE_KEY = 'dhl-harness.session.v1';

/** The two ids a reload needs to find the same conversation again. */
export interface PersistedSession {
  /** The session the pane was showing. */
  view: string | null;
  /** The session the connection was driving, when it was driving one. */
  live: string | null;
}

/** The slice of `localStorage` this module uses. */
export interface KeyValueStore {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem(key: string): void;
}

/**
 * The store the tests inject, or `null` to fall back to the browser's.
 *
 * A module-level override rather than an argument on every call, so the store
 * code reads the same in production and in a test — the seam is `setShared…`,
 * the same shape `useWebSocket` uses.
 */
let injected: KeyValueStore | null = null;

export function setSessionStorage(store: KeyValueStore | null): void {
  injected = store;
}

/** The browser's `localStorage`, or `null` when there is none to be had. */
function browserStorage(): KeyValueStore | null {
  if (injected !== null) return injected;
  try {
    return globalThis.localStorage ?? null;
  } catch {
    // Access itself can throw when a browser blocks storage for this origin.
    return null;
  }
}

function idOrNull(value: unknown): string | null {
  return typeof value === 'string' && value !== '' ? value : null;
}

/**
 * Reads the stored record, or `null` when there is nothing usable to restore.
 *
 * A missing key, unreadable storage, a body that is not JSON and a record with
 * no ids at all are the same answer to the caller: there is no previous
 * conversation to go back to.
 */
export function readPersistedSession(storage: KeyValueStore | null = browserStorage()): PersistedSession | null {
  if (storage === null) return null;

  let raw: string | null;
  try {
    raw = storage.getItem(SESSION_STORAGE_KEY);
  } catch {
    return null;
  }
  if (raw === null) return null;

  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch {
    return null;
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return null;

  const record = parsed as Record<string, unknown>;
  const view = idOrNull(record['view']);
  const live = idOrNull(record['live']);
  if (view === null && live === null) return null;
  return { view, live };
}

/**
 * Writes the two ids, or clears the entry when there is no session to name.
 *
 * Clearing rather than writing `{view:null,live:null}` is what lets a browser
 * that never had a session stay a blank slate: a fresh page must not reattach to
 * a conversation it never opened. A write that fails (storage full, storage
 * refused) is dropped — persistence is an optimisation, never a requirement.
 */
export function writePersistedSession(
  view: string | null,
  live: string | null,
  storage: KeyValueStore | null = browserStorage(),
): void {
  if (storage === null) return;

  try {
    if (view === null && live === null) {
      storage.removeItem(SESSION_STORAGE_KEY);
      return;
    }
    storage.setItem(SESSION_STORAGE_KEY, JSON.stringify({ view, live }));
  } catch {
    // Storage unavailable or full: the page still works, it just forgets.
  }
}
