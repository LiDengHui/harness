import { defineConfig } from 'vitest/config';

/**
 * The tests cover the protocol layer and the transport, both of which are
 * framework-free TypeScript, so the node environment is enough. Nothing under
 * test touches `document`.
 */
export default defineConfig({
  test: {
    environment: 'node',
    include: ['src/**/*.test.ts'],
  },
});
