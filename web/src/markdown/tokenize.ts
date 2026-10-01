/**
 * A pure Markdown tokenizer for the transcript.
 *
 * It is deliberately a subset — headings, fences, inline code, emphasis,
 * strikethrough, links, images, inline and display math, lists (nested),
 * blockquotes, rules, pipe tables and paragraphs — because that is what the
 * model's answers use. The point of having it here rather than a library is the
 * second half of the architecture: this module returns *data*, so
 * `MarkdownView.vue` can build text nodes and elements directly and never touch
 * `v-html`. Tool output is arbitrary text (file contents, fetched pages), so
 * structural escaping is the security boundary, not an optional nicety.
 *
 * A math token carries TeX, which the renderer has to hand to KaTeX; that
 * library returns HTML, and it is the single exception to the rule above. The
 * exception is contained in `math.ts`, which turns KaTeX's trust extensions
 * off, and this module still never produces markup — it only records the TeX.
 *
 * No Vue, no DOM, no imports.
 */

import type {
  BlockToken,
  HeadingLevel,
  InlineToken,
  ListItem,
  TableAlign,
} from './types';

/** The only schemes a link may carry; everything else stays literal text. */
const SAFE_SCHEMES = ['http', 'https', 'mailto'];

/**
 * The only schemes an image may carry.
 *
 * Narrower than a link's list on purpose: `mailto:` is a sensible link and a
 * nonsense image, and fetching an image is the more dangerous of the two, so it
 * gets the short list — `http:` and `https:` only.
 */
const IMAGE_SCHEMES = ['http', 'https'];

/** Turn a source string into a block token list. */
export function tokenize(source: string): BlockToken[] {
  return parseBlocks(splitLines(source));
}

/**
 * Whether a link target is allowed to become an anchor.
 *
 * A scheme is required: `http:`, `https:` or `mailto:` (case-insensitively),
 * and nothing else. A relative path or a fragment therefore stays text too,
 * which is the contract this app asked for. The scheme is read with a strict
 * pattern, so `java\tscript:` — which a browser would fold to `javascript:` —
 * never matches and is treated as unsafe.
 */
export function isSafeHref(href: string): boolean {
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(href.trim());
  if (match === null) return false;
  return SAFE_SCHEMES.includes(match[1].toLowerCase());
}

/**
 * Whether an image target is allowed to become an `<img>`.
 *
 * The same strict scheme read as `isSafeHref`, against the shorter image list,
 * so `data:`, `javascript:` and every relative path stay literal text.
 */
export function isSafeImageSrc(src: string): boolean {
  const match = /^([a-zA-Z][a-zA-Z0-9+.-]*):/.exec(src.trim());
  if (match === null) return false;
  return IMAGE_SCHEMES.includes(match[1].toLowerCase());
}

/* ------------------------------------------------------------------ lines */

function splitLines(source: string): string[] {
  const normalized = source.replace(/\r\n?/g, '\n');
  const lines = normalized.split('\n');
  if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();
  return lines;
}

function isBlank(line: string): boolean {
  return line.trim() === '';
}

function leadingSpaces(line: string): number {
  return /^( *)/.exec(line)![1].length;
}

function skipBlanks(lines: string[], index: number): number {
  let i = index;
  while (i < lines.length && isBlank(lines[i])) i++;
  return i;
}

/** Remove up to `width` leading spaces, keeping any indentation beyond it. */
function dedent(line: string, width: number): string {
  return line.slice(Math.min(leadingSpaces(line), width));
}

/* ---------------------------------------------------------------- matchers */

