import { beforeEach, describe, expect, it } from 'vitest';
import type { LocationQueryRaw } from 'vue-router';

import { accessToken, carryToken, forgetAccessToken, type NavigationTarget } from './token';

const TOKEN = 'd622e608208f088708872f368c610b5c';

const target = (path: string, query: LocationQueryRaw = {}): NavigationTarget => ({
  path,
  query,
  hash: '',
});

beforeEach(() => {
  forgetAccessToken();
});

describe('accessToken', () => {
  it('captures the token from the URL the server opened', () => {
    expect(accessToken(`?token=${TOKEN}`)).toBe(TOKEN);
  });

  it('still answers after a navigation that dropped the query', () => {
    // The bug this exists for: the router rewrites the address bar, so reading
    // the token from it per request is what made a menu click break the app.
    accessToken(`?token=${TOKEN}`);
    expect(accessToken('')).toBe(TOKEN);
  });

  it('lets a URL that names a token win, so a restart takes effect', () => {
    accessToken(`?token=${TOKEN}`);
    expect(accessToken('?token=newer')).toBe('newer');
  });

  it('answers null when no token was ever handed out, as on a loopback bind', () => {
    expect(accessToken('')).toBeNull();
  });

  it('forgets on request, which is what the tests need between cases', () => {
    accessToken(`?token=${TOKEN}`);
    forgetAccessToken();
    expect(accessToken('')).toBeNull();
  });
});

describe('carryToken', () => {
  it('puts the token back on a link that dropped it', () => {
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
