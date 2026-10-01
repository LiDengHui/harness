/**
 * The parts of `MermaidDiagram.vue` that need no DOM.
 *
 * The component itself cannot be tested here: vitest runs in the `node`
 * environment with no Vue plugin, so there is no `document` for mermaid to
 * render into and no way to mount a single-file component. Everything that is a
 * *decision* rather than a side effect therefore lives in this module — the
 * configuration handed to mermaid, the id policy, and the "show the source
 * instead" fallback — and the `.vue` file is left with nothing but DOM calls.
 *
 * Mermaid is imported as a **type** only. The runtime import stays inside the
 * component, behind `import()`, so pulling this module into a test does not
 * drag the library in.
 */

import type { MermaidConfig } from 'mermaid';

/**
 * Prefix for the id handed to `mermaid.render`.
 *
 * A letter first, so the id is also a legal CSS identifier: mermaid builds
 * `#<id>` selectors and `url(#<id>)` references from it, and an id beginning
 * with a digit would make those invalid. The counter appended by
 * {@link nextDiagramId} is what makes it unique.
 */
const ID_PREFIX = 'harness-mermaid-';

/** Longest error text kept; a parse error embeds the offending source line. */
const MAX_ERROR_LENGTH = 240;

/** Shown when a thrown value carries nothing readable. */
export const UNKNOWN_ERROR = 'The diagram could not be rendered.';

/** The app's font stack, so diagram text matches the transcript around it. */
const FONT_FAMILY =
  "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, " +
  "'Microsoft YaHei', 'PingFang SC', 'Hiragino Sans GB', 'Noto Sans CJK SC', sans-serif";

/**
 * A diagram that keeps its natural size, so a wide one scrolls inside its block
 * instead of being scaled down to illegibility. The wrapper in the component
 * supplies the `overflow-x`, which is what stops the page itself widening.
 */
const NATURAL_WIDTH = { useMaxWidth: false } as const;

/**
 * What mermaid is initialised with, once per page.
 *
 * `securityLevel: 'strict'` is the one that matters for safety: it runs the
 * generated SVG through DOMPurify, drops `<script>`, and disables the
 * `click`/`javascript:` interaction hooks a diagram definition can otherwise
 * attach to a node. Mermaid's output is generated markup, so it has to be
 * injected as markup — strict is what makes that acceptable.
 *
 * `startOnLoad: false` keeps mermaid from scanning the document for `.mermaid`
 * elements on load: this component owns every render, and an auto-run would
 * both race the first `render()` and render blocks twice.
 *
 * `suppressErrorRendering: true` stops mermaid inserting its own red "Syntax
 * error" diagram into `<body>` when a definition is invalid. Without it, a
 * broken fence leaves a stray node behind that no one removes.
 *
 * `logLevel: 'fatal'` silences mermaid's own `console.error` on a parse
 * failure. The failure is already shown to the reader in the transcript; a
 * second, duplicated report in the console is noise.
 *
 * `htmlLabels: false` renders node labels as SVG `<text>` rather than
 * `<foreignObject>` HTML. That removes the HTML path entirely, which is both
 * smaller to sanitise and less prone to the clipping that foreignObject labels
 * suffer inside a scrolling box.
 *
 * The theme is the built-in `dark` (a coherent dark palette for *every*
 * diagram type, not just flowcharts) with the app's own custom properties
 * layered on top, so a diagram reads as part of the page instead of a light
 * rectangle pasted onto it.
 */
