/**
 * The Markdown token tree.
 *
 * The renderer this app needs is deliberately not CommonMark: it covers the
 * subset the transcript actually shows, and every token here is plain data so
 * that `tokenize` can be tested without Vue or a DOM. Nothing in this module
 * knows how a token is drawn — `MarkdownView.vue` owns that mapping, and it
 * builds VNodes, never markup, so a token's text can only ever become a text
 * node.
 *
 * A math token is the one place that promise bends: its `value` is TeX, and
 * the renderer has to hand it to KaTeX, which returns HTML. That HTML is
 * produced only by `renderMath` in `math.ts`, which disables KaTeX's trust
 * extensions — the tokenizer itself stays pure data.
 */

/** `#` through `######`. */
export type HeadingLevel = 1 | 2 | 3 | 4 | 5 | 6;

/** A GFM table column's alignment, or `null` when the row did not ask for one. */
export type TableAlign = 'left' | 'center' | 'right';

/** One item of a list; its body is a token list so a nested list just works. */
export interface ListItem {
  children: BlockToken[];
}

/** A block-level token: the children of the document, a quote or a list item. */
export type BlockToken =
  | { type: 'heading'; level: HeadingLevel; children: InlineToken[] }
  | { type: 'paragraph'; children: InlineToken[] }
  | { type: 'code_block'; language: string | null; value: string }
  | { type: 'blockquote'; children: BlockToken[] }
  | { type: 'list'; ordered: boolean; start: number; items: ListItem[] }
  | {
      type: 'table';
      align: (TableAlign | null)[];
      header: InlineToken[][];
      rows: InlineToken[][][];
    }
  | { type: 'math_display'; value: string }
  | { type: 'rule' };

/** A span-level token. `text` is the only one that carries a bare string. */
export type InlineToken =
  | { type: 'text'; value: string }
  | { type: 'code'; value: string }
  | { type: 'strong'; children: InlineToken[] }
  | { type: 'em'; children: InlineToken[] }
  | { type: 'strike'; children: InlineToken[] }
  | { type: 'link'; href: string; children: InlineToken[] }
  | { type: 'image'; src: string; alt: string }
  | { type: 'math_inline'; value: string }
  | { type: 'math_display'; value: string };

/** A code block token, narrowed for the renderer's header bar. */
export type CodeBlockToken = Extract<BlockToken, { type: 'code_block' }>;

/** A table token, narrowed for the renderer's cell walk. */
export type TableToken = Extract<BlockToken, { type: 'table' }>;

/** A display-math block, narrowed for the renderer's centred block. */
export type MathDisplayToken = Extract<BlockToken, { type: 'math_display' }>;
