/**
 * Picking, applying and persisting the UI language.
 *
 * The active locale has three possible sources, tried in this order:
 *
 * 1. the choice this browser stored under `harness.locale`;
 * 2. `GET /api/ui`'s `language` field, which is how a server operator sets the
 *    default for everyone;
 * 3. {@link DEFAULT_LOCALE}.
 *
 * The server route is optional — an older server answers 404 and an unreachable
 * one throws — so both are read as "no opinion" rather than as failures.
 *
 * The boot path touches three globals (`localStorage`, `document`, `fetch`),
 * all of which are injected through {@link LocaleEnvironment} so the resolution
 * order can be driven in a test without a DOM.
 */

import { computed, type ComputedRef } from 'vue';

import { getJson } from '../api/http';
import { i18n } from './index';
import { DEFAULT_LOCALE, isLocale, type Locale } from './locales';

/** Where an explicit choice is remembered between visits. */
export const LOCALE_STORAGE_KEY = 'harness.locale';

/** The route that advertises the server's own language, if it has one. */
const UI_ROUTE = '/api/ui';

/** The slice of `localStorage` this module uses. */
export interface LocaleStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

/** The globals the boot path touches, injectable so it runs headless. */
export interface LocaleEnvironment {
  storage: LocaleStorage | null;
  /** The element whose `lang` attribute mirrors the locale, normally `<html>`. */
  html: { lang: string } | null;
  /** Resolves the server's language, or `null` when it has none to give. */
  serverLanguage: () => Promise<string | null>;
}

/**
 * `GET /api/ui`'s `language`, or `null`.
 *
 * A 404, an unreachable server and a response without a usable field are all
 * the same answer here: the server has no opinion, and the default stands.
 */
export async function serverLanguage(): Promise<string | null> {
  try {
    const info = await getJson<unknown>(UI_ROUTE);
    if (typeof info !== 'object' || info === null) return null;
    const language = (info as Record<string, unknown>)['language'];
    return typeof language === 'string' ? language : null;
  } catch {
    return null;
  }
}

/** The real globals, resolved lazily so importing this module never throws. */
export function browserEnvironment(): LocaleEnvironment {
  return {
    storage: typeof localStorage === 'undefined' ? null : localStorage,
    html: typeof document === 'undefined' ? null : document.documentElement,
    serverLanguage,
  };
}

/**
 * The resolution order, as a pure function: a stored choice wins, the server's
 * language is the fallback, and anything unrecognised is ignored.
 */
export function resolveLocale(
  stored: string | null | undefined,
  server: string | null | undefined,
): Locale {
  if (isLocale(stored)) return stored;
  if (isLocale(server)) return server;
  return DEFAULT_LOCALE;
}

/** Points vue-i18n and `<html lang>` at `locale`. */
function apply(locale: Locale, env: LocaleEnvironment): void {
  i18n.global.locale.value = locale;
  if (env.html !== null) env.html.lang = locale;
}

/**
 * Applies the initial locale and returns it.
 *
 * A stored choice is applied synchronously, before the first await, so a
 * returning visitor never sees the default flash past; only the server probe
 * settles later. Called once from `main.ts`.
 */
export async function initLocale(env: LocaleEnvironment = browserEnvironment()): Promise<Locale> {
  const stored = env.storage?.getItem(LOCALE_STORAGE_KEY) ?? null;
  if (isLocale(stored)) {
    apply(stored, env);
    return stored;
  }

  let server: string | null = null;
  try {
    server = await env.serverLanguage();
  } catch {
    // A probe that rejects says no more than a 404 does.
    server = null;
  }

  const locale = resolveLocale(stored, server);
  apply(locale, env);
  return locale;
}

/** Records `locale` as this browser's choice, and switches to it. */
export function setLocale(locale: Locale, env: LocaleEnvironment = browserEnvironment()): void {
  env.storage?.setItem(LOCALE_STORAGE_KEY, locale);
  apply(locale, env);
}

/** The active locale, and the setter that persists a change to it. */
export function useLocale(): {
  locale: ComputedRef<Locale>;
  setLocale: (locale: Locale) => void;
} {
  return {
    locale: computed(() => {
      const active = i18n.global.locale.value;
      return isLocale(active) ? active : DEFAULT_LOCALE;
    }),
    setLocale,
  };
}
