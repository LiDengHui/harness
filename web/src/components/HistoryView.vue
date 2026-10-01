<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

import HistoryBlocks from './history/HistoryBlocks.vue';
import { groupMessages } from './history/group';
import MarkdownView from './MarkdownView.vue';

const { t } = useI18n();

/**
 * The stored conversation as it came off the wire. Deliberately `readonly
 * unknown[]`: this is a database record replayed over REST, so nothing about it
 * is assumed here — `groupMessages` re-reads and narrows every field, and an
 * array that is not one at all renders the empty state instead of throwing.
 */
const props = defineProps<{ messages: readonly unknown[] }>();

const grouped = computed(() => groupMessages(props.messages));

/** Nothing to replay: an empty array, or one whose entries were all unreadable. */
const isEmpty = computed(
  () => grouped.value.preamble.length === 0 && grouped.value.turns.length === 0,
);
</script>

<template>
  <!--
    A replay of a finished conversation. It reads like the live transcript —
    the same user bubble, the same `reply {index}` heading, the same folded tool
    cards and Markdown — but it says up front what it is not: a stored record has
    no timing, no in-flight state and no token usage, and none of those are shown
    as an empty field.
  -->
  <div class="history">
    <header class="history-head mono">
      <span class="history-title">{{ t('history.head.stored') }}</span>
      <span class="muted">{{ t('history.head.turns', { count: grouped.turns.length }) }}</span>
    </header>
    <p class="history-note muted">{{ t('history.head.note') }}</p>

    <p v-if="isEmpty" class="history-empty muted">{{ t('history.empty.none') }}</p>

    <template v-else>
      <!-- Anything before the first user message: the system prompt, usually. -->
      <section v-if="grouped.preamble.length > 0" class="preamble">
        <div class="preamble-label mono muted">{{ t('history.preamble.label') }}</div>
        <HistoryBlocks :blocks="grouped.preamble" />
      </section>

      <template v-for="turn in grouped.turns" :key="turn.index">
        <div v-if="turn.prompt !== null" class="user-entry">
          <span class="mono muted">{{ t('history.role.user') }}</span>
          <MarkdownView
            class="user-text"
            :source="turn.prompt"
            :copy-label="t('chat.code.copy')"
          />
        </div>

        <!-- A turn whose user message carried nothing but that still opened it. -->
        <section v-if="turn.blocks.length > 0" class="turn">
          <header class="turn-head mono">
            <span class="turn-index" :title="t('chat.turn.labelTitle')">
              {{ t('chat.turn.label', { index: turn.index }) }}
            </span>
          </header>
          <HistoryBlocks :blocks="turn.blocks" />
        </section>
      </template>
    </template>
  </div>
</template>

<style scoped>
/*
 * The pane mirrors `TranscriptView`'s, because this view takes its place when a
 * stored conversation is opened.
 */
.history {
  flex: 1;
  overflow-y: auto;
  padding: 0.75rem 1rem 1.5rem;
  min-height: 0;
}

.history-head {
  display: flex;
  align-items: baseline;
  gap: 0.6rem;
  font-size: 0.72rem;
}

.history-title {
  color: var(--fg);
  font-weight: 600;
}

.history-note {
  margin: 0.3rem 0 0;
  font-size: 0.74rem;
}

.history-empty {
  margin: 1rem 0;
  font-size: 0.85rem;
}

/* The preamble is not a turn, so it is marked apart rather than numbered. */
.preamble {
  margin: 0.75rem 0;
  padding-left: 0.75rem;
  border-left: 2px dashed var(--border);
}

.preamble-label {
  font-size: 0.72rem;
  margin-bottom: 0.35rem;
}

/* The user's own message, in the live transcript's bubble. */
.user-entry {
  margin: 0.75rem 0;
  padding: 0.5rem 0.7rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--panel-2);
}

.user-text {
  margin-top: 0.2rem;
  min-width: 0;
  overflow-wrap: anywhere;
}

/* A turn's rail, exactly as `TurnView` draws it. */
.turn {
  border-left: 2px solid var(--border);
  padding-left: 0.75rem;
  margin: 0.75rem 0;
  min-width: 0;
}

.turn-head {
  display: flex;
  align-items: center;
  gap: 0.6rem;
  flex-wrap: wrap;
  font-size: 0.72rem;
  color: var(--muted);
}

.turn-index {
  color: var(--fg);
  font-weight: 600;
}

@media (max-width: 640px) {
  .history {
    padding: 0.6rem 0.6rem 1rem;
  }

  .preamble,
  .turn {
    margin-left: 0;
    padding-left: 0.5rem;
  }
}
</style>
