<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

import { endonym, LOCALES, type Locale } from '../i18n/locales';
import { useLocale } from '../i18n/useLocale';

const { t } = useI18n();
const { locale, setLocale } = useLocale();

/**
 * The next locale in `LOCALES`, wrapping around. With the two locales shipped
 * today that is simply "the other one", and it keeps working if a third is
 * added — no dropdown, one click per switch.
 */
const next = computed<Locale>(() => {
  const index = LOCALES.findIndex((candidate) => candidate.code === locale.value);
  return LOCALES[(index + 1) % LOCALES.length].code;
});

/** Reads as the action, since the button itself reads as the current state. */
const label = computed(() => t('common.language.switch', { language: endonym(next.value) }));
</script>

<template>
  <button type="button" class="lang" :title="label" :aria-label="label" @click="setLocale(next)">
    {{ endonym(locale) }}
  </button>
</template>

<style scoped>
.lang {
  font: inherit;
  font-size: 0.78rem;
  padding: 0.16rem 0.5rem;
  border: 1px solid var(--border);
  border-radius: 3px;
  background: transparent;
  color: var(--muted);
  cursor: pointer;
}

.lang:hover {
  color: var(--fg);
  border-color: var(--muted);
}

/*
 * Phone (≤ 640px): the chip keeps its size, only its hit area grows. The
 * pseudo-element belongs to the button, so a tap anywhere on it is a tap on the
 * button — the box stays chip-sized in a header that has no room to spare, while
 * the target clears the 44px a thumb needs.
 */
@media (max-width: 640px) {
  .lang {
    position: relative;
  }

  .lang::after {
    content: '';
    position: absolute;
    top: 50%;
    left: -6px;
    right: -6px;
    height: var(--touch-target, 44px);
    transform: translateY(-50%);
  }
}
</style>
