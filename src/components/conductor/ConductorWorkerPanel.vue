<script setup lang="ts">
/**
 * Worker panel for a session — the only orchestration-specific UI. It sits in
 * the run viewer's right sidebar and shows the worker list (with a "needs
 * permission" flag) plus the control-plane event timeline. Everything else
 * about the session is just the normal session screen.
 *
 * Clicking a worker opens that worker's own session IN-PLACE (it is an ordinary
 * run), replacing the main view in the current window while this panel stays put.
 * A "back to session" row returns the main view to the session itself.
 *
 * Two shapes:
 *  - expanded: worker rows (density shared with History via
 *    uiLayout.listDensity) + a collapsible, resizable timeline;
 *  - `collapsed`: a thin rail with one status dot per worker.
 */
import { computed, onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useElementSize, useNow } from '@vueuse/core'
import { ChevronLeft, ChevronRight, ShieldQuestion } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import { useRunsStore, runActivityAt, type RunRecord } from '@/stores/runs'
import { useUILayoutStore, TIMELINE_MIN_HEIGHT } from '@/stores/uiLayout'
import {
  compactSpan,
  formatRunTimestamp,
  relativeAge,
  runStatusColorClass,
  runStatusDotClass,
  runStatusIcon,
  shortModelId,
} from '@/lib/runDisplay'
import type { ConductorWorker, ConductorEvent } from '@/stores/conductor'

const props = defineProps<{
  workers: ConductorWorker[]
  events: ConductorEvent[]
  /** The run currently shown in the main view — highlights that worker's row. */
  activeWorkerId?: string | null
  /** The session this panel is anchored to (used to detect "viewing a worker"). */
  conductorRunId?: string | null
  /** Render as the thin collapsed rail (status dots only). */
  collapsed?: boolean
}>()
const emit = defineEmits<{
  openSession: [runId: string]
  backToConductor: []
}>()

// A worker (not the session itself) is the open run — offer a way back.
const viewingWorker = computed(
  () => !!props.activeWorkerId && props.activeWorkerId !== props.conductorRunId,
)
function isActive(w: ConductorWorker): boolean {
  return !!props.activeWorkerId && props.activeWorkerId === w.worker_id
}

const { t } = useI18n()
const live = useLiveRunsStore()
const claudeStore = useClaudeAccountsStore()
const runsStore = useRunsStore()
const uiLayout = useUILayoutStore()
const density = computed(() => uiLayout.listDensity)

// Ticks so a running worker's elapsed time and the relative ages stay fresh.
const now = useNow({ interval: 30_000 })

// Worker run rows (for started/last-activity times), keyed by run id.
const runById = computed(() => {
  const map = new Map<string, RunRecord>()
  for (const r of runsStore.runs) map.set(r.id, r)
  return map
})

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
// "account · model" (model without its "claude-" prefix when `short`).
function workerAccountModel(w: ConductorWorker, short = true): string {
  const model = short ? shortModelId(workerModelLabel(w)) : workerModelLabel(w)
  const acc = workerAccountLabel(w)
  return acc ? `${acc} · ${model}` : model
}

function hasPending(workerId: string): boolean {
  return !!live.get(workerId)?.permissionQueue?.length
}
function shortId(id: string): string {
  return id.slice(0, 8)
}

// Mirror the History rows: a status-tinted icon that spins while running,
// instead of a separate status pill.
function isRunning(w: ConductorWorker): boolean {
  return w.status === 'running'
}
function statusTitle(w: ConductorWorker): string {
  const key = `common.status.${w.status}`
  const translated = t(key)
  return translated === key ? w.status : translated
}

