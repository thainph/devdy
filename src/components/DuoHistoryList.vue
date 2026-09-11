<script setup lang="ts">
/**
 * The saved-Duo history list (sidebar). Pure switcher: it reads the persisted
 * history from the orchestrator store and triggers restore / new / delete.
 * Optionally scoped to a project so it can live in the project screen's rail.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Plus, Trash2 } from 'lucide-vue-next'
import { useOrchestratorStore } from '@/stores/orchestrator'
import { Button } from '@/components/ui'

const props = defineProps<{ projectId?: string }>()

const { t } = useI18n()
const orch = useOrchestratorStore()

const isActive = computed(() => orch.state.active)
const entries = computed(() =>
  props.projectId ? orch.history.filter((h) => h.projectId === props.projectId) : orch.history,
)

function formatWhen(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return d.toLocaleString([], { month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' })
}

async function load(id: string) {
  // Allow re-selecting the active duo (no-op), block switching away while running.
  if (isActive.value && orch.state.id !== id) return
  await orch.restore(id)
}
function newDuo() {
  if (isActive.value) return
  orch.newDraft()
}
function remove(id: string) {
  orch.deleteHistory(id)
}
</script>

<template>
  <div class="flex flex-col overflow-hidden h-full">
    <!-- Toolbar mirrors the Session tab: a full-width primary action. -->
    <div class="p-3 border-b border-border/60 shrink-0">
      <Button class="w-full" :disabled="isActive" @click="newDuo">
        <Plus class="h-3.5 w-3.5" :stroke-width="2" />
        {{ t('duo.action.newDuo') }}
      </Button>
    </div>
    <div class="flex-1 overflow-y-auto">
      <p v-if="!entries.length" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
        {{ t('duo.noHistory') }}
      </p>
      <div
        v-for="h in entries"
        :key="h.id"
        class="group relative border-b border-border/30 transition-colors hover:bg-accent/40"
        :class="{ 'bg-accent/60': orch.state.id === h.id }"
      >
        <span v-if="orch.state.id === h.id" class="absolute left-0 inset-y-0 w-0.5 bg-primary" />
        <button
          class="w-full text-left px-3 py-2.5 cursor-pointer focus:outline-none disabled:cursor-not-allowed"
          :disabled="isActive && orch.state.id !== h.id"
          @click="load(h.id)"
        >
          <div class="flex items-center gap-1.5">
            <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight">
              {{ h.goal?.trim() || `${h.designerLabel} ↔ ${h.reviewerLabel}` }}
            </span>
          </div>
          <div class="mt-1 flex items-center gap-1.5 text-[10px] text-muted-foreground">
            <span class="font-mono">{{ h.designerEngine }}↔{{ h.reviewerEngine }}</span>
            <span>·</span>
            <span>{{ t('duo.roundCounter', { n: h.round, max: h.maxRounds }) }}</span>
            <span
              class="ml-auto rounded px-1 py-px text-[9px] font-medium"
              :class="h.phase === 'done' ? 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'
                : h.phase === 'error' ? 'bg-amber-500/15 text-amber-600 dark:text-amber-400'
                : 'bg-muted text-muted-foreground'"
            >{{ t(`duo.phase.${h.phase}`) }}</span>
          </div>
          <div class="mt-0.5 text-[10px] text-muted-foreground/70">{{ formatWhen(h.savedAt) }}</div>
        </button>
        <button
          v-if="!isActive"
          class="absolute top-2 right-2 h-6 w-6 rounded-md flex items-center justify-center text-foreground/40 hover:text-destructive hover:bg-background opacity-0 group-hover:opacity-100 transition-opacity cursor-pointer"
          :title="t('duo.action.deleteHistory')"
          @click.stop="remove(h.id)"
        >
          <Trash2 class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </div>
    </div>
  </div>
</template>
