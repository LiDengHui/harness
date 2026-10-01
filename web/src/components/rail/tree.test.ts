/**
 * The rail's rule, tested where it can be tested: as a pure function.
 *
 * The three things this suite is here to hold down are the three the redesign
 * turns on — a sub-agent's session never becomes a top-level row, a main
 * conversation says how many sub-agents it has and lists them when opened, and a
 * search that matches a child reveals it rather than reporting a hit the reader
 * cannot reach.
 */

import { describe, expect, it } from 'vitest';

import type { SessionInfo } from '../../protocol';
import { buildRail, declaredParent, type RailOptions } from './tree';

/** One stored session, with the fields the rail reads. */
function session(id: string, parent: string | null = null, title = ''): SessionInfo {
  return {
    id,
    label: 'serve:default',
    created_at: '2026-10-01T00:00:00Z',
    nodes: 1,
    tokens: 1,
    head: null,
    forked_from: null,
    forked_from_session: parent,
    title,
  };
}

function rail(sessions: SessionInfo[], extra: Partial<RailOptions> = {}) {
  return buildRail({
    sessions,
    search: '',
    nameOf: (item) => item.title ?? '',
    open: new Set<string>(),
    ...extra,
  });
}

describe('declaredParent', () => {
  it('reads the id a session was forked from', () => {
    expect(declaredParent(session('b', 'a'))).toBe('a');
  });

  it('reads an absent field as no parent at all', () => {
    const bare = session('a');
    delete bare.forked_from_session;
    expect(declaredParent(bare)).toBeNull();
  });

  it('reads an empty string as no parent', () => {
    expect(declaredParent(session('a', ''))).toBeNull();
  });
});

describe('buildRail', () => {
  it('keeps only sessions with no parent at the top level', () => {
    const roots = rail([session('main'), session('child', 'main')]);

    expect(roots.map((root) => root.session.id)).toEqual(['main']);
    expect(roots[0]?.childCount).toBe(1);
  });

  it('holds a root’s children back until the reader opens it', () => {
    const roots = rail([session('main'), session('child', 'main')]);

    expect(roots[0]?.open).toBe(false);
    expect(roots[0]?.children).toEqual([]);
  });

  it('lists the children, indented, once the root is open', () => {
    const roots = rail([session('main'), session('child', 'main')], {
      open: new Set(['main']),
    });

    expect(roots[0]?.open).toBe(true);
    expect(roots[0]?.children.map((child) => [child.session.id, child.depth])).toEqual([
      ['child', 1],
    ]);
  });

  it('nests a fork of a fork one level deeper, under the same root', () => {
    const roots = rail(
      [session('main'), session('child', 'main'), session('grandchild', 'child')],
      { open: new Set(['main']) },
    );

    expect(roots).toHaveLength(1);
    expect(roots[0]?.children.map((child) => [child.session.id, child.depth])).toEqual([
      ['child', 1],
      ['grandchild', 2],
    ]);
    expect(roots[0]?.childCount).toBe(2);
    expect(roots[0]?.children[0]?.childCount).toBe(1);
  });

  it('draws a fork whose parent is not in the list as a conversation of its own', () => {
    const roots = rail([session('orphan', 'gone')]);

    expect(roots.map((root) => root.session.id)).toEqual(['orphan']);
    expect(roots[0]?.childCount).toBe(0);
  });

  it('breaks a cycle rather than recursing forever', () => {
    const sessions = [session('a', 'b'), session('b', 'a')];
    const roots = rail(sessions, { open: new Set(['a', 'b']) });

    // Whichever link was dropped, the rail terminates and every session is
    // still somewhere on it: a broken cycle is a wrong parent, never a lost row.
    const shown = roots.flatMap((root) => [
      root.session.id,
      ...root.children.map((child) => child.session.id),
    ]);
    expect([...shown].sort()).toEqual(['a', 'b']);
  });

  it('treats a session forked from itself as a root', () => {
    const roots = rail([session('self', 'self')]);

    expect(roots.map((root) => root.session.id)).toEqual(['self']);
    expect(roots[0]?.childCount).toBe(0);
  });

  it('keeps the order the store sorted, parents and children alike', () => {
    const roots = rail([session('newest'), session('older'), session('kid', 'newest')], {
      open: new Set(['newest']),
    });

    expect(roots.map((root) => root.session.id)).toEqual(['newest', 'older']);
  });
});

