/**
 * Tokenizer tests.
 *
 * The tokenizer is the tested surface on purpose: it is pure data in, pure data
 * out, so the whole rendering contract can be pinned here without a DOM. The
 * component is a thin mapping over these tokens and has no tests of its own.
 */

import { describe, expect, it } from 'vitest';

import { isSafeHref, isSafeImageSrc, tokenize } from './tokenize';
import type { BlockToken, InlineToken } from './types';

/** Narrow to one block token type, failing loudly when it is something else. */
function block<T extends BlockToken['type']>(
  tokens: BlockToken[],
  index: number,
  type: T,
): Extract<BlockToken, { type: T }> {
  const token = tokens[index];
  expect(token?.type).toBe(type);
  return token as Extract<BlockToken, { type: T }>;
}

/** The inline tokens of a paragraph, which is what most assertions need. */
function paragraph(tokens: BlockToken[], index = 0): InlineToken[] {
  return block(tokens, index, 'paragraph').children;
}

describe('headings', () => {
  it('reads every level from one through six', () => {
    const source = ['# one', '## two', '### three', '#### four', '##### five', '###### six'].join(
      '\n\n',
    );

    const tokens = tokenize(source);

    expect(tokens).toHaveLength(6);
    for (let level = 1; level <= 6; level++) {
      const heading = block(tokens, level - 1, 'heading');
      expect(heading.level).toBe(level);
      expect(heading.children).toEqual([{ type: 'text', value: expect.any(String) }]);
    }
    expect(block(tokens, 0, 'heading').children).toEqual([{ type: 'text', value: 'one' }]);
    expect(block(tokens, 5, 'heading').children).toEqual([{ type: 'text', value: 'six' }]);
  });

  it('does not treat a hash without a space as a heading', () => {
    expect(tokenize('#nope')[0]).toMatchObject({ type: 'paragraph' });
  });
});

describe('fenced code blocks', () => {
  it('keeps the language tag and the body verbatim', () => {
    const token = block(tokenize('```ts\nconst x = 1;\n```'), 0, 'code_block');

    expect(token.language).toBe('ts');
    expect(token.value).toBe('const x = 1;');
  });

  it('reports no language when the fence names none', () => {
    const token = block(tokenize('```\nplain\n```'), 0, 'code_block');

    expect(token.language).toBeNull();
    expect(token.value).toBe('plain');
  });

  it('does not interpret anything inside the fence', () => {
    const token = block(
      tokenize('```\n# not a heading\n**not bold**\n- not a list\n```'),
      0,
      'code_block',
    );

    expect(token.value).toBe('# not a heading\n**not bold**\n- not a list');
  });

  it('runs an unclosed fence to the end of the source', () => {
    const tokens = tokenize('```js\nconst x = 1;');

    expect(tokens).toHaveLength(1);
    const token = block(tokens, 0, 'code_block');
    expect(token.language).toBe('js');
    expect(token.value).toBe('const x = 1;');
  });
});

describe('inline', () => {
  it('keeps a code span literal, including its asterisks', () => {
    expect(paragraph(tokenize('a `b * c` d'))).toEqual([
      { type: 'text', value: 'a ' },
      { type: 'code', value: 'b * c' },
      { type: 'text', value: ' d' },
    ]);
  });

  it('reads bold, italic and strikethrough', () => {
    expect(paragraph(tokenize('**bold** *italic* ~~gone~~'))).toEqual([
      { type: 'strong', children: [{ type: 'text', value: 'bold' }] },
      { type: 'text', value: ' ' },
      { type: 'em', children: [{ type: 'text', value: 'italic' }] },
      { type: 'text', value: ' ' },
      { type: 'strike', children: [{ type: 'text', value: 'gone' }] },
    ]);
  });

  it('leaves underscores inside a word alone', () => {
    expect(paragraph(tokenize('snake_case_name'))).toEqual([
      { type: 'text', value: 'snake_case_name' },
    ]);
  });
});

describe('links', () => {
  it('turns an http link into an anchor token', () => {
    expect(paragraph(tokenize('[docs](https://example.com/a)'))).toEqual([
      {
        type: 'link',
        href: 'https://example.com/a',
        children: [{ type: 'text', value: 'docs' }],
      },
    ]);
  });

  it('keeps a javascript: link as literal text', () => {
    expect(paragraph(tokenize('[x](javascript:alert(1))'))).toEqual([
      { type: 'text', value: '[x](javascript:alert(1))' },
    ]);
  });

  it('allows only http, https and mailto', () => {
    expect(isSafeHref('https://example.com')).toBe(true);
    expect(isSafeHref('http://example.com')).toBe(true);
    expect(isSafeHref('mailto:a@b.c')).toBe(true);
    expect(isSafeHref('HTTPS://EXAMPLE.COM')).toBe(true);
    expect(isSafeHref('javascript:alert(1)')).toBe(false);
    expect(isSafeHref('data:text/html,<script>')).toBe(false);
    expect(isSafeHref('/relative/path')).toBe(false);
    expect(isSafeHref('java\tscript:alert(1)')).toBe(false);
  });
});

describe('lists', () => {
  it('nests an indented list inside its parent item', () => {
    const list = block(
      tokenize('- one\n- two\n  - two-a\n  - two-b\n- three'),
      0,
      'list',
    );

    expect(list.ordered).toBe(false);
    expect(list.start).toBe(1);
    expect(list.items).toHaveLength(3);

    const nested = list.items[1].children[1];
    expect(nested).toMatchObject({ type: 'list', ordered: false });
    if (nested.type !== 'list') throw new Error('expected a nested list');
    expect(nested.items).toHaveLength(2);
    expect(nested.items[0].children[0]).toEqual({
      type: 'paragraph',
      children: [{ type: 'text', value: 'two-a' }],
    });
  });

  it('reads an ordered list and its starting number', () => {
    const list = block(tokenize('3. three\n4. four'), 0, 'list');

    expect(list.ordered).toBe(true);
    expect(list.start).toBe(3);
    expect(list.items).toHaveLength(2);
  });
});

