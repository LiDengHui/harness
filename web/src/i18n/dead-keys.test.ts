/**
 * The dead-key guard: the other half of `missing-keys.test.ts`.
 *
 * That guard fails when the UI calls a key no locale defines. This one fails the
 * opposite way — when a locale defines a key nothing names. A key with no caller
 * is a sentence that was translated twice, or a wording a refactor left behind;
 * either way it is a second owner for something that should have exactly one, so
 * the fix is to delete it rather than to leave it sitting there to be shadowed.
 *
 * A key built at runtime is not spelled out anywhere, so a template head stands
 * in for it: every key under `errors.transport.` counts as named, because the
 * transport composes its wording from the code it was handed.
 *
 * Test files are not scanned. A test names a key to assert *about* it, not to
 * render it; counting those would let a dead key hide behind its own assertion.
 */

import { describe, expect, it } from 'vitest';

import { messages } from './index';
import { LOCALES } from './locales';
import { SOURCES, namesKey, scanTemplateHeads } from './scan';

/** One key a locale file defines, and the namespace it belongs to. */
interface DefinedKey {
  locale: string;
  namespace: string;
  key: string;
}

/**
 * Every leaf of a message tree, as `namespace.path.to.leaf`.
 *
 * A leaf is a string; the objects above it are the namespaces and the groups
 * that make the files readable. `path[0]` is therefore always the namespace.
 */
function definedKeys(locale: string, tree: unknown): DefinedKey[] {
  const found: DefinedKey[] = [];
  const walk = (node: unknown, path: string[]): void => {
    if (typeof node === 'string') {
      found.push({ locale, namespace: path[0], key: path.join('.') });
      return;
    }
    if (typeof node !== 'object' || node === null) return;
    for (const [key, value] of Object.entries(node)) walk(value, [...path, key]);
  };
  walk(tree, []);
  return found;
}

/** The keys a locale file defines, by path, for the membership test below. */
const defined = Object.entries(messages).flatMap(([locale, tree]) => definedKeys(locale, tree));
const keySet = new Set(defined.map((entry) => entry.key));

/** The keys every non-test source names, and the template heads that stand for the rest. */
const references = (() => {
  const keys = new Set<string>();
  const heads: string[] = [];
  for (const [path, source] of Object.entries(SOURCES)) {
    if (path.endsWith('.test.ts')) continue;
    for (const key of keySet) {
      if (namesKey(source, key)) keys.add(key);
    }
    heads.push(...scanTemplateHeads(source));
  }
  return { keys, heads };
})();

function isNamed(key: string): boolean {
  return references.keys.has(key) || references.heads.some((head) => key.startsWith(head));
}

describe('the literal scan', () => {
  it('names a key held in a lookup table, not just a call', () => {
    const table = "const KEYS = { pending: 'views.entries.status.pending' };";
    expect(namesKey(table, 'views.entries.status.pending')).toBe(true);
  });

  it('names a key inside a double-quoted Vue attribute', () => {
    const attribute = ':title="t(\'chat.composer.askTitle\')"';
    expect(namesKey(attribute, 'chat.composer.askTitle')).toBe(true);
  });

  it('does not match a key that only starts the same way', () => {
    expect(namesKey("t('chat.tool.showFull')", 'chat.tool')).toBe(false);
  });

  it('reads the head of a composed key as a prefix', () => {
    expect(scanTemplateHeads('i18n.global.t(`errors.transport.${error.code}`)')).toEqual([
      'errors.transport.',
    ]);
  });

  it('does not read a plain literal as a prefix', () => {
    expect(scanTemplateHeads("t('errors.transport.openFailed')")).toEqual([]);
  });

  it('drops a head that opens straight into its interpolation', () => {
    expect(scanTemplateHeads('`${prefix}_${sequence}`')).toEqual([]);
  });
});

describe('every key a locale defines', () => {
  it('finds keys to check, so a passing run means something', () => {
    expect(defined.length).toBeGreaterThan(0);
    expect(references.keys.size).toBeGreaterThan(0);
  });

  it('is named by some source in src/', () => {
    const unused = defined.filter((entry) => !isNamed(entry.key));
    const report = unused.map((entry) => `[${entry.locale}] ${entry.namespace}  ${entry.key}`);

    expect(unused, `keys no source names:\n${report.join('\n')}`).toEqual([]);
  });

  it('is defined in every locale, so neither can drift', () => {
    const byLocale = LOCALES.map((locale) => ({
      locale: locale.code,
      keys: new Set(definedKeys(locale.code, messages[locale.code]).map((entry) => entry.key)),
    }));

    const [first, ...rest] = byLocale;
    for (const other of rest) {
      const onlyInFirst = [...first.keys].filter((key) => !other.keys.has(key));
      const onlyInOther = [...other.keys].filter((key) => !first.keys.has(key));
      const report = [
        ...onlyInFirst.map((key) => `[${first.locale}] ${key}`),
        ...onlyInOther.map((key) => `[${other.locale}] ${key}`),
      ];
      expect(report, 'keys that exist in only one locale').toEqual([]);
    }
  });
});