describe('reveal', () => {
  const chain = [session('main'), session('child', 'main'), session('deep', 'child')];

  it('opens the main conversation holding the session on screen', () => {
    const roots = rail(chain, { reveal: 'child' });

    expect(roots[0]?.open).toBe(true);
    expect(roots[0]?.children.map((child) => child.session.id)).toEqual(['child', 'deep']);
  });

  it('follows a chain of forks up to the main conversation', () => {
    const roots = rail(chain, { reveal: 'deep' });

    expect(roots[0]?.session.id).toBe('main');
    expect(roots[0]?.open).toBe(true);
  });

  it('marks the conversation holding the session on screen, so a preview keeps it', () => {
    const roots = rail(chain, { reveal: 'deep' });

    expect(roots[0]?.holdsView).toBe(true);
  });

  it('marks nothing when the reader is in a main conversation', () => {
    const roots = rail(chain, { reveal: 'main' });

    expect(roots[0]?.holdsView).toBe(false);
  });

  it('leaves a main conversation closed when the reader is in it', () => {
    const roots = rail(chain, { reveal: 'main' });

    expect(roots[0]?.open).toBe(false);
    expect(roots[0]?.children).toEqual([]);
  });

  it('does not open anything for a session that is not in the list', () => {
    const roots = rail(chain, { reveal: 'gone' });

    expect(roots[0]?.open).toBe(false);
  });
});

describe('search', () => {
  const sessions = [
    session('main-one', null, 'count the lines'),
    session('main-two', null, 'write the parser'),
    session('sub-one', 'main-one', 'read the manifest'),
    session('sub-two', 'main-two', 'wire it in'),
  ];

  it('matches a child by its title and reveals it under its parent', () => {
    const roots = rail(sessions, { search: 'manifest' });

    expect(roots.map((root) => root.session.id)).toEqual(['main-one']);
    expect(roots[0]?.open).toBe(true);
    expect(roots[0]?.children.map((child) => child.session.id)).toEqual(['sub-one']);
  });

  it('does not report a match the reader cannot reach', () => {
    const roots = rail(sessions, { search: 'wire' });

    expect(roots.map((root) => root.session.id)).toEqual(['main-two']);
    expect(roots[0]?.children.map((child) => child.session.id)).toEqual(['sub-two']);
  });

  it('matches a main conversation without pulling in children that do not match', () => {
    const roots = rail(sessions, { search: 'parser' });

    expect(roots.map((root) => root.session.id)).toEqual(['main-two']);
    expect(roots[0]?.children).toEqual([]);
  });

  it('matches by id as well as by title', () => {
    const roots = rail(sessions, { search: 'sub-one' });

    expect(roots.map((root) => root.session.id)).toEqual(['main-one']);
    expect(roots[0]?.children.map((child) => child.session.id)).toEqual(['sub-one']);
  });

  it('finds nothing when nothing matches', () => {
    expect(rail(sessions, { search: 'nothing here' })).toEqual([]);
  });

  it('opens a root for a search even when the reader had it closed', () => {
    const roots = rail(sessions, { search: 'manifest', open: new Set<string>() });

    expect(roots[0]?.open).toBe(true);
  });

  it('follows a match down through a chain of forks', () => {
    const chain = [session('main'), session('child', 'main'), session('deep', 'child', 'needle')];
    const roots = rail(chain, { search: 'needle' });

    expect(roots[0]?.children.map((child) => [child.session.id, child.depth])).toEqual([
      ['child', 1],
      ['deep', 2],
    ]);
  });
});
