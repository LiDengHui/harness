/**
 * Skills, MCP servers and WASM plugins — what the harness can reach for.
 *
 * `GET /api/extensions` is newer than this client, so the store degrades
 * instead of throwing: a missing route leaves `available` false and the view
 * explains itself. A panel that says "this build has no extensions endpoint" is
 * more useful than a red error on a page that is otherwise fine.
 */

import { defineStore } from 'pinia';
import { computed, ref } from 'vue';

import { ApiError, getJson, requestFailureText } from '../api/http';
import { i18n } from '../i18n';
import {
  parsePluginInfo,
  type ExtensionsResponse,
  type McpServerInfo,
  type PluginInfo,
  type SkillInfo,
} from '../protocol';

function asArray<T>(value: unknown): T[] {
  return Array.isArray(value) ? (value as T[]) : [];
}

/**
 * Reads the plugin list, which is the one part of the body that cannot be cast:
 * its `capabilities` are a union of the manifest's spellings, so they have to be
 * narrowed rather than asserted.
 */
function asPlugins(value: unknown): PluginInfo[] {
  if (!Array.isArray(value)) return [];
  const parsed: PluginInfo[] = [];
  for (const entry of value) {
    const plugin = parsePluginInfo(entry);
    if (plugin !== null) parsed.push(plugin);
  }
  return parsed;
}

export const useExtensionsStore = defineStore('extensions', () => {
  const skills = ref<SkillInfo[]>([]);
  const mcp = ref<McpServerInfo[]>([]);
  const plugins = ref<PluginInfo[]>([]);

  const loading = ref(false);
  const loaded = ref(false);
  /** False when the route is missing or unreachable; the view then explains why. */
  const available = ref(false);
  /** The failure behind `reason`, kept whole so its wording follows the locale. */
  const failure = ref<{ cause: unknown } | null>(null);

  /** Composed on read, so a locale switch re-words what is already on screen. */
  const reason = computed<string | null>(() =>
    failure.value === null ? null : describeFailure(failure.value.cause),
  );

  const skillsByName = computed(() => new Map(skills.value.map((skill) => [skill.name, skill])));
  const mcpByName = computed(() => new Map(mcp.value.map((server) => [server.name, server])));
  const pluginsByName = computed(() => new Map(plugins.value.map((plugin) => [plugin.name, plugin])));

  /** Total metadata cost across skills — what the model pays before loading any. */
  const metadataTokens = computed(() =>
    skills.value.reduce((total, skill) => total + (skill.metadata_tokens ?? 0), 0),
  );

  const bodyTokens = computed(() =>
    skills.value.reduce((total, skill) => total + (skill.body_tokens ?? 0), 0),
  );

  const isEmpty = computed(
    () => skills.value.length === 0 && mcp.value.length === 0 && plugins.value.length === 0,
  );

  async function load(force = false): Promise<void> {
    if (loaded.value && !force) return;
    loading.value = true;
    failure.value = null;
    try {
      const body = await getJson<ExtensionsResponse>('/api/extensions');
      skills.value = asArray<SkillInfo>(body.skills);
      mcp.value = asArray<McpServerInfo>(body.mcp);
      plugins.value = asPlugins(body.plugins);
      available.value = true;
    } catch (err) {
      skills.value = [];
      mcp.value = [];
      plugins.value = [];
      available.value = false;
      failure.value = { cause: err };
    } finally {
      loading.value = false;
      loaded.value = true;
    }
  }

  return {
    skills,
    mcp,
    plugins,
    loading,
    loaded,
    available,
    reason,
    skillsByName,
    mcpByName,
    pluginsByName,
    metadataTokens,
    bodyTokens,
    isEmpty,
    load,
  };
});

function describeFailure(err: unknown): string {
  if (err instanceof ApiError && err.isMissing) {
    return i18n.global.t('errors.api.extensionsUnavailable');
  }
  return requestFailureText(err);
}
