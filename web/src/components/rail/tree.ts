/**
 * The rail's rule: which sessions are main conversations, and which belong to one.
 *
 * The server stores a delegated run as a session of its own, forked from the
 * session that delegated it, so `GET /api/sessions` returns both the
 * conversations a person started and the ones their agents started. The rail
 * shows the first kind as rows: a session whose `forked_from_session` names
 * another session in the list is a sub-agent's conversation, and it is drawn
 * *under* the conversation it came from rather than beside it.
 *
 * Nothing is hidden by that rule. A main conversation that has sub-agents says
 * how many, and the reader can open it to list them — which keeps the rail
 * honest about what the store holds while still answering "main conversations
 * only". Search reaches the children too: a child that matches pulls its parent
 * in as context, so a search never reports a hit the reader cannot then see.
 *
 * The module is deliberately free of Vue and of i18n — the caller passes the
 * words a session can be matched by — so it can be tested in the node
 * environment the rest of this suite uses.
 */

import type { SessionInfo } from '../../protocol';

/** One sub-agent session hanging under a main conversation. */
export interface RailChild {
  session: SessionInfo;
  /** 1 for a session forked straight from the root, 2 for one forked from that. */
  depth: number;
  /** The main conversation at the top of this child's chain. */
  rootId: string;
  /** How many sessions were forked from this one, at any depth. */
  childCount: number;
}

/** One main conversation, and the sub-agent sessions the rail may draw under it. */
export interface RailRoot {
  session: SessionInfo;
  /** How many sessions were forked from this one, at any depth. */
  childCount: number;
  /** Whether the reader has this root's sub-agent sessions open. */
  open: boolean;
  /**
   * Whether the session on screen is inside this conversation.
   *
   * The rail's collapsed list holds back all but its first few conversations,
   * and this is what says which one cannot be held back: the reader is in it.
   */
  holdsView: boolean;
  /** The children to draw, in tree order, each already carrying its depth. */
  children: RailChild[];
}

export interface RailOptions {
  /** Every stored session, newest first as the store sorted them. */
  sessions: readonly SessionInfo[];
  /** The filter text; empty means "every session". */
  search: string;
  /**
   * The words a session can also be matched by — its title, in the caller's
   * locale. The id and the label are always searched as well.
   */
  nameOf: (session: SessionInfo) => string;
  /** The main conversations the reader has opened, by id. */
  open: ReadonlySet<string>;
  /**
   * The session the reader is looking at, if any.
   *
   * The main conversation holding it is drawn open whatever the reader last did.
   * A sub-agent's conversation is reached by opening one, and a reader who has
   * arrived there — by a click, or by a reload that restored the selection —
   * must be able to see where they are: a rail that hides the row you are in is
   * the one thing a trace cannot afford. It is ignored for a session that is
   * itself a main conversation, whose children are the reader's to open.
   */
  reveal?: string | null;
}

/**
 * The session a stored session was forked from, as the wire declares it.
 *
 * Read defensively rather than through the declared type alone: the field is
 * still rolling out, and a server that does not send it leaves every session a
 * main conversation — a flat rail, which is the honest reading of "nobody said
 * this one has a parent".
 */
export function declaredParent(session: SessionInfo): string | null {
  const parent = session.forked_from_session;
  return typeof parent === 'string' && parent !== '' ? parent : null;
}

/** Whether a session's own id, label or title holds the needle. */
function matches(session: SessionInfo, needle: string, nameOf: (item: SessionInfo) => string): boolean {
  if (session.id.toLowerCase().includes(needle)) return true;
  if ((session.label ?? '').toLowerCase().includes(needle)) return true;
  return nameOf(session).toLowerCase().includes(needle);
}

/**
 * The parent every session in `known` was forked from, for the links this rail
 * can actually follow.
 *
 * A session that names no parent, or whose parent is not in `known`, gets no
 * entry — which is what leaves it at the root rather than dropping it. A parent
 * this rail cannot name is no parent at all: a fork of a session since removed
 * is drawn as a conversation of its own, not hidden.
 */
function parentLinks(sessions: readonly SessionInfo[], known: Map<string, SessionInfo>): Map<string, string> {
  const parents = new Map<string, string>();
  for (const session of sessions) {
    const declared = declaredParent(session);
    if (declared === null || declared === session.id || !known.has(declared)) continue;
    parents.set(session.id, declared);
  }
  breakCycles(parents);
  return parents;
}

