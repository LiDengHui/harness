import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { i18n } from './index';
import { DEFAULT_LOCALE, LOCALES } from './locales';
import {
  LOCALE_STORAGE_KEY,
  browserEnvironment,
  initLocale,
  resolveLocale,
  serverLanguage,
  setLocale,
  useLocale,
  type LocaleEnvironment,
} from './useLocale';

/** A stored choice, a server answer and a `<html>` element, none of them real. */
function fakeEnvironment(options: { stored?: string; server?: string | Error } = {}) {
  const entries = new Map<string, string>();
  if (options.stored !== undefined) entries.set(LOCALE_STORAGE_KEY, options.stored);

  const html = { lang: '' };
  let probes = 0;

  const env: LocaleEnvironment = {
    storage: {
      getItem: (key) => entries.get(key) ?? null,
      setItem: (key, value) => {
        entries.set(key, value);
      },
    },
    html,
    serverLanguage: async () => {
      probes += 1;
      if (options.server instanceof Error) throw options.server;
      return options.server ?? null;
    },
  };

  return { env, entries, html, probes: () => probes };
}

/** The shape `getJson` reads: `ok`, `status` and `json()`. */
function response(status: number, body: unknown): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as unknown as Response;
}

beforeEach(() => {
  i18n.global.locale.value = DEFAULT_LOCALE;
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('resolveLocale', () => {
  it('prefers a stored choice to the server language', () => {
    expect(resolveLocale('en', 'zh-CN')).toBe('en');
  });

  it('uses the server language when nothing is stored', () => {
    expect(resolveLocale(null, 'en')).toBe('en');
  });

  it('falls back to the default when neither source is usable', () => {
    expect(resolveLocale(null, null)).toBe(DEFAULT_LOCALE);
    expect(resolveLocale(undefined, undefined)).toBe(DEFAULT_LOCALE);
    expect(resolveLocale('fr', 'de')).toBe(DEFAULT_LOCALE);
  });
});

describe('initLocale', () => {
  it('applies a stored choice without consulting the server', async () => {
    const { env, html, probes } = fakeEnvironment({ stored: 'en', server: 'zh-CN' });

    await expect(initLocale(env)).resolves.toBe('en');
    expect(i18n.global.locale.value).toBe('en');
    expect(html.lang).toBe('en');
    expect(probes()).toBe(0);
  });

  it('takes the server language when nothing is stored', async () => {
    const { env, html } = fakeEnvironment({ server: 'en' });

    await expect(initLocale(env)).resolves.toBe('en');
    expect(i18n.global.locale.value).toBe('en');
    expect(html.lang).toBe('en');
  });

  it('uses the default when the server has no language to give', async () => {
    const { env, html } = fakeEnvironment({ server: undefined });

    await expect(initLocale(env)).resolves.toBe(DEFAULT_LOCALE);
    expect(i18n.global.locale.value).toBe(DEFAULT_LOCALE);
    expect(html.lang).toBe(DEFAULT_LOCALE);
  });

  it('falls through to the default when the server call fails', async () => {
    const { env, html } = fakeEnvironment({ server: new Error('offline') });

    await expect(initLocale(env)).resolves.toBe(DEFAULT_LOCALE);
    expect(html.lang).toBe(DEFAULT_LOCALE);
  });

  it('ignores a stored value that is not a shipped locale', async () => {
    const { env, html } = fakeEnvironment({ stored: 'fr', server: 'en' });

    await expect(initLocale(env)).resolves.toBe('en');
    expect(html.lang).toBe('en');
  });

  it('boots without a DOM, leaving the default in place', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response(404, { error: 'no such route' })));
    const env = browserEnvironment();
    expect(env.storage).toBeNull();
    expect(env.html).toBeNull();

    await expect(initLocale(env)).resolves.toBe(DEFAULT_LOCALE);
    expect(i18n.global.locale.value).toBe(DEFAULT_LOCALE);
  });
});

describe('setLocale', () => {
  it('persists the choice, switches the messages and relabels <html>', () => {
    const { env, entries, html } = fakeEnvironment({ stored: DEFAULT_LOCALE });

    setLocale('en', env);

    expect(entries.get(LOCALE_STORAGE_KEY)).toBe('en');
    expect(i18n.global.locale.value).toBe('en');
    expect(html.lang).toBe('en');
    expect(i18n.global.t('common.nav.chat')).toBe('Chat');
  });

  it('is what the switcher sees through useLocale', () => {
    const { locale, setLocale: switchTo } = useLocale();

    expect(locale.value).toBe(DEFAULT_LOCALE);
    switchTo('en');
    expect(locale.value).toBe('en');
    expect(i18n.global.locale.value).toBe('en');
    expect(i18n.global.t('common.nav.chat')).toBe('Chat');
  });
});

describe('serverLanguage', () => {
  it('reads the language field', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response(200, { language: 'en' })));
    await expect(serverLanguage()).resolves.toBe('en');
  });

  it('treats a 404 as no opinion', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response(404, { error: 'not found' })));
    await expect(serverLanguage()).resolves.toBeNull();
  });

  it('treats an unreachable server as no opinion', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('fetch failed');
      }),
    );
    await expect(serverLanguage()).resolves.toBeNull();
  });

  it('ignores a field that is not a string', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => response(200, { language: 7 })));
    await expect(serverLanguage()).resolves.toBeNull();
  });
});

describe('the shipped locales', () => {
  it('covers every locale in the switcher', () => {
    expect(LOCALES.map((locale) => locale.code)).toEqual(['zh-CN', 'en']);
    expect(LOCALES.map((locale) => locale.endonym)).toEqual(['中文', 'English']);
  });
});
