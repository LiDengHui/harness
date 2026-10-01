<script lang="ts">
/**
 * Renders a Markdown source string as Vue elements.
 *
 * The component is intentionally thin: `tokenize` (a pure function in
 * `src/markdown/`) produces a typed tree, and this file maps each token to a
 * VNode. Text from a tool result — a file's contents, a fetched page — can only
 * become a text node, because there is no HTML path to escape.
 *
 * A formula is the one exception. KaTeX has to return HTML for the layout to
 * exist at all, so `MathNode` below inserts that HTML with `innerHTML`. The
 * safety is in the KaTeX options, not in this file: `renderMath` in
 * `src/markdown/math.ts` disables KaTeX's trust extensions, so the returned
 * string can never contain an anchor or an image. Everything else here still
 * builds VNodes only.
 *
 * It renders inline and adds no outer margin; the bubble that hosts it owns
 * the spacing around it. The optional `copyLabel` is a translated string the
 * parent supplies: with it a code block shows a copy button, without it the
 * header keeps the language label and no button appears.
 */

import { computed, defineComponent, h, ref, watchEffect, type VNode } from 'vue';

import { tokenize } from '../markdown';
import { preloadMath, renderMath } from '../markdown/math';
import type {
  BlockToken,
  CodeBlockToken,
  InlineToken,
  TableAlign,
  TableToken,
} from '../markdown/types';
import MermaidDiagram from './MermaidDiagram.vue';

// KaTeX's stylesheet is global, so it belongs to the entry CSS rather than to
// this component's scoped block. Importing it here — while the library itself
// stays in a lazily fetched chunk — keeps the CSS on the critical path, which
// is what stops a formula from flashing unstyled.
import 'katex/dist/katex.min.css';

// Start the KaTeX chunk at boot. It is not needed to paint, but fetching it now
// means the first formula is usually rendered on its first frame rather than
// falling back to raw TeX.
preloadMath();

/** The fallback shown when a fence carries no language tag. */
const NO_LANGUAGE = 'text';

/** A fence tagged with this language is drawn as a diagram, not shown as code. */
const MERMAID_LANGUAGE = 'mermaid';

/**
 * One formula.
 *
 * KaTeX arrives asynchronously, so the first paint of a formula may happen
 * before the chunk does; until then the raw TeX is shown, which is exactly what
 * a reader would see if KaTeX never loaded at all. `preloadMath` keeps that
 * window to the first moment after boot.
 */
const MathNode = defineComponent({
  name: 'MathNode',
  props: {
    value: { type: String, required: true },
    display: { type: Boolean, default: false },
  },
  setup(props) {
    const html = ref<string | null>(null);

    watchEffect((onCleanup) => {
      let cancelled = false;
      onCleanup(() => {
        cancelled = true;
      });
      html.value = null;
      void renderMath(props.value, props.display).then((rendered) => {
        if (!cancelled) html.value = rendered;
      });
    });

    return () => {
      const className = props.display ? 'md-math-display' : 'md-math-inline';
      if (html.value === null) {
        return h('span', { class: `${className} md-math-pending` }, props.value);
      }
      // The deliberate exception: KaTeX HTML, produced with `trust: false` and
      // `throwOnError: false`, so it holds no link, no image and never throws.
      return h('span', { class: className, innerHTML: html.value });
    };
  },
});

function renderInline(tokens: InlineToken[]): (VNode | string)[] {
  return tokens.map((token) => {
    switch (token.type) {
      case 'text':
        return token.value;
      case 'code':
        return h('code', { class: 'md-code-inline' }, token.value);
      case 'strong':
        return h('strong', renderInline(token.children));
      case 'em':
        return h('em', renderInline(token.children));
      case 'strike':
        return h('del', renderInline(token.children));
      case 'link':
        return h(
          'a',
          {
            href: token.href,
            target: '_blank',
            rel: 'noopener noreferrer',
          },
          renderInline(token.children),
        );
      case 'image':
        return h('img', {
          class: 'md-image',
          src: token.src,
          alt: token.alt,
          loading: 'lazy',
          referrerpolicy: 'no-referrer',
        });
      case 'math_inline':
        return h(MathNode, { value: token.value, display: false });
      case 'math_display':
        return h(MathNode, { value: token.value, display: true });
    }
  });
}

function renderBlocks(tokens: BlockToken[], copyLabel: string | undefined): VNode[] {
  return tokens.map((token) => renderBlock(token, copyLabel));
}

