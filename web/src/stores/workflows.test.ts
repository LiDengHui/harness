import { createPinia, setActivePinia } from 'pinia';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { i18n } from '../i18n';
import { parseWorkflow, parseWorkflowList, useWorkflowsStore } from './workflows';

/** One fetch call, as the stub recorded it. */
interface Call {
  path: string;
  method: string;
  body: unknown;
}

/** A `fetch` answering with one status and body, recording what it was asked. */
function respond(status: number, body: unknown): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    'fetch',
    vi.fn(async (path: unknown, init?: { method?: string; body?: string }) => {
      calls.push({
        path: String(path),
        method: init?.method ?? 'GET',
        body: init?.body === undefined ? undefined : JSON.parse(init.body),
      });
      return {
        ok: status >= 200 && status < 300,
        status,
        json: async () => body,
      };
    }),
  );
  return calls;
}

beforeEach(() => {
  setActivePinia(createPinia());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('parseWorkflow', () => {
  it('reads an entry, keeping the when that says what it is for', () => {
    expect(
      parseWorkflow({ id: 'ship-it', name: 'Ship it', when: 'when a task changes both ends' }),
    ).toEqual({
      id: 'ship-it',
      name: 'Ship it',
      when: 'when a task changes both ends',
      source_path: null,
    });
  });

  it('falls back to the id when the server named no name', () => {
    expect(parseWorkflow({ id: 'ship-it' })?.name).toBe('ship-it');
    expect(parseWorkflow({ id: 'ship-it' })?.when).toBe('');
  });

  it('refuses an entry with no id, which nothing could select', () => {
    expect(parseWorkflow({ name: 'no id' })).toBeNull();
    expect(parseWorkflow('ship-it')).toBeNull();
  });
});

describe('parseWorkflowList', () => {
  it('reads a bare array, the shape the session route uses', () => {
    expect(parseWorkflowList([{ id: 'a' }, { id: 'b' }])?.map((entry) => entry.id)).toEqual([
      'a',
      'b',
    ]);
  });

  it('reads a wrapped list too', () => {
    expect(parseWorkflowList({ workflows: [{ id: 'a' }] })?.map((entry) => entry.id)).toEqual(['a']);
  });

  it('drops an unreadable entry rather than the whole list', () => {
    expect(parseWorkflowList([{ id: 'a' }, { name: 'no id' }])?.map((entry) => entry.id)).toEqual([
      'a',
    ]);
  });

  it('is null for a body that is not a list at all', () => {
    expect(parseWorkflowList({ nope: true })).toBeNull();
    expect(parseWorkflowList('workflows')).toBeNull();
  });
});

describe('the workflows store', () => {
  it('reads the list once and caches it', async () => {
    respond(200, { workflows: [{ id: 'ship-it', name: 'Ship it', when: 'both ends change' }] });
    const store = useWorkflowsStore();

    await store.load();
    await store.load();

    expect(store.list.map((workflow) => workflow.id)).toEqual(['ship-it']);
    expect(store.loaded).toBe(true);
    expect(fetch).toHaveBeenCalledTimes(1);
  });

  it('degrades to an empty list with a failure when the route is not there', async () => {
    respond(404, { error: 'no route for GET /api/workflows' });
    const store = useWorkflowsStore();

    await store.load();

    expect(store.list).toEqual([]);
    expect(store.loaded).toBe(false);
    // The wording follows the locale, like every other failure.
    expect(store.error).toBe(i18n.global.t('errors.api.statusWithDetail', {
      path: '/api/workflows',
      status: 404,
      detail: 'no route for GET /api/workflows',
    }));
  });

  it('treats a body that is not a list as a failure, not as an empty list', async () => {
    respond(200, { unexpected: true });
    const store = useWorkflowsStore();

    await store.load();

    expect(store.list).toEqual([]);
    expect(store.error).not.toBeNull();
  });

  /**
   * A `fetch` that answers the create POST and the list GET differently, so a
   * test can prove the store re-reads rather than only patching locally.
   */
  function respondCreateThenList(created: unknown, listed: unknown): Call[] {
    const calls: Call[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (path: unknown, init?: { method?: string; body?: string }) => {
        const method = init?.method ?? 'GET';
        calls.push({
          path: String(path),
          method,
          body: init?.body === undefined ? undefined : JSON.parse(init.body),
        });
        return { ok: true, status: 200, json: async () => (method === 'POST' ? created : listed) };
      }),
    );
    return calls;
  }

  it('posts the description, then re-reads the list the server now holds', async () => {
    const calls = respondCreateThenList(
      { workflow: { id: 'ship-it', name: 'Ship it', when: 'both ends' } },
      [{ id: 'ship-it', name: 'Ship it', when: 'both ends' }],
    );
    const store = useWorkflowsStore();

    const created = await store.create('when a task changes both ends');

    expect(created.id).toBe('ship-it');
    expect(calls).toEqual([
      {
        path: '/api/workflows',
        method: 'POST',
        body: { description: 'when a task changes both ends' },
      },
      { path: '/api/workflows', method: 'GET', body: undefined },
    ]);
    expect(store.list.map((workflow) => workflow.id)).toEqual(['ship-it']);
  });

  it('keeps a created workflow the re-read does not list yet', async () => {
    // The server has not folded the new workflow into its registry, so the
    // re-read comes back empty. The created entry must still be selectable.
    respondCreateThenList({ workflow: { id: 'ship-it', name: 'Ship it' } }, []);
    const store = useWorkflowsStore();

    const created = await store.create('anything');

    expect(created.id).toBe('ship-it');
    expect(store.list.map((workflow) => workflow.id)).toEqual(['ship-it']);
  });

  it('reads a workflow returned unwrapped too', async () => {
    respondCreateThenList({ id: 'ship-it', name: 'Ship it' }, [{ id: 'ship-it', name: 'Ship it' }]);
    const store = useWorkflowsStore();

    await expect(store.create('anything')).resolves.toMatchObject({ id: 'ship-it' });
  });

  it('fails loudly when the server did not return the workflow it made', async () => {
    respond(200, { ok: true });
    const store = useWorkflowsStore();

    await expect(store.create('anything')).rejects.toThrow(/did not return/);
  });

  it('propagates a refused create, so the panel can say why', async () => {
    respond(500, { error: 'the model could not write a workflow' });
    const store = useWorkflowsStore();

    await expect(store.create('anything')).rejects.toThrow(/500/);
  });
});
