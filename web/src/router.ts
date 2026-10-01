import { createRouter, createWebHistory, type RouteRecordRaw } from 'vue-router';

import ChatView from './views/ChatView.vue';

const routes: RouteRecordRaw[] = [
  { path: '/', name: 'chat', component: ChatView },
  { path: '/agents', name: 'agents', component: () => import('./views/AgentsView.vue') },
  { path: '/extensions', name: 'extensions', component: () => import('./views/ExtensionsView.vue') },
  { path: '/:pathMatch(.*)*', redirect: '/' },
];

/**
 * History mode, so URLs read like `…/agents`. The Rust binary has to answer an
 * unknown path with `index.html` for a deep link to work; the in-app links never
 * leave the SPA.
 */
export const router = createRouter({
  history: createWebHistory(),
  routes,
});
