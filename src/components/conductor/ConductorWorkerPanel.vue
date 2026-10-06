<script setup lang="ts">
/**
 * Worker panel for a conductor session — the only conductor-specific UI. It sits
 * in the run viewer's right sidebar and shows the worker tree (with a "needs
 * permission" flag) plus the control-plane event timeline. Everything else about
 * a conductor is just the normal session screen.
 *
 * Clicking a worker opens that worker's own session IN-PLACE (it is an ordinary
 * run), replacing the main view in the current window while this panel stays put.
 * A "back to conductor" row returns the main view to the conductor.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronLeft, Cpu, Loader2, ShieldQuestion } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import type { ConductorWorker, ConductorEvent } from '@/stores/conductor'

const props = defineProps<{
  workers: ConductorWorker[]
  events: ConductorEvent[]
  /** The run currently shown in the main view — highlights that worker's row. */
  activeWorkerId?: string | null
  /** The conductor this panel is anchored to (used to detect "viewing a worker"). */
  conductorRunId?: string | null
}>()
const emit = defineEmits<{
  openSession: [runId: string]
  backToConductor: []
}>()

// A worker (not the conductor itself) is the open run — offer a way back.
const viewingWorker = computed(
  () => !!props.activeWorkerId && props.activeWorkerId !== props.conductorRunId,
)
function isActive(w: ConductorWorker): boolean {
  return !!props.activeWorkerId && props.activeWorkerId === w.worker_id
}

const { t } = useI18n()
const live = useLiveRunsStore()
const claudeStore = useClaudeAccountsStore()

// Same rules as RunView's runClaudeAccountLabel: only Claude workers with a
// known account get a badge; Codex / legacy runs show nothing.
function workerAccountLabel(w: ConductorWorker): string {
  if (w.engine !== 'claude' || !w.claude_account_id) return ''
  return claudeStore.accounts.find((a) => a.id === w.claude_account_id)?.label ?? ''
}
// Mirrors RunView's runModelLabel: persisted model, else the engine name.
function workerModelLabel(w: ConductorWorker): string {
  return w.model || w.engine
}

function hasPending(workerId: string): boolean {
  return !!live.get(workerId)?.permissionQueue?.length
}
function shortId(id: string): string {
  return id.slice(0, 8)
}

// Mirror the History (session) rows: a worker is an ordinary run, so render it
// the same way — a status-tinted icon that spins while running, instead of a
// separate status pill.
function isRunning(w: ConductorWorker): boolean {
  return w.status === 'running'
}
function workerIcon(w: ConductorWorker) {
  return isRunning(w) ? Loader2 : Cpu
}
// Status→color, mirroring RunView's runIconColorClass / StatusBadge palette so
// workers read the same as sessions at a glance.
function workerIconColor(w: ConductorWorker): string {
  switch (w.status) {
    case 'running': return 'text-blue-500'
    case 'done': return 'text-emerald-500'
    case 'failed': return 'text-red-500'
    case 'cancelled': return 'text-amber-500'
    case 'fetched': return 'text-violet-500'
    default: return 'text-muted-foreground'
  }
}
function statusTitle(w: ConductorWorker): string {
  const key = `common.status.${w.status}`
  const translated = t(key)
  return translated === key ? w.status : translated
}
function eventSummary(e: ConductorEvent): string {
  const p = (e.payload ?? {}) as Record<string, unknown>
  switch (e.kind) {
    case 'spawn':
      return t('conductor.event.spawn', { role: String(p.role_label ?? 'worker'), id: shortId(String(p.worker_id ?? '')) })
    case 'send':
      return t('conductor.event.send', { id: shortId(String(p.worker_id ?? '')) })
    case 'cancel':
      return t('conductor.event.cancel', { id: shortId(String(p.worker_id ?? '')) })
    default:
      return e.kind
  }
}
function formatWhen(iso: string): string {
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? '' : d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })
}
</script>

