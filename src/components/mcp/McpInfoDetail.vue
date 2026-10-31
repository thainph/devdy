<script setup lang="ts">
// Right pane of the MCP servers screen for a row that is not a stored server:
// a built-in native server (injected at run launch; switched on/off here or per
// project, never deleted) or a recommended catalog entry (one click adds it as
// a stub to finish here).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Plus, Power, PowerOff, Settings2 } from 'lucide-vue-next'
import { Badge, Button } from '@/components/ui'
import DetailHeader from '@/components/listDetail/DetailHeader.vue'
import DetailSection from '@/components/listDetail/DetailSection.vue'
import type { CatalogEntry, McpRow } from '@/lib/mcpRows'

const props = defineProps<{
  row: McpRow
  /** Catalog rows: their entry (secret keys to fill in). */
  entry: CatalogEntry | null
  /** Built-in rows: their status label. */
  statusLabel: string
  /** Built-in rows: switched on app-wide. */
  enabled?: boolean
  /** Built-in rows: the switch is being applied. */
  toggling?: boolean
  /** Built-in rows that need setup elsewhere (accounts): show a Settings button. */
  configurable?: boolean
  /** Built-in rows: tool groups to list. */
  toolGroups?: string[]
  adding: boolean
}>()

const emit = defineEmits<{ add: []; settings: []; toggle: [] }>()

const { t } = useI18n()

const isBuiltin = computed(() => props.row.kind === 'builtin')
const meta = computed(() =>
  isBuiltin.value ? props.statusLabel : `${props.row.transport} · ${t('mcp.detail.notAdded')}`,
)
</script>

<template>
  <section class="flex min-w-0 flex-1 flex-col">
    <DetailHeader :title="row.name">
      <template #meta>{{ meta }}</template>
      <template #actions>
        <template v-if="isBuiltin">
          <Button v-if="configurable" variant="outline" size="sm" @click="emit('settings')">
            <Settings2 class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ t('mcp.list.builtin.settings') }}
          </Button>
          <Button :variant="enabled ? 'outline' : 'primary'" size="sm" :disabled="toggling" @click="emit('toggle')">
            <component :is="enabled ? PowerOff : Power" class="h-3.5 w-3.5" :stroke-width="1.75" />
            {{ enabled ? t('mcp.list.disable') : t('mcp.list.enable') }}
          </Button>
        </template>
        <Button v-else size="sm" :disabled="adding" @click="emit('add')">
          <Plus class="h-3.5 w-3.5" :stroke-width="2" />
          {{ adding ? t('mcp.list.adding') : t('common.add') }}
        </Button>
      </template>
    </DetailHeader>

    <div class="min-h-0 flex-1 overflow-y-auto px-5">
      <div class="max-w-xl">
        <DetailSection :title="t('mcp.detail.about')" :hint="isBuiltin ? t('mcp.detail.builtinHint') : t('mcp.detail.addHint')">
          <p class="text-xs leading-relaxed">{{ row.description }}</p>
        </DetailSection>

        <DetailSection v-if="isBuiltin" :title="t('mcp.detail.tools')">
          <code class="font-mono text-xs">{{ row.line }}</code>
          <ul v-if="toolGroups?.length" class="mt-2 space-y-1 text-[11px] text-muted-foreground">
            <li v-for="group in toolGroups" :key="group">{{ group }}</li>
          </ul>
        </DetailSection>

        <DetailSection v-if="isBuiltin" :title="t('mcp.detail.perProject')">
          <p class="text-[11px] leading-relaxed text-muted-foreground">{{ t('mcp.detail.perProjectHint') }}</p>
        </DetailSection>

        <template v-else-if="entry">
          <DetailSection :title="entry.transport === 'stdio' ? t('mcp.detail.command') : t('mcp.editor.url')">
            <code class="block break-all rounded-md bg-muted/60 px-2.5 py-1.5 font-mono text-xs">{{ row.line }}</code>
          </DetailSection>
          <DetailSection v-if="entry.transport === 'stdio'" :title="t('mcp.detail.secretsToFill')">
            <div v-if="entry.env?.length" class="flex flex-wrap gap-1">
              <Badge v-for="key in entry.env" :key="key" tone="neutral" size="sm" class="font-mono">{{ key }}</Badge>
            </div>
            <p v-else class="text-[11px] text-muted-foreground">{{ t('mcp.detail.noSecrets') }}</p>
          </DetailSection>
        </template>
      </div>
    </div>
  </section>
</template>
