<script setup lang="ts">
import { computed, onMounted } from 'vue';
import { useI18n } from 'vue-i18n';

import { pluginCapabilityLabel } from '../protocol/api';
import { useExtensionsStore } from '../stores/extensions';

const { t } = useI18n();
const extensions = useExtensionsStore();

/** How much cheaper the metadata is than the bodies — the point of the split. */
const disclosure = computed(() => {
  if (extensions.bodyTokens === 0) return null;
  return Math.round((extensions.metadataTokens / extensions.bodyTokens) * 100);
});

/** The transport keeps its wire value on screen; the tooltip explains it. */
const KIND_KEYS: Record<string, string | undefined> = {
  stdio: 'views.extensions.mcp.kind.stdio',
  http: 'views.extensions.mcp.kind.http',
};

function kindHint(kind: string): string | undefined {
  const key = KIND_KEYS[kind];
  return key === undefined ? undefined : t(key);
}

onMounted(() => {
  void extensions.load();
});
</script>

<template>
  <div class="page">
    <header class="page-head">
      <h1>{{ t('views.extensions.title') }}</h1>
      <p class="muted">{{ t('views.extensions.intro') }}</p>
    </header>

    <p v-if="extensions.loading && !extensions.loaded" class="muted">
      {{ t('views.extensions.loading') }}
    </p>

    <section v-else-if="!extensions.available" class="empty-state">
      <h2>{{ t('views.extensions.unavailable.title') }}</h2>
      <p>{{ t('views.extensions.unavailable.explanation') }}</p>
      <p v-if="extensions.reason" class="muted mono">{{ extensions.reason }}</p>
      <button class="btn btn-small" type="button" @click="extensions.load(true)">
        {{ t('common.actions.retry') }}
      </button>
    </section>

    <p v-else-if="extensions.isEmpty" class="muted">{{ t('views.extensions.empty') }}</p>

    <template v-else>
      <section class="block">
        <header class="block-head">
          <h2>{{ t('views.extensions.skills.heading') }}</h2>
          <span class="muted mono">
            {{
              t('views.extensions.skills.cost', {
                metadata: extensions.metadataTokens.toLocaleString(),
                bodies: extensions.bodyTokens.toLocaleString(),
              })
            }}
            <template v-if="disclosure !== null">
              {{ t('views.extensions.skills.disclosure', { percent: disclosure }) }}
            </template>
          </span>
        </header>

        <p v-if="extensions.skills.length === 0" class="muted">
          {{ t('views.extensions.skills.empty') }}
        </p>

        <table v-else class="table">
          <thead>
            <tr>
              <th>{{ t('views.extensions.skills.columns.name') }}</th>
              <th>{{ t('views.extensions.skills.columns.description') }}</th>
              <th>{{ t('views.extensions.skills.columns.model') }}</th>
              <th>{{ t('views.extensions.skills.columns.allowedTools') }}</th>
              <th class="num">{{ t('views.extensions.skills.columns.metadataTokens') }}</th>
              <th class="num">{{ t('views.extensions.skills.columns.bodyTokens') }}</th>
              <th>{{ t('views.extensions.skills.columns.source') }}</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="skill in extensions.skills" :key="skill.name">
              <td class="mono" :data-label="t('views.extensions.skills.columns.name')">
                <span class="cell">{{ skill.name }}</span>
              </td>
              <td class="wrap" :data-label="t('views.extensions.skills.columns.description')">
                <span class="cell">{{ skill.description }}</span>
              </td>
              <td class="mono" :data-label="t('views.extensions.skills.columns.model')">
                <span class="cell">{{ skill.model ?? t('views.extensions.skills.inherit') }}</span>
              </td>
              <td :data-label="t('views.extensions.skills.columns.allowedTools')">
                <span class="cell">
                  <span v-if="skill.allowed_tools.length === 0" class="muted">—</span>
                  <span v-for="tool in skill.allowed_tools" :key="tool" class="tag mono">{{ tool }}</span>
                </span>
              </td>
              <td class="num mono muted" :data-label="t('views.extensions.skills.columns.metadataTokens')">
                <span class="cell">{{ skill.metadata_tokens }}</span>
              </td>
              <td class="num mono" :data-label="t('views.extensions.skills.columns.bodyTokens')">
                <span class="cell">{{ skill.body_tokens }}</span>
              </td>
              <td
                class="mono path"
                :title="skill.source_path"
                :data-label="t('views.extensions.skills.columns.source')"
              >
                <span class="cell">{{ skill.source_path }}</span>
              </td>
            </tr>
          </tbody>
        </table>
      </section>

      <section class="block">
        <header class="block-head">
          <h2>{{ t('views.extensions.mcp.heading') }}</h2>
          <span class="muted mono">
            {{ t('views.extensions.mcp.configured', { count: extensions.mcp.length }) }}
          </span>
        </header>

        <p v-if="extensions.mcp.length === 0" class="muted">
          {{ t('views.extensions.mcp.empty') }}
        </p>

        <div v-else class="cards">
          <article v-for="server in extensions.mcp" :key="server.name" class="card">
            <header class="card-head">
              <span class="mono card-name">{{ server.name }}</span>
              <span class="tag mono" :title="kindHint(server.kind)">{{ server.kind }}</span>
              <span class="tag mono" :class="server.enabled ? 'tag-ok' : 'tag-error'">
                {{ server.enabled ? t('views.extensions.mcp.enabled') : t('views.extensions.mcp.disabled') }}
              </span>
              <span v-if="server.lazy" class="tag tag-warn mono">
                {{ t('views.extensions.mcp.lazy') }}
              </span>
            </header>
            <p class="mono muted target" :title="server.target">{{ server.target }}</p>
            <div class="tags">
              <span v-if="server.tools.length === 0" class="muted">
                {{ t('views.extensions.mcp.noTools') }}
              </span>
              <span v-for="tool in server.tools" :key="tool" class="tag mono">{{ tool }}</span>
            </div>
          </article>
        </div>
      </section>

      <section class="block">
        <header class="block-head">
          <h2>{{ t('views.extensions.plugins.heading') }}</h2>
          <span class="muted mono">
            {{ t('views.extensions.plugins.loaded', { count: extensions.plugins.length }) }}
          </span>
        </header>

        <p v-if="extensions.plugins.length === 0" class="muted">
          {{ t('views.extensions.plugins.empty') }}
        </p>

        <div v-else class="cards">
          <article v-for="plugin in extensions.plugins" :key="plugin.name" class="card">
            <header class="card-head">
              <span class="mono card-name">{{ plugin.name }}</span>
              <span class="tag mono">{{ plugin.version }}</span>
              <span class="mono muted">
                {{ t('views.extensions.plugins.entry', { entry: plugin.entry }) }}
              </span>
            </header>
            <p class="mono muted target" :title="plugin.module">{{ plugin.module }}</p>

            <div class="capabilities">
              <span class="capabilities-label">{{ t('views.extensions.plugins.capabilities') }}</span>
              <span v-if="plugin.capabilities.length === 0" class="tag mono tag-ok">
                {{ t('views.extensions.plugins.noCapabilities') }}
              </span>
              <span
                v-for="capability in plugin.capabilities"
                :key="capability"
                class="cap mono"
                :title="capability"
              >
                {{ pluginCapabilityLabel(capability) }}
              </span>
            </div>
          </article>
        </div>
      </section>
    </template>
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
  max-width: 60rem;
}

