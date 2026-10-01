/**
 * The i18n instance.
 *
 * Messages are *discovered*, not imported one by one: every
 * `messages/<locale>/<namespace>.ts` is pulled in by a build-time glob and its
 * default export is merged under the directory's locale. Dropping a new
 * namespace file next to `common.ts` is therefore the whole of adding one — no
 * edit here, and no registration list to forget.
 *
 * A namespace file's default export is keyed by namespace:
 *
 * ```ts
 * export default { chat: { placeholder: '…' } };
 * ```
 */

import { createI18n, type LocaleMessageValue } from 'vue-i18n';

import { DEFAULT_LOCALE, FALLBACK_LOCALES } from './locales';

/** One namespace file's default export, e.g. `{ chat: { … } }`. */
type MessageNamespace = Record<string, LocaleMessageValue>;

/** Everything the app can translate: locale → namespace → keys. */
type MessagesByLocale = Record<string, MessageNamespace>;

const files = import.meta.glob('./messages/*/*.ts', { eager: true }) as Record<
  string,
  { default?: MessageNamespace }
>;

const LOCALE_DIRECTORY = /^\.\/messages\/([^/]+)\/[^/]+\.ts$/;

function isNamespace(value: unknown): value is MessageNamespace {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Merges every discovered file into one tree, by locale directory name.
 *
 * Namespaces merge one level deep rather than being replaced, so two files that
 * happen to declare the same namespace add to each other instead of one
 * silently winning on module order.
 */
function collect(files: Record<string, { default?: MessageNamespace }>): MessagesByLocale {
  const messages: MessagesByLocale = {};

  for (const [path, module] of Object.entries(files)) {
    const locale = LOCALE_DIRECTORY.exec(path)?.[1];
    if (locale === undefined || !isNamespace(module.default)) continue;

    const namespaces = (messages[locale] ??= {});
    for (const [namespace, value] of Object.entries(module.default)) {
      const existing = namespaces[namespace];
      namespaces[namespace] =
        isNamespace(existing) && isNamespace(value) ? { ...existing, ...value } : value;
    }
  }

  return messages;
}

/**
 * The merged message tree, and the value `i18n` is built from. Exported because
 * tests read raw keys out of it — the guard test included.
 */
export const messages: MessagesByLocale = collect(files);

/**
 * Composition API only (`legacy: false`): every component reads keys through
 * `useI18n()`, and the active locale is a ref the composables can watch.
 */
export const i18n = createI18n({
  legacy: false,
  locale: DEFAULT_LOCALE,
  fallbackLocale: [...FALLBACK_LOCALES],
  messages,
});
