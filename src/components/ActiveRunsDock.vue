<script setup lang="ts">
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { Activity, ShieldAlert, CheckCircle2, XCircle, Network } from 'lucide-vue-next'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useProjectsStore } from '@/stores/projects'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'

/**
 * App-wide dock of all currently-active runs (streaming or awaiting a
 * permission / question), shown in the sidebar so the user can monitor and
 * jump between concurrent conversations on different projects from anywhere in
 * the app. It doubles as a permission center: a run blocked on input shows an
 * amber "needs permission" affordance that deep-links to its full modal in
 * RunView (we deliberately don't approve diffs blind from this narrow panel).
 */

const route = useRoute()
const router = useRouter()
const { t } = useI18n()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const projectsStore = useProjectsStore()
const tabsStore = useWorkspaceTabsStore()

interface DockRow {
  runId: string
  projectId: string
  status: string
  pending: number
  /** Finished run the user hasn't opened yet — shows a "done" notification. */
  done: boolean
  /** Orchestration role from the run metadata ('conductor' | null). */
  role: string | null
  /** Id of the conductor that spawned this run, when it is a worker. */
  conductorRunId: string | null
}

const rows = computed<DockRow[]>(() => {
  const out: DockRow[] = []
  live.sessions.forEach((s) => {
    const pending = s.permissionQueue.length
    // Keep a row while running, while blocked on a permission, OR while a
    // finished run still has an unseen "done" notification.
    if (s.status === 'running' || pending > 0 || s.notifyDone) {
      const meta = runsStore.runMeta.get(s.runId)
      out.push({
        runId: s.runId,
        projectId: s.projectId,
        status: s.status,
        pending,
        done: s.notifyDone && s.status !== 'running',
        role: meta?.role ?? null,
        conductorRunId: meta?.conductor_run_id ?? null,
      })
    }
  })
  // Order: awaiting permission first, then finished notifications, then running.
  return out.sort(
    (a, b) => Number(b.pending > 0) - Number(a.pending > 0) || Number(b.done) - Number(a.done),
  )
})

const waitingCount = computed(() => rows.value.filter((r) => r.pending > 0).length)
const doneCount = computed(() => rows.value.filter((r) => r.done).length)

// ── Conductor grouping (conductor → workers) ───────────────────────────────
// Workers are NEVER listed on their own in the dock: a conductor is the single
// unit the user tracks here. Every worker spawned by a conductor is folded into
// that conductor's row, which aggregates the workers' state (a running worker
// keeps the conductor "running"; a worker awaiting permission floats the
// conductor and lights its Review badge; finished workers show as a count).
// Clicking the conductor opens its session, where the worker sidebar lists the
// individual workers. Grouping is purely a display concern: the row opens the
// conductor's own run.id.
interface DisplayRow {
  row: DockRow
  /** True for a conductor that owns at least one active worker. */
  isGroup: boolean
  /** Active workers owned by this conductor (badge on the row). */
  workerCount: number
  /** Finished workers, surfaced as a row badge. */
  groupDone: number
}

