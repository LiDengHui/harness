<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue';
import { useI18n } from 'vue-i18n';
import { RouterLink, RouterView } from 'vue-router';

import LanguageSwitcher from './components/LanguageSwitcher.vue';
import { useWebSocket } from './composables/useWebSocket';
import { useSessionStore } from './stores/session';

const { t } = useI18n();
const sessions = useSessionStore();
let unsubscribe: (() => void) | null = null;

onMounted(() => {
  const ws = useWebSocket();
  // Every frame goes to the store, sub-agent lanes included; the store decides
  // where each one belongs. The socket is opened once, here.
  unsubscribe = ws.onFrame(sessions.ingest);
  ws.connect();
});

onUnmounted(() => {
  unsubscribe?.();
});
</script>

<template>
  <div class="app">
    <header class="app-head">
      <span class="brand mono">{{ t('common.appTitle') }}</span>
      <nav class="nav">
        <RouterLink to="/">{{ t('common.nav.chat') }}</RouterLink>
        <RouterLink to="/agents">{{ t('common.nav.agents') }}</RouterLink>
        <RouterLink to="/extensions">{{ t('common.nav.extensions') }}</RouterLink>
      </nav>
      <LanguageSwitcher class="lang-slot" />
    </header>
    <div class="app-body">
      <RouterView />
    </div>
    <!--
      The phone's navigation. It carries the same three routes as the top nav
      rather than replacing it in the markup, because which of the two is shown
      is a media query's decision and `display: none` keeps the hidden one out of
      the accessibility tree as well.
    -->
    <nav class="tabbar" :aria-label="t('common.nav.ariaLabel')">
      <RouterLink to="/">{{ t('common.nav.chat') }}</RouterLink>
      <RouterLink to="/agents">{{ t('common.nav.agents') }}</RouterLink>
      <RouterLink to="/extensions">{{ t('common.nav.extensions') }}</RouterLink>
    </nav>
  </div>
</template>

<style scoped>
.app {
  display: flex;
  flex-direction: column;
  height: 100%;
  min-height: 0;
}

.app-head {
  display: flex;
  align-items: center;
  gap: 1.25rem;
  padding: 0 0.9rem;
  height: 2.4rem;
  border-bottom: 1px solid var(--border);
  background: var(--panel);
  flex: none;
}

.brand {
  font-weight: 700;
  letter-spacing: 0.02em;
}

.nav {
  display: flex;
  gap: 0.25rem;
}

.nav a {
  font-size: 0.82rem;
  color: var(--muted);
  text-decoration: none;
  padding: 0.25rem 0.6rem;
  border-radius: 3px;
}

.nav a:hover {
  color: var(--fg);
  background: var(--panel-2);
}

.nav a.router-link-active {
  color: var(--fg);
  background: var(--panel-2);
}

.lang-slot {
  margin-left: auto;
}

.app-body {
  flex: 1;
  min-height: 0;
}

/* Phone-only; the top nav is the navigation everywhere else. */
.tabbar {
  display: none;
}

/*
 * Phone (≤ 640px). The layout does not change — the same column, the same
 * routes — only where the navigation sits and how much room the header takes.
 * Everything below is scoped to this query, so tablet and desktop render the
 * header and the body exactly as they did before.
 */
@media (max-width: 640px) {
  /*
   * One row of chrome: the title and the language switcher, nothing else. The
   * connection badge lives in the chat header just below and is deliberately
   * left there — a live-status tool has to show whether it is connected without
   * anyone opening a menu, so nothing here may swallow that row.
   */
  .app-head {
    gap: 0.5rem;
    /* As tall as a tap target, so the switcher's 44px hit area fits the row. */
    height: var(--touch-target, 44px);
    padding: 0 0.75rem;
  }

  .nav {
    display: none;
  }

  /*
   * The tab bar is fixed and opaque, so the body clears it by exactly the bar's
   * height: content ends where the bar begins, and the last row of every view
   * stays reachable. The fallback is the bar's own two tokens, for the case
   * where the published variable is not there.
   */
  .app-body {
    padding-bottom: var(--tabbar-height, calc(var(--tabbar-content-height, 56px) + var(--safe-bottom, 0px)));
  }

  .tabbar {
    display: flex;
    position: fixed;
    left: 0;
    right: 0;
    bottom: 0;
    z-index: 10;
    /*
     * The shell's half of the contract, spelled the way `styles.css` states it:
     * 56px of bar, then the home-indicator inset below it, so the bar's total
     * height is `--tabbar-height` to the pixel.
     */
    height: var(--tabbar-content-height, 56px);
    padding-bottom: var(--safe-bottom, 0px);
    border-top: 1px solid var(--border);
    background: var(--panel);
  }

  /*
   * Each tab fills the bar's full 56px, well past the 44px a thumb needs. The
   * transparent top border is the active marker's space, so marking a tab does
   * not shift the row.
   */
  .tabbar a {
    flex: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    min-height: var(--touch-target, 44px);
    font-size: 0.82rem;
    color: var(--muted);
    text-decoration: none;
    border-top: 2px solid transparent;
  }

  .tabbar a.router-link-active {
    color: var(--fg);
    background: var(--panel-2);
    border-top-color: var(--accent);
  }
}
</style>
