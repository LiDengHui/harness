/**
 * The agent registry as the server sees it: what `harness` found while scanning
 * `agents/*.agent.md`.
 */

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import { getJson, requestFailureText } from '../api/http';
import type { AgentSpec } from '../protocol';

export const useAgentsStore = defineStore('agents', () => {
  const list = ref<AgentSpec[]>([]);
  const loading = ref(false);
  const loaded = ref(false);
  /** The failure behind `error`, kept whole so its wording follows the locale. */
  const loadFailure = ref<{ cause: unknown } | null>(null);

  const byId = computed(() => new Map(list.value.map((agent) => [agent.id, agent])));

  /** Composed on read, so a locale switch re-words what is already on screen. */
  const error = computed<string | null>(() =>
    loadFailure.value === null ? null : requestFailureText(loadFailure.value.cause),
  );

  function get(id: string): AgentSpec | null {
    return byId.value.get(id) ?? null;
  }

  async function load(force = false): Promise<void> {
    if (loaded.value && !force) return;
    loading.value = true;
    loadFailure.value = null;
    try {
      const agents = await getJson<AgentSpec[]>('/api/agents');
      list.value = Array.isArray(agents) ? agents : [];
      loaded.value = true;
    } catch (err) {
      loadFailure.value = { cause: err };
    } finally {
      loading.value = false;
    }
  }

  return { list, loading, loaded, error, byId, get, load };
});
