/**
 * Who owns the access token, and how it survives navigation.
 *
 * The server hands the token over in the URL it opens. The page used to read it
 * back out of `location.search` on every request, which made its availability
 * depend on the address bar — and the router rewrites the address bar. A
 * navigation that dropped the query left every API call answering 401 and every
 * reconnect refused, which is what made a live session look like it had expired.
 *
 * Two halves fix that. `accessToken` captures the token once and hands out the
 * capture, so a request no longer has to trust where the reader has since
 * navigated to; `carryToken` keeps the query on the URL anyway, so a reload, a
 * bookmark and a copied address all still work.
 *
 * Neither half is storage. The capture lives in this module and dies with the
 * page, which is the property reading from the URL had and the reason it was
 * written that way.
 */

import type { LocationQuery, LocationQueryRaw } from 'vue-router';

let captured: string | null = null;

/**
 * The token to authenticate with, captured from the URL the first time it is
 * seen there and remembered afterwards.
 *
 * A URL that names a token always wins, so a server restarted onto a new token
 * takes effect on the next load without anything having to clear the old one.
 */
export function accessToken(search: string): string | null {
  const fromUrl = new URLSearchParams(search).get('token');
  if (fromUrl) captured = fromUrl;
  return captured;
}

/** Forgets the capture, so a test can start from nothing. */
export function forgetAccessToken(): void {
  captured = null;
}

/** The parts of a route this module needs. */
export interface NavigationTarget {
  path: string;
  query: LocationQueryRaw;
  hash: string;
}

/**
 * Keeps the token in the query string across a navigation.
 *
 * A router link drops the query, so without this the address bar stops naming
 * the token the moment the reader leaves the page the server opened. Putting it
 * back keeps the URL the place a reload and a bookmark find it.
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