function matchFence(line: string): { char: string; length: number; info: string } | null {
  const match = /^( {0,3})(`{3,}|~{3,})(.*)$/.exec(line);
  if (match === null) return null;
  const info = match[3].trim();
  // A backtick fence's info string may not itself contain a backtick.
  if (match[2][0] === '`' && info.includes('`')) return null;
  return { char: match[2][0], length: match[2].length, info };
}

function matchHeading(line: string): { level: HeadingLevel; text: string } | null {
  const match = /^( {0,3})(#{1,6})(?:\s+(.*?))?\s*$/.exec(line);
  if (match === null) return null;
  const text = (match[3] ?? '').replace(/\s+#+\s*$/, '').trim();
  return { level: match[2].length as HeadingLevel, text };
}

function isRule(line: string): boolean {
  const trimmed = line.trim();
  if (trimmed.length < 3) return false;
  return /^(\*|-|_)(\s*\1){2,}$/.test(trimmed);
}

function isBlockquoteLine(line: string): boolean {
  return /^ {0,3}>/.test(line);
}

/** The text after a line's leading `$$`, or `null` when the line is not one. */
function matchDisplayMathStart(line: string): string | null {
  const match = /^ {0,3}\$\$(.*)$/.exec(line);
  return match === null ? null : match[1];
}

/**
 * A `$$…$$` block: either `$$x$$` on one line, or an opening `$$` with the
 * closer on a later line.
 *
 * An opening `$$` that never closes is not a block, so the line falls back to
 * a paragraph and stays literal — the same "no closing delimiter, no
 * construct" rule the inline `$…$` case follows. Nothing here runs inside a
 * fence, because the fence is matched before this is ever reached.
 */
function parseDisplayMath(lines: string[], start: number): Parsed | null {
  const rest = matchDisplayMathStart(lines[start]);
  if (rest === null) return null;

  const sameLine = rest.indexOf('$$');
  if (sameLine !== -1) {
    const value = rest.slice(0, sameLine).trim();
    if (value === '') return null;
    return { token: { type: 'math_display', value }, next: start + 1 };
  }

  const body = [rest];
  let i = start + 1;
  while (i < lines.length) {
    const close = lines[i].indexOf('$$');
    if (close !== -1) {
      body.push(lines[i].slice(0, close));
      const value = body.join('\n').trim();
      if (value === '') return null;
      return { token: { type: 'math_display', value }, next: i + 1 };
    }
    body.push(lines[i]);
    i++;
  }
  return null;
}

interface ImageMatch {
  alt: string;
  src: string;
  raw: string;
  safe: boolean;
  next: number;
}

/**
 * `![alt](src)`, mirroring `matchLink`.
 *
 * The one difference is the label: it is kept as a plain string, because an
 * image's alt text is text and can never be markup.
 */
function matchImage(text: string, start: number): ImageMatch | null {
  if (text[start] !== '!' || text[start + 1] !== '[') return null;
  const labelEnd = findClosing(text, start + 1, '[', ']');
  if (labelEnd === -1 || text[labelEnd + 1] !== '(') return null;
  const srcEnd = findClosing(text, labelEnd + 1, '(', ')');
  if (srcEnd === -1) return null;
  const src = text.slice(labelEnd + 2, srcEnd).trim();
  return {
    alt: text.slice(start + 2, labelEnd),
    src,
    raw: text.slice(start, srcEnd + 1),
    safe: isSafeImageSrc(src),
    next: srcEnd + 1,
  };
}

interface MathMatch {
  value: string;
  next: number;
}

/**
 * `$…$` or `$$…$$`, with the closing delimiter required on the same line.
 *
 * `$` is an ordinary character in prose, so the bar for math is deliberate: the
 * opener may not be followed by whitespace, the content may not be empty or end
 * in whitespace, and a bare number is read as money rather than a formula. That
 * is what keeps a lone `$` and `$5 and $10` literal while `$x^2$` is math.
 */
function matchMath(text: string, start: number, delimiter: string): MathMatch | null {
  const from = start + delimiter.length;
  const first = text[from];
  if (first === undefined || /\s/.test(first)) return null;

  const newline = text.indexOf('\n', from);
  const limit = newline === -1 ? text.length : newline;
  let close = text.indexOf(delimiter, from);
  while (close !== -1 && close < limit) {
    const inner = text.slice(from, close);
    if (isPlausibleMath(inner)) return { value: inner, next: close + delimiter.length };
    close = text.indexOf(delimiter, close + 1);
  }
  return null;
}

function isPlausibleMath(inner: string): boolean {
  if (inner.length === 0) return false;
  if (/\s$/.test(inner)) return false;
  // `$5$` and `$1,000$` read as currency, not as a formula worth rendering.
  if (/^\d+(?:[.,]\d+)*$/.test(inner)) return false;
  return true;
}

interface ListItemMatch {
  indent: number;
  ordered: boolean;
  number: number;
  content: string;
  contentIndent: number;
}

function matchListItem(line: string): ListItemMatch | null {
  const match = /^( *)([-*+]|\d{1,9}[.)])( +)(.*)$/.exec(line);
  if (match === null) return null;
  const marker = match[2];
  const ordered = /^\d/.test(marker);
  return {
    indent: match[1].length,
    ordered,
    number: ordered ? Number.parseInt(marker, 10) : 0,
    content: match[4],
    contentIndent: match[1].length + marker.length + match[3].length,
  };
}

/** Split a table row on unescaped pipes, dropping the outer ones. */
function splitRow(line: string): string[] {
  const text = line.trim().replace(/^\|/, '').replace(/\|$/, '');
  const cells: string[] = [];
  let current = '';
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch === '\\' && text[i + 1] === '|') {
      current += '|';
      i++;
      continue;
    }
    if (ch === '|') {
      cells.push(current.trim());
      current = '';
      continue;
    }
    current += ch;
  }
  cells.push(current.trim());
  return cells;
}

const DELIMITER_CELL = /^:?-+:?$/;

/** The `|---|:--:|` row that turns the line above it into a header. */
function matchDelimiterRow(line: string): (TableAlign | null)[] | null {
  if (!line.includes('|')) return null;
  const cells = splitRow(line);
  if (cells.length === 0) return null;
  const align: (TableAlign | null)[] = [];
  for (const cell of cells) {
    if (!DELIMITER_CELL.test(cell)) return null;
    const left = cell.startsWith(':');
    const right = cell.endsWith(':');
    align.push(left && right ? 'center' : right ? 'right' : left ? 'left' : null);
  }
  return align;
}

/** Whether a line opens a block, which is what ends a paragraph. */
function startsBlock(lines: string[], index: number): boolean {
  const line = lines[index];
  if (isBlank(line)) return true;
  if (matchFence(line) !== null) return true;
  if (matchHeading(line) !== null) return true;
  if (isRule(line)) return true;
  if (isBlockquoteLine(line)) return true;
  if (matchDisplayMathStart(line) !== null) return true;
  if (matchListItem(line) !== null) return true;
  if (
    index + 1 < lines.length &&
    line.includes('|') &&
    matchDelimiterRow(lines[index + 1]) !== null
  ) {
    return true;
  }
  return false;
}

/* ----------------------------------------------------------------- blocks */

interface Parsed {
  token: BlockToken;
  next: number;
}

function parseBlocks(lines: string[]): BlockToken[] {
  const tokens: BlockToken[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (isBlank(line)) {
      i++;
      continue;
    }

    const fence = matchFence(line);
    if (fence !== null) {
      const parsed = parseFence(lines, i, fence);
      tokens.push(parsed.token);
      i = parsed.next;
      continue;
    }

    const heading = matchHeading(line);
    if (heading !== null) {
      tokens.push({
        type: 'heading',
        level: heading.level,
        children: parseInline(heading.text),
      });
      i++;
      continue;
    }

    // A rule is checked before a list because `- - -` reads as both.
    if (isRule(line)) {
      tokens.push({ type: 'rule' });
      i++;
      continue;
    }

    if (isBlockquoteLine(line)) {
      const parsed = parseBlockquote(lines, i);
      tokens.push(parsed.token);
      i = parsed.next;
      continue;
    }

    const displayMath = parseDisplayMath(lines, i);
    if (displayMath !== null) {
      tokens.push(displayMath.token);
      i = displayMath.next;
      continue;
    }

    if (
      line.includes('|') &&
      i + 1 < lines.length &&
      matchDelimiterRow(lines[i + 1]) !== null
    ) {
      const parsed = parseTable(lines, i);
      tokens.push(parsed.token);
      i = parsed.next;
      continue;
    }

    const item = matchListItem(line);
    if (item !== null) {
      const parsed = parseList(lines, i, item.indent);
      tokens.push(parsed.token);
      i = parsed.next;
      continue;
    }

    const parsed = parseParagraph(lines, i);
    tokens.push(parsed.token);
    i = parsed.next;
  }
  return tokens;
}

function parseFence(
  lines: string[],
  start: number,
  fence: { char: string; length: number; info: string },
): Parsed {
  const closer = new RegExp(`^ {0,3}${fence.char}{${fence.length},}\\s*$`);
  const body: string[] = [];
  let i = start + 1;
  while (i < lines.length) {
    if (closer.test(lines[i])) {
      i++;
      break;
    }
    body.push(lines[i]);
    i++;
  }
  // An unclosed fence runs to the end of the source; that is still a block.
  const language = fence.info === '' ? null : fence.info.split(/\s+/)[0];
  return {
    token: { type: 'code_block', language, value: body.join('\n') },
    next: i,
  };
}

function parseBlockquote(lines: string[], start: number): Parsed {
  const body: string[] = [];
  let i = start;
  while (i < lines.length) {
    const match = /^ {0,3}> ?(.*)$/.exec(lines[i]);
    if (match !== null) {
      body.push(match[1]);
      i++;
      continue;
    }
    if (isBlank(lines[i])) {
      const after = skipBlanks(lines, i);
      if (after < lines.length && isBlockquoteLine(lines[after])) {
        body.push('');
        i++;
        continue;
      }
    }
    break;
  }
  return { token: { type: 'blockquote', children: parseBlocks(body) }, next: i };
}

function parseList(lines: string[], start: number, baseIndent: number): Parsed {
  const first = matchListItem(lines[start])!;
  const ordered = first.ordered;
  const startNumber = ordered ? first.number : 1;
  const items: ListItem[] = [];
  let i = start;

  while (i < lines.length) {
    if (isBlank(lines[i])) break;
    const item = matchListItem(lines[i]);
    if (item === null || item.indent !== baseIndent || item.ordered !== ordered) break;

    const body: string[] = [item.content];
    let j = i + 1;
    while (j < lines.length) {
      const line = lines[j];
      if (isBlank(line)) {
        // A blank only belongs to the item when a later line still does.
        const after = skipBlanks(lines, j);
        if (after >= lines.length) break;
        const nextItem = matchListItem(lines[after]);
        const belongs =
          (nextItem !== null && nextItem.indent >= baseIndent) ||
          leadingSpaces(lines[after]) > baseIndent;
        if (!belongs) break;
        body.push('');
        j++;
        continue;
      }
      const nextItem = matchListItem(line);
      if (nextItem !== null) {
        if (nextItem.indent <= baseIndent) break;
      } else if (leadingSpaces(line) <= baseIndent) {
        break;
      }
      body.push(dedent(line, item.contentIndent));
      j++;
    }

    items.push({ children: parseBlocks(body) });
    i = j;
  }

  return {
    token: { type: 'list', ordered, start: startNumber, items },
    next: i,
  };
}

function parseTable(lines: string[], start: number): Parsed {
  const header = splitRow(lines[start]).map(parseInline);
  const align = matchDelimiterRow(lines[start + 1])!;
  const rows: InlineToken[][][] = [];
  let i = start + 2;
  while (i < lines.length && !isBlank(lines[i]) && lines[i].includes('|')) {
    const cells = splitRow(lines[i]).map(parseInline);
    // Pad or clip so every row lines up with the header.
    while (cells.length < header.length) cells.push([]);
    rows.push(cells.slice(0, header.length));
    i++;
  }
  return { token: { type: 'table', align, header, rows }, next: i };
}

function parseParagraph(lines: string[], start: number): Parsed {
  const parts: string[] = [];
  let i = start;
  while (i < lines.length) {
    if (i > start && startsBlock(lines, i)) break;
    parts.push(lines[i].trim());
    i++;
  }
  return {
    token: { type: 'paragraph', children: parseInline(parts.join('\n')) },
    next: i,
  };
}

/* ----------------------------------------------------------------- inline */

/** Parse a span of text into inline tokens. Exported for the tests. */
export function parseInline(text: string): InlineToken[] {
  const tokens: InlineToken[] = [];
  let buffer = '';
  let i = 0;

  const flush = (): void => {
    if (buffer !== '') {
      tokens.push({ type: 'text', value: buffer });
      buffer = '';
    }
  };

  while (i < text.length) {
    const ch = text[i];

    if (ch === '\\' && i + 1 < text.length && isEscapable(text[i + 1])) {
      buffer += text[i + 1];
      i += 2;
      continue;
    }

    if (ch === '`') {
      const span = matchCodeSpan(text, i);
      if (span !== null) {
        flush();
        tokens.push({ type: 'code', value: span.value });
        i = span.next;
        continue;
      }
    }

    // Before the link branch: `![x](y)` must not be read as `!` plus a link.
    if (ch === '!' && text[i + 1] === '[') {
      const image = matchImage(text, i);
      if (image !== null) {
        flush();
        if (image.safe) {
          tokens.push({ type: 'image', src: image.src, alt: image.alt });
        } else {
          // Same contract as an unsafe link: the construct is shown as written
          // and no element is produced, so `javascript:` never reaches the DOM.
          tokens.push({ type: 'text', value: image.raw });
        }
        i = image.next;
        continue;
      }
    }

    if (ch === '[') {
      const link = matchLink(text, i);
      if (link !== null) {
        flush();
        if (link.safe) {
          tokens.push({
            type: 'link',
            href: link.href,
            children: parseInline(link.label),
          });
        } else {
          // Unsafe target: the whole construct is shown literally, which both
          // keeps the author's text and guarantees no anchor is produced.
          tokens.push({ type: 'text', value: link.raw });
        }
        i = link.next;
        continue;
      }
    }

    if (ch === '*' || ch === '_') {
      const emphasis = matchEmphasis(text, i);
      if (emphasis !== null) {
        flush();
        tokens.push({ type: emphasis.kind, children: parseInline(emphasis.inner) });
        i = emphasis.next;
        continue;
      }
    }

    if (ch === '~' && text[i + 1] === '~') {
      const strike = matchDelimiterPair(text, i, '~~');
      if (strike !== null) {
        flush();
        tokens.push({ type: 'strike', children: parseInline(strike.inner) });
        i = strike.next;
        continue;
      }
    }

    // `$$` is tried before `$` so a display delimiter is never read as two
    // inline ones.
    if (ch === '$') {
      const delimiter = text[i + 1] === '$' ? '$$' : '$';
      const math = matchMath(text, i, delimiter);
      if (math !== null) {
        flush();
        tokens.push({
          type: delimiter === '$$' ? 'math_display' : 'math_inline',
          value: math.value,
        });
        i = math.next;
        continue;
      }
    }

    buffer += ch;
    i++;
  }

  flush();
  return tokens;
}