// Row time: a running worker shows how long it has been running; a settled one
// shows the relative age of its last activity (same format as History).
function workerTime(w: ConductorWorker): string {
  const run = runById.value.get(w.worker_id)
  if (!run) return ''
  if (isRunning(w) && run.started_at) {
    return compactSpan(now.value.getTime() - new Date(run.started_at).getTime(), t('conductor.justStarted'))
  }
  return relativeAge(runActivityAt(run), t('run.relNow'), now.value.getTime())
}
function workerTimeTitle(w: ConductorWorker): string {
  const run = runById.value.get(w.worker_id)
  if (!run) return ''
  if (isRunning(w) && run.started_at) {
    return t('conductor.runningSince', { time: formatRunTimestamp(run.started_at) })
  }
  return `${t('run.lastActivity')}: ${formatRunTimestamp(runActivityAt(run))}`
}
function workerTitle(w: ConductorWorker): string {
  return w.title ?? shortId(w.worker_id)
}
// Compact/dense rows fold the full "account · model" (compact shows it
// shortened), the id and the full time into the tooltip.
function workerTooltip(w: ConductorWorker): string {
  const parts = [workerTitle(w), workerAccountModel(w, false)]
  parts.push(shortId(w.worker_id))
  const time = workerTimeTitle(w)
  if (time) parts.push(time)
  return parts.join('\n')
}
// Rail dot tooltip: name + status (+ "needs permission").
function railTooltip(w: ConductorWorker): string {
  const parts = [workerTitle(w), statusTitle(w)]
  if (hasPending(w.worker_id)) parts.push(t('conductor.needsPermission'))
  return parts.join(' · ')
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
// Newest event for the collapsed timeline header (by timestamp, not position).
const latestEvent = computed<ConductorEvent | null>(() => {
  let best: ConductorEvent | null = null
  for (const e of props.events) {
    if (!best || new Date(e.ts).getTime() >= new Date(best.ts).getTime()) best = e
  }
  return best
})

// ── Timeline resize ─────────────────────────────────────────────────────────
// Drag the handle above the open timeline to set its height, clamped to
// [TIMELINE_MIN_HEIGHT, 60% of the panel]. Persisted via uiLayout on release.
const rootEl = ref<HTMLElement | null>(null)
const { height: rootHeight } = useElementSize(rootEl)
const dragHeight = ref<number | null>(null)
function maxTimelineHeight(): number {
  return Math.max(TIMELINE_MIN_HEIGHT, Math.floor(rootHeight.value * 0.6))
}
const timelineHeight = computed(() =>
  Math.min(dragHeight.value ?? uiLayout.timelineHeight, maxTimelineHeight()),
)
let dragStartY = 0
let dragStartHeight = 0
function onDragMove(e: PointerEvent) {
  const next = dragStartHeight + (dragStartY - e.clientY)
  dragHeight.value = Math.min(maxTimelineHeight(), Math.max(TIMELINE_MIN_HEIGHT, next))
}
// The handle captures the pointer, so move/up keep arriving even outside the
// window; cancel / lost capture end the drag too, so it can never get stuck.
let dragHandle: HTMLElement | null = null
const DRAG_END_EVENTS = ['pointerup', 'pointercancel', 'lostpointercapture'] as const
function onDragEnd() {
  if (!dragHandle) return
  const handle = dragHandle
  dragHandle = null
  handle.removeEventListener('pointermove', onDragMove)
  for (const ev of DRAG_END_EVENTS) handle.removeEventListener(ev, onDragEnd)
  if (dragHeight.value != null) uiLayout.setTimelineHeight(dragHeight.value)
  dragHeight.value = null
}
function onDragStart(e: PointerEvent) {
  e.preventDefault()
  onDragEnd()
  const handle = e.currentTarget as HTMLElement
  dragHandle = handle
  dragStartY = e.clientY
  dragStartHeight = timelineHeight.value
  handle.setPointerCapture(e.pointerId)
  handle.addEventListener('pointermove', onDragMove)
  for (const ev of DRAG_END_EVENTS) handle.addEventListener(ev, onDragEnd)
}
onBeforeUnmount(onDragEnd)
</script>

<template>
  <!-- Collapsed rail: one status dot per worker (pulsing when it needs a
       permission answer); click opens that worker in-place. Scrolls when the
       workers outgrow the rail. -->
  <div v-if="collapsed" class="flex-1 min-h-0 flex flex-col items-center overflow-y-auto py-1.5 gap-1">
    <button
      v-if="viewingWorker"
      type="button"
      class="flex items-center justify-center h-6 w-6 shrink-0 rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
      :title="t('conductor.backToConductor')"
      :aria-label="t('conductor.backToConductor')"
      @click="emit('backToConductor')"
    >
      <ChevronLeft class="h-3.5 w-3.5" :stroke-width="2" />
    </button>
    <button
      v-for="w in workers"
      :key="w.worker_id"
      type="button"
      class="relative flex items-center justify-center h-6 w-6 shrink-0 rounded transition-colors cursor-pointer hover:bg-accent focus:outline-none focus-visible:ring-1 focus-visible:ring-ring"
      :class="{ 'bg-accent/60 ring-1 ring-primary/50': isActive(w) }"
      :title="railTooltip(w)"
      :aria-label="railTooltip(w)"
      @click="emit('openSession', w.worker_id)"
    >
      <span
        v-if="hasPending(w.worker_id)"
        class="absolute h-3.5 w-3.5 animate-ping rounded-full bg-primary/40"
      />
      <span
        class="relative h-2 w-2 rounded-full"
        :class="[
          hasPending(w.worker_id) ? 'bg-primary' : runStatusDotClass(w.status),
          { 'animate-pulse': isRunning(w) && !hasPending(w.worker_id) },
        ]"
      />
    </button>
  </div>

  <div v-else ref="rootEl" class="flex flex-1 min-h-0 flex-col overflow-hidden">
    <!-- Workers -->
    <div class="flex-1 min-h-0 overflow-y-auto">
      <!-- Viewing a worker in-place: a one-tap way back to the session's own
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
          class="w-full text-left px-3 cursor-pointer focus:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-inset"
          :class="density === 'comfortable' ? 'py-3' : density === 'compact' ? 'py-2' : 'py-1.5'"
          :title="density === 'comfortable' ? undefined : workerTooltip(w)"
          @click="emit('openSession', w.worker_id)"
        >
          <!-- Title line: status icon (spins while running) · title · pending
               marker · time (compact/dense). -->
          <div class="flex items-center" :class="density === 'comfortable' ? 'gap-2' : 'gap-1.5'">
            <component
              :is="runStatusIcon(w.status)"
              class="h-3.5 w-3.5 shrink-0"
              :class="[runStatusColorClass(w.status), { 'animate-spin': isRunning(w) }]"
              :stroke-width="1.75"
              :title="statusTitle(w)"
            />
            <span
              class="flex-1 min-w-0 truncate font-medium leading-tight"
              :class="density === 'dense' ? 'text-xs' : 'text-[13px]'"
              :title="density === 'comfortable' ? (w.title ?? w.worker_id) : undefined"
            >
              {{ workerTitle(w) }}
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
            <span
              v-if="density !== 'comfortable'"
              class="shrink-0 font-mono text-[10px]"
              :class="isRunning(w) ? 'text-blue-500/80' : 'text-muted-foreground/70'"
            >{{ workerTime(w) }}</span>
          </div>
          <!-- Compact: "account · model" (model without "claude-"). -->
          <p
            v-if="density === 'compact'"
            class="mt-0.5 pl-5 truncate text-[10px] leading-tight text-muted-foreground/70"
          >
            {{ workerAccountModel(w) }}
          </p>
          <!-- Comfortable meta: account + model badges + short id. -->
          <div
            v-else-if="density === 'comfortable'"
            class="mt-2 pl-6 pr-3 flex items-center gap-1.5 text-[10px] text-muted-foreground/70"
          >
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

    <!-- Timeline: a one-line header (latest event) by default; click to open.
         When open, the handle on its top edge resizes it. -->
    <div class="shrink-0 flex flex-col overflow-hidden border-t border-border">
      <div
        v-if="uiLayout.timelineOpen"
        class="h-1.5 -mt-px shrink-0 cursor-row-resize bg-transparent hover:bg-primary/30 active:bg-primary/40 transition-colors touch-none"
        role="separator"
        aria-orientation="horizontal"
        :aria-label="t('conductor.resizeTimeline')"
        :title="t('conductor.resizeTimeline')"
        @pointerdown="onDragStart"
      />
      <button
        type="button"
        class="flex items-center gap-1.5 px-3 py-1.5 shrink-0 text-left cursor-pointer hover:bg-accent/40 transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-inset"
        :class="{ 'border-b border-border/40': uiLayout.timelineOpen }"
        :aria-expanded="uiLayout.timelineOpen"
        :title="uiLayout.timelineOpen ? t('conductor.collapse') : t('conductor.expand')"
        @click="uiLayout.setTimelineOpen(!uiLayout.timelineOpen)"
      >
        <ChevronRight
          class="h-3 w-3 shrink-0 text-muted-foreground transition-transform"
          :class="{ 'rotate-90': uiLayout.timelineOpen }"
          :stroke-width="2"
        />
        <span class="shrink-0 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
          {{ t('conductor.timeline') }}
        </span>
        <!-- Collapsed: the newest event inline, so the header still informs. -->
        <span
          v-if="!uiLayout.timelineOpen && latestEvent"
          class="flex-1 min-w-0 truncate text-[11px] text-foreground/70"
          :title="`${formatWhen(latestEvent.ts)} ${eventSummary(latestEvent)}`"
        >
          · <span class="font-mono text-muted-foreground/70">{{ formatWhen(latestEvent.ts) }}</span>
          {{ eventSummary(latestEvent) }}
        </span>
      </button>
      <div
        v-if="uiLayout.timelineOpen"
        class="min-h-0 overflow-y-auto px-3 py-2 space-y-1"
        :style="{ height: `${timelineHeight}px` }"
      >
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
