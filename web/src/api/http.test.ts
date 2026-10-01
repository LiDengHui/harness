import { afterEach, describe, expect, it, vi } from 'vitest';

import { i18n } from '../i18n';
import { ApiError, apiFailureText, getJson, postJson, requestFailureText } from './http';

/** The slice of `Response` this client actually reads. */
interface FakeResponse {
  ok: boolean;
  status: number;
  json(): Promise<unknown>;
}

function isOk(status: number): boolean {
  return status >= 200 && status < 300;
}

/** A `fetch` that answers with the given status and JSON body. */
function respond(status: number, body: unknown): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async (): Promise<FakeResponse> => ({
        ok: isOk(status),
        status,
        json: async () => body,
      }),
    ),
  );
}

/** A `fetch` that answers with a body that is not JSON at all. */
function respondGarbage(status = 200): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(
      async (): Promise<FakeResponse> => ({
        ok: isOk(status),
        status,
        json: async () => {
          throw new SyntaxError('Unexpected token <');
        },
      }),
    ),
  );
}

/** A `fetch` that fails the way an unreachable server does. */
function rejectWith(err: unknown): void {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => {
      throw err;
    }),
  );
}

/** Runs `getJson` and hands back the `ApiError` it threw. */
async function failure(path = '/api/agents'): Promise<ApiError> {
  try {
    await getJson(path);
  } catch (err) {
    if (err instanceof ApiError) return err;
    throw err;
  }
  throw new Error('the request was expected to fail');
}

const INITIAL_LOCALE = i18n.global.locale.value;

afterEach(() => {
  vi.unstubAllGlobals();
  i18n.global.locale.value = INITIAL_LOCALE;
});

describe('getJson', () => {
  it('returns the parsed body of a good response', async () => {
    respond(200, [{ id: 'default' }]);
    await expect(getJson('/api/agents')).resolves.toEqual([{ id: 'default' }]);
  });

  it('keeps a transport failure as a code with its fields', async () => {
    rejectWith(new TypeError('Failed to fetch'));

    const err = await failure();

    expect(err.failure).toEqual({
      code: 'unreachable',
      path: '/api/agents',
      reason: 'Failed to fetch',
    });
    expect(err.status).toBe(0);
    expect(err.isMissing).toBe(false);
  });

  it('keeps the status and the detail the error body carried', async () => {
    respond(404, { error: 'no such session' });

    const err = await failure('/api/sessions/sess_1');

    expect(err.failure).toEqual({
      code: 'status',
      path: '/api/sessions/sess_1',
      status: 404,
      detail: 'no such session',
    });
    expect(err.status).toBe(404);
    expect(err.isMissing).toBe(true);
  });

  it('reports a body that is not JSON without losing the status', async () => {
    respondGarbage(200);

    const err = await failure();

    expect(err.failure.code).toBe('invalid_json');
    expect(err.failure.status).toBe(200);
  });

  it('words the failure in the active locale, on read rather than at the throw', async () => {
    rejectWith(new TypeError('Failed to fetch'));
    const err = await failure();

    i18n.global.locale.value = 'en';
    const english = err.message;
    // Captured while `en` is active, so the expectation is that locale's wording.
    const expectedEnglish = apiFailureText({
      code: 'unreachable',
      path: '/api/agents',
      reason: 'Failed to fetch',
    });
    i18n.global.locale.value = 'zh-CN';
    const chinese = err.message;

    expect(english).toBe(expectedEnglish);
    expect(english).not.toBe(chinese);
    expect(english).toContain('/api/agents');
    expect(chinese).toContain('/api/agents');
  });

  it('reads the server detail back into the sentence', () => {
    const text = apiFailureText({
      code: 'status',
      path: '/api/sessions/sess_1',
      status: 404,
      detail: 'no such session',
    });
    expect(text).toBe(
      i18n.global.t('errors.api.statusWithDetail', {
        path: '/api/sessions/sess_1',
        status: 404,
        detail: 'no such session',
      }),
    );
  });

  it('wraps a thrown value that never reached the client', () => {
    const text = requestFailureText(new Error('boom'));
    expect(text).toBe(i18n.global.t('errors.api.unknown', { reason: 'boom' }));
    expect(text).not.toContain('errors.api');
  });
});

describe('postJson', () => {
  /** A `fetch` that records the request and answers with one body. */
  function record(status: number, body: unknown): { calls: unknown[] } {
    const calls: unknown[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: unknown, init: unknown) => {
        calls.push({ path, init });
        return { ok: isOk(status), status, json: async () => body };
      }),
    );
    return { calls };
  }

  it('writes the body as JSON and reads the answer', async () => {
    const { calls } = record(200, { id: 'ship-it' });

    await expect(postJson('/api/workflows', { description: 'when both ends change' })).resolves.toEqual(
      { id: 'ship-it' },
    );

    expect(calls).toEqual([
      {
        path: '/api/workflows',
        init: {
          method: 'POST',
          headers: { 'content-type': 'application/json', accept: 'application/json' },
          body: JSON.stringify({ description: 'when both ends change' }),
        },
      },
    ]);
  });

  it('fails the same way a read does, with the status and detail kept', async () => {
    record(404, { error: 'no route for POST /api/workflows' });

    await expect(postJson('/api/workflows', { description: 'x' })).rejects.toMatchObject({
      failure: {
        code: 'status',
        path: '/api/workflows',
        status: 404,
        detail: 'no route for POST /api/workflows',
      },
    });
  });
});
