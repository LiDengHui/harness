<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';

import { useNow } from '../composables/useClock';
import { TOOL_OUTPUT_PREVIEW_CHARS, previewOf, type ToolCall } from '../stores/transcript';

const { t } = useI18n();

const props = defineProps<{ call: ToolCall }>();

/**
 * Whether the full output is showing rather than the preview. The card itself
 * opens through the native `<details>`, so this is the only toggle the script
 * owns; it is deliberately separate from the disclosure state.
 */
const full = ref(false);
const now = useNow();

const truncated = computed(() => props.call.output.length > TOOL_OUTPUT_PREVIEW_CHARS);
const shown = computed(() =>
  full.value ? props.call.output : previewOf(props.call.output, TOOL_OUTPUT_PREVIEW_CHARS),
);

/** Finished calls report their own duration; running ones count up. */
const durationMs = computed(() => {
  if (props.call.durationMs !== null) return props.call.durationMs;
  if (props.call.endedAt !== null) return props.call.endedAt - props.call.startedAt;
  return Math.max(0, now.value - props.call.startedAt);
});

/** The time reads as done or as still counting, because the number keeps moving. */
const durationLabel = computed(() =>
  props.call.status === 'running'
    ? t('chat.duration.elapsed', { ms: durationMs.value })
    : t('chat.duration.done', { ms: durationMs.value }),
);

/**
 * Tool names as a person reads them.
 *
 * The wire name is what you grep for, so it is never lost: it is the tooltip on
 * the collapsed line and it is spelled out again once the card is open.
 */
const TOOL_NAME_KEYS: Record<string, string> = {
  read_file: 'chat.toolNames.read_file',
  write_file: 'chat.toolNames.write_file',
  edit_file: 'chat.toolNames.edit_file',
  list_dir: 'chat.toolNames.list_dir',
  grep: 'chat.toolNames.grep',
  shell: 'chat.toolNames.shell',
  web_fetch: 'chat.toolNames.web_fetch',
};

/** `mcp__<server>__<tool>`, the shape an MCP-registered tool arrives in. */
const MCP_TOOL_NAME = /^mcp__(.+?)__(.+)$/;

/**
 * What the call was, in words.
 *
 * A name this build has no word for is shown exactly as it came rather than
 * guessed at, and an MCP tool is named by the server it belongs to, which is the
 * part a reader recognises.
 */
const toolLabel = computed(() => {
  const key = TOOL_NAME_KEYS[props.call.name];
  if (key !== undefined) return t(key);
  const mcp = MCP_TOOL_NAME.exec(props.call.name);
  if (mcp) return t('chat.toolNames.mcp', { tool: mcp[2], server: mcp[1] });
  return props.call.name;
});

const argumentsText = computed(() => {
  const value = props.call.arguments;
  if (typeof value === 'string') return value;
  return JSON.stringify(value);
});

/**
 * Statuses arrive as wire tokens, with `running` standing in until the end
 * frame lands. A known one reads as its translation, an unknown one as it came.
 */
function statusLabel(status: string): string {
  switch (status) {
    case 'running':
      return t('chat.toolStatus.running');
    case 'ok':
      return t('chat.toolStatus.ok');
    case 'error':
      return t('chat.toolStatus.error');
    case 'rejected':
      return t('chat.toolStatus.rejected');
    case 'timeout':
      return t('chat.toolStatus.timeout');
    default:
      return status;
  }
}
</script>

<template>
  <!--
    A tool call is machinery, so it reads as one line until it is opened: what
    the call was in plain words, how it ended, and how long it took. The raw tool
    name is the tooltip on that line and is spelled out again below, because it
    is the string a reader searches the logs for. `<details>` keeps the arguments
    and the output one tap away rather than on screen.
  -->
  <details class="tool" :class="`tool-${call.status}`">
    <summary class="tool-head" :title="t('chat.tool.nameTitle', { name: call.name })">
      <span class="tool-dot" aria-hidden="true" />
      <span class="tool-name">{{ toolLabel }}</span>
      <span class="tool-status-word">{{ statusLabel(call.status) }}</span>
      <span class="muted tool-duration">{{ durationLabel }}</span>
      <span v-if="call.lane" class="tag tag-lane mono">{{ call.lane }}</span>
    </summary>

    <div class="tool-id mono muted" :title="t('chat.tool.idTitle')">
      {{ call.name }} · {{ call.toolCallId }}
    </div>

    <template v-if="argumentsText && argumentsText !== '{}'">
      <div class="tool-label">{{ t('chat.tool.arguments') }}</div>
      <pre class="tool-args mono">{{ argumentsText }}</pre>
    </template>

    <template v-if="call.progress.length > 0">
      <div class="tool-label">{{ t('chat.tool.progress') }}</div>
      <ul class="tool-progress mono">
        <li v-for="(line, index) in call.progress" :key="index">{{ line }}</li>
      </ul>
    </template>

    <template v-if="call.output">
      <div class="tool-label">{{ t('chat.tool.output') }}</div>
      <pre class="tool-output mono">{{ shown }}</pre>
    </template>
    <button v-if="truncated" class="btn btn-small tool-toggle" type="button" @click="full = !full">
      {{ full ? t('chat.tool.hideFull') : t('chat.tool.showFull', { count: call.output.length }) }}
    </button>
  </details>
