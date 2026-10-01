<script setup lang="ts">
import { computed, ref } from 'vue';
import { useI18n } from 'vue-i18n';

import { latestUsage, totalTokens, type Transcript } from '../stores/transcript';

const { t } = useI18n();

const props = defineProps<{ transcript: Transcript }>();

const latest = computed(() => latestUsage(props.transcript));
const total = computed(() => totalTokens(props.transcript));
const blocked = computed(() => props.transcript.diagnostics.guardrails.filter((note) => note.blocked));

/** Guardrail names as a person reads them; an unknown one keeps its wire name. */
const GUARDRAIL_NAME_KEYS: Record<string, string> = {
  secret_scanner: 'chat.diagnostics.guardrailNames.secret_scanner',
  tool_policy: 'chat.diagnostics.guardrailNames.tool_policy',
  pii_detector: 'chat.diagnostics.guardrailNames.pii_detector',
  llm_judge: 'chat.diagnostics.guardrailNames.llm_judge',
  content_fence: 'chat.diagnostics.guardrailNames.content_fence',
  behavior_monitor: 'chat.diagnostics.guardrailNames.behavior_monitor',
};

function guardrailName(name: string): string {
  const key = GUARDRAIL_NAME_KEYS[name];
  return key === undefined ? name : t(key);
}

/**
 * Whether the samples are showing. The strip is reference data, so it stays one
 * line at every width — the numbers are one tap away rather than always on
 * screen.
 */
const expanded = ref(false);

/** The one number worth carrying on the collapsed line. */
const brief = computed(() => {
  const count = (total.value.input_tokens + total.value.output_tokens).toLocaleString();
  return latest.value === null
    ? t('chat.diagnostics.noReports')
    : t('chat.diagnostics.brief', { count });
});

/** The budget is only interesting once the server has reported one. */
const budget = computed(() => latest.value?.budgetRemaining ?? null);

/**
 * Guardrail names and details come off the wire; the wording of the names is
 * ours, and a run with no notes still gets a sentence saying what is being
 * counted rather than an empty tooltip.
 */
const guardrailTitle = computed(() => {
  const { guardrails } = props.transcript.diagnostics;
  if (guardrails.length === 0) return t('chat.diagnostics.guardrailsTitle');
  return guardrails
    .map(
      (note) =>
        `${guardrailName(note.name)}${note.blocked ? t('chat.diagnostics.blocked') : ''}: ${note.detail}`,
    )
    .join('\n');
});
</script>

<template>
  <section class="diagnostics mono">
    <!-- The whole strip folded into one tappable line. -->
    <button
      class="diagnostics-toggle"
      type="button"
      :aria-expanded="expanded"
      :title="t('chat.diagnostics.toggleTitle')"
      @click="expanded = !expanded"
    >
      <span class="diagnostics-caret" aria-hidden="true">{{ expanded ? '▾' : '▸' }}</span>
      <span class="diagnostics-title">{{ t('chat.diagnostics.title') }}</span>
      <span class="diagnostics-brief">{{ brief }}</span>
      <span v-if="blocked.length > 0" class="sample-error">
        {{ t('chat.diagnostics.guardrailsBlocked', { count: blocked.length }) }}
      </span>
    </button>

    <div v-if="expanded" class="diagnostics-items">
      <template v-if="latest">
        <span class="sample" :title="t('chat.diagnostics.usageTitle')">
          {{
            t('chat.diagnostics.usage', {
              input: latest.usage.input_tokens.toLocaleString(),
              output: latest.usage.output_tokens.toLocaleString(),
            })
          }}
        </span>
        <span class="sample muted" :title="t('chat.diagnostics.totalTitle')">
          {{ t('chat.diagnostics.total', { count: (total.input_tokens + total.output_tokens).toLocaleString() }) }}
        </span>
        <span v-if="budget !== null" class="sample" :class="{ 'sample-warn': budget < 2000 }">
          {{ t('chat.diagnostics.budget', { count: budget.toLocaleString() }) }}
        </span>
        <span v-else class="sample muted">{{ t('chat.diagnostics.budgetUnbounded') }}</span>
      </template>
      <span v-else class="sample muted">{{ t('chat.diagnostics.noReports') }}</span>

      <span class="sample" :class="{ 'sample-error': blocked.length > 0 }" :title="guardrailTitle">
        {{ t('chat.diagnostics.guardrails', { count: transcript.diagnostics.guardrails.length }) }}
        <template v-if="blocked.length > 0">
          {{ t('chat.diagnostics.guardrailsBlocked', { count: blocked.length }) }}
        </template>
      </span>

      <span v-if="Object.keys(transcript.lanes).length > 0" class="sample">
        {{ t('chat.diagnostics.lanes', { lanes: Object.keys(transcript.lanes).join(', ') }) }}
      </span>
    </div>
  </section>
</template>

<style scoped>
.diagnostics {
  border-top: 1px solid var(--border);
  background: var(--panel);
  font-size: 0.74rem;
  color: var(--muted);
}

.diagnostics-toggle {
  display: flex;
  align-items: center;
  gap: 0.4rem;
  width: 100%;
  padding: 0.3rem 0.75rem;
  border: 0;
  background: none;
  color: inherit;
  font: inherit;
  font-size: 0.74rem;
  text-align: left;
  cursor: pointer;
}

.diagnostics-caret {
  color: var(--fg);
}

.diagnostics-title {
  color: var(--fg);
  font-weight: 600;
}

.diagnostics-brief {
  margin-left: auto;
  color: var(--muted);
}

.diagnostics-items {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 0.4rem 0.75rem;
  padding: 0 0.75rem 0.5rem;
}

.sample-warn {
  color: var(--warn);
}

.sample-error {
  color: var(--error);
}

@media (max-width: 640px) {
  .diagnostics-toggle {
    min-height: 44px;
    padding: 0.3rem 0.6rem;
  }

  .diagnostics-items {
    padding: 0 0.6rem 0.5rem;
  }
}
</style>
