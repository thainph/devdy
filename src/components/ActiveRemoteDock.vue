<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Radio, Smartphone } from 'lucide-vue-next'
import { useRemoteControlStore } from '@/stores/remoteControl'
import { useRunsStore } from '@/stores/runs'
import { useProjectsStore } from '@/stores/projects'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'

/**
 * Sidebar dock for the active Remote Control session, mirroring ActiveRunsDock.
 * Remote is single-session by design, so this shows at most one row: the run a
 * phone is currently paired to. Clicking jumps to that run (where the Remote
 * button reopens the full session modal). Status is kept fresh by both the
 * remote:// lifecycle events and a light poll fallback.
 */

const route = useRoute()
const router = useRouter()
const store = useRemoteControlStore()
const runsStore = useRunsStore()
const projectsStore = useProjectsStore()
const tabsStore = useWorkspaceTabsStore()

const unlisteners: UnlistenFn[] = []
let pollTimer: ReturnType<typeof setInterval> | null = null

const status = computed(() => store.status)

// A remote session is worth showing once the agent is running and a run has been
// bound to it (link created → pairing → live control).
const boundRunId = computed(() =>
  status.value?.enabled && status.value?.running ? (status.value?.bound_run_id ?? null) : null,
)
const authenticated = computed(() => status.value?.session_authenticated ?? false)

const projectId = computed(() =>
  boundRunId.value ? (runsStore.runMeta.get(boundRunId.value)?.project_id ?? null) : null,
)

const activeRunId = computed(() =>
  typeof route.params.runId === 'string' ? route.params.runId : null,
)

function runLabel(runId: string): string {
  const run = runsStore.runMeta.get(runId)
  if (run && run.run_type !== 'session') {
    if (run.ref_number != null) return `${run.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${run.ref_number}`
  } else if (run?.title) {
    return run.title
  }
  const pid = run?.project_id
  return projectsStore.projects.find((p) => p.id === pid)?.name ?? 'Run'
}

function projectName(pid: string | null): string {
  return projectsStore.projects.find((p) => p.id === pid)?.name ?? 'Project'
}

function open() {
  const runId = boundRunId.value
  const pid = projectId.value
  if (!runId || !pid) return
  tabsStore.open(pid, runId)
  if (runId === activeRunId.value) return
  router
    .push({ name: 'project-run-detail', params: { projectId: pid, runId } })
    .catch(() => {})
}

onMounted(async () => {
  store.refreshStatus().catch(() => {})
  // Snappy updates on session lifecycle transitions…
  for (const ev of [
    'remote://controller-requested-session',
    'remote://connection-authenticated',
    'remote://auth-failed',
    'remote://disconnected',
    'remote://session-expired',
  ]) {
    unlisteners.push(await listen(ev, () => store.refreshStatus().catch(() => {})))
  }
  // …plus a low-frequency poll as a safety net (events can be missed if this
  // mounts mid-session).
  pollTimer = setInterval(() => store.refreshStatus().catch(() => {}), 8000)
})

onBeforeUnmount(() => {
  if (pollTimer) clearInterval(pollTimer)
  for (const un of unlisteners) {
    try {
      un()
    } catch {
      // ignore
    }
  }
})
</script>

<template>
  <div v-if="boundRunId" class="px-2 pb-2 border-t border-border/50 pt-2">
    <div class="flex items-center gap-1.5 px-2 mb-1">
      <Radio class="h-3 w-3 text-muted-foreground" :stroke-width="2" />
      <span class="text-[10px] font-semibold uppercase tracking-wider text-muted-foreground flex-1">
        Active remote
      </span>
    </div>

    <button
      type="button"
      class="w-full flex items-center gap-2 px-2 py-1.5 rounded-md text-left transition-colors cursor-pointer select-none"
      :class="boundRunId === activeRunId
        ? 'bg-accent text-foreground'
        : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
      :title="projectName(projectId) + ' — ' + runLabel(boundRunId)"
      @click="open"
    >
      <span class="relative flex h-2 w-2 shrink-0">
        <span
          v-if="authenticated"
          class="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-60 animate-ping"
        />
        <span
          class="relative inline-flex h-2 w-2 rounded-full"
          :class="authenticated ? 'bg-emerald-500' : 'bg-amber-500'"
        />
      </span>
      <span class="min-w-0 flex-1">
        <span class="block truncate text-[13px] leading-tight">{{ runLabel(boundRunId) }}</span>
        <span class="block truncate text-[10px] text-muted-foreground/70 leading-tight">{{ projectName(projectId) }}</span>
      </span>
      <span
        class="flex items-center gap-0.5 rounded px-1 py-0.5 text-[10px] font-medium shrink-0"
        :class="authenticated
          ? 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400'
          : 'bg-amber-500/15 text-amber-600 dark:text-amber-400'"
      >
        <Smartphone class="h-3 w-3" :stroke-width="2" />
        {{ authenticated ? 'Live' : 'Pairing' }}
      </span>
    </button>
  </div>
</template>
