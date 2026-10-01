/**
 * The missing-key guard.
 *
 * Every key `src/` calls — through `t`, `$t`, or an `i18n-t` element's
 * `keypath` — must resolve in *every* shipped locale. A key that only exists in
 * one of them is the failure mode this catches: the UI looks translated until
 * someone switches language, and then it shows raw key names.
 *
 * The scan itself lives in `scan.ts`, shared with `dead-keys.test.ts`, which
 * checks the other direction: that every key a locale defines is called
 * somewhere. It reads the sources as text rather than through the module graph,
 * because a key used in a `.vue` template is not visible to the TypeScript
 * checker at all.
 */

import { describe, expect, it } from 'vitest';

import { i18n } from './index';
import { LOCALES } from './locales';
import { SOURCES, display, scanKeys } from './scan';

/**
 * This file is skipped: the fixtures below are written as `t('…')` calls, so
 * scanning it would report the scanner's own test data as missing keys.
 */
const SELF = 'missing-keys.test.ts';

describe('scanKeys', () => {
  it('reads both the t and the $t form, in either quote', () => {
    const source =
      '<p>{{ $t("common.nav.chat") }}</p>\n<span>{{ t(\'common.actions.retry\') }}</span>';
    expect(scanKeys(source)).toEqual([
      { key: 'common.nav.chat', line: 1 },
      { key: 'common.actions.retry', line: 2 },
    ]);
  });

  it('ignores calls that merely end in t(', () => {
    expect(scanKeys('format(x)\nexpect(y).toBe(z)\nentry.at(1)')).toEqual([]);
  });

  it('ignores a key that is built at runtime', () => {
    expect(scanKeys('t(`chat.${kind}.title`)')).toEqual([]);
  });

  it('reports the line the call is on', () => {
    expect(scanKeys("\n\n   t('common.actions.retry')")).toEqual([
      { key: 'common.actions.retry', line: 3 },
    ]);
  });
});

describe('every key used in src/', () => {
  const usages = Object.entries(SOURCES)
    .filter(([key]) => !key.endsWith(SELF))
    .flatMap(([file, source]) => scanKeys(source).map((found) => ({ file, ...found })));

  it('finds keys to check, so a passing run means something', () => {
    expect(usages.length).toBeGreaterThan(0);
  });

  it('resolves in every locale', () => {
    const missing = usages.flatMap(({ file, key, line }) =>
      LOCALES.filter((locale) => !i18n.global.te(key, locale.code)).map(
        (locale) => `${display(file)}:${line}  ${key}  [${locale.code}]`,
      ),
    );

    expect(missing, `keys that no locale defines:\n${missing.join('\n')}`).toEqual([]);
  });
});

describe('the discovered namespaces', () => {
  it('carries the common namespace in both locales', () => {
    const keys = [
      'common.appTitle',
      'common.nav.chat',
      'common.nav.agents',
      'common.nav.extensions',
      'common.actions.retry',
      'common.actions.close',
      'common.language.switch',
    ];

    for (const locale of LOCALES) {
      for (const key of keys) {
        expect(i18n.global.te(key, locale.code), `${key} [${locale.code}]`).toBe(true);
      }
    }
  });
});
