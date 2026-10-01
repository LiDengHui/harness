<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

import { useWebSocket } from '../composables/useWebSocket';

const { t } = useI18n();

const { state, attempts, queued, lastError, connect } = useWebSocket();

const label = computed(() => {
  switch (state.value) {
    case 'open':
      return t('chat.connection.connected');
    case 'connecting':
      return t('chat.connection.connecting');
    case 'reconnecting':
      return attempts.value > 1
        ? t('chat.connection.reconnecting', { attempt: attempts.value })
        : t('chat.connection.reconnectingPlain');
    case 'closed':
    default:
      return t('chat.connection.disconnected');
  }
});
</script>

<template>
  <div class="connection" :class="`connection-${state}`">
    <span class="dot" aria-hidden="true" />
    <span class="connection-label">{{ label }}</span>
    <span
      v-if="queued > 0"
      class="muted mono"
      :title="t('chat.connection.queuedTitle', { count: queued })"
    >
      {{ t('chat.connection.queued', { count: queued }) }}
    </span>
    <button v-if="state === 'closed'" class="btn btn-small" type="button" @click="connect()">
      {{ t('chat.connection.connect') }}
    </button>
    <span v-if="lastError" class="connection-error" :title="lastError">{{ lastError }}</span>
  </div>
</template>

<style scoped>
.connection {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  min-width: 0;
  font-size: 0.78rem;
}

.dot {
  width: 0.5rem;
  height: 0.5rem;
  border-radius: 50%;
  background: var(--muted);
  flex: none;
}

.connection-open .dot {
  background: var(--ok);
}

.connection-connecting .dot,
.connection-reconnecting .dot {
  background: var(--warn);
}

.connection-closed .dot {
  background: var(--error);
}

.connection-error {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 26rem;
  color: var(--warn);
}

/*
 * Phone: the badge shares a wrapping header row, so it is allowed to fold and
 * its error text is capped to a share of the screen rather than 26rem.
 */
@media (max-width: 640px) {
  .connection {
    flex-wrap: wrap;
    gap: 0.35rem;
  }

  .connection-error {
    max-width: 100%;
    flex-basis: 100%;
  }
}
</style>
