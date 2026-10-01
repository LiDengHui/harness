/**
 * The grouping rule, tested without a DOM.
 *
 * `vitest.config.ts` runs the node environment with no Vue plugin, so the pure
 * module is the only place these cases can live. Each one is a shape a real
 * stored conversation produced, or one the record could be malformed into.
 */

import { describe, expect, it } from 'vitest';

import { groupMessages } from './group';

const user = (content: string) => ({ role: 'user', content });
const assistant = (content: string, toolCalls: unknown[] = []) => ({
  role: 'assistant',
  content,
  tool_calls: toolCalls,
});

describe('groupMessages', () => {
  it('splits two user messages into two turns, in order', () => {
    const grouped = groupMessages([
      user('first question'),
      assistant('first answer'),
      user('second question'),
      assistant('second answer'),
    ]);

    expect(grouped.preamble).toEqual([]);
    expect(grouped.turns).toHaveLength(2);
    expect(grouped.turns[0]).toEqual({
      index: 1,
      prompt: 'first question',
      blocks: [{ kind: 'text', text: 'first answer' }],
    });
    expect(grouped.turns[1]).toEqual({
      index: 2,
      prompt: 'second question',
      blocks: [{ kind: 'text', text: 'second answer' }],
    });
  });

  it('pairs a tool result with the assistant call it answers', () => {
    const grouped = groupMessages([
      user('go'),
      assistant('', [{ id: 'c1', name: 'read_file', arguments: { path: 'a.rs' } }]),
      { role: 'tool', tool_call_id: 'c1', name: 'read_file', content: 'fn main() {}' },
    ]);

    // The result is folded into the call's card, so the turn holds one block.
    expect(grouped.turns[0].blocks).toEqual([
      {
        kind: 'tool',
        tool: {
          kind: 'call',
          id: 'c1',
          name: 'read_file',
          arguments: { path: 'a.rs' },
          result: 'fn main() {}',
          resultName: 'read_file',
        },
      },
    ]);
  });

  it('keeps a result whose tool_call_id matches no visible call', () => {
    const grouped = groupMessages([
      user('go'),
      { role: 'tool', tool_call_id: 'gone', content: 'stray output' },
    ]);

    expect(grouped.turns[0].blocks).toEqual([
      {
        kind: 'tool',
        tool: { kind: 'result', toolCallId: 'gone', name: null, content: 'stray output' },
      },
    ]);
  });

  it('returns nothing for an empty array, and for a value that is not one', () => {
    expect(groupMessages([])).toEqual({ preamble: [], turns: [] });
    expect(groupMessages(undefined)).toEqual({ preamble: [], turns: [] });
    expect(groupMessages('not a list')).toEqual({ preamble: [], turns: [] });
    expect(groupMessages([null, 7, 'x'])).toEqual({ preamble: [], turns: [] });
  });

  it('keeps both the text and the calls of one assistant message', () => {
    const grouped = groupMessages([
      user('go'),
      assistant('let me check', [{ id: 'c1', name: 'shell', arguments: { command: 'ls' } }]),
    ]);

    expect(grouped.turns[0].blocks).toEqual([
      { kind: 'text', text: 'let me check' },
      {
        kind: 'tool',
        tool: {
          kind: 'call',
          id: 'c1',
          name: 'shell',
          arguments: { command: 'ls' },
          result: null,
          resultName: null,
        },
      },
    ]);
  });

  it('holds a system message before the first user message as the preamble', () => {
    const grouped = groupMessages([
      { role: 'system', content: 'you are a helpful agent' },
      user('hello'),
      assistant('hi'),
    ]);

    expect(grouped.preamble).toEqual([
      { kind: 'note', note: { role: 'system', text: 'you are a helpful agent' } },
    ]);
    expect(grouped.turns).toHaveLength(1);
    expect(grouped.turns[0].prompt).toBe('hello');
  });

  it('keeps an assistant message with neither text nor calls as a note', () => {
    const grouped = groupMessages([user('go'), { role: 'assistant' }]);

    expect(grouped.turns[0].blocks).toEqual([
      { kind: 'note', note: { role: 'assistant', text: null } },
    ]);
  });

  it('keeps an unknown role as a note rather than guessing at it', () => {
    const grouped = groupMessages([
      user('go'),
      { role: 'critic', content: 'the answer is wrong' },
    ]);

    expect(grouped.turns[0].blocks).toEqual([
      { kind: 'note', note: { role: 'critic', text: 'the answer is wrong' } },
    ]);
  });

  it('pairs a call and its result that sit either side of other output', () => {
    const grouped = groupMessages([
      user('go'),
      assistant('', [{ id: 'c1', name: 'grep', arguments: {} }]),
      assistant('while waiting'),
      { role: 'tool', tool_call_id: 'c1', content: 'match' },
    ]);

    expect(grouped.turns[0].blocks).toEqual([
      {
        kind: 'tool',
        tool: {
          kind: 'call',
          id: 'c1',
          name: 'grep',
          arguments: {},
          result: 'match',
          resultName: null,
        },
      },
      { kind: 'text', text: 'while waiting' },
    ]);
  });
});
