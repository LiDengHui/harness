import { createPinia } from 'pinia';
import { createApp } from 'vue';

import App from './App.vue';
import { i18n } from './i18n';
import { initLocale } from './i18n/useLocale';
import { router } from './router';
import './styles.css';

// Runs before the mount so a stored choice is already in place at the first
// paint; when there is none, the `GET /api/ui` probe settles afterwards and the
// default stands until it does.
void initLocale();

createApp(App).use(createPinia()).use(router).use(i18n).mount('#app');
