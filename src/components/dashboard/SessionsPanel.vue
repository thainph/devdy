<script setup lang="ts">
// Work Dashboard — the running sessions column (`list_active_runs`, every
// project): "N running" + "M awaiting approval". Awaiting a permission first
// (warning tint, "View request" opens the session that holds the prompt — a
// worker folded into its conductor included; a menu picks one when several
// wait — where the full prompt is shown, never approved blind from here),
// then the longest running. Each tile: engine (Claude / Codex icons as in the
// run view, pulsing while it streams), name, project · workers · duration,
// what it is doing when the app already knows it, Open / Stop. At the bottom,
// sessions that finished and were not opened yet (the sidebar dock's "done").
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Activity, CheckCircle2, ChevronDown, Cpu, ShieldAlert, Square, Terminal, Users, XCircle } from 'lucide-vue-next'
import { Badge, Banner, Button, DropdownItem, DropdownMenu } from '@/components/ui'
import { sessionActivity } from '@/lib/dashboardLayout'
import type { DashboardRun, WaitingSession } from '@/lib/workDashboard'
import type { SourceState } from '@/composables/useWorkDashboard'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useProjectsStore } from '@/stores/projects'
import DashboardWidget from './DashboardWidget.vue'
import { useDurationLabel } from './useDurationLabel'

const props = defineProps<{
  runs: DashboardRun[]
  state: SourceState
  now: number
  /** Run ids being stopped. */
  stopping: Set<string>
  /** Project filter (null = all): limits "finished recently". */
  projectFilter: string | null
}>()

const emit = defineEmits<{ open: [projectId: string, runId: string]; stop: [row: DashboardRun] }>()

const { t } = useI18n()
const { duration } = useDurationLabel()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const projects = useProjectsStore()

const waitingCount = computed(() => props.runs.filter((r) => r.pending > 0).length)

function label(row: DashboardRun): string {
  const r = row.run
  if (r.title?.trim()) return r.title.trim()
  if (r.run_type !== 'session' && r.ref_number != null) {
    return `${r.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${r.ref_number}`
  }
  return t('dashboard.runs.untitled')
}

function meta(row: DashboardRun): string {
  const parts = [row.run.project_name]
  if (row.workerCount) parts.push(t('dashboard.runs.workers', { n: row.workerCount }, row.workerCount))
  if (row.sinceMs !== null) parts.push(duration(row.sinceMs))
  return parts.join(' · ')
}

function isCodex(row: DashboardRun): boolean {
  return row.run.engine === 'codex'
}

/** What it is doing, only from what the live store already streams. */
function activity(row: DashboardRun): string | null {
  const waiting = row.waiting[0]
  const id = waiting?.runId ?? row.run.run_id
  const s = live.get(id)
  const a = sessionActivity(s?.entries, s?.permissionQueue)
  if (!a) return null
  return a.kind === 'permission'
    ? t('dashboard.runs.activity.permission', { tool: a.tool })
    : t('dashboard.runs.activity.tool', { tool: a.tool })
}

function openRow(row: DashboardRun) {
  emit('open', row.run.project_id, row.run.run_id)
}
function openWaiting(w: WaitingSession) {
  emit('open', w.projectId, w.runId)
}
function waitingLabel(w: WaitingSession): string {
  const name = w.title?.trim() || t('dashboard.runs.untitled')
  return t('dashboard.runs.waitingItem', { name, n: w.pending }, w.pending)
}

