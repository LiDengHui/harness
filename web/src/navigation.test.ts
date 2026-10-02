import { describe, expect, it } from 'vitest';
import type { LocationQueryRaw } from 'vue-router';

import { carryToken, type NavigationTarget } from './navigation';

const TOKEN = '8fdbe66ff149163c5aa694a2de7a010c';

const target = (path: string, query: LocationQueryRaw = {}): NavigationTarget => ({
  path,
  query,
  hash: '',
});

describe('carryToken', () => {
  it('puts the token back on a link that dropped it', () => {
    // The bug this exists for: clicking the top navigation took the app to
    // `/agents`, the query vanished, and every request from there answered 401.
    expect(carryToken(target('/agents'), { query: {} }, `?token=${TOKEN}`)).toEqual({
      path: '/agents',
      query: { token: TOKEN },
      hash: '',
      replace: true,
    });
  });

  it('takes the token from the route being left, which is where a link starts', () => {
    expect(carryToken(target('/extensions'), { query: { token: TOKEN } }, '')).toEqual({
      path: '/extensions',
      query: { token: TOKEN },
      hash: '',
      replace: true,
    });
  });

  it('leaves a navigation that already carries the token alone', () => {
    // A redirect here would re-enter the guard with the same target.
    expect(carryToken(target('/agents', { token: TOKEN }), { query: {} }, `?token=${TOKEN}`)).toBe(
      true,
    );
  });

  it('does nothing when no token was ever issued, as on a loopback bind', () => {
    expect(carryToken(target('/agents'), { query: {} }, '')).toBe(true);
  });

  it('does nothing for an empty token rather than adding a useless parameter', () => {
    expect(carryToken(target('/agents'), { query: {} }, '?token=')).toBe(true);
  });

  it('keeps the destination own query parameters', () => {
    expect(carryToken(target('/agents', { tab: 'tools' }), { query: {} }, `?token=${TOKEN}`)).toEqual(
      {
        path: '/agents',
        query: { tab: 'tools', token: TOKEN },
        hash: '',
        replace: true,
      },
    );
  });

  it('prefers a token the destination names itself', () => {
    expect(carryToken(target('/agents', { token: 'other' }), { query: {} }, `?token=${TOKEN}`)).toBe(
      true,
    );
  });

  it('carries the fragment too', () => {
    expect(
      carryToken({ path: '/agents', query: {}, hash: '#top' }, { query: {} }, `?token=${TOKEN}`),
    ).toEqual({ path: '/agents', query: { token: TOKEN }, hash: '#top', replace: true });
  });
});
