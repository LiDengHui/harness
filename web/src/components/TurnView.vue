<script setup lang="ts">
import { computed } from 'vue';
import { useI18n } from 'vue-i18n';

import { useNow } from '../composables/useClock';
import { completionReasonText, turnChanges, type TurnEntry } from '../stores/transcript';
import MarkdownView from './MarkdownView.vue';
import ToolCard from './ToolCard.vue';

const { t } = useI18n();

const props = defineProps<{ turn: TurnEntry }>();

const now = useNow();

const open = computed(() => props.turn.endedAt === null);
const durationMs = computed(
  () => (props.turn.endedAt ?? now.value) - props.turn.startedAt,
);
const thinkingChars = (text: string) => text.length.toLocaleString();

/**
 * What this turn changed on disk, or `null` when it wrote nothing.
 *
 * This is the one piece of process the transcript lifts to the top level: a
 * reader wants to know what a turn touched without opening a single card, and
 * the count and the paths come from the turn's own tool calls, not from a
 * second bookkeeping channel.
 */
const changes = computed(() => turnChanges(props.turn));
</script>

<template>
  <section class="turn" :class="{ 'turn-open': open }">
    <header class="turn-head mono">
      <span class="turn-index" :title="t('chat.turn.labelTitle')">
        {{ t('chat.turn.label', { index: turn.index }) }}
      </span>
      <span class="muted">{{ t('chat.turn.agent', { id: turn.agentId }) }}</span>
      <span class="muted">
        {{ open ? t('chat.duration.elapsed', { ms: durationMs }) : t('chat.duration.done', { ms: durationMs }) }}
      </span>
      <span v-if="turn.done" class="tag" :class="turn.done === 'end_turn' ? 'tag-ok' : 'tag-warn'">
        {{ completionReasonText(turn.done) }}
      </span>
    </header>

    <template v-for="block in turn.blocks" :key="block.id">
      <MarkdownView
        v-if="block.kind === 'text'"
        class="answer"
        :source="block.text"
        :copy-label="t('chat.code.copy')"
      />

      <details v-else-if="block.kind === 'thinking'" class="thinking">
        <summary class="mono" :title="t('chat.turn.thinkingTitle')">
          {{ t('chat.turn.thinking', { count: thinkingChars(block.text) }) }}
        </summary>
        <pre class="mono">{{ block.text }}</pre>
      </details>

      <ToolCard v-else :call="block.call" />
    </template>

    <section v-if="changes" class="changes">
      <header class="changes-head mono">
        <span class="changes-count">{{ t('chat.change.summary', { count: changes.files.length }) }}</span>
        <span class="changes-delta" :title="t('chat.change.deltaTitle')">
          <span class="changes-added" :title="t('chat.change.addedTitle')">+{{ changes.added }}</span>
          <span class="changes-removed" :title="t('chat.change.removedTitle')">−{{ changes.removed }}</span>
        </span>
      </header>
      <ul class="changes-list mono">
        <li v-for="file in changes.files" :key="file.path">
          <span class="changes-path">{{ file.path }}</span>
          <span class="changes-delta">
            <span class="changes-added" :title="t('chat.change.addedTitle')">+{{ file.added }}</span>
            <span class="changes-removed" :title="t('chat.change.removedTitle')">−{{ file.removed }}</span>
          </span>
        </li>
      </ul>
    </section>
  </section>
</template>

<style scoped>
.turn {
  border-left: 2px solid var(--border);
  padding-left: 0.75rem;
  margin: 0.75rem 0;
  min-width: 0;
}

.turn-open {
  border-left-color: var(--accent);
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

/* The answer is the point of the turn; everything else is set around it. */
.answer {
  margin: 0.5rem 0;
  min-width: 0;
  overflow-wrap: anywhere;
}

.thinking {
  margin: 0.35rem 0;
  border: 1px dashed var(--border);
  border-radius: 4px;
  padding: 0.3rem 0.5rem;
  background: var(--panel-2);
}

.thinking summary {
  cursor: pointer;
  font-size: 0.72rem;
  color: var(--muted);
  list-style: none;
}

.thinking summary::-webkit-details-marker {
  display: none;
}

.thinking summary::before {
  content: '▸ ';
}

.thinking[open] > summary::before {
  content: '▾ ';
}

.thinking pre {
  margin: 0.4rem 0 0;
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 0.76rem;
  line-height: 1.45;
  color: var(--muted);
  max-height: 22rem;
  overflow: auto;
}

/*
 * The change summary: the one card a reader wants without opening anything.
 * It sits under the answer so it never pushes the answer down.
 */
.changes {
  margin: 0.5rem 0 0.35rem;
  padding: 0.4rem 0.6rem;
  border: 1px solid var(--border);
  border-left: 3px solid var(--ok);
  border-radius: 4px;
  background: var(--panel-2);
  font-size: 0.76rem;
}

.changes-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 0.75rem;
}

.changes-count {
  color: var(--fg);
  font-weight: 600;
}

.changes-delta {
  display: inline-flex;
  gap: 0.5rem;
  white-space: nowrap;
}

.changes-added {
  color: var(--ok);
}

.changes-removed {
  color: var(--error);
}

.changes-list {
  list-style: none;
  margin: 0.35rem 0 0;
  padding: 0;
  color: var(--muted);
}

.changes-list li {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 0.75rem;
}

.changes-path {
  overflow-wrap: anywhere;
}

/*
 * Phone: the turn's indent is reduced to give the content the width, the
 * thinking block keeps its own summary as the tap target, and its body gets a
 * shorter leash — reference text should not own the screen.
 */
@media (max-width: 640px) {
  .turn {
    padding-left: 0.5rem;
  }

  .thinking summary {
    display: flex;
    align-items: center;
    min-height: 44px;
  }

  .thinking summary::before {
    content: '▸';
    margin-right: 0.3rem;
  }

  .thinking[open] > summary::before {
    content: '▾';
  }

  .thinking pre {
    max-height: 14rem;
  }
}
</style>