export const MERMAID_INIT_CONFIG: MermaidConfig = {
  startOnLoad: false,
  securityLevel: 'strict',
  suppressErrorRendering: true,
  logLevel: 'fatal',
  theme: 'dark',
  darkMode: true,
  htmlLabels: false,
  fontFamily: FONT_FAMILY,
  themeVariables: {
    fontFamily: FONT_FAMILY,

    background: '#191c21',
    primaryColor: '#20242b',
    primaryTextColor: '#dfe3ea',
    primaryBorderColor: '#2a2f38',
    secondaryColor: '#20242b',
    tertiaryColor: '#14161a',
    textColor: '#dfe3ea',
    lineColor: '#8792a2',

    mainBkg: '#20242b',
    nodeBorder: '#2a2f38',
    nodeTextColor: '#dfe3ea',
    clusterBkg: '#191c21',
    clusterBorder: '#2a2f38',
    titleColor: '#dfe3ea',
    edgeLabelBackground: '#14161a',

    actorBkg: '#20242b',
    actorBorder: '#2a2f38',
    actorTextColor: '#dfe3ea',
    actorLineColor: '#2a2f38',
    signalColor: '#8792a2',
    signalTextColor: '#dfe3ea',
    labelBoxBkgColor: '#20242b',
    labelBoxBorderColor: '#2a2f38',
    labelTextColor: '#dfe3ea',
    loopTextColor: '#dfe3ea',
    noteBkgColor: '#2a2f38',
    noteTextColor: '#dfe3ea',
    noteBorderColor: '#4b9ddd',
    activationBkgColor: '#20242b',
    activationBorderColor: '#4b9ddd',

    sectionBkgColor: '#191c21',
    altSectionBkgColor: '#20242b',
    sectionBkgColor2: '#20242b',
    taskBorderColor: '#2a2f38',
    taskBkgColor: '#20242b',
    taskTextColor: '#dfe3ea',
    taskTextOutsideColor: '#dfe3ea',
    taskTextLightColor: '#dfe3ea',
    gridColor: '#2a2f38',

    pie1: '#4b9ddd',
    pie2: '#8a6fd0',
    pie3: '#4caf7d',
    pie4: '#d9a23b',
    pie5: '#d9534f',
    pie6: '#5a7f9e',
    pie7: '#6f7fd0',
    pie8: '#3f8f6f',
    pieTitleTextColor: '#dfe3ea',
    pieSectionTextColor: '#14161a',
    pieLegendTextColor: '#dfe3ea',
    pieStrokeColor: '#14161a',
    pieOuterStrokeColor: '#2a2f38',
  },
  flowchart: { ...NATURAL_WIDTH },
  sequence: { ...NATURAL_WIDTH },
  gantt: { ...NATURAL_WIDTH },
  journey: { ...NATURAL_WIDTH },
  timeline: { ...NATURAL_WIDTH },
  class: { ...NATURAL_WIDTH },
  state: { ...NATURAL_WIDTH },
  er: { ...NATURAL_WIDTH },
  pie: { ...NATURAL_WIDTH },
  quadrantChart: { ...NATURAL_WIDTH },
  xyChart: { ...NATURAL_WIDTH },
  requirement: { ...NATURAL_WIDTH },
  architecture: { ...NATURAL_WIDTH },
  mindmap: { ...NATURAL_WIDTH },
  ishikawa: { ...NATURAL_WIDTH },
  kanban: { ...NATURAL_WIDTH },
  gitGraph: { ...NATURAL_WIDTH },
  c4: { ...NATURAL_WIDTH },
  sankey: { ...NATURAL_WIDTH },
  packet: { ...NATURAL_WIDTH },
  block: { ...NATURAL_WIDTH },
  usecase: { ...NATURAL_WIDTH },
  radar: { ...NATURAL_WIDTH },
  venn: { ...NATURAL_WIDTH },
};

/**
 * Whether a source string is worth handing to mermaid.
 *
 * An empty (or whitespace-only) definition makes mermaid throw, and a
 * ```mermaid fence with nothing in it is not an error worth reporting — there
 * is no diagram and no source to show.
 */
export function isRenderable(source: string): boolean {
  return source.trim() !== '';
}

/** The id for the `sequence`-th render of this page. */
export function diagramId(sequence: number): string {
  return `${ID_PREFIX}${sequence}`;
}

let counter = 0;

/**
 * The next id.
 *
 * A module-level counter, so every render on the page gets a different one:
 * two diagrams mounted side by side would otherwise collide, and mermaid keys
 * its temporary DOM node, its `#id` selectors and its `url(#…)` marker
 * references off this string. It is also what lets a stale render be swept up
 * after the fact — see `sweepStrayNode` in the component.
 */
export function nextDiagramId(): string {
  counter += 1;
  return diagramId(counter);
}

/** The message inside a thrown value, whatever shape it arrived in. */
function rawMessage(error: unknown): string {
  if (typeof error === 'string') return error;
  if (error instanceof Error) return error.message;
  if (typeof error === 'object' && error !== null) {
    const record = error as Record<string, unknown>;
    // Mermaid's parse errors are plain objects carrying the text in `message`
    // (with `str` and `hash` alongside it) rather than Error instances.
    if (typeof record.message === 'string') return record.message;
    if (typeof record.str === 'string') return record.str;
  }
  return '';
}

/**
 * A one-line, bounded version of whatever went wrong.
 *
 * Mermaid's parse errors are multi-line — a `Parse error on line N:` header
 * followed by the source line and a caret — so they are collapsed to a single
 * line and capped. An unreadable thrown value becomes {@link UNKNOWN_ERROR}
 * rather than an empty string, because the reader still needs to be told that
 * the diagram is not coming.
 */
export function errorMessage(error: unknown): string {
  const condensed = rawMessage(error).replace(/\s+/g, ' ').trim();
  if (condensed === '') return UNKNOWN_ERROR;
  return condensed.length > MAX_ERROR_LENGTH
    ? `${condensed.slice(0, MAX_ERROR_LENGTH - 1)}…`
    : condensed;
}

/** What the component shows when a diagram cannot be drawn. */
export interface DiagramFailure {
  /** One line explaining what went wrong. */
  message: string;
  /** The original definition, echoed back so it is still readable. */
  source: string;
}

/**
 * The fallback decision.
 *
 * A diagram that will not parse must degrade to the error *and* the source:
 * a blank box tells the reader nothing, and the source is often the only copy
 * of what the model meant to draw.
 */
export function diagramFailure(source: string, error: unknown): DiagramFailure {
  return { message: errorMessage(error), source };
}