function isEscapable(ch: string): boolean {
  return '\\`*_{}[]()#+-.!|~>$'.includes(ch);
}

interface CodeSpan {
  value: string;
  next: number;
}

function matchCodeSpan(text: string, start: number): CodeSpan | null {
  let run = 0;
  while (text[start + run] === '`') run++;
  const delimiter = '`'.repeat(run);
  const from = start + run;
  const close = text.indexOf(delimiter, from);
  if (close === -1) return null;
  let value = text.slice(from, close);
  // CommonMark drops one space from each end when both are present, so
  // `` ` x ` `` reads as `x` rather than ` x `.
  if (value.length > 1 && value.startsWith(' ') && value.endsWith(' ')) {
    value = value.slice(1, -1);
  }
  return { value, next: close + run };
}

interface LinkMatch {
  label: string;
  href: string;
  raw: string;
  safe: boolean;
  next: number;
}

function matchLink(text: string, start: number): LinkMatch | null {
  if (text[start] !== '[') return null;
  const labelEnd = findClosing(text, start, '[', ']');
  if (labelEnd === -1 || text[labelEnd + 1] !== '(') return null;
  const hrefEnd = findClosing(text, labelEnd + 1, '(', ')');
  if (hrefEnd === -1) return null;
  const href = text.slice(labelEnd + 2, hrefEnd).trim();
  return {
    label: text.slice(start + 1, labelEnd),
    href,
    raw: text.slice(start, hrefEnd + 1),
    safe: isSafeHref(href),
    next: hrefEnd + 1,
  };
}

