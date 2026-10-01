<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import type { TimelineEntry } from '../stores/transcript';
import ErrorLine from './ErrorLine.vue';
import TurnView from './TurnView.vue';

defineProps<{ entries: TimelineEntry[] }>();

const { t } = useI18n();

const time = (at: number) => new Date(at).toLocaleTimeString();

/** A status this build cannot name is shown as the server sent it. */
const STATUS_KEYS: Record<string, string | undefined> = {
  pending: 'views.entries.status.pending',
  running: 'views.entries.status.running',
  succeeded: 'views.entries.status.succeeded',
  failed: 'views.entries.status.failed',
  skipped: 'views.entries.status.skipped',
};

function statusLabel(status: string): string {
  const key = STATUS_KEYS[status];
  return key === undefined ? status : t(key);
}
</script>

<template>
  <template v-for="entry in entries" :key="entry.id">
    <div v-if="entry.kind === 'user'" class="user-entry">
      <span class="mono muted">{{ time(entry.at) }}</span>
      <p>{{ entry.text }}</p>
    </div>

    <TurnView v-else-if="entry.kind === 'turn'" :turn="entry" />

    <div v-else-if="entry.kind === 'handoff'" class="handoff mono">
      <span class="tag tag-lane">{{ t('views.entries.handoff') }}</span>
      {{ entry.from }} → {{ entry.to }}: {{ entry.reason }}
    </div>

    <div v-else-if="entry.kind === 'subtask'" class="subtask">
      <header class="mono">
        <span class="tag" :class="`tag-${entry.status}`">
          {{ t('views.entries.subtask', { status: statusLabel(entry.status) }) }}
        </span>
        <span class="muted">{{ entry.subtaskId }}</span>
      </header>
      <p>{{ entry.objective }}</p>
      <p v-if="entry.summary" class="muted">{{ entry.summary }}</p>
    </div>

    <ErrorLine v-else :entry="entry" />
  </template>
</template>

<style scoped>
.user-entry {
  margin: 0.75rem 0;
  padding: 0.5rem 0.7rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--panel-2);
}

.user-entry p {
  margin: 0.2rem 0 0;
  white-space: pre-wrap;
  word-break: break-word;
}

.handoff {
  margin: 0.5rem 0;
  font-size: 0.78rem;
  color: var(--muted);
  /* `anywhere` rather than `break-word` because it also shrinks the min-content
     width — a long lane id then cannot push the transcript pane wide. */
  overflow-wrap: anywhere;
}

.subtask {
  margin: 0.5rem 0;
  padding: 0.4rem 0.7rem;
  border: 1px solid var(--border);
  border-radius: 4px;
}

.subtask header {
  overflow-wrap: anywhere;
}

.subtask p {
  margin: 0.25rem 0 0;
  font-size: 0.85rem;
  overflow-wrap: anywhere;
}
</style>