function renderBlock(token: BlockToken, copyLabel: string | undefined): VNode {
  switch (token.type) {
    case 'heading':
      return h(`h${token.level}`, { class: 'md-heading' }, renderInline(token.children));
    case 'paragraph':
      return h('p', { class: 'md-paragraph' }, renderInline(token.children));
    case 'code_block':
      // A ```mermaid fence is a diagram, not code: the component owns its own
      // failure rendering, so a definition mermaid rejects still shows the
      // source rather than an empty box.
      return token.language?.toLowerCase() === MERMAID_LANGUAGE
        ? h(MermaidDiagram, { source: token.value })
        : renderCodeBlock(token, copyLabel);
    case 'blockquote':
      return h('blockquote', { class: 'md-quote' }, renderBlocks(token.children, copyLabel));
    case 'list': {
      const items = token.items.map((item, index) =>
        h('li', { key: index }, renderBlocks(item.children, copyLabel)),
      );
      return token.ordered
        ? h('ol', { class: 'md-list', start: token.start }, items)
        : h('ul', { class: 'md-list' }, items);
    }
    case 'table':
      return renderTable(token);
    case 'math_display':
      return h(MathNode, { value: token.value, display: true });
    case 'rule':
      return h('hr', { class: 'md-rule' });
  }
}

function renderCodeBlock(token: CodeBlockToken, copyLabel: string | undefined): VNode {
  const head: (VNode | string)[] = [
    h('span', { class: 'md-code-lang mono' }, token.language ?? NO_LANGUAGE),
  ];
  if (copyLabel !== undefined) {
    head.push(
      h(
        'button',
        {
          class: 'btn btn-small md-copy',
          type: 'button',
          onClick: () => {
            void copyText(token.value);
          },
        },
        copyLabel,
      ),
    );
  }

  return h('div', { class: 'md-code-block' }, [
    h('div', { class: 'md-code-head' }, head),
    h('pre', { class: 'md-pre' }, [h('code', { class: 'md-code-text' }, token.value)]),
  ]);
}

async function copyText(value: string): Promise<void> {
  if (typeof navigator === 'undefined' || navigator.clipboard === undefined) return;
  try {
    await navigator.clipboard.writeText(value);
  } catch {
    // A denied clipboard is not worth a console error; the button is a
    // convenience, and the code stays selectable either way.
  }
}

function renderTable(token: TableToken): VNode {
  const cell = (
    children: InlineToken[],
    tag: 'th' | 'td',
    align: TableAlign | null,
    key: number,
  ): VNode =>
    h(tag, { key, class: align === null ? undefined : `md-align-${align}` }, renderInline(children));

  const header = h(
    'tr',
    token.header.map((children, index) =>
      cell(children, 'th', token.align[index] ?? null, index),
    ),
  );
  const body = token.rows.map((row, rowIndex) =>
    h(
      'tr',
      { key: rowIndex },
      row.map((children, index) => cell(children, 'td', token.align[index] ?? null, index)),
    ),
  );

  return h('div', { class: 'md-table-wrap' }, [
    h('table', { class: 'md-table' }, [h('thead', [header]), h('tbody', body)]),
  ]);
}

export default defineComponent({
  name: 'MarkdownView',
  props: {
    source: { type: String, required: true },
    copyLabel: { type: String, default: undefined },
  },
  setup(props) {
    const blocks = computed(() => tokenize(props.source));
    return () => h('div', { class: 'md' }, renderBlocks(blocks.value, props.copyLabel));
  },
});
</script>

<style scoped>
/*
 * The root adds no margin: the bubble around it decides the spacing between
 * messages. Only the spacing *inside* the rendered answer lives here.
 */
.md {
  margin: 0;
  min-width: 0;
  max-width: 100%;
  line-height: 1.55;
}

.md > :first-child {
  margin-top: 0;
}

.md > :last-child {
  margin-bottom: 0;
}

.md-heading {
  margin: 0.9rem 0 0.4rem;
  line-height: 1.3;
  font-weight: 600;
}

.md-heading:first-child {
  margin-top: 0;
}

h1.md-heading {
  font-size: 1.35rem;
}

h2.md-heading {
  font-size: 1.2rem;
}

h3.md-heading {
  font-size: 1.08rem;
}

h4.md-heading,
h5.md-heading,
h6.md-heading {
  font-size: 1rem;
}

.md-paragraph {
  margin: 0.45rem 0;
  /* The tokenizer keeps the author's soft line breaks, so the paragraph has to
     show them. `anywhere` stops a long unbroken token from widening the pane. */
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.md-code-inline {
  padding: 0.05em 0.3em;
  border: 1px solid var(--border);
  border-radius: 3px;
  background: var(--code);
  font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace;
  font-size: 0.86em;
}

/* ------------------------------------------------------- fenced code block */

.md-code-block {
  margin: 0.5rem 0;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--code);
  overflow: hidden;
  max-width: 100%;
  min-width: 0;
}

