<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';

import { TOOL_OUTPUT_PREVIEW_CHARS, previewOf } from '../../stores/transcript';
import type { StoredTool } from './group';

const { t } = useI18n();

const props = defineProps<{ tool: StoredTool }>();

/**
 * Whether the full result is showing rather than the preview, exactly as the
 * live card does it. A stored result can be as long as a live one — a `read_file`
 * of a large file is thousands of characters — so it gets the same leash.
 */
const full = ref(false);

/** The wire name, or `''` for a result whose record named no tool. */
const name = computed(() => (props.tool.kind === 'call' ? props.tool.name : (props.tool.name ?? '')));

/**
 * Tool names as a person reads them — the same table the live card uses, kept
 * here because a replay has to read like the transcript it replays and
 * `ToolCard.vue` does not export it.
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

function label(value: string): string {
  const key = TOOL_NAME_KEYS[value];
  if (key !== undefined) return t(key);
  const mcp = MCP_TOOL_NAME.exec(value);
  if (mcp) return t('chat.toolNames.mcp', { tool: mcp[2], server: mcp[1] });
  return value;
}

/**
 * What the collapsed line calls this. A call is named by its tool; a result that
 * matched no call says that, because that is the fact a reader needs.
 */
const headLabel = computed(() =>
  props.tool.kind === 'call' ? label(props.tool.name) : t('history.tool.orphan'),
);

/** The tool a stray result named, when it named one. */
const headSub = computed(() =>
  props.tool.kind === 'result' && name.value !== '' ? label(name.value) : '',
);

const titleText = computed(() =>
  name.value === '' ? t('history.tool.orphan') : t('chat.tool.nameTitle', { name: name.value }),
);

/** `name · id`, dropping whichever half the record did not carry. */
const idLine = computed(() => {
  if (props.tool.kind === 'call') {
    return props.tool.id === '' ? props.tool.name : `${props.tool.name} · ${props.tool.id}`;
  }
  const parts: string[] = [];
  if (props.tool.name !== null) parts.push(props.tool.name);
  if (props.tool.toolCallId !== null) parts.push(props.tool.toolCallId);
  return parts.join(' · ');
});

const argumentsText = computed(() => {
  if (props.tool.kind !== 'call') return '';
  const value = props.tool.arguments;
  if (typeof value === 'string') return value;
  if (value === undefined) return '';
  return JSON.stringify(value);
});

/** The recorded result: `null` means none was stored, `''` means an empty one. */
const resultText = computed<string | null>(() =>
  props.tool.kind === 'call' ? props.tool.result : props.tool.content,
);

const resultLength = computed(() => resultText.value?.length ?? 0);
const truncated = computed(() => resultLength.value > TOOL_OUTPUT_PREVIEW_CHARS);
const shown = computed(() => {
  const value = resultText.value;
  if (value === null) return '';
  return full.value ? value : previewOf(value, TOOL_OUTPUT_PREVIEW_CHARS);
});
</script>

<template>
  <!--
    A stored tool call reads exactly like a live one: one folded line naming what
    the call was, and the arguments and result a tap away. Two fields are absent
    on purpose — the status dot's colour and the duration — because a stored
    record has neither an outcome nor a timing, and inventing them would make the
    replay lie.
  -->
  <details class="tool">
    <summary class="tool-head" :title="titleText">
      <span class="tool-dot" aria-hidden="true" />
      <span class="tool-name">{{ headLabel }}</span>
      <span v-if="headSub" class="muted">{{ headSub }}</span>
    </summary>

    <div v-if="idLine" class="tool-id mono muted" :title="t('chat.tool.idTitle')">
      {{ idLine }}
    </div>

    <template v-if="argumentsText && argumentsText !== '{}'">
      <div class="tool-label">{{ t('chat.tool.arguments') }}</div>
      <pre class="tool-args mono">{{ argumentsText }}</pre>
    </template>

    <div v-if="resultText === null" class="tool-label">{{ t('history.tool.noResult') }}</div>
    <template v-else-if="resultText !== ''">
      <div class="tool-label">{{ t('chat.tool.output') }}</div>
      <pre class="tool-output mono">{{ shown }}</pre>
    </template>

    <button v-if="truncated" class="btn btn-small tool-toggle" type="button" @click="full = !full">
      {{ full ? t('chat.tool.hideFull') : t('chat.tool.showFull', { count: resultLength }) }}
    </button>
  </details>
</template>

<style scoped>
/*
 * Mirrors `ToolCard.vue`'s card so a replay looks like the transcript it
 * replays. The status rules are dropped rather than copied: a stored call has
 * no outcome, so its dot stays the neutral muted one and no status word appears.
 */
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

.tool-name {
  font-weight: 600;
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
.tool-label + .tool-output {
  margin-top: 0.2rem;
}

.tool-args,
.tool-output {
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

.tool-toggle {
  margin-top: 0.35rem;
}

/* Phone: the folded line has to be hittable, and the card's text is the densest. */
@media (max-width: 640px) {
  .tool {
    padding: 0.4rem 0.5rem;
  }

  .tool-head {
    gap: 0.4rem;
    min-height: 44px;
  }

  .tool-args,
  .tool-output {
    overflow-wrap: anywhere;
    max-height: 16rem;
  }

  .tool-toggle {
    min-height: 44px;
  }
}
</style>
