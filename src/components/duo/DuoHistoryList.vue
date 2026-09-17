<script setup lang="ts">
/**
 * The saved-Duo history list (sidebar). Pure switcher: it reads the persisted
 * history from the orchestrator store and triggers restore / new / delete.
 * Optionally scoped to a project so it can live in the project screen's rail.
 *
 * Mirrors the Session tab's History: a pinned header (title + Clear all +
 * search) that never scrolls away, and per-row actions behind one overflow (⋯)
 * menu instead of a bare icon button.
 */
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { MoreHorizontal, Plus, Search, Trash2, X, ExternalLink } from 'lucide-vue-next'
import { useOrchestratorStore, type OrchestratorStore } from '@/stores/orchestrator'
import { Button, Input, DropdownMenu, DropdownItem, DropdownSeparator } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'

type DuoHistoryEntry = OrchestratorStore['history'][number]

const props = defineProps<{ projectId?: string }>()
const emit = defineEmits<{ openSession: [runId: string] }>()

const { t } = useI18n()
const orch = useOrchestratorStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const isActive = computed(() => orch.state.active)
// Row whose ⋯ menu is open — keeps that trigger visible while the menu is up.
const menuOpenId = ref<string | null>(null)
/** Everything saved for this rail's project — the set "Clear all" acts on. */
const projectEntries = computed(() =>
  props.projectId ? orch.history.filter((h) => h.projectId === props.projectId) : orch.history,
)

// Free-text filter over what the row actually shows (goal/labels, engines,
// phase), so whatever the user reads in the list is what they can type to find.
const search = ref('')
const entries = computed(() => {
  const q = search.value.trim().toLowerCase()
  if (!q) return projectEntries.value
  return projectEntries.value.filter((h) =>
    [
      entryLabel(h),
      h.goal,
      h.designerLabel,
      h.reviewerLabel,
      h.designerEngine,
      h.reviewerEngine,
      h.phase,
      t(`duo.phase.${h.phase}`),
    ]
      .join(' ')
      .toLowerCase()
      .includes(q),
  )
})

/** Row title: the goal if there is one, else the two side names. */
function entryLabel(h: DuoHistoryEntry): string {
  return h.goal?.trim() || `${h.designerLabel} ↔ ${h.reviewerLabel}`
}

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
async function remove(h: DuoHistoryEntry) {
  if (!(await confirm({
    title: t('duo.deleteHistoryTitle'),
    message: t('duo.deleteHistoryMessage', { label: entryLabel(h) }),
    confirmLabel: t('common.delete'),
  }))) return
  orch.deleteHistory(h.id)
}
/** Clear only what this rail lists — a project rail must not wipe other projects. */
async function clearAll() {
  const count = projectEntries.value.length
  if (!count) return
  if (!(await confirm({
    title: t('duo.clearHistoryTitle'),
    message: t('duo.clearHistoryMessage', { n: count }),
    confirmLabel: t('duo.clearAll'),
  }))) return
  orch.clearHistory(props.projectId)
  search.value = ''
  toast.success(t('duo.historyCleared'))
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

    <!-- Pinned header: only the rows below it scroll, so the filter and the
         bulk action stay reachable however far down the list the user is. -->
    <div class="shrink-0 border-b border-border/40">
      <div class="flex items-center justify-between px-4 py-2.5">
        <p class="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider">{{ t('duo.historyTitle') }}</p>
        <button
          v-if="projectEntries.length > 0"
          class="flex items-center gap-1 text-[10px] text-muted-foreground/70 hover:text-destructive transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
          :disabled="isActive"
          :title="isActive ? t('duo.clearAllBlocked') : t('duo.clearAllTitle')"
          @click="clearAll"
        >
          <Trash2 class="h-3 w-3" :stroke-width="1.75" />
          {{ t('duo.clearAll') }}
        </button>
      </div>
      <div v-if="projectEntries.length > 0" class="px-3 pb-2.5">
        <div class="relative">
          <Search class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
          <Input
            v-model="search"
            :placeholder="t('duo.searchHistoryPlaceholder')"
            class="h-8 pl-8 pr-8 text-xs"
          />
          <button
            v-if="search"
            type="button"
            class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground cursor-pointer"
            :title="t('duo.clearSearch')"
            @click="search = ''"
          >
            <X class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
        </div>
      </div>
    </div>

    <div class="flex-1 min-h-0 overflow-y-auto">
      <p v-if="!projectEntries.length" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
        {{ t('duo.noHistory') }}
      </p>
      <p v-else-if="!entries.length" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
        {{ t('duo.noMatchingHistory') }}
      </p>
      <div
        v-for="h in entries"
        :key="h.id"
        class="group relative border-b border-border/30 transition-colors hover:bg-accent/40 focus-within:bg-accent/40"
        :class="{ 'bg-accent/60': orch.state.id === h.id }"
      >
        <span v-if="orch.state.id === h.id" class="absolute left-0 inset-y-0 w-0.5 bg-primary" />
        <button
          class="w-full text-left px-3 py-2.5 cursor-pointer focus:outline-none disabled:cursor-not-allowed"
          :disabled="isActive && orch.state.id !== h.id"
          @click="load(h.id)"
        >
          <div class="flex items-center gap-1.5">
            <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight" :title="entryLabel(h)">
              {{ entryLabel(h) }}
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

        <!-- Actions: one overflow (⋯) menu revealed on hover/focus, like the
             Session rows. Anchored to the timestamp line so it never covers the
             title or the phase badge. Delete is disabled while a duo is running. -->
        <div
          class="absolute right-2 bottom-2 flex items-center opacity-0 pointer-events-none transition-opacity group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto"
          :class="{ '!opacity-100 !pointer-events-auto': menuOpenId === h.id }"
        >
          <DropdownMenu align="right" @update:open="(o) => (menuOpenId = o ? h.id : null)">
            <template #trigger>
              <button
                type="button"
                class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground/70 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                :aria-label="t('duo.moreActions')"
                :title="t('duo.moreActions')"
              >
                <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
              </button>
            </template>

            <DropdownItem v-if="h.designerRunId" @click="emit('openSession', h.designerRunId)">
              <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
              {{ t('duo.action.openSide', { label: h.designerLabel || t('duo.designer') }) }}
            </DropdownItem>
            <DropdownItem v-if="h.reviewerRunId" @click="emit('openSession', h.reviewerRunId)">
              <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
              {{ t('duo.action.openSide', { label: h.reviewerLabel || t('duo.reviewer') }) }}
            </DropdownItem>
            <DropdownSeparator v-if="h.designerRunId || h.reviewerRunId" />
            <DropdownItem variant="destructive" :disabled="isActive" @click="remove(h)">
              <Trash2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
              {{ t('duo.action.deleteHistory') }}
            </DropdownItem>
          </DropdownMenu>
        </div>
      </div>
    </div>
  </div>
</template>