.md-code-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 0.5rem;
  padding: 0.15rem 0.5rem;
  border-bottom: 1px solid var(--border);
  background: var(--panel-2);
  font-size: 0.72rem;
}

.md-code-lang {
  color: var(--muted);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.md-copy {
  font-size: 0.7rem;
  padding: 0.08rem 0.5rem;
}

/*
 * The code scrolls sideways inside its own block instead of widening the page.
 * The global phone rule lets `pre` wrap; the block's own rule is more specific
 * and wins, because wrapping code is not reading it.
 */
.md-pre {
  margin: 0;
  padding: 0.5rem 0.6rem;
  max-width: 100%;
  overflow-x: auto;
}

.md-code-text {
  display: block;
  font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace;
  font-size: 0.78rem;
  line-height: 1.45;
  white-space: pre;
  word-break: normal;
  overflow-wrap: normal;
}

/* -------------------------------------------------------------- other blocks */

.md-quote {
  margin: 0.5rem 0;
  padding: 0.1rem 0 0.1rem 0.7rem;
  border-left: 3px solid var(--border);
  color: var(--muted);
}

.md-list {
  margin: 0.45rem 0;
  padding-left: 1.4rem;
}

.md-list .md-list {
  margin: 0.15rem 0;
}

.md-list li {
  margin: 0.15rem 0;
}

.md-table-wrap {
  margin: 0.5rem 0;
  max-width: 100%;
  min-width: 0;
  overflow-x: auto;
}

.md-table {
  border-collapse: collapse;
  font-size: 0.86rem;
}

.md-table th,
.md-table td {
  border: 1px solid var(--border);
  padding: 0.25rem 0.5rem;
  text-align: left;
  vertical-align: top;
}

/*
 * A wide table scrolls inside its block instead of widening the transcript.
 *
 * The wrapper is the scroll container, but on its own that is not enough here:
 * `.answer` sets `overflow-wrap: anywhere`, which table cells inherit, and
 * `anywhere` counts as a break opportunity when intrinsic sizes are computed —
 * so the auto layout squeezes every column until the table fits and the
 * scrollbar never appears. Keeping the cells at their natural width instead
 * would push the table's min-content up through `.transcript`, a grid item
 * with `min-width: auto`, and widen the whole layout past the viewport, where
 * `html { overflow-x: hidden }` clips it unreachably.
 *
 * `contain: inline-size` breaks that chain: the wrapper is then sized without
 * regard to the table, so it stays at the width it is given and the table
 * overflows into its scrollbar. The cell rule rides along, because it is only
 * safe once the containment is there — and both are behind `@supports`, so a
 * browser without containment keeps the old wrapping behaviour rather than
 * inheriting the blow-out.
 */
@supports (contain: inline-size) {
  .md-table-wrap {
    contain: inline-size;
  }

  .md-table th,
  .md-table td {
    overflow-wrap: break-word;
    word-break: normal;
  }
}

.md-table th {
  background: var(--panel-2);
  font-weight: 600;
}

/*
 * Alignment from the delimiter row. The selectors are qualified with
 * `.md-table` on purpose: `.md-table th` above is (0,1,1) and would otherwise
 * outrank a bare `.md-align-center` (0,1,0), leaving every cell left-aligned
 * no matter what the delimiter row said.
 */
.md-table .md-align-center {
  text-align: center;
}

.md-table .md-align-right {
  text-align: right;
}

/* ---------------------------------------------------------------- images */

/*
 * A large image must not be able to widen the page: it is capped to its
 * container and keeps its ratio. It stays inline-level, so an image written
 * mid-sentence sits in the sentence; a standalone one lands in its own
 * paragraph.
 */
.md-image {
  max-width: 100%;
  height: auto;
  border-radius: 4px;
  vertical-align: top;
}

/* ------------------------------------------------------------------ math */

/*
 * KaTeX's stylesheet is global (imported from the script block); these rules
 * only wrap its output. Display math gets its own centred row that scrolls
 * sideways inside the block — the same contract as a wide table — so a long
 * formula cannot widen the page.
 */
.md-math-inline {
  white-space: nowrap;
}

.md-math-display {
  display: block;
  margin: 0.5rem 0;
  max-width: 100%;
  overflow-x: auto;
  overflow-y: hidden;
}

/* Only ever seen while the KaTeX chunk is in flight. */
.md-math-pending {
  font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace;
  color: var(--muted);
}

.md-rule {
  border: 0;
  border-top: 1px solid var(--border);
  margin: 0.8rem 0;
}

.md a {
  color: var(--accent);
}

@media (max-width: 640px) {
  .md-code-head {
    padding: 0.1rem 0.4rem;
  }

  /* The one control the block hides behind. */
  .md-copy {
    min-height: 44px;
  }

  .md-list {
    padding-left: 1.2rem;
  }
}
</style>
