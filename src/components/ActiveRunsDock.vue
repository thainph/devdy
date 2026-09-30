<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { Activity, ShieldAlert, CheckCircle2, XCircle, ChevronRight, Network } from 'lucide-vue-next'
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
// A worker run (its conductor_run_id points at a conductor that also has an
// active row) is nested under that conductor so the dock isn't cluttered by
// every spawned worker — mirroring the History list. Grouping is purely a
// display concern: every row still opens its own run.id.
interface DisplayRow {
  row: DockRow
  /** True for a conductor header that owns at least one active worker. */
  isGroup: boolean
  /** True for a worker rendered nested under its conductor. */
  isWorker: boolean
  /** Active workers owned by this conductor (badge on the header). */
  workerCount: number
  /** For a group header: whether its workers are currently shown. */
  expanded: boolean
  /** Finished workers in a folded group, surfaced as a header badge. */
  groupDone: number
}

// Conductors the user has expanded. In-memory only — resets each session.
const expandedConductors = ref<Set<string>>(new Set())
function toggleConductor(id: string) {
  const next = new Set(expandedConductors.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expandedConductors.value = next
}

// Flattened, grouped view of `rows`. Top-level items keep the same urgency sort
// (a group sorts by the combined urgency of its header + workers); a conductor's
// workers are emitted right after it. A group auto-expands while any of its
// workers is awaiting permission so a blocked worker is never hidden by a fold.
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
    // A worker owned by a listed conductor is emitted under it, not top-level.
    if (r.conductorRunId && groupIds.has(r.conductorRunId)) continue
    if (groupIds.has(r.runId)) {
      tops.push({ kind: 'group', id: r.runId, header: r, workers: workersByParent.get(r.runId)! })
      seenGroups.add(r.runId)
    } else {
      tops.push({ kind: 'single', row: r })
    }
  }
  // A conductor can finish its own turn while its workers keep running, so it
  // won't have an active row — synthesize a header from the cached metadata so
  // those workers still nest instead of scattering to the top level.
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

  // Sort key: pending first, then done — same priority as the flat list, but a
  // group folds in its workers' urgency so a group with a blocked worker floats.
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
      out.push({ row: top.row, isGroup: false, isWorker: false, workerCount: 0, expanded: false, groupDone: 0 })
      continue
    }
    const groupPending = top.workers.reduce((s, w) => s + w.pending, 0)
    const groupDone = top.workers.filter((w) => w.done).length
    const expanded = expandedConductors.value.has(top.id) || groupPending > 0
    out.push({
      row: top.header,
      isGroup: true,
      isWorker: false,
      workerCount: top.workers.length,
      expanded,
      groupDone,
    })
    if (expanded) {
      for (const w of top.workers) {
        out.push({ row: w, isGroup: false, isWorker: true, workerCount: 0, expanded: false, groupDone: 0 })
      }
    }
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
        v-for="{ row, isGroup, isWorker, workerCount, expanded, groupDone } in displayRows"
        :key="row.runId"
        class="relative"
      >
        <!-- Worker rows nest under their conductor: a guide rail marks the
             parent relationship. -->
        <span
          v-if="isWorker"
          class="absolute left-3 inset-y-0 w-px bg-border/60"
          aria-hidden="true"
        />
        <button
          type="button"
          class="w-full flex items-center gap-2 py-1.5 rounded-md text-left transition-colors cursor-pointer select-none"
          :class="[
            row.runId === activeRunId
              ? 'bg-accent text-foreground'
              : 'text-muted-foreground hover:text-foreground hover:bg-accent/50',
            isWorker ? 'pl-6 pr-2' : 'px-2',
          ]"
          :title="projectName(row.projectId) + ' — ' + label(row)"
          @click="open(row)"
        >
          <!-- Conductor group toggle: folds/unfolds its worker rows. A span (not
               a nested button) with @click.stop so tapping it never opens the
               conductor session. -->
          <span
            v-if="isGroup"
            role="button"
            tabindex="0"
            class="flex h-4 w-4 shrink-0 items-center justify-center -ml-1 rounded text-muted-foreground hover:text-foreground hover:bg-accent cursor-pointer"
            :aria-label="expanded ? t('conductor.collapse') : t('conductor.expand')"
            :aria-expanded="expanded"
            @click.stop.prevent="toggleConductor(row.runId)"
            @keyup.enter.stop.prevent="toggleConductor(row.runId)"
            @keyup.space.stop.prevent="toggleConductor(row.runId)"
          >
            <ChevronRight
              class="h-3.5 w-3.5 transition-transform"
              :class="{ 'rotate-90': expanded }"
              :stroke-width="2"
            />
          </span>
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
          <!-- Active-worker count for a conductor group. -->
          <span
            v-if="isGroup"
            class="shrink-0 flex items-center gap-0.5 rounded-full bg-primary/10 px-1.5 py-0.5 text-[9px] font-medium text-primary"
            :title="t('conductor.workerCountShort', { n: workerCount })"
          >
            <Network class="h-2.5 w-2.5" :stroke-width="2" />
            {{ workerCount }}
          </span>
          <!-- A folded group still surfaces a finished worker so the badge is
               not lost behind the collapse (pending auto-expands, so it only
               applies to done here). -->
          <span
            v-if="isGroup && !expanded && groupDone > 0"
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
            v-else-if="row.done && isFailure(row.status)"
            class="flex items-center gap-0.5 rounded bg-red-500/15 px-1 py-0.5 text-[10px] font-medium text-red-600 dark:text-red-400 shrink-0"
          >
            <XCircle class="h-3 w-3" :stroke-width="2" />
            {{ row.status === 'cancelled' ? 'Stopped' : 'Failed' }}
          </span>
          <span
            v-else-if="row.done"
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