// ── Finished recently (unseen "done", like the sidebar dock) ──────────────
interface DoneRow { runId: string, projectId: string, name: string, failed: boolean }
const RECENT_DONE = 3
const recentDone = computed<DoneRow[]>(() => {
  const out: DoneRow[] = []
  live.sessions.forEach((s) => {
    const m = runsStore.runMeta.get(s.runId)
    if (!s.notifyDone || s.status === 'running' || m?.conductor_run_id) return
    if (props.projectFilter && s.projectId !== props.projectFilter) return
    const title = m?.title?.trim()
      || (m && m.run_type !== 'session' && m.ref_number != null ? `${m.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${m.ref_number}` : '')
      || projects.projects.find((p) => p.id === s.projectId)?.name
      || t('dashboard.runs.untitled')
    out.push({ runId: s.runId, projectId: s.projectId, name: title, failed: s.status === 'failed' || s.status === 'cancelled' })
  })
  return out.slice(-RECENT_DONE).reverse()
})
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.runs.title')"
    :icon="Activity"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :stale-after="Infinity"
  >
    <template #answer>
      <span class="text-3xl font-semibold leading-none tabular-nums" :class="runs.length ? 'text-foreground' : 'text-muted-foreground'">{{ runs.length }}</span>
      <span class="text-[13px] font-medium">{{ t('dashboard.runs.answer') }}</span>
      <Badge v-if="waitingCount" tone="warning" size="xs">{{ t('dashboard.runs.answerWaiting', { n: waitingCount }) }}</Badge>
    </template>

    <Banner v-if="state.error" tone="destructive" class="mx-3 my-2">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="!state.error && runs.length === 0" class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.runs.empty') }}
    </p>
    <ul v-else class="grid gap-2 border-t border-border/50 p-3 sm:grid-cols-2 xl:grid-cols-1">
      <li
        v-for="row in runs"
        :key="row.run.run_id"
        class="flex min-w-0 flex-col gap-1 rounded-md border px-2.5 py-2"
        :class="row.pending > 0 ? 'border-warning/40 bg-warning/10' : 'border-border/60 bg-muted/20'"
      >
        <button type="button" class="flex min-w-0 cursor-pointer items-center gap-2 text-left" @click="openRow(row)">
          <span
            class="relative flex h-6 w-6 shrink-0 items-center justify-center rounded-md"
            :class="row.pending > 0 ? 'bg-warning/15 text-warning' : 'bg-muted text-foreground'"
            :title="isCodex(row) ? 'Codex' : 'Claude'"
          >
            <component :is="isCodex(row) ? Terminal : Cpu" class="h-3.5 w-3.5" :stroke-width="1.75" />
            <span v-if="row.run.status === 'running' && row.pending === 0" class="absolute -right-0.5 -top-0.5 flex h-2 w-2">
              <span class="absolute inline-flex h-full w-full animate-ping rounded-full bg-primary opacity-60 motion-reduce:animate-none" />
              <span class="relative inline-flex h-2 w-2 rounded-full bg-primary" />
            </span>
            <span class="sr-only">{{ isCodex(row) ? 'Codex' : 'Claude' }}</span>
          </span>
          <span class="min-w-0 flex-1">
            <span class="block truncate text-xs font-medium text-foreground" :title="label(row)">{{ label(row) }}</span>
            <span class="flex min-w-0 items-center gap-1 truncate text-[11px] text-muted-foreground">
              <Users v-if="row.workerCount" class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              <span class="truncate">{{ meta(row) }}</span>
            </span>
          </span>
        </button>
        <p v-if="row.pending > 0" class="flex items-center gap-1 text-[11px] font-medium text-warning">
          <ShieldAlert class="h-3 w-3 shrink-0" :stroke-width="2" />
          <span class="truncate">{{ activity(row) ?? t('dashboard.runs.waitingPermission', { n: row.pending }, row.pending) }}</span>
        </p>
        <p v-else-if="row.run.status !== 'running'" class="text-[11px] text-muted-foreground">{{ t('dashboard.runs.idle') }}</p>
        <p v-else-if="activity(row)" class="truncate text-[11px] text-foreground/80">{{ activity(row) }}</p>
        <div class="flex items-center gap-1">
          <DropdownMenu v-if="row.waiting.length > 1" align="left">
            <template #trigger>
              <Button variant="primary" size="xs">
                {{ t('dashboard.runs.review') }}
                <ChevronDown class="h-3 w-3" :stroke-width="2" />
              </Button>
            </template>
            <DropdownItem v-for="w in row.waiting" :key="w.runId" @click="openWaiting(w)">
              <ShieldAlert class="h-3.5 w-3.5 text-warning" :stroke-width="1.75" />
              {{ waitingLabel(w) }}
              <span v-if="w.worker" class="text-[11px] text-muted-foreground">{{ t('dashboard.runs.worker') }}</span>
            </DropdownItem>
          </DropdownMenu>
          <Button v-else-if="row.waiting.length === 1" variant="primary" size="xs" @click="openWaiting(row.waiting[0])">
            {{ t('dashboard.runs.review') }}
          </Button>
          <Button v-else variant="outline" size="xs" @click="openRow(row)">
            {{ t('dashboard.runs.open') }}
          </Button>
          <Button
            variant="destructive-ghost"
            size="xs"
            :disabled="stopping.has(row.run.run_id)"
            @click="emit('stop', row)"
          >
            <Square class="h-3 w-3" :stroke-width="2" />{{ t('dashboard.runs.stop') }}
          </Button>
        </div>
      </li>
    </ul>

    <section v-if="recentDone.length" class="border-t border-border/50 pt-1">
      <h3 class="px-4 pt-1.5 pb-0.5 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
        {{ t('dashboard.runs.recentDone') }}
      </h3>
      <ul>
        <li v-for="d in recentDone" :key="d.runId">
          <button
            type="button"
            class="flex w-full cursor-pointer items-center gap-1.5 px-4 py-1 text-left text-[11px] text-muted-foreground hover:bg-accent/50 hover:text-foreground"
            @click="emit('open', d.projectId, d.runId)"
          >
            <XCircle v-if="d.failed" class="h-3 w-3 shrink-0 text-destructive" :stroke-width="2" />
            <CheckCircle2 v-else class="h-3 w-3 shrink-0 text-success" :stroke-width="2" />
            <span class="truncate">{{ d.name }}</span>
            <span class="sr-only">{{ d.failed ? t('dashboard.runs.failed') : t('dashboard.runs.done') }}</span>
          </button>
        </li>
      </ul>
    </section>
  </DashboardWidget>
</template>