</template>

<style scoped>
.tool {
  border: 1px solid var(--border);
  border-left: 3px solid var(--muted);
  border-radius: 4px;
  background: var(--panel-2);
  padding: 0.4rem 0.6rem;
  margin: 0.35rem 0;
  min-width: 0;
  max-width: 100%;
}

.tool-ok {
  border-left-color: var(--ok);
}

.tool-error,
.tool-timeout {
  border-left-color: var(--error);
}

.tool-rejected {
  border-left-color: var(--warn);
}

.tool-running {
  border-left-color: var(--accent);
}

/*
 * The whole collapsed line is the tap target. The marker is replaced by a
 * caret drawn from the open state, because a flex summary loses the native one.
 */
.tool-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
  font-size: 0.8rem;
  cursor: pointer;
  list-style: none;
}

.tool-head::-webkit-details-marker {
  display: none;
}

.tool-head::before {
  content: '▸';
  color: var(--muted);
  flex: none;
}

.tool[open] > .tool-head::before {
  content: '▾';
}

.tool-dot {
  width: 0.45rem;
  height: 0.45rem;
  border-radius: 50%;
  background: var(--muted);
  flex: none;
}

.tool-ok .tool-dot {
  background: var(--ok);
}

.tool-error .tool-dot,
.tool-timeout .tool-dot {
  background: var(--error);
}

.tool-rejected .tool-dot {
  background: var(--warn);
}

/* A run in flight is the one card that is still moving, and it says so. */
.tool-running .tool-dot {
  background: var(--accent);
  animation: pulse 1s ease-in-out infinite;
}

.tool-name {
  font-weight: 600;
}

/*
 * The status word carries the outcome when the card is collapsed, which is the
 * only place a failure has to be readable without opening it.
 */
.tool-status-word {
  color: var(--muted);
}

.tool-ok .tool-status-word {
  color: var(--ok);
}

.tool-error .tool-status-word,
.tool-timeout .tool-status-word {
  color: var(--error);
  font-weight: 600;
}

.tool-rejected .tool-status-word {
  color: var(--warn);
  font-weight: 600;
}

.tool-running .tool-status-word {
  color: var(--accent);
}

.tool-duration {
  margin-left: auto;
}

.tool-id {
  margin-top: 0.4rem;
  overflow-wrap: anywhere;
}

/* Each block the card opens to says what it is before showing it. */
.tool-label {
  margin-top: 0.5rem;
  font-size: 0.7rem;
  color: var(--muted);
}

.tool-label + .tool-args,
.tool-label + .tool-output,
.tool-label + .tool-progress {
  margin-top: 0.2rem;
}

.tool-args,
.tool-output,
.tool-progress {
  margin: 0.4rem 0 0;
  padding: 0.4rem 0.5rem;
  background: var(--code);
  border-radius: 3px;
  font-size: 0.76rem;
  line-height: 1.45;
  white-space: pre-wrap;
  word-break: break-word;
  max-height: 24rem;
  overflow: auto;
}

.tool-progress {
  list-style: none;
  color: var(--muted);
}

.tool-toggle {
  margin-top: 0.35rem;
}

@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }
  50% {
    opacity: 0.25;
  }
}

/*
 * Phone: the collapsed line has to be hittable with a finger, and a tool's
 * arguments and output are the app's densest text, so they are where a long
 * unbroken token would otherwise widen the page; `anywhere` breaks one rather
 * than letting the card grow.
 */
@media (max-width: 640px) {
  .tool {
    padding: 0.4rem 0.5rem;
  }

  .tool-head {
    gap: 0.4rem;
    min-height: 44px;
  }

  .tool-args,
  .tool-output,
  .tool-progress {
    overflow-wrap: anywhere;
    max-height: 16rem;
  }

  /* The one control a long output hides behind. */
  .tool-toggle {
    min-height: 44px;
  }
}
</style>