.block {
  margin-bottom: 1.75rem;
}

.block-head {
  display: flex;
  align-items: baseline;
  gap: 0.75rem;
  margin-bottom: 0.5rem;
  flex-wrap: wrap;
}

.block-head h2 {
  font-size: 0.95rem;
  margin: 0;
}

.block-head .muted {
  font-size: 0.76rem;
}

.empty-state {
  border: 1px dashed var(--border);
  border-radius: 4px;
  padding: 1rem 1.25rem;
  max-width: 46rem;
  background: var(--panel);
}

.empty-state h2 {
  margin: 0 0 0.4rem;
  font-size: 0.95rem;
}

.empty-state p {
  margin: 0 0 0.75rem;
  font-size: 0.85rem;
  line-height: 1.5;
  color: var(--muted);
}

.cards {
  display: grid;
  /*
   * `min(24rem, 100%)` keeps the desktop column width at 24rem exactly while
   * letting a phone, whose content box is narrower than that, fall to one full
   * width column instead of overflowing.
   */
  grid-template-columns: repeat(auto-fill, minmax(min(24rem, 100%), 1fr));
  gap: 0.75rem;
}

.card {
  border: 1px solid var(--border);
  border-radius: 4px;
  background: var(--panel);
  padding: 0.6rem 0.75rem;
}

.card-head {
  display: flex;
  align-items: center;
  gap: 0.5rem;
  flex-wrap: wrap;
  font-size: 0.8rem;
}

.card-name {
  font-weight: 600;
}

.target {
  margin: 0.4rem 0;
  font-size: 0.76rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tags {
  display: flex;
  gap: 0.3rem;
  flex-wrap: wrap;
  font-size: 0.74rem;
}

.capabilities {
  display: flex;
  gap: 0.35rem;
  flex-wrap: wrap;
  align-items: center;
  margin-top: 0.5rem;
  padding-top: 0.5rem;
  border-top: 1px solid var(--border);
}

.capabilities-label {
  font-size: 0.68rem;
  text-transform: uppercase;
  letter-spacing: 0.08em;
  color: var(--muted);
}

.cap {
  font-size: 0.74rem;
  font-weight: 600;
  padding: 0.1rem 0.45rem;
  border-radius: 3px;
  background: var(--cap-bg);
  color: var(--cap-fg);
  border: 1px solid var(--cap-fg);
}

.wrap {
  white-space: normal;
  max-width: 30rem;
}

.path {
  max-width: 14rem;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/*
 * Phone: the skills table turns into one card per skill, each cell drawing its
 * own column name from `data-label`, so the markup stays a single table and the
 * header row is simply dropped. The section headings stick to the top of the
 * scrolling page, which is what keeps three long lists apart on a small screen,
 * and the capability tags stay whole — they are the point of the screen.
 */
@media (max-width: 640px) {
  /*
   * No top padding on the scroll container: a sticky heading pins to the top of
   * the content box, and anything left above it is a gap the rows scroll
   * through. The page header carries the space instead.
   */
  .page {
    padding: 0 0.85rem calc(1.25rem + var(--tabbar-height, 0px));
  }

  .page-head {
    padding-top: 0.75rem;
  }

  .page-head p {
    font-size: 0.8rem;
  }

  .block-head {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--bg);
    border-bottom: 1px solid var(--border);
    padding: 0.45rem 0 0.4rem;
    margin-bottom: 0.6rem;
  }

  .block-head h2 {
    font-size: 1rem;
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

  .table td.wrap {
    max-width: none;
  }

  .card {
    padding: 0.55rem 0.65rem;
  }

  /* A command line or a module path is worth reading in full on a phone, where
     there is no hover for the tooltip the desktop ellipsis leans on. */
  .card .target {
    white-space: normal;
    overflow: visible;
    text-overflow: clip;
    overflow-wrap: anywhere;
  }

  .cap {
    font-size: 0.76rem;
  }
}
</style>
