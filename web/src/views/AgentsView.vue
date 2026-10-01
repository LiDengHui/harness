<script setup lang="ts">
import { computed, onMounted, ref } from 'vue';
import { useI18n } from 'vue-i18n';

import { useAgentsStore } from '../stores/agents';

const { t } = useI18n();
const agents = useAgentsStore();
const selectedId = ref<string | null>(null);

const selected = computed(() => (selectedId.value === null ? null : agents.get(selectedId.value)));

onMounted(() => {
  void agents.load();
});
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>{{ t('views.agents.title') }}</h1>
      <p class="muted">
        <i18n-t keypath="views.agents.intro" scope="global">
          <template #file><span class="mono">*.agent.md</span></template>
          <template #tools><span class="mono">tools</span></template>
        </i18n-t>
      </p>
    </header>

    <p v-if="agents.loading && agents.list.length === 0" class="muted">
      {{ t('views.agents.loading') }}
    </p>
    <p v-else-if="agents.error" class="error-text">{{ agents.error }}</p>
    <p v-else-if="agents.list.length === 0" class="muted">{{ t('views.agents.empty') }}</p>

    <table v-else class="table">
      <thead>
        <tr>
          <th>{{ t('views.agents.columns.id') }}</th>
          <th>{{ t('views.agents.columns.name') }}</th>
          <th>{{ t('views.agents.columns.model') }}</th>
          <th>{{ t('views.agents.columns.tools') }}</th>
          <th>{{ t('views.agents.columns.skills') }}</th>
          <th class="num">{{ t('views.agents.columns.maxTokens') }}</th>
          <th>{{ t('views.agents.columns.source') }}</th>
        </tr>
      </thead>
      <tbody>
        <tr
          v-for="agent in agents.list"
          :key="agent.id"
          :class="{ selected: agent.id === selectedId }"
          @click="selectedId = agent.id === selectedId ? null : agent.id"
        >
          <td class="mono" :data-label="t('views.agents.columns.id')">
            <span class="cell">{{ agent.id }}</span>
          </td>
          <td :data-label="t('views.agents.columns.name')">
            <span class="cell">{{ agent.name }}</span>
          </td>
          <td class="mono" :data-label="t('views.agents.columns.model')">
            <span class="cell">{{ agent.model ?? t('views.agents.inherit') }}</span>
          </td>
          <td :data-label="t('views.agents.columns.tools')">
            <span class="cell">
              <span v-if="agent.tools.length === 0" class="muted">{{ t('views.agents.allTools') }}</span>
              <span v-for="tool in agent.tools" :key="tool" class="tag mono">{{ tool }}</span>
            </span>
          </td>
          <td :data-label="t('views.agents.columns.skills')">
            <span class="cell">
              <span v-if="agent.skills.length === 0" class="muted">—</span>
              <span v-for="skill in agent.skills" :key="skill" class="tag mono">{{ skill }}</span>
            </span>
          </td>
          <td class="num mono" :data-label="t('views.agents.columns.maxTokens')">
            <span class="cell">{{ agent.max_tokens ?? '—' }}</span>
          </td>
          <td
            class="mono path"
            :title="agent.source_path"
            :data-label="t('views.agents.columns.source')"
          >
            <span class="cell">{{ agent.source_path }}</span>
          </td>
        </tr>
      </tbody>
    </table>

    <section v-if="selected" class="detail">
      <header class="detail-head">
        <h2 class="mono">{{ selected.id }}</h2>
        <button class="btn btn-small" type="button" @click="selectedId = null">
          {{ t('common.actions.close') }}
        </button>
      </header>
      <p class="detail-desc">{{ selected.description }}</p>
      <dl class="kv mono">
        <dt>{{ t('views.agents.columns.model') }}</dt>
        <dd>{{ selected.model ?? t('views.agents.detail.modelInherited') }}</dd>
        <dt>{{ t('views.agents.columns.maxTokens') }}</dt>
        <dd>{{ selected.max_tokens ?? t('views.agents.detail.unbounded') }}</dd>
        <dt>{{ t('views.agents.columns.tools') }}</dt>
        <dd>{{ selected.tools.length === 0 ? t('views.agents.detail.everyTool') : selected.tools.join(', ') }}</dd>
        <dt>{{ t('views.agents.columns.skills') }}</dt>
        <dd>{{ selected.skills.length === 0 ? t('views.agents.detail.none') : selected.skills.join(', ') }}</dd>
        <dt>{{ t('views.agents.columns.source') }}</dt>
        <dd>{{ selected.source_path }}</dd>
      </dl>
    </section>
  </div>
</template>

<style scoped>
.page {
  /* The shell's bottom tab bar overlays the page on phone; `--tabbar-height` is
     0px above that breakpoint, so this is the plain 2rem everywhere else. */
  padding: 1rem 1.25rem calc(2rem + var(--tabbar-height, 0px));
  overflow-y: auto;
  height: 100%;
}

.page-head h1 {
  font-size: 1.05rem;
  margin: 0 0 0.25rem;
}

.page-head p {
  margin: 0 0 1rem;
  font-size: 0.82rem;
}

.detail {
  margin-top: 1.25rem;
  border: 1px solid var(--border);
  border-radius: 4px;
  padding: 0.75rem 1rem;
  background: var(--panel);
  max-width: 60rem;
}

.detail-head {
  display: flex;
  align-items: center;
  justify-content: space-between;
}

.detail-head h2 {
  font-size: 0.9rem;
  margin: 0;
}

.detail-desc {
  white-space: pre-wrap;
  font-size: 0.85rem;
  line-height: 1.5;
}

.path {
  max-width: 18rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/*
 * Phone: the table turns into a list of cards, one per agent. The header row is
 * dropped and each cell draws its own column name from `data-label`, so the
 * markup stays a single table and nothing is rendered twice. The value column is
 * `minmax(0, 1fr)` and its `.cell` may break anywhere, which is what keeps a
 * ULID or a Windows path from widening the page.
 */
@media (max-width: 640px) {
  .page {
    padding: 0.75rem 0.85rem calc(1.25rem + var(--tabbar-height, 0px));
  }

  .page-head p {
    font-size: 0.8rem;
  }

  .table,
  .table tbody,
  .table tr,
  .table td {
    display: block;
  }

  .table thead {
    display: none;
  }

  .table tbody tr {
    border: 1px solid var(--border);
    border-radius: 6px;
    background: var(--panel);
    padding: 0.5rem 0.7rem;
    margin-bottom: 0.6rem;
  }

  .table td {
    display: grid;
    grid-template-columns: minmax(5.5rem, 34%) minmax(0, 1fr);
    gap: 0.15rem 0.6rem;
    align-items: baseline;
    padding: 0.14rem 0;
    border-bottom: none;
  }

  .table td::before {
    content: attr(data-label);
    color: var(--muted);
    font-size: 0.68rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .table td.num {
    text-align: left;
  }

  .table .cell {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .table td:first-child .cell {
    font-weight: 600;
  }

  .table td.path {
    max-width: none;
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
  }

  .detail {
    padding: 0.6rem 0.75rem;
  }

  .detail-head {
    gap: 0.5rem;
  }

  .detail-head h2 {
    min-width: 0;
    overflow-wrap: anywhere;
  }

  .detail-desc {
    overflow-wrap: anywhere;
  }

  .detail .kv {
    grid-template-columns: minmax(5rem, 34%) minmax(0, 1fr);
  }
}
</style>