// Flattened view of `rows`: normal sessions and conductors only. A conductor's
// workers are merged into its row rather than emitted separately.
const displayRows = computed<DisplayRow[]>(() => {
  const base = rows.value

  // Workers grouped by the conductor that spawned them.
  const workersByParent = new Map<string, DockRow[]>()
  for (const r of base) {
    if (r.conductorRunId) {
      const list = workersByParent.get(r.conductorRunId) ?? []
      list.push(r)
      workersByParent.set(r.conductorRunId, list)
    }
  }
  const groupIds = new Set(workersByParent.keys())

  type Top =
    | { kind: 'group'; id: string; header: DockRow; workers: DockRow[] }
    | { kind: 'single'; row: DockRow }

  const tops: Top[] = []
  const seenGroups = new Set<string>()
  for (const r of base) {
    // A worker owned by a conductor never appears on its own — it's merged in.
    if (r.conductorRunId && groupIds.has(r.conductorRunId)) continue
    if (groupIds.has(r.runId)) {
      tops.push({ kind: 'group', id: r.runId, header: r, workers: workersByParent.get(r.runId)! })
      seenGroups.add(r.runId)
    } else {
      tops.push({ kind: 'single', row: r })
    }
  }
  // A conductor can finish its own turn while its workers keep running, so it
  // won't have an active row — synthesize one from the cached metadata so those
  // workers still report under the conductor instead of vanishing.
  for (const id of groupIds) {
    if (seenGroups.has(id)) continue
    const workers = workersByParent.get(id)!
    const meta = runsStore.runMeta.get(id)
    tops.push({
      kind: 'group',
      id,
      header: {
        runId: id,
        projectId: meta?.project_id ?? workers[0].projectId,
        status: meta?.status ?? 'running',
        pending: 0,
        done: false,
        role: 'conductor',
        conductorRunId: null,
      },
      workers,
    })
  }

  // Sort key: pending first, then done — a conductor folds in its workers'
  // urgency so a conductor with a blocked worker floats to the top.
  const urgency = (t: Top): [number, number] => {
    if (t.kind === 'single') return [t.row.pending > 0 ? 1 : 0, t.row.done ? 1 : 0]
    const pending = t.header.pending + t.workers.reduce((s, w) => s + w.pending, 0)
    const done = (t.header.done ? 1 : 0) + t.workers.filter((w) => w.done).length
    return [pending > 0 ? 1 : 0, done > 0 ? 1 : 0]
  }
  tops.sort((a, b) => urgency(b)[0] - urgency(a)[0] || urgency(b)[1] - urgency(a)[1])

  const out: DisplayRow[] = []
  for (const top of tops) {
    if (top.kind === 'single') {
      out.push({ row: top.row, isGroup: false, workerCount: 0, groupDone: 0 })
      continue
    }
    // Merge the workers' state onto the conductor row so one row conveys the
    // whole group: pending = any worker needs permission; running = the
    // conductor or any worker is still streaming; done = everything settled.
    const groupPending = top.header.pending + top.workers.reduce((s, w) => s + w.pending, 0)
    const groupDone = (top.header.done ? 1 : 0) + top.workers.filter((w) => w.done).length
    const anyRunning =
      top.header.status === 'running' || top.workers.some((w) => w.status === 'running')
    const merged: DockRow = {
      ...top.header,
      status: anyRunning ? 'running' : top.header.status,
      pending: groupPending,
      done: !anyRunning && groupPending === 0 && groupDone > 0,
    }
    out.push({ row: merged, isGroup: true, workerCount: top.workers.length, groupDone })
  }
  return out
})

function isFailure(status: string): boolean {
  return status === 'failed' || status === 'cancelled'
}

const activeRunId = computed(() =>
  typeof route.params.runId === 'string' ? route.params.runId : null,
)

function label(row: DockRow): string {
  // Read from the session-wide metadata cache, not `runsStore.runs` — that
  // array only holds the foreground project's runs and is replaced on every tab
  // switch, so a background project's active run would otherwise fall back to
  // the project name (the "title changes on tab switch" bug).
  const run = runsStore.runMeta.get(row.runId)
  if (run && run.run_type !== 'session') {
    if (run.ref_number != null) return `${run.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${run.ref_number}`
  } else if (run?.title) {
    return run.title
  }
  return projectsStore.projects.find((p) => p.id === row.projectId)?.name ?? 'Run'
}

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? 'Project'
}

function open(row: DockRow) {
  // Opening the run counts as viewing it — clear its finished notification.
  live.markSeen(row.runId)
  // A conductor row stands in for its workers, so opening it acknowledges their
  // finished notifications too — no need to visit each worker. Pending
  // permissions are left alone; those still need an explicit answer.
  if (row.role === 'conductor') {
    const finishedWorkers: string[] = []
    live.sessions.forEach((s) => {
      if (s.notifyDone && runsStore.runMeta.get(s.runId)?.conductor_run_id === row.runId) {
        finishedWorkers.push(s.runId)
      }
    })
    finishedWorkers.forEach((id) => live.markSeen(id))
  }
  tabsStore.open(row.projectId, row.runId)
  if (row.runId === activeRunId.value) return
  router
    .push({ name: 'project-run-detail', params: { projectId: row.projectId, runId: row.runId } })
    .catch(() => {})
}
</script>

