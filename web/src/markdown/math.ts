/**
 * KaTeX, and the one place this app inserts markup it did not build itself.
 *
 * Everywhere else `MarkdownView.vue` maps tokens to VNodes, so text from a tool
 * result can only ever become a text node. A formula cannot be drawn that way:
 * KaTeX's whole job is to turn TeX into positioned HTML, and there is no way to
 * get that layout without taking the string it returns. The exception is
 * contained here, behind three settings that make the result safe to insert:
 *
 * - `trust: false` — the setting that carries the security. KaTeX's `\href`,
 *   `\url` and `\includegraphics` are the commands that can emit an `<a>` or an
 *   `<img>`; with trust off they emit neither, so a formula can never introduce
 *   a link or a fetch. `math.test.ts` pins that.
 * - `throwOnError: false` — a typo in a formula renders as a red error, the way
 *   any other Markdown typo renders as itself. A bad formula must never throw
 *   out of the render and take the transcript with it.
 * - `strict: false` — a stray unicode character or a non-standard command
 *   produces a warning rather than a thrown `ParseError`.
 *
 * The library is loaded lazily: KaTeX is a few hundred kB of JavaScript plus
 * its fonts, and this page is served to a phone over a LAN. `preloadMath`
 * starts the fetch at boot so the chunk is normally warm before the first
 * formula arrives and the render never has to fall back.
 */

type Katex = (typeof import('katex'))['default'];

let pending: Promise<Katex> | null = null;

/** The cached KaTeX module, fetched on first use. */
export function loadMath(): Promise<Katex> {
  pending ??= import('katex').then((module) => module.default);
  return pending;
}

/** Start fetching KaTeX without waiting for it; safe to call more than once. */
export function preloadMath(): void {
  void loadMath().catch(() => {
    // A failed chunk is retried by the next `renderMath`; there is nothing
    // useful to do with the error here.
  });
}

/**
 * Render TeX to KaTeX HTML.
 *
 * The caller inserts the result with `innerHTML` — the exception documented
 * above, and the reason the options are what they are.
 */
export function renderMath(value: string, display: boolean): Promise<string> {
  return loadMath().then((katex) =>
    katex.renderToString(value, {
      displayMode: display,
      throwOnError: false,
      strict: false,
      trust: false,
      output: 'htmlAndMathml',
    }),
  );
}