describe('tables', () => {
  it('reads the header, the alignment row and the body', () => {
    const table = block(
      tokenize('| Name | Qty | Note |\n| :--- | ---: | :---: |\n| a | 1 | x |\n| b | 2 | y |'),
      0,
      'table',
    );

    expect(table.align).toEqual(['left', 'right', 'center']);
    expect(table.header).toEqual([
      [{ type: 'text', value: 'Name' }],
      [{ type: 'text', value: 'Qty' }],
      [{ type: 'text', value: 'Note' }],
    ]);
    expect(table.rows).toHaveLength(2);
    expect(table.rows[0][0]).toEqual([{ type: 'text', value: 'a' }]);
  });
});

describe('other blocks', () => {
  it('reads a blockquote and a horizontal rule', () => {
    const tokens = tokenize('> quoted\n\n---');

    expect(block(tokens, 0, 'blockquote').children[0]).toEqual({
      type: 'paragraph',
      children: [{ type: 'text', value: 'quoted' }],
    });
    expect(tokens[1]).toEqual({ type: 'rule' });
  });

  it('returns nothing for an empty string', () => {
    expect(tokenize('')).toEqual([]);
  });
});

describe('math', () => {
  it('reads an inline formula between text', () => {
    expect(paragraph(tokenize('area $\\pi r^2$ now'))).toEqual([
      { type: 'text', value: 'area ' },
      { type: 'math_inline', value: '\\pi r^2' },
      { type: 'text', value: ' now' },
    ]);
  });

  it('reads a single-line display formula as its own block', () => {
    expect(tokenize('$$E = mc^2$$')).toEqual([{ type: 'math_display', value: 'E = mc^2' }]);
  });

  it('reads a multi-line display formula as one block', () => {
    expect(tokenize('$$\n\\int_0^1 x^2\\,dx\n$$')).toEqual([
      { type: 'math_display', value: '\\int_0^1 x^2\\,dx' },
    ]);
  });

  it('reads an inline display formula inside a sentence', () => {
    expect(paragraph(tokenize('so $$x$$ here'))).toEqual([
      { type: 'text', value: 'so ' },
      { type: 'math_display', value: 'x' },
      { type: 'text', value: ' here' },
    ]);
  });

  it('ends a paragraph at a display formula', () => {
    const tokens = tokenize('before\n\n$$x$$\n\nafter');

    expect(tokens.map((token) => token.type)).toEqual(['paragraph', 'math_display', 'paragraph']);
  });

  it('leaves a lone dollar sign literal', () => {
    expect(paragraph(tokenize('costs $ here'))).toEqual([{ type: 'text', value: 'costs $ here' }]);
    expect(paragraph(tokenize('$'))).toEqual([{ type: 'text', value: '$' }]);
  });

  it('keeps prices literal', () => {
    expect(paragraph(tokenize('$5 and $10'))).toEqual([{ type: 'text', value: '$5 and $10' }]);
  });

  it('keeps an unclosed dollar literal', () => {
    expect(paragraph(tokenize('a $x + y'))).toEqual([{ type: 'text', value: 'a $x + y' }]);
  });

  it('does not interpret math inside a fenced block', () => {
    const token = block(tokenize('```tex\n$x$ and $$y$$\n```'), 0, 'code_block');

    expect(token.value).toBe('$x$ and $$y$$');
  });

  it('lets a backslash escape a dollar sign', () => {
    expect(paragraph(tokenize('costs \\$5'))).toEqual([{ type: 'text', value: 'costs $5' }]);
  });
});

describe('images', () => {
  it('turns a safe image into an image token', () => {
    expect(paragraph(tokenize('![a cat](https://example.com/cat.png)'))).toEqual([
      { type: 'image', src: 'https://example.com/cat.png', alt: 'a cat' },
    ]);
  });

  it('keeps a javascript: image as literal text', () => {
    expect(paragraph(tokenize('![x](javascript:alert(1))'))).toEqual([
      { type: 'text', value: '![x](javascript:alert(1))' },
    ]);
  });

  it('keeps a data: image as literal text', () => {
    expect(paragraph(tokenize('![x](data:image/png;base64,AAAA)'))).toEqual([
      { type: 'text', value: '![x](data:image/png;base64,AAAA)' },
    ]);
  });

  it('keeps a relative image as literal text', () => {
    expect(paragraph(tokenize('![x](/local.png)'))).toEqual([
      { type: 'text', value: '![x](/local.png)' },
    ]);
  });

  it('allows only http and https', () => {
    expect(isSafeImageSrc('https://example.com/a.png')).toBe(true);
    expect(isSafeImageSrc('http://example.com/a.png')).toBe(true);
    expect(isSafeImageSrc('HTTP://EXAMPLE.COM/a.png')).toBe(true);
    expect(isSafeImageSrc('mailto:a@b.c')).toBe(false);
    expect(isSafeImageSrc('javascript:alert(1)')).toBe(false);
    expect(isSafeImageSrc('data:image/png;base64,AAAA')).toBe(false);
    expect(isSafeImageSrc('/relative.png')).toBe(false);
    expect(isSafeImageSrc('java\tscript:alert(1)')).toBe(false);
  });

  it('keeps alt as a plain string, never markup', () => {
    expect(paragraph(tokenize('![<b>x</b>](https://example.com/a.png)'))).toEqual([
      { type: 'image', src: 'https://example.com/a.png', alt: '<b>x</b>' },
    ]);
  });
});
