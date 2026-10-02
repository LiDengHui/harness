/**
 * The one navigation rule the app needs, kept out of `router.ts` so it can be
 * tested without a DOM — creating a router requires `window`, and the tests run
 * in the `node` environment.
 */

import type { LocationQuery, LocationQueryRaw } from 'vue-router';

/** The parts of a route this module needs. */
export interface NavigationTarget {
  path: string;
  query: LocationQueryRaw;
  hash: string;
}

/**
 * Carries the access token across a navigation.
 *
 * The token arrives in the URL the server prints, and the REST helpers and the
 * WebSocket read it back out of `location.search` — deliberately, so it is never
 * written to storage. A router link drops the query, though, so a single click on
 * the top navigation used to leave the app on a URL that could not authenticate:
 * every API call answered 401, and a reconnect would have been refused outright,
 * which is what made a live session look like it had expired. Putting the token
 * back keeps the address bar the one place it lives, and that is also what keeps
 * a reload and a bookmark working.
 */
export function carryToken(
  to: NavigationTarget,
  from: { query: LocationQuery },
  search: string,
): true | (NavigationTarget & { replace: boolean }) {
  const token =
    to.query.token ?? from.query.token ?? new URLSearchParams(search).get('token') ?? undefined;
  if (typeof token !== 'string' || token === '' || to.query.token === token) return true;
  return { path: to.path, query: { ...to.query, token }, hash: to.hash, replace: true };
}
