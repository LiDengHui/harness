<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import MarkdownView from '../MarkdownView.vue';
import HistoryToolCard from './HistoryToolCard.vue';
import type { HistoryBlock } from './group';

defineProps<{ blocks: HistoryBlock[] }>();

const { t } = useI18n();
</script>

<template>
  <!--
    One block list, rendered the same way for a turn and for the preamble. The
    shapes mirror the live transcript: text through the Markdown renderer, a tool
    exchange through the same folded card, and everything else folded into a note
    so a system prompt or an unfamiliar role cannot take over the page.
  -->
  <template v-for="(block, index) in blocks" :key="index">
    <MarkdownView
      v-if="block.kind === 'text'"
      class="answer"
      :source="block.text"
      :copy-label="t('chat.code.copy')"
    />

    <HistoryToolCard v-else-if="block.kind === 'tool'" :tool="block.tool" />

    <details v-else-if="block.kind === 'note' && block.note.text !== null" class="note">
      <summary class="mono">
        {{
          block.note.role === 'system'
            ? t('history.system.label')
            : t('history.role.unknown', { role: block.note.role })
        }}
      </summary>
      <pre class="mono">{{ block.note.text }}</pre>
    </details>

    <p v-else class="note-empty mono muted">{{ t('history.message.empty') }}</p>
  </template>
</template>

<style scoped>
/* The answer is the point of a turn; everything else is set around it. */
.answer {
  margin: 0.5rem 0;
  min-width: 0;
  overflow-wrap: anywhere;
}

/*
 * A note is machinery, so it folds like the live transcript's thinking block.
 * A system prompt can be long and a reader rarely wants it inline; a record with
 * no text at all has nothing to fold, so it is one muted line.
 */
.note {
  margin: 0.35rem 0;
  border: 1px dashed var(--border);
  border-radius: 4px;
  padding: 0.3rem 0.5rem;
  background: var(--panel-2);
}

.note summary {
  cursor: pointer;
  font-size: 0.72rem;
  color: var(--muted);
  list-style: none;
}

.note summary::-webkit-details-marker {
  display: none;
}

.note summary::before {
  content: '▸ ';
}

.note[open] > summary::before {
  content: '▾ ';
}

.note pre {
  margin: 0.4rem 0 0;
  white-space: pre-wrap;
  word-break: break-word;
  font-size: 0.76rem;
  line-height: 1.45;
  color: var(--muted);
  max-height: 22rem;
  overflow: auto;
}

.note-empty {
  margin: 0.35rem 0;
  font-size: 0.74rem;
}

@media (max-width: 640px) {
  .note summary {
    display: flex;
    align-items: center;
    min-height: 44px;
  }

  .note summary::before {
    content: '▸';
    margin-right: 0.3rem;
  }

  .note[open] > summary::before {
    content: '▾';
  }

  .note pre {
    max-height: 14rem;
  }
}
</style>
