<script setup lang="ts">
/**
 * One ```mermaid fenced block, drawn as a diagram.
 *
 * The contract is deliberately tiny — a `source` string in, a diagram out —
 * and the component must never throw: a definition the model got wrong is a
 * normal event in a transcript, not a reason for the answer to disappear. So
 * every path that could fail (the library import, the parse, the draw) ends in
 * {@link diagramFailure}, which shows the reason and the original source.
 *
 * Mermaid is loaded with a dynamic `import()`. It is a large library and this
 * app is served to a phone over a LAN, where a page with no diagrams should not
 * pay for it; the import only happens the first time a block actually needs to
 * be drawn.
 *
 * The rendered SVG is injected with `v-html`, which is the one place in this
 * app that puts generated markup into the DOM. That is inherent — mermaid
 * returns a string — and `securityLevel: 'strict'` in the config is what makes
 * it safe: mermaid sanitises its own output and strips the script and click
 * hooks before returning it. See `mermaidDiagram.ts` for the full config.
 */

import { onBeforeUnmount, ref, watch } from 'vue';
import type { Mermaid } from 'mermaid';

import {
  MERMAID_INIT_CONFIG,
  diagramFailure,
  isRenderable,
  nextDiagramId,
  type DiagramFailure,
} from './mermaidDiagram';

const props = defineProps<{ source: string }>();

/** The last successfully rendered SVG markup, or '' before the first one. */
const svg = ref('');
const failure = ref<DiagramFailure | null>(null);

/**
 * Which render is current.
 *
 * A streaming transcript can change `source` faster than mermaid finishes, and
 * `render()` is serialised inside the library. Each run takes a number and
 * refuses to publish a result once a newer run has started, so a slow render
 * can never overwrite a fast one that came after it.
 */
let latest = 0;
let disposed = false;

/** The library, loaded once and shared by every diagram on the page. */
let loaded: Promise<Mermaid> | null = null;

function loadMermaid(): Promise<Mermaid> {
  loaded ??= import('mermaid').then((module) => {
    const mermaid = module.default;
    mermaid.initialize(MERMAID_INIT_CONFIG);
    return mermaid;
  });
  return loaded;
}

/**
 * Remove anything mermaid left in `<body>`.
 *
 * Mermaid draws into a temporary `<div id="d<id>">` appended to the body and
 * removes it itself, but an interrupted render — the component unmounting while
 * a draw is in flight — is exactly when it does not. The sweep only touches
 * nodes that are *direct children of the body*: the SVG this component renders
 * lives inside `#app`, so it can never be mistaken for a stray.
 */
function sweepStrayNode(id: string): void {
  if (typeof document === 'undefined' || document.body === null) return;
  for (const candidate of [`d${id}`, id]) {
    const node = document.getElementById(candidate);
    if (node !== null && node.parentNode === document.body) node.remove();
  }
}

async function render(): Promise<void> {
  const sequence = ++latest;
  const source = props.source;

  if (!isRenderable(source)) {
    svg.value = '';
    failure.value = null;
    return;
  }

  let mermaid: Mermaid;
  try {
    mermaid = await loadMermaid();
  } catch (error) {
    if (sequence === latest && !disposed) {
      svg.value = '';
      failure.value = diagramFailure(source, error);
    }
    return;
  }

  if (sequence !== latest || disposed) return;

  const id = nextDiagramId();
  try {
    const result = await mermaid.render(id, source);
    if (sequence !== latest || disposed) return;
    svg.value = result.svg;
    failure.value = null;
  } catch (error) {
    if (sequence === latest && !disposed) {
      svg.value = '';
      failure.value = diagramFailure(source, error);
    }
  } finally {
    // Runs even when the component was removed mid-render: the promise is not
    // cancellable, so this is the only chance to clear up after it.
    sweepStrayNode(id);
  }
}

watch(() => props.source, () => void render(), { immediate: true });

onBeforeUnmount(() => {
  disposed = true;
});
</script>

<template>
  <div class="mermaid-block">
    <div v-if="svg !== ''" class="mermaid-svg" v-html="svg" />
    <div v-else-if="failure !== null" class="mermaid-failure">
      <p class="mermaid-failure-title">{{ failure.message }}</p>
      <pre class="mermaid-failure-source"><code>{{ failure.source }}</code></pre>
    </div>
  </div>
</template>

<style scoped>
/*
 * The block owns the spacing around a diagram, the same way `.md-code-block`
 * does for a fence. `min-width: 0` is what lets it shrink inside a flex or grid
 * parent instead of forcing the transcript wide.
 *
 * `contain: inline-size` is the load-bearing rule on a phone. A mermaid SVG
 * carries its width as an attribute in pixels, and an element's intrinsic width
 * propagates upward as a *min-content* size; a grid or flex ancestor left at
 * the default `min-width: auto` therefore grows its track to fit the whole
 * diagram. At 390px that made the transcript 798px wide and the browser clipped
 * the diagram at the viewport edge instead of scrolling it. Inline-size
 * containment stops the width from propagating — the block takes its width from
 * its container — while the height stays content-driven and the `overflow-x`
 * below still does the scrolling. The failure source, which is a `<pre>` with
 * the same problem, is covered by the same rule.
 */
.mermaid-block {
  margin: 0.5rem 0;
  max-width: 100%;
  min-width: 0;
  contain: inline-size;
}

/*
 * The scroll container. Mermaid is configured with `useMaxWidth: false`, so a
 * wide diagram keeps its natural width; this box — not the page — is what
 * scrolls when it does not fit at 390px.
 */
.mermaid-svg {
  max-width: 100%;
  overflow-x: auto;
  padding: 0.4rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--panel);
}

/*
 * Scoped styles do not reach `v-html` content, so the injected SVG needs
 * `:deep`. `display: block` removes the inline-baseline gap under the diagram;
 * the intrinsic width is deliberately left alone so the box above can scroll.
 */
.mermaid-svg :deep(svg) {
  display: block;
}

/* --------------------------------------------------------- failed diagram */

/*
 * A failure keeps the shape of a code block with an error bar on top: the
 * reader gets the reason and then the source, never an empty box.
 */
.mermaid-failure {
  max-width: 100%;
  min-width: 0;
  border: 1px solid var(--border);
  border-left: 3px solid var(--error);
  border-radius: 4px;
  background: var(--code);
  overflow: hidden;
}

.mermaid-failure-title {
  margin: 0;
  padding: 0.3rem 0.5rem;
  border-bottom: 1px solid var(--border);
  color: var(--error);
  font-size: 0.78rem;
  overflow-wrap: anywhere;
}

.mermaid-failure-source {
  margin: 0;
  padding: 0.5rem 0.6rem;
  max-width: 100%;
  overflow-x: auto;
  font-family: ui-monospace, SFMono-Regular, 'SF Mono', Menlo, Consolas, 'Liberation Mono', monospace;
  font-size: 0.78rem;
  line-height: 1.45;
}

.mermaid-failure-source code {
  display: block;
  white-space: pre;
  word-break: normal;
  overflow-wrap: normal;
}
</style>
