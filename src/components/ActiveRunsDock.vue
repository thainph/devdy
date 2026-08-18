<script setup lang="ts">
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { Activity, ShieldAlert, CheckCircle2, XCircle } from 'lucide-vue-next'
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
}

const rows = computed<DockRow[]>(() => {
  const out: DockRow[] = []
  live.sessions.forEach((s) => {
    const pending = s.permissionQueue.length
    // Keep a row while running, while blocked on a permission, OR while a
    // finished run still has an unseen "done" notification.
    if (s.status === 'running' || pending > 0 || s.notifyDone) {
      out.push({
        runId: s.runId,
        projectId: s.projectId,
        status: s.status,
        pending,
        done: s.notifyDone && s.status !== 'running',
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
      <button
        v-for="row in rows"
        :key="row.runId"
        type="button"
        class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left transition-colors cursor-pointer select-none"
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
</template>
