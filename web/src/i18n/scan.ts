/**
 * The key scanner the two i18n guards share.
 *
 * The scan reads the sources as text rather than through the module graph,
 * because a key used in a `.vue` template is not visible to the TypeScript
 * checker at all. It reads two different things, because the guards ask two
 * different questions:
 *
 *   * `scanKeys` — the keys a source *calls*, for the guard that every call
 *     resolves. It understands the `t`/`$t` call form and the `keypath`
 *     attribute an `i18n-t` element takes.
 *   * `namesKey` / `scanTemplateHeads` — the keys a source *names*, for the guard
 *     that every defined key has a caller. A key is often held in a table and
 *     handed to `t` later, so looking only for call sites would report that key
 *     as dead.
 */

/**
 * A `t`/`$t` call with a quoted key, preceded by anything that is not a word
 * character or a `$`, so `format(` and `.test(` are not mistaken for calls.
 * A backtick key is only read when it holds no `${…}`: a key built at runtime is
 * not spelled out here, and `scanTemplateHeads` collects its prefix instead.
 */
const KEY_CALL = /(?:^|[^\w$])\$?t\(\s*(['"`])([^'"`$]+)\1/g;

/** An `i18n-t` element's `keypath` attribute — the markup form of a call. */
const KEYPATH_ATTRIBUTE = /\bkeypath\s*=\s*(["'])([^"'$]+)\1/g;

/** The literal head of a template literal, up to its first interpolation. */
const TEMPLATE_HEAD = /`([^`\n]*?)\$\{/g;

export interface FoundKey {
  key: string;
  line: number;
}

/** The line a match ends on, so a call on its own line is not blamed on the one above. */
function endLine(source: string, match: RegExpMatchArray): number {
  const end = (match.index ?? 0) + match[0].length;
  return source.slice(0, end).split('\n').length;
}

/** Every statically spelled key a source calls, with the line it sits on. */
export function scanKeys(source: string): FoundKey[] {
  const found: FoundKey[] = [];
  for (const match of source.matchAll(KEY_CALL)) {
    const key = match[2];
    if (key.trim() !== '') found.push({ key, line: endLine(source, match) });
  }
  for (const match of source.matchAll(KEYPATH_ATTRIBUTE)) {
    const key = match[2];
    if (key.trim() !== '') found.push({ key, line: endLine(source, match) });
  }
  return found;
}

/**
 * True when a source spells a key out as a complete quoted literal.
 *
 * Searching for the quoted form rather than tokenising the source is what keeps
 * this honest: a Vue attribute that wraps a `t` call in double quotes is not
 * itself a JavaScript string, and a lexer would read the whole attribute as one
 * string, the key inside it included. The closing quote is part of the needle,
 * so a key that merely starts with another key's name does not match.
 */
export function namesKey(source: string, key: string): boolean {
  return (
    source.includes(`'${key}'`) ||
    source.includes(`"${key}"`) ||
    source.includes('`' + key + '`')
  );
}

/**
 * The literal head of every template literal a source holds.
 *
 * A head is a prefix, not a key: the transport composes its error wording from
 * the code it was handed, so every key under that literal head is reachable. A
 * head that is empty — the literal opens straight into its interpolation —
 * carries nothing to match on and is dropped rather than left to stand for
 * every key at once.
 */
export function scanTemplateHeads(source: string): string[] {
  return [...source.matchAll(TEMPLATE_HEAD)]
    .map((match) => match[1])
    .filter((head) => head !== '');
}

/** Every `.ts` and `.vue` under `src/`, keyed by path relative to `src/i18n/`. */
export const SOURCES = import.meta.glob('../**/*.{ts,vue}', {
  eager: true,
  query: '?raw',
  import: 'default',
}) as Record<string, string>;

/** Glob keys read as `../App.vue`; the report spells them `src/App.vue`. */
export function display(key: string): string {
  return `src/${key.replace(/^\.\.\//, '')}`;
}