/**
 * Drops the parent links that would loop, so a cycle renders as roots instead
 * of recursing forever: a session whose ancestor chain comes back to itself is
 * not a child of anything.
 */
function breakCycles(parents: Map<string, string>): void {
  for (const id of [...parents.keys()]) {
    const seen = new Set<string>([id]);
    let current = parents.get(id);
    while (current !== undefined) {
      if (seen.has(current)) {
        parents.delete(id);
        break;
      }
      seen.add(current);
      current = parents.get(current);
    }
  }
}

/**
 * The main conversation at the top of `id`'s chain of forks.
 *
 * Walks the links the rail can actually follow, and stops if it meets one it has
 * already seen — a cycle is broken by `breakCycles`, but the walk is bounded here
 * too rather than trusting that.
 */
function rootOf(id: string, parents: Map<string, string>): string {
  const seen = new Set<string>([id]);
  let top = id;
  let current = parents.get(id);
  while (current !== undefined && !seen.has(current)) {
    seen.add(current);
    top = current;
    current = parents.get(current);
  }
  return top;
}

/**
 * The rail's rows: one entry per main conversation, each with the children the
 * reader is currently allowed to see.
 *
 * A root is drawn when it is a conversation in its own right — which, while
 * something is being searched for, means either that it matches or that
 * something forked from it does. A matching child is always revealed: a search
 * that reported a hit the reader then could not see would be worse than no
 * search at all.
 *
 * With no search running, a root's children are drawn only once the reader has
 * opened it — or once the reader is *in* one of them, which is what `reveal`
 * carries.
 */
export function buildRail(options: RailOptions): RailRoot[] {
  const { sessions, nameOf } = options;
  const needle = options.search.trim().toLowerCase();
  const searching = needle !== '';

  const known = new Map(sessions.map((session) => [session.id, session]));
  const parents = parentLinks(sessions, known);

  const viewing = options.reveal ?? null;
  const revealRoot = viewing !== null && parents.has(viewing) ? rootOf(viewing, parents) : null;

  const childrenOf = new Map<string, SessionInfo[]>();
  const roots: SessionInfo[] = [];
  for (const session of sessions) {
    const parent = parents.get(session.id);
    if (parent === undefined) {
      roots.push(session);
      continue;
    }
    const siblings = childrenOf.get(parent);
    if (siblings === undefined) childrenOf.set(parent, [session]);
    else siblings.push(session);
  }

  /** How many sessions hang under `id`, at any depth. */
  const size = (id: string): number =>
    (childrenOf.get(id) ?? []).reduce((total, child) => total + 1 + size(child.id), 0);

  const hit = (session: SessionInfo): boolean =>
    !searching || matches(session, needle, nameOf);

  /** How many of `id`'s descendants match, at any depth. */
  const hitsBelow = (id: string): number =>
    (childrenOf.get(id) ?? []).reduce(
      (total, child) => total + (hit(child) ? 1 : 0) + hitsBelow(child.id),
      0,
    );

  /**
   * Whether a child is worth drawing: it matches, or something forked from it
   * does — a child that is only the way to a match is still the reader's route
   * to it, and its depth is what says which conversation the match belongs to.
   */
  const reachable = (session: SessionInfo): boolean =>
    !searching || hit(session) || hitsBelow(session.id) > 0;

  const collect = (parentId: string, depth: number, rootId: string, out: RailChild[]): void => {
    for (const child of childrenOf.get(parentId) ?? []) {
      if (!reachable(child)) continue;
      out.push({ session: child, depth, rootId, childCount: size(child.id) });
      collect(child.id, depth + 1, rootId, out);
    }
  };

  const rail: RailRoot[] = [];
  for (const root of roots) {
    if (!reachable(root)) continue;
    // A search opens every root it kept: the children are the reason it is here.
    // So does being *in* one of a root's sub-agent conversations.
    const open = searching || options.open.has(root.id) || root.id === revealRoot;
    const children: RailChild[] = [];
    if (open) collect(root.id, 1, root.id, children);
    rail.push({
      session: root,
      childCount: size(root.id),
      open,
      holdsView: root.id === revealRoot,
      children,
    });
  }
  return rail;
}
