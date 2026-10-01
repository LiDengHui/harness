import vue from '@vitejs/plugin-vue';
import { defineConfig } from 'vite';

/**
 * The dev proxy exists so `pnpm dev` talks to a locally running
 * `harness serve` without any CORS story: the page, the REST API and the
 * WebSocket share one origin. In production the Rust binary serves `dist/`
 * itself, so the same relative `/api` and `/ws` paths keep working unchanged.
 *
 * The target is the default `harness.server.port`; change it here if you start
 * the server somewhere else.
 */
const HARNESS_ORIGIN = 'http://127.0.0.1:8787';

export default defineConfig({
  plugins: [vue()],
  server: {
    proxy: {
      '/api': { target: HARNESS_ORIGIN, changeOrigin: true },
      '/ws': { target: HARNESS_ORIGIN, ws: true, changeOrigin: true },
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
});
