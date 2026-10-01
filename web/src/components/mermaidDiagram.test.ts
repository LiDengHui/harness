import { describe, expect, it } from 'vitest';

import {
  MERMAID_INIT_CONFIG,
  UNKNOWN_ERROR,
  diagramFailure,
  diagramId,
  errorMessage,
  isRenderable,
  nextDiagramId,
} from './mermaidDiagram';

describe('isRenderable', () => {
  it('accepts a definition with any content', () => {
    expect(isRenderable('flowchart TD\n  a --> b')).toBe(true);
  });

  it('rejects an empty definition', () => {
    expect(isRenderable('')).toBe(false);
  });

  it('rejects a definition that is only whitespace', () => {
    expect(isRenderable('   \n\t\n')).toBe(false);
  });
});

describe('diagramId', () => {
  it('starts with a letter, so the id is a legal CSS identifier', () => {
    expect(/^[a-zA-Z]/.test(diagramId(1))).toBe(true);
  });

  it('contains no character that would need escaping in a selector', () => {
    expect(diagramId(42)).toMatch(/^[A-Za-z][A-Za-z0-9_-]*$/);
  });

  it('differs for different sequence numbers', () => {
    expect(diagramId(1)).not.toBe(diagramId(2));
  });
});

describe('nextDiagramId', () => {
  it('never repeats an id across a page', () => {
    const ids = new Set<string>();
    for (let i = 0; i < 100; i += 1) ids.add(nextDiagramId());
    expect(ids.size).toBe(100);
  });

  it('returns a fresh id on every call', () => {
    expect(nextDiagramId()).not.toBe(nextDiagramId());
  });
});

describe('errorMessage', () => {
  it('reads an Error message', () => {
    expect(errorMessage(new Error('Parse error on line 2'))).toBe('Parse error on line 2');
  });

  it('reads the message off a plain mermaid parse error object', () => {
    const thrown = { str: 'ignored', hash: { text: 'a' }, message: 'Parse error on line 2' };
    expect(errorMessage(thrown)).toBe('Parse error on line 2');
  });

  it('falls back to `str` when there is no message', () => {
    expect(errorMessage({ str: 'Parse error on line 3', hash: {} })).toBe(
      'Parse error on line 3',
    );
  });

  it('reads a bare string', () => {
    expect(errorMessage('boom')).toBe('boom');
  });

  it('collapses a multi-line parse error to one line', () => {
    const thrown = new Error('Parse error on line 2:\n...a --> b\n-----------^\nExpecting EOF');
    expect(errorMessage(thrown)).toBe('Parse error on line 2: ...a --> b -----------^ Expecting EOF');
  });

  it('caps an absurdly long message', () => {
    const message = errorMessage(new Error('x'.repeat(1000)));
    expect(message.length).toBe(240);
    expect(message.endsWith('…')).toBe(true);
  });

  it('names the failure when the thrown value says nothing', () => {
    expect(errorMessage(new Error(''))).toBe(UNKNOWN_ERROR);
    expect(errorMessage(null)).toBe(UNKNOWN_ERROR);
    expect(errorMessage(undefined)).toBe(UNKNOWN_ERROR);
    expect(errorMessage({})).toBe(UNKNOWN_ERROR);
    expect(errorMessage(42)).toBe(UNKNOWN_ERROR);
  });
});

describe('diagramFailure', () => {
  it('carries both the reason and the original source', () => {
    const failure = diagramFailure('flowchart TD\n  a -->', new Error('Parse error'));
    expect(failure.message).toBe('Parse error');
    expect(failure.source).toBe('flowchart TD\n  a -->');
  });

  it('still carries the source when nothing readable was thrown', () => {
    const failure = diagramFailure('nonsense', undefined);
    expect(failure.message).toBe(UNKNOWN_ERROR);
    expect(failure.source).toBe('nonsense');
  });
});

describe('MERMAID_INIT_CONFIG', () => {
  it('sanitises the generated SVG and never auto-runs', () => {
    expect(MERMAID_INIT_CONFIG.securityLevel).toBe('strict');
    expect(MERMAID_INIT_CONFIG.startOnLoad).toBe(false);
  });

  it('does not let mermaid inject its own error diagram into the page', () => {
    expect(MERMAID_INIT_CONFIG.suppressErrorRendering).toBe(true);
  });

  it('silences mermaid own console report, which the transcript already shows', () => {
    expect(MERMAID_INIT_CONFIG.logLevel).toBe('fatal');
  });

  it('uses a dark theme with the app palette', () => {
    expect(MERMAID_INIT_CONFIG.theme).toBe('dark');
    expect(MERMAID_INIT_CONFIG.darkMode).toBe(true);
    expect(MERMAID_INIT_CONFIG.themeVariables?.background).toBe('#191c21');
    expect(MERMAID_INIT_CONFIG.themeVariables?.primaryTextColor).toBe('#dfe3ea');
  });

  it('renders labels as SVG text rather than HTML', () => {
    expect(MERMAID_INIT_CONFIG.htmlLabels).toBe(false);
  });

  it('keeps diagrams at their natural size so a wide one scrolls', () => {
    expect(MERMAID_INIT_CONFIG.flowchart?.useMaxWidth).toBe(false);
    expect(MERMAID_INIT_CONFIG.sequence?.useMaxWidth).toBe(false);
    expect(MERMAID_INIT_CONFIG.gantt?.useMaxWidth).toBe(false);
  });
});
