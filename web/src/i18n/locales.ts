/**
 * The locales the UI ships, and how one is picked.
 *
 * `zh-CN` is the default: the product is written for a Chinese-speaking
 * audience first, with English as the second locale rather than the base one.
 * The codes double as the BCP 47 tags written to `<html lang>` and as the
 * directory names under `messages/`, so shipping another locale means adding a
 * directory of namespace files and one entry here.
 */

/** One shipped locale: its code, and the name it calls itself. */
export interface LocaleInfo {
  /** BCP 47 tag, e.g. `zh-CN`. */
  code: string;
  /** The language's own name for itself — never a translation of it. */
  endonym: string;
}

export const LOCALES = [
  { code: 'zh-CN', endonym: '中文' },
  { code: 'en', endonym: 'English' },
] as const satisfies readonly LocaleInfo[];

/** Every code the UI can be in. */
export type Locale = (typeof LOCALES)[number]['code'];

/** Where the UI starts when nothing else says otherwise. */
export const DEFAULT_LOCALE: Locale = 'zh-CN';

/**
 * The order vue-i18n walks when a key is absent from the active locale.
 *
 * This is a safety net, not a strategy: `missing-keys.test.ts` fails on any key
 * that does not resolve in *every* locale, so a key reaching this list is
 * already a bug the test would have caught.
 */
export const FALLBACK_LOCALES: readonly Locale[] = ['zh-CN', 'en'];

/** True when `value` is a locale this UI ships messages for. */
export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && LOCALES.some((locale) => locale.code === value);
}

/** How `locale` names itself, for the switcher. */
export function endonym(locale: Locale): string {
  return LOCALES.find((candidate) => candidate.code === locale)?.endonym ?? locale;
}
