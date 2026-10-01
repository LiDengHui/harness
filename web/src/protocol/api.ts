/**
 * The REST shapes, mirroring the JSON the server actually writes.
 *
 * These come from two places: `crates/harness-server/src/api.rs` for what the
 * routes hand out, and `harness-core`'s `SessionInfo` / `MemoryStats` for the
 * payloads that are serialized straight from a domain type.
 */

import { i18n } from '../i18n';

/** One element of `GET /api/agents`. */
export interface AgentSpec {
  id: string;
  name: string;
  description: string;
  /** `"provider/model"`, or `null` to inherit the router default. */
  model: string | null;
  /** Tool whitelist; empty means "every registered tool". */
  tools: string[];
  skills: string[];
  /** Hard ceiling on tokens per run; `null` (and `0`) mean unbounded. */
  max_tokens: number | null;
  source_path: string;
}

/** `GET /api/health`. */
export interface HealthResponse {
  status: string;
  protocol: number;
  version: string;
}

/** One element of `GET /api/sessions`, from `harness_core::memory::SessionInfo`. */
export interface SessionInfo {
  id: string;
  label: string | null;
  /** RFC 3339, as `chrono::DateTime<Utc>` serializes. */
  created_at: string;
  /** Node rows this session owns; a forked session shares its ancestors. */
  nodes: number;
  /** Estimated tokens across the rows this session owns. */
  tokens: number;
  head: string | null;
  forked_from: string | null;
  /**
   * The session this one was forked from, as a session id, or `null` for a
   * conversation that stands on its own.
   *
   * Optional because the field is still rolling out: a server that does not send
   * it is read as "no session has a parent", which draws a flat rail rather than
   * a broken one. The distinction this field carries is the rail's whole rule —
   * a sub-agent's run is stored as a session forked from the one that delegated
   * it, so a session with a parent is that sub-agent's conversation, not a main
   * one. See `components/rail/tree.ts`.
   */
  forked_from_session?: string | null;
  /**
   * The session's own words: a preview of its first user message, or `null`
   * until the user has said something. Optional for the same reason as
   * `forked_from_session` — it is newer than the route.
   */
  title?: string | null;
}

/** `GET /api/memory/stats`. */
export interface MemoryStats {
  sessions: number;
  nodes: number;
  blobs: number;
  blob_bytes: number;
  total_tokens: number;
}

/** One skill, as advertised by `GET /api/extensions`. */
export interface SkillInfo {
  name: string;
  description: string;
  model: string | null;
  allowed_tools: string[];
  /** What the model pays to *see* the skill. */
  metadata_tokens: number;
  /** What it pays to actually load the body — the point of progressive disclosure. */
  body_tokens: number;
  source_path: string;
}

/** One MCP server. */
export interface McpServerInfo {
  name: string;
  kind: 'stdio' | 'http';
  enabled: boolean;
  /** `true` when the server is contacted on first use rather than at startup. */
  lazy: boolean;
  /** Command line or URL, depending on `kind`. */
  target: string;
  /** Tools advertised under the `mcp__<name>__<tool>` namespace. */
  tools: string[];
}

/**
 * The capability names a plugin manifest may declare, spelled the way the
 * manifest spells them.
 *
 * `GET /api/extensions` copies the manifest's own TOML strings into the response
 * (`harness_sandbox::Capability` serializes to exactly these), so the wire values
 * are `file_read`, not `FileRead`. The union is what stops a client from
 * inventing a spelling the server never sends.
 */
export const PLUGIN_CAPABILITIES = [
  'file_read',
  'file_write',
  'network_access',
  'shell_exec',
] as const;

export type PluginCapability = (typeof PLUGIN_CAPABILITIES)[number];

/** True when `value` is one of the capability names the server sends. */
export function isPluginCapability(value: unknown): value is PluginCapability {
  return typeof value === 'string' && (PLUGIN_CAPABILITIES as readonly string[]).includes(value);
}

/**
 * How a capability reads in the UI: `file_read` becomes `File read`.
 *
 * The manifest's spelling is the wire value, so the label is a lookup rather
 * than a transformation: the wording for each name lives in the locale files.
 */
export function pluginCapabilityLabel(capability: PluginCapability): string {
  return i18n.global.t(`errors.capability.${capability}`);
}

/** One WASM plugin. */
export interface PluginInfo {
  name: string;
  version: string;
  /** Exported entry point, e.g. `run`. */
  entry: string;
  /** What the module is allowed to touch — the security surface. */
  capabilities: PluginCapability[];
  module: string;
}

/**
 * Reads one plugin entry, narrowing its capabilities to the known names.
 *
 * A name outside the union is dropped rather than carried through as an
 * arbitrary string. The sandbox refuses such a manifest when it loads the plugin
 * (its `Capability` enum does not deserialize `file_reed`), so a server that
 * lists one is already reporting a manifest the host will not run; dropping the
 * name keeps the rest of the plugin visible, where rejecting the entry would
 * hide a plugin that is otherwise fine.
 */
export function parsePluginInfo(value: unknown): PluginInfo | null {
  if (typeof value !== 'object' || value === null) return null;
  const record = value as Record<string, unknown>;

  const name = record['name'];
  const version = record['version'];
  const entry = record['entry'];
  const module = record['module'];
  if (
    typeof name !== 'string' ||
    typeof version !== 'string' ||
    typeof entry !== 'string' ||
    typeof module !== 'string'
  ) {
    return null;
  }

  const capabilities: PluginCapability[] = [];
  const declared = record['capabilities'];
  if (Array.isArray(declared)) {
    for (const candidate of declared) {
      if (isPluginCapability(candidate)) capabilities.push(candidate);
    }
  }

  return { name, version, entry, capabilities, module };
}

/** `GET /api/extensions`. */
export interface ExtensionsResponse {
  skills: SkillInfo[];
  mcp: McpServerInfo[];
  plugins: PluginInfo[];
}
