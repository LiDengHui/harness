/**
 * The workflow registry as the server sees it: what it found while reading
 * `workflows/*.workflow.md`.
 *
 * A workflow is a named plan recipe: it says *when* it applies and *what* steps
 * it runs. The `when` is the whole reason the picker can be used at all — a
 * name alone does not tell a reader which one their task needs — so it is read
 * and shown beside the name rather than kept for a tooltip.
 *
 * Nothing here is required for the rest of the app to work. The route may not
 * exist yet on a given server, so every read degrades to an empty list plus a
 * failure the view can word, and the composer keeps its automatic mode.
 */

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import { getJson, postJson, requestFailureText } from '../api/http';

/** One workflow, as `GET /api/workflows` reports it. */
export interface WorkflowInfo {
  id: string;
  name: string;
  /** When this workflow applies — what a reader chooses between. */
  when: string;
  source_path: string | null;
}

/**
 * Reads one workflow entry, or `null` when it is not one.
 *
 * Only the id is required: a name the server left off falls back to the id, and
 * an absent `when` is read as empty rather than invented, so a half-filled entry
 * still lists and can still be chosen.
 */
export function parseWorkflow(value: unknown): WorkflowInfo | null {
  if (typeof value !== 'object' || value === null) return null;
  const record = value as Record<string, unknown>;

  const id = record['id'];
  if (typeof id !== 'string' || id.trim() === '') return null;

  const name = record['name'];
  const when = record['when'];
  const sourcePath = record['source_path'];
  return {
    id,
    name: typeof name === 'string' && name.trim() !== '' ? name : id,
    when: typeof when === 'string' ? when : '',
    source_path: typeof sourcePath === 'string' ? sourcePath : null,
  };
}

/**
 * Reads the list route's body, or `null` when it is not a list.
 *
 * A bare array and a `{ workflows: [...] }` wrapper are both accepted: the
 * session route returns a bare array while newer routes on this server wrap
 * their payload, and guessing which one this route chose would break the picker
 * over a shape that carries no information either way.
 */
export function parseWorkflowList(body: unknown): WorkflowInfo[] | null {
  const entries = Array.isArray(body)
    ? body
    : typeof body === 'object' && body !== null && Array.isArray((body as { workflows?: unknown }).workflows)
      ? ((body as { workflows: unknown[] }).workflows)
      : null;
  if (entries === null) return null;
  return entries.map(parseWorkflow).filter((entry): entry is WorkflowInfo => entry !== null);
}

export const useWorkflowsStore = defineStore('workflows', () => {
  const list = ref<WorkflowInfo[]>([]);
  const loading = ref(false);
  const loaded = ref(false);
  /** The failure behind `error`, kept whole so its wording follows the locale. */
  const listFailure = ref<{ cause: unknown } | null>(null);

  /** Composed on read, so a locale switch re-words what is already on screen. */
  const error = computed<string | null>(() =>
    listFailure.value === null ? null : requestFailureText(listFailure.value.cause),
  );

  /**
   * Reads the workflow list.
   *
   * A route that is not there yet is not a failure the user caused: the picker
   * then offers automatic selection alone and says why, rather than showing an
   * error where a choice should be.
   */
  async function load(force = false): Promise<void> {
    if (loaded.value && !force) return;
    loading.value = true;
    listFailure.value = null;
    try {
      const body = await getJson<unknown>('/api/workflows');
      const parsed = parseWorkflowList(body);
      if (parsed === null) {
        list.value = [];
        listFailure.value = { cause: new TypeError('the workflow list was not a list') };
      } else {
        list.value = parsed;
        loaded.value = true;
      }
    } catch (err) {
      listFailure.value = { cause: err };
    } finally {
      loading.value = false;
    }
  }

  /**
   * Asks the server to turn a description into a saved workflow.
   *
   * The description is the input; the server writes the file and answers with
   * the workflow it made, which is what the caller selects. A body that cannot
   * be read as a workflow is a failure rather than a silent no-op: selecting a
   * workflow that does not exist would send a job the server cannot match.
   *
   * The list is then re-read instead of only patched locally, so what the
   * picker shows is the server's own answer and a page reload cannot show a
   * different set. The server is expected to fold the new workflow into its
   * registry as it saves; until that hook exists the re-read can come back
   * without it, so the created entry is merged in when it is missing — a
   * workflow the reader just made must not vanish from the picker.
   */
  async function create(description: string): Promise<WorkflowInfo> {
    const body = await postJson<unknown>('/api/workflows', { description });
    const wrapped =
      typeof body === 'object' && body !== null
        ? (body as { workflow?: unknown }).workflow
        : undefined;
    const created = parseWorkflow(wrapped ?? body);
    if (created === null) {
      throw new TypeError('the server did not return the workflow it created');
    }

    await load(true);
    if (!list.value.some((workflow) => workflow.id === created.id)) {
      list.value = [...list.value, created];
    }
    return created;
  }

  return { list, loading, loaded, error, load, create };
});
