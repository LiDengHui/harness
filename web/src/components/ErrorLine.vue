<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

import { describeError, type ErrorEntry } from '../stores/transcript';

const { t } = useI18n();

const props = defineProps<{ entry: ErrorEntry }>();

/**
 * A wire code reads as its translation; one we have no word for is shown as it
 * came. The message itself is the server's own text and stays as it arrived.
 *
 * `tool_approval_request` is deliberately absent: `describeError` words it from
 * the fields the frame carries, so the code never has to name it here.
 */
function codeLabel(code: string): string {
  switch (code) {
    case 'busy':
      return t('chat.error.codes.busy');
    case 'not_running':
      return t('chat.error.codes.not_running');
    case 'agent_not_found':
      return t('chat.error.codes.agent_not_found');
    case 'unknown_session':
      return t('chat.error.codes.unknown_session');
    case 'bad_message':
      return t('chat.error.codes.bad_message');
    case 'config':
      return t('chat.error.codes.config');
    case 'session_error':
      return t('chat.error.codes.session_error');
    case 'guardrail':
      return t('chat.error.codes.guardrail');
    default:
      return code;
  }
}

/**
 * The store words the codes it knows; for the rest it hands back the bare code,
 * which is the one thing this line has to name itself.
 */
const shown = computed(() => {
  const described = describeError(props.entry);
  return described.label === props.entry.code
    ? { ...described, label: codeLabel(props.entry.code) }
    : described;
});
</script>

<template>
  <div
    class="inline-error"
    :class="{
      'inline-error-blocked': entry.code === 'guardrail',
      'inline-error-notice': shown.tone === 'notice',
    }"
  >
    <span class="mono" :title="t('chat.error.codeTitle', { code: entry.code })">{{ shown.label }}</span>
    <span>{{ shown.message }}</span>
  </div>
</template>

<style scoped>
.inline-error {
  display: flex;
  gap: 0.6rem;
  margin: 0.5rem 0;
  padding: 0.4rem 0.7rem;
  border: 1px solid var(--error);
  border-radius: 4px;
  background: rgba(220, 80, 80, 0.08);
  color: var(--error);
  font-size: 0.82rem;
}

/* A notice is the transport explaining itself, not the run failing, so it reads
   muted instead of alarming. */
.inline-error-notice {
  border-color: var(--border);
  background: var(--panel-2);
  color: var(--muted);
}

/*
 * Phone: the code and its sentence stack, because side by side they squeeze
 * each other, and a long server message breaks rather than widening the page.
 */
@media (max-width: 640px) {
  .inline-error {
    flex-direction: column;
    gap: 0.15rem;
    overflow-wrap: anywhere;
  }
}
</style>