function findClosing(text: string, open: number, opener: string, closer: string): number {
  let depth = 0;
  for (let i = open; i < text.length; i++) {
    const ch = text[i];
    if (ch === '\\') {
      i++;
      continue;
    }
    if (ch === opener) depth++;
    else if (ch === closer) {
      depth--;
      if (depth === 0) return i;
    }
  }
  return -1;
}

interface DelimiterPair {
  inner: string;
  next: number;
}

function matchDelimiterPair(text: string, start: number, delimiter: string): DelimiterPair | null {
  const from = start + delimiter.length;
  let close = text.indexOf(delimiter, from);
  while (close !== -1) {
    const inner = text.slice(from, close);
    // `**` around nothing, or around whitespace, is not emphasis.
    if (inner.length > 0 && !/^\s|\s$/.test(inner)) {
      return { inner, next: close + delimiter.length };
    }
    close = text.indexOf(delimiter, close + delimiter.length);
  }
  return null;
}

function isWordChar(ch: string | undefined): boolean {
  return ch !== undefined && /[0-9A-Za-z]/.test(ch);
}

interface EmphasisMatch {
  kind: 'strong' | 'em';
  inner: string;
  next: number;
}

function matchEmphasis(text: string, start: number): EmphasisMatch | null {
  const ch = text[start];
  // Underscores inside a word are left alone: `snake_case_name` is a name,
  // not emphasis. Asterisks are allowed mid-word, as GFM does.
  if (ch === '_' && isWordChar(text[start - 1])) return null;

  const strong = text[start + 1] === ch;
  if (strong) {
    const pair = matchDelimiterPair(text, start, ch + ch);
    if (pair !== null && !(ch === '_' && isWordChar(text[pair.next]))) {
      return { kind: 'strong', inner: pair.inner, next: pair.next };
    }
  }

  const pair = matchDelimiterPair(text, start, ch);
  if (pair !== null && !(ch === '_' && isWordChar(text[pair.next]))) {
    return { kind: 'em', inner: pair.inner, next: pair.next };
  }
  return null;
}