<template>
  <div v-if="rows.length > 0" class="px-2 pb-2 border-t border-border/50 pt-2">
    <div class="flex items-center gap-1.5 px-2 mb-1">
      <Activity class="h-3 w-3 text-muted-foreground" :stroke-width="2" />
      <span class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground flex-1">
        Active runs
      </span>
      <span
        v-if="waitingCount > 0"
        class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-amber-500 px-1 text-[10px] font-medium text-white leading-none"
      >{{ waitingCount }}</span>
      <span
        v-else-if="doneCount > 0"
        class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-emerald-500 px-1 text-[10px] font-medium text-white leading-none"
      >{{ doneCount }}</span>
    </div>

    <div class="space-y-0.5 max-h-[210px] overflow-y-auto">
      <div
        v-for="{ row, isGroup, workerCount, groupDone } in displayRows"
        :key="row.runId"
        class="relative"
      >
        <button
          type="button"
          class="w-full flex items-center gap-2 py-1.5 px-2 rounded-md text-left transition-colors cursor-pointer select-none"
          :class="row.runId === activeRunId
            ? 'bg-accent text-foreground'
            : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
          :title="projectName(row.projectId) + ' — ' + label(row)"
          @click="open(row)"
        >
          <span class="relative flex h-2 w-2 shrink-0">
            <span
              v-if="row.status === 'running'"
              class="absolute inline-flex h-full w-full rounded-full bg-primary opacity-60 animate-ping"
            />
            <span
              class="relative inline-flex h-2 w-2 rounded-full"
              :class="row.pending > 0
                ? 'bg-amber-500'
                : row.status === 'running'
                  ? 'bg-primary'
                  : row.done
                    ? (isFailure(row.status) ? 'bg-red-500' : 'bg-emerald-500')
                    : 'bg-muted-foreground/40'"
            />
          </span>
          <span class="min-w-0 flex-1">
            <span class="block truncate text-[13px] leading-tight">{{ label(row) }}</span>
            <span class="block truncate text-[10px] text-muted-foreground/70 leading-tight">{{ projectName(row.projectId) }}</span>
          </span>
          <!-- Active-worker count for a conductor. -->
          <span
            v-if="isGroup"
            class="shrink-0 flex items-center gap-0.5 rounded-full bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium text-primary"
            :title="t('conductor.workerCountShort', { n: workerCount })"
          >
            <Network class="h-2.5 w-2.5" :stroke-width="2" />
            {{ workerCount }}
          </span>
          <!-- Finished workers of a conductor, surfaced as a count. -->
          <span
            v-if="isGroup && groupDone > 0"
            class="flex items-center gap-0.5 rounded bg-emerald-500/15 px-1 py-0.5 text-[10px] font-medium text-emerald-600 dark:text-emerald-400 shrink-0"
          >
            <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
            {{ groupDone }}
          </span>
          <span
            v-if="row.pending > 0"
            class="flex items-center gap-0.5 rounded bg-amber-500/15 px-1 py-0.5 text-[10px] font-medium text-amber-600 dark:text-amber-400 shrink-0"
          >
            <ShieldAlert class="h-3 w-3" :stroke-width="2" />
            Review
          </span>
          <span
            v-else-if="!isGroup && row.done && isFailure(row.status)"
            class="flex items-center gap-0.5 rounded bg-red-500/15 px-1 py-0.5 text-[10px] font-medium text-red-600 dark:text-red-400 shrink-0"
          >
            <XCircle class="h-3 w-3" :stroke-width="2" />
            {{ row.status === 'cancelled' ? 'Stopped' : 'Failed' }}
          </span>
          <span
            v-else-if="!isGroup && row.done"
            class="flex items-center gap-0.5 rounded bg-emerald-500/15 px-1 py-0.5 text-[10px] font-medium text-emerald-600 dark:text-emerald-400 shrink-0"
          >
            <CheckCircle2 class="h-3 w-3" :stroke-width="2" />
            Done
          </span>
        </button>
      </div>
    </div>
  </div>
</template>
