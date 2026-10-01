import { describe, expect, it } from 'vitest';

import { i18n } from '../i18n';
import {
  PLUGIN_CAPABILITIES,
  isPluginCapability,
  parsePluginInfo,
  pluginCapabilityLabel,
  type PluginInfo,
} from './api';

/**
 * The `plugins` array of a `GET /api/extensions` body, as
 * `crates/harness-server/src/api.rs` writes it for this repository.
 *
 * Copied from the values the server's own tests pin:
 * `capabilities_keep_the_spelling_the_manifest_uses` asserts that `file_snoop`
 * carries `["file_read"]` and module `plugins/file_snoop.wat`, and
 * `the_plugin_view_matches_the_manifests_in_this_repository` compares every
 * entry against `plugins/*.plugin.toml`. The capabilities therefore arrive in
 * the manifest's snake_case, not as the Rust enum's variant names.
 */
const PLUGINS: unknown[] = [
  { name: 'echo', version: '0.1.0', entry: 'run', capabilities: [], module: 'plugins/echo.wat' },
  {
    name: 'file_dump',
    version: '0.1.0',
    entry: 'run',
    capabilities: ['file_write'],
    module: 'plugins/file_dump.wat',
  },
  {
    name: 'file_snoop',
    version: '0.1.0',
    entry: 'run',
    capabilities: ['file_read'],
    module: 'plugins/file_snoop.wat',
  },
  {
    name: 'http_fetch',
    version: '0.1.0',
    entry: 'run',
    capabilities: ['network_access'],
    module: 'plugins/http_fetch.wat',
  },
  { name: 'logger', version: '0.1.0', entry: 'run', capabilities: [], module: 'plugins/logger.wat' },
  {
    name: 'memory_hog',
    version: '0.1.0',
    entry: 'run',
    capabilities: [],
    module: 'plugins/memory_hog.wat',
  },
  {
    name: 'path_escape',
    version: '0.1.0',
    entry: 'run',
    capabilities: ['file_read', 'file_write'],
    module: 'plugins/path_escape.wat',
  },
  { name: 'spin', version: '0.1.0', entry: 'run', capabilities: [], module: 'plugins/spin.wat' },
];

function parsed(): PluginInfo[] {
  const plugins: PluginInfo[] = [];
  for (const entry of PLUGINS) {
    const plugin = parsePluginInfo(entry);
    if (plugin !== null) plugins.push(plugin);
  }
  return plugins;
}

function plugin(name: string): PluginInfo {
  const found = parsed().find((candidate) => candidate.name === name);
  if (!found) throw new Error(`${name} is missing from the fixture`);
  return found;
}

describe('plugin capabilities', () => {
  it('narrows every plugin in a real response body', () => {
    expect(parsed()).toHaveLength(PLUGINS.length);
  });

  it('keeps the capability spellings the manifest uses', () => {
    expect(plugin('file_snoop').capabilities).toEqual(['file_read']);
    expect(plugin('path_escape').capabilities).toEqual(['file_read', 'file_write']);
    expect(plugin('http_fetch').capabilities).toEqual(['network_access']);
    expect(plugin('echo').capabilities).toEqual([]);
  });

  it('accepts the manifest spelling and refuses the Rust variant name', () => {
    for (const capability of PLUGIN_CAPABILITIES) {
      expect(isPluginCapability(capability)).toBe(true);
    }
    expect(isPluginCapability('FileRead')).toBe(false);
    expect(isPluginCapability('file_reed')).toBe(false);
    expect(isPluginCapability(42)).toBe(false);
  });

  it('hands the narrowed names to the label the view renders', () => {
    // `.map(pluginCapabilityLabel)` only type-checks because the parse narrowed
    // `capabilities` to the union; a `string[]` would be a compile error here.
    expect(plugin('path_escape').capabilities.map(pluginCapabilityLabel)).toEqual([
      i18n.global.t('errors.capability.file_read'),
      i18n.global.t('errors.capability.file_write'),
    ]);
  });

  it('labels every capability the server can send, in the active locale', () => {
    const labels = PLUGIN_CAPABILITIES.map(pluginCapabilityLabel);
    expect(labels).toEqual(
      PLUGIN_CAPABILITIES.map((capability) => i18n.global.t(`errors.capability.${capability}`)),
    );
    // A missing key resolves to the key itself, so this catches a gap.
    for (const label of labels) expect(label).not.toContain('errors.capability');
  });

  it('re-words a capability when the locale changes', () => {
    const initial = i18n.global.locale.value;
    try {
      i18n.global.locale.value = 'en';
      const english = pluginCapabilityLabel('file_read');
      i18n.global.locale.value = 'zh-CN';
      const chinese = pluginCapabilityLabel('file_read');

      expect(english).not.toBe(chinese);
      expect(english).not.toContain('errors.capability');
      expect(chinese).not.toContain('errors.capability');
    } finally {
      i18n.global.locale.value = initial;
    }
  });

  it('drops a capability the sandbox would refuse rather than the whole plugin', () => {
    const rogue = parsePluginInfo({
      name: 'typo',
      version: '1.0.0',
      entry: 'run',
      capabilities: ['file_reed', 'file_read'],
      module: 'plugins/typo.wat',
    });

    expect(rogue?.capabilities).toEqual(['file_read']);
  });

  it('refuses an entry that is not a plugin', () => {
    expect(parsePluginInfo(null)).toBeNull();
    expect(parsePluginInfo('echo')).toBeNull();
    expect(parsePluginInfo({ name: 'echo', version: '0.1.0' })).toBeNull();
    expect(parsePluginInfo({ name: 'echo', version: '0.1.0', entry: 'run' })).toBeNull();
  });
});