<template>
  <div class="flex h-full flex-col overflow-hidden">
    <!-- Workers -->
    <div class="flex-1 min-h-0 flex flex-col overflow-hidden">
      <div class="flex-1 min-h-0 overflow-y-auto">
        <!-- Viewing a worker in-place: a one-tap way back to the conductor's own
             stream, since the worker now fills the main view. -->
        <button
          v-if="viewingWorker"
          type="button"
          class="w-full flex items-center gap-1.5 px-3 py-2 border-b border-border/30 text-[12px] text-muted-foreground hover:text-foreground hover:bg-accent/40 transition-colors cursor-pointer"
          @click="emit('backToConductor')"
        >
          <ChevronLeft class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
          {{ t('conductor.backToConductor') }}
        </button>
        <p v-if="!workers.length" class="px-3 py-6 text-center text-[11px] text-muted-foreground">
          {{ t('conductor.noWorkers') }}
        </p>
        <div
          v-for="w in workers"
          :key="w.worker_id"
          class="group relative border-b border-border/30 transition-colors"
          :class="isActive(w) ? 'bg-accent/60' : 'hover:bg-accent/40'"
        >
          <!-- Selected indicator bar, mirroring the History list's active row. -->
          <span v-if="isActive(w)" class="absolute left-0 inset-y-0 w-0.5 bg-primary" aria-hidden="true" />
          <button
            class="w-full text-left px-3 py-3 cursor-pointer"
            @click="emit('openSession', w.worker_id)"
          >
            <!-- Title + status: a status-tinted icon (spins while running) replaces
                 the status pill, exactly like the History session rows. -->
            <div class="flex items-center gap-2">
              <component
                :is="workerIcon(w)"
                class="h-3.5 w-3.5 shrink-0"
                :class="[workerIconColor(w), { 'animate-spin': isRunning(w) }]"
                :stroke-width="1.75"
                :title="statusTitle(w)"
              />
              <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight" :title="w.title ?? w.worker_id">
                {{ w.title ?? shortId(w.worker_id) }}
              </span>
              <!-- Animated attention marker: this worker is waiting for a
                   permission answer (mirrors the session rows' ping marker). -->
              <span
                v-if="hasPending(w.worker_id)"
                class="relative flex h-3.5 w-3.5 shrink-0 items-center justify-center text-primary"
                :title="t('conductor.needsPermission')"
              >
                <span class="absolute inset-0 animate-ping rounded-full bg-primary/30" />
                <ShieldQuestion class="relative h-3 w-3" :stroke-width="2" />
              </span>
            </div>
            <!-- Meta: account + model badges mirror the session (History) rows. -->
            <div class="mt-2 pl-6 pr-3 flex items-center gap-1.5 text-[10px] text-muted-foreground/70">
              <span
                v-if="workerAccountLabel(w)"
                class="shrink-0 truncate max-w-[10rem] px-1.5 py-0.5 rounded bg-primary/10 text-[9px] font-medium tracking-wide text-primary"
                :title="workerAccountLabel(w)"
              >
                {{ workerAccountLabel(w) }}
              </span>
              <span
                class="min-w-0 truncate px-1.5 py-0.5 rounded bg-muted/60 font-mono text-[9px] tracking-wide text-muted-foreground"
                :title="workerModelLabel(w)"
              >
                {{ workerModelLabel(w) }}
              </span>
              <span class="font-mono">{{ shortId(w.worker_id) }}</span>
            </div>
          </button>
        </div>
      </div>
    </div>

    <!-- Timeline -->
    <div class="h-48 shrink-0 flex flex-col overflow-hidden border-t border-border">
      <div class="px-3 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground border-b border-border/40 shrink-0">
        {{ t('conductor.timeline') }}
      </div>
      <div class="flex-1 min-h-0 overflow-y-auto px-3 py-2 space-y-1">
        <p v-if="!events.length" class="text-[11px] text-muted-foreground text-center py-4">
          {{ t('conductor.noEvents') }}
        </p>
        <div v-for="(e, i) in events" :key="i" class="flex items-start gap-2 text-[11px]">
          <span class="font-mono text-muted-foreground/70 shrink-0">{{ formatWhen(e.ts) }}</span>
          <span class="text-foreground/80">{{ eventSummary(e) }}</span>
        </div>
      </div>
    </div>
  </div>
</template>
