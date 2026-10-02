/**
 * The REST client: deliberately just `fetch`.
 *
 * The server is same-origin in both serving modes (the dev proxy in
 * development, the Rust binary in production) and every route returns JSON, so
 * a wrapper library would only add a dependency to hide two lines of error
 * handling.
 */

import { i18n } from '../i18n';
import { accessToken } from '../token';

/** What went wrong, as a code the UI can word for itself. */
export type ApiFailureCode = 'unreachable' | 'status' | 'invalid_json';

/** A REST failure with its fields intact, so the sentence is built later. */
export interface ApiFailure {
  code: ApiFailureCode;
  path: string;
  /** The status, when the server answered at all. */
  status?: number;
  /** The server's own `{"error": "..."}`, when it sent one. */
  detail?: string | null;
  /** Why the request died before a response, or why the body did not parse. */
  reason?: string;
}

/** The sentence for one failure, in whichever locale is active now. */
export function apiFailureText(failure: ApiFailure): string {
  switch (failure.code) {
    case 'unreachable':
      return i18n.global.t('errors.api.unreachable', {
        path: failure.path,
        reason: failure.reason ?? '',
      });
    case 'status':
      return failure.detail === null || failure.detail === undefined || failure.detail === ''
        ? i18n.global.t('errors.api.status', {
            path: failure.path,
            status: failure.status ?? 0,
          })
        : i18n.global.t('errors.api.statusWithDetail', {
            path: failure.path,
            status: failure.status ?? 0,
            detail: failure.detail,
          });
    case 'invalid_json':
      return i18n.global.t('errors.api.notJson', {
        path: failure.path,
        reason: failure.reason ?? '',
      });
  }
}

/**
 * The sentence for anything a store caught, REST-shaped or not.
 *
 * A thrown value that is not an `ApiError` never went through this client, so
 * its own message is all there is to show — wrapped so the line still reads as
 * a failure rather than as a bare browser string.
 */
export function requestFailureText(err: unknown): string {
  if (err instanceof ApiError) return err.message;
  return i18n.global.t('errors.api.unknown', { reason: errorMessage(err) });
}

export class ApiError extends Error {
  /** Everything the failure is made of; the wording is derived from it. */
  readonly failure: ApiFailure;

  constructor(failure: ApiFailure) {
    super();
    this.name = 'ApiError';
    this.failure = failure;
  }

  /** True when the route simply is not implemented yet. */
  get isMissing(): boolean {
    return this.failure.status === 404;
  }

  /** The status, or `0` when the server never answered. */
  get status(): number {
    return this.failure.status ?? 0;
  }

  get path(): string {
    return this.failure.path;
  }

  /** Composed on read, so the error reads in whichever locale is active. */
  override get message(): string {
    return apiFailureText(this.failure);
  }
}

/**
 * The token the server prints when it is bound to the network.
 *
 * It arrives in the URL the CLI opens, and `accessToken` captures it there so a
 * request does not depend on the address bar still naming it. Absent on a
 * loopback bind, where the server asks for none.
 */
function withToken(path: string): string {
  if (typeof location === 'undefined') return path;
  const token = accessToken(location.search);
  if (!token) return path;
  const separator = path.includes('?') ? '&' : '?';
  return `${path}${separator}token=${encodeURIComponent(token)}`;
}

export async function getJson<T>(path: string): Promise<T> {
  let response: Response;
  try {
    response = await fetch(withToken(path), { headers: { accept: 'application/json' } });
  } catch (err) {
    throw new ApiError({ code: 'unreachable', path, reason: errorMessage(err) });
  }

  if (!response.ok) {
    const detail = await readErrorDetail(response);
    throw new ApiError({ code: 'status', path, status: response.status, detail });
  }

  try {
    return (await response.json()) as T;
  } catch (err) {
    throw new ApiError({
      code: 'invalid_json',
      path,
      status: response.status,
      reason: errorMessage(err),
    });
  }
}

/**
 * Posts a JSON body and reads a JSON answer, with the same failure handling as
 * `getJson`.
 *
 * The body is written by the caller rather than described by a schema: the two
 * callers each send one field, and a shared shape would be more machinery than
 * the payload is worth.
 */
export async function postJson<T>(path: string, body: unknown): Promise<T> {
  let response: Response;
  try {
    response = await fetch(withToken(path), {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(body),
    });
  } catch (err) {
    throw new ApiError({ code: 'unreachable', path, reason: errorMessage(err) });
  }

  if (!response.ok) {
    const detail = await readErrorDetail(response);
    throw new ApiError({ code: 'status', path, status: response.status, detail });
  }

  try {
    return (await response.json()) as T;
  } catch (err) {
    throw new ApiError({
      code: 'invalid_json',
      path,
      status: response.status,
      reason: errorMessage(err),
    });
  }
}

/** The server's failure bodies carry `{ "error": "..." }` and nothing else. */
async function readErrorDetail(response: Response): Promise<string | null> {
  try {
    const body: unknown = await response.json();
    if (typeof body === 'object' && body !== null && 'error' in body) {
      const detail = (body as { error: unknown }).error;
      if (typeof detail === 'string') return detail;
    }
  } catch {
    // A body that is not JSON says nothing useful either way.
  }
  return null;
}

function errorMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}
