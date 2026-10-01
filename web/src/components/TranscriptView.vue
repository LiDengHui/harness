<script setup lang="ts">
import { computed, nextTick, ref, watch } from 'vue';
import { useI18n } from 'vue-i18n';

import {
  completionReasonText,
  laneTarget,
  type SubagentLane,
  type SubagentTarget,
  type Transcript,
} from '../stores/transcript';
import EntriesView from './EntriesView.vue';

const { t } = useI18n();

const props = defineProps<{ transcript: Transcript }>();

/**
 * A lane asking to be opened as the conversation it belongs to.
 *
 * The lane is the only place that knows which sub-agent a run was and what it
 * was asked to do, so the request carries both rather than leaving the view to
 * re-derive them from an id it would have to look up.
 */
const emit = defineEmits<{ openSubagent: [target: SubagentTarget] }>();

/** Opens a lane's own conversation, when the server has given it one. */
function openLane(lane: SubagentLane): void {
  const target = laneTarget(lane);
  if (target !== null) emit('openSubagent', target);
}

const pane = ref<HTMLElement | null>(null);

/**
 * A cheap stand-in for the transcript's size, used to decide when to follow the
 * stream. Watching the entries deeply would re-walk every block on every delta.
 */
const revision = computed(() => {
  const { entries } = props.transcript;
  const last = entries[entries.length - 1];
  let size = entries.length * 100_000;
  if (last && last.kind === 'turn') {
    size += last.blocks.length * 1_000;
    const tail = last.blocks[last.blocks.length - 1];
    if (tail && tail.kind !== 'tool') size += tail.text.length;
  }
  return size;
});

watch(revision, () => {
  const element = pane.value;
  if (!element) return;
  const nearBottom = element.scrollHeight - element.scrollTop - element.clientHeight < 160;
  if (!nearBottom) return;
  void nextTick(() => {
    element.scrollTop = element.scrollHeight;
  });
});
</script>

<template>
  <div ref="pane" class="transcript">
    <EntriesView :entries="transcript.entries" />

    <!--
      A sub-agent's lane is machinery too: it stays folded to its one-line
      summary until the reader opens it, like every other card here. Beside the
      fold is the way into the sub-agent's own conversation — the server stores
      each delegated run as a session of its own, and that session is what holds
      the worker's turns. A lane whose frames carried no session id keeps its
      records and says plainly that they cannot be opened yet, rather than
      offering a control that would fetch nothing.
    -->
    <details v-for="(lane, id) in transcript.lanes" :key="id" class="lane">
      <summary class="mono" :title="t('chat.lane.labelTitle')">
        <span class="lane-name">
          {{ t('chat.lane.label') }} <strong>{{ lane.subagentId }}</strong>
        </span>
        <span class="muted">{{ t('chat.lane.entries', { count: lane.entries.length }) }}</span>
        <button
          v-if="lane.subagentSessionId"
          class="btn btn-small lane-open"
          type="button"
          :title="t('chat.lane.openTitle')"
          @click.prevent.stop="openLane(lane)"
        >
          {{ t('chat.lane.open') }}
        </button>
        <span v-else class="muted lane-closed" :title="t('chat.lane.noSessionTitle')">
          {{ t('chat.lane.noSession') }}
        </span>
      </summary>
      <EntriesView :entries="lane.entries" />
    </details>

    <footer v-if="transcript.lastCompletion && transcript.status !== 'running'" class="run-end mono">
      {{ t('chat.runFinished', { reason: completionReasonText(transcript.lastCompletion) }) }}
    </footer>
  </div>
</template>

<style scoped>
.transcript {
  flex: 1;
  overflow-y: auto;
  padding: 0.75rem 1rem 1.5rem;
  min-height: 0;
}

.lane {
  margin: 0.75rem 0 0.75rem 1rem;
  border-left: 2px solid var(--lane);
  padding-left: 0.75rem;
}

.lane summary {
  cursor: pointer;
  font-size: 0.76rem;
  color: var(--muted);
  display: flex;
  gap: 0.6rem;
  align-items: center;
  list-style: none;
}

.lane summary::-webkit-details-marker {
  display: none;
}

.lane summary::before {
  content: '▸';
}

.lane[open] > summary::before {
  content: '▾';
}

/* The lane's own name takes the room; the controls after it keep their size. */
.lane-name {
  flex: 1 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.lane-open {
  flex: none;
}

/* The lane that cannot be opened says so in place of the control. */
.lane-closed {
  flex: none;
  font-style: italic;
  cursor: help;
}

.run-end {
  margin-top: 1rem;
  font-size: 0.74rem;
  color: var(--muted);
  border-top: 1px dashed var(--border);
  padding-top: 0.4rem;
}

/* Phone: the gutter is spent on width, and a lane's indent has to go too. */
@media (max-width: 640px) {
  .transcript {
    padding: 0.6rem 0.6rem 1rem;
  }

  .lane {
    margin-left: 0;
    padding-left: 0.5rem;
  }

  /* The lane's controls drop to their own line rather than squeezing the name
     into an unreadable column, which is also what keeps the row inside a
     phone's width. */
  .lane summary {
    flex-wrap: wrap;
    gap: 0.4rem;
  }

  .lane-open {
    min-height: 36px;
  }
}
</style>
