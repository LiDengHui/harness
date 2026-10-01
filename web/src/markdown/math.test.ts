/**
 * The KaTeX safety contract.
 *
 * `renderMath` is the only function in the app that hands markup to the DOM, so
 * the guarantee has to be pinned here rather than assumed: with `trust: false`,
 * a formula can ask for a link or an image and get neither.
 */

import { describe, expect, it } from 'vitest';

import { renderMath } from './math';

/** A real element, not the `<annotation>` MathML tag that also starts with `<a`. */
const ANCHOR = /<a[\s>]/;
const IMAGE = /<img[\s>]/;

describe('renderMath', () => {
  it('renders an inline formula', async () => {
    const html = await renderMath('E = mc^2', false);

    expect(html).toContain('class="katex"');
    expect(html).not.toContain('katex-error');
  });

  it('renders display math as a display block', async () => {
    const html = await renderMath('E = mc^2', true);

    expect(html).toContain('katex-display');
  });

  it('produces no anchor for a javascript: href', async () => {
    const html = await renderMath(String.raw`\href{javascript:alert(1)}{x}`, false);

    expect(html).not.toMatch(ANCHOR);
    expect(html).not.toContain('href=');
  });

  it('produces no anchor for an https href either', async () => {
    const html = await renderMath(String.raw`\href{https://example.com}{x}`, false);

    expect(html).not.toMatch(ANCHOR);
  });

  it('produces no image for includegraphics', async () => {
    const html = await renderMath(String.raw`\includegraphics{https://example.com/x.png}`, false);

    expect(html).not.toMatch(IMAGE);
  });

  it('renders a broken formula as an error instead of throwing', async () => {
    const html = await renderMath(String.raw`\frac{1}{`, false);

    expect(html).toContain('katex-error');
  });
});
