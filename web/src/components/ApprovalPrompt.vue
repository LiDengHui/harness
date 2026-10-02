<script setup lang="ts">
import { useI18n } from 'vue-i18n';

import type { PendingApproval } from '../stores/transcript';

const { t } = useI18n();

defineProps<{ items: PendingApproval[] }>();
const emit = defineEmits<{ answer: [toolCallId: string, approved: boolean] }>();

/**
 * The call's arguments as text.
 *
 * They are arbitrary JSON, so they are pretty-printed rather than shown as one
 * compact line: the decision is made from them, and a nested object is unreadable
 * without the indentation. A bare string is already text and is left alone.
 */
function argumentsText(item: PendingApproval): string {
  if (typeof item.arguments === 'string') return item.arguments;
  return JSON.stringify(item.arguments, null, 2);
}
</script>

<template>
  <section class="approval" role="dialog" :aria-label="t('chat.approval.heading')">
    <header class="approval-head">
      <h3 class="approval-title">{{ t('chat.approval.heading') }}</h3>
      <span class="tag approval-count">{{ t('chat.approval.waiting', { count: items.length }) }}</span>
    </header>

    <ul class="approval-list">
      <li v-for="item in items" :key="item.toolCallId" class="approval-item">
        <div class="approval-tool mono">{{ t('chat.approval.tool', { name: item.name }) }}</div>
        <p class="approval-reason">{{ t('chat.approval.reason', { reason: item.reason }) }}</p>
        <template v-if="argumentsText(item) && argumentsText(item) !== '{}'">
          <div class="approval-label muted tiny">{{ t('chat.tool.arguments') }}</div>
          <pre class="approval-args mono">{{ argumentsText(item) }}</pre>
        </template>
        <div class="approval-actions">
          <button
            class="btn btn-small"
            type="button"
            @click="emit('answer', item.toolCallId, false)"
          >
            {{ t('chat.approval.deny') }}
          </button>
          <button
            class="btn btn-primary btn-small"
            type="button"
            @click="emit('answer', item.toolCallId, true)"
          >
            {{ t('chat.approval.approve') }}
          </button>
        </div>
      </li>
    </ul>
  </section>
</template>

<style scoped>
/*
 * A non-modal panel, deliberately: there is no backdrop, so the session rail and
 * the top navigation stay clickable while a decision waits — the user is not
 * trapped by it. It is absolutely positioned inside the chat column, which is
 * what keeps it off the rail on desktop, and it is docked in the middle of that
 * column so it never covers the chat header above it or the composer below.
 *
 * The z-index is capped at 5 on purpose. It must lose to the phone drawer (30)
 * and the tab bar (10) on every viewport, so a pending decision can never cover
 * navigation. Raising it would break that guarantee; the box-shadow, not a
 * scrim, is what makes it read as a panel.
 */
.approval {
  position: absolute;
  top: 50%;
  right: 0.75rem;
  transform: translateY(-50%);
  z-index: 5;
  width: min(26rem, calc(100% - 1.5rem));
  max-height: 70%;
  overflow-y: auto;
  display: flex;
  flex-direction: column;
  gap: 0.5rem;
  padding: 0.75rem;
  border: 1px solid var(--warn);
  border-radius: 6px;
  background: var(--panel);
  box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
  font-size: 0.82rem;
}

.approval-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
}

.approval-title {
  margin: 0;
  font-size: 0.85rem;
}

.approval-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: 0.6rem;
}

.approval-item {
  display: flex;
  flex-direction: column;
  gap: 0.35rem;
  padding-top: 0.6rem;
  border-top: 1px solid var(--border);
}

.approval-item:first-child {
  padding-top: 0;
  border-top: none;
}

.approval-tool {
  font-weight: 600;
  overflow-wrap: anywhere;
}

.approval-reason {
  margin: 0;
  overflow-wrap: anywhere;
}

/* The densest text in the panel: it scrolls inside rather than growing it. */
.approval-args {
  margin: 0;
  padding: 0.4rem 0.5rem;
  background: var(--code);
  border-radius: 3px;
  font-size: 0.76rem;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 12rem;
  overflow: auto;
}

.approval-actions {
  display: flex;
  justify-content: flex-end;
  gap: 0.4rem;
  margin-top: 0.15rem;
}

/*
 * Phone: the panel takes nearly the full width, since there is no rail column to
 * keep clear — the rail is a drawer above it — and the decision buttons are tap
 * targets.
 */
@media (max-width: 640px) {
  .approval {
    right: 0.5rem;
    width: calc(100% - 1rem);
    max-height: 60%;
  }

  .approval-actions .btn {
    min-height: 44px;
  }
}
</style>
