<script setup lang="ts">
import { computed, onMounted, onBeforeUnmount } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { X, FolderOpen } from 'lucide-vue-next'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useProjectsStore } from '@/stores/projects'

/**
 * Horizontal tab bar for fast switching between PROJECTS. One tab per open
 * project; switching runs inside a project is still done from RunView's History
 * list. Clicking a tab returns to the project's last-viewed run. Aggregate live
 * status (any run streaming / awaiting permission) is read from liveRuns.
 */

const route = useRoute()
const router = useRouter()
const tabsStore = useWorkspaceTabsStore()
const live = useLiveRunsStore()
const projectsStore = useProjectsStore()

const activeProjectId = computed(() =>
  typeof route.params.projectId === 'string' ? route.params.projectId : null,
)

function projectName(projectId: string): string {
  return projectsStore.projects.find((p) => p.id === projectId)?.name ?? 'Project'
}

interface ProjectState {
  running: boolean
  pending: number
}
function stateOf(projectId: string): ProjectState {
  let running = false
  let pending = 0
  live.sessions.forEach((s) => {
    if (s.projectId !== projectId) return
    if (s.status === 'running') running = true
    pending += s.permissionQueue.length
  })
  return { running, pending }
}

function select(projectId: string) {
  if (projectId === activeProjectId.value) return
  const tab = tabsStore.tabs.find((t) => t.projectId === projectId)
  if (tab?.lastRunId) {
    router
      .push({ name: 'project-run-detail', params: { projectId, runId: tab.lastRunId } })
      .catch(() => {})
  } else {
    router.push({ name: 'project-run', params: { projectId } }).catch(() => {})
  }
}

function closeTab(projectId: string, e?: MouseEvent) {
  e?.stopPropagation()
  const wasActive = projectId === activeProjectId.value
  const next = tabsStore.close(projectId)
  if (!wasActive) return
  if (next) {
    select(next.projectId)
  } else {
    router.push({ name: 'projects' }).catch(() => {})
  }
}

// --- keyboard: Cmd/Ctrl+1..9 switch project, Cmd/Ctrl+W close active -------
function onKeydown(e: KeyboardEvent) {
  if (!(e.metaKey || e.ctrlKey)) return
  if (e.key === 'w' || e.key === 'W') {
    if (activeProjectId.value && tabsStore.has(activeProjectId.value)) {
      e.preventDefault()
      closeTab(activeProjectId.value)
    }
    return
  }
  const n = Number(e.key)
  if (Number.isInteger(n) && n >= 1 && n <= 9) {
    const tab = tabsStore.tabs[n - 1]
    if (tab) {
      e.preventDefault()
      select(tab.projectId)
    }
  }
}

onMounted(() => window.addEventListener('keydown', onKeydown))
onBeforeUnmount(() => window.removeEventListener('keydown', onKeydown))
</script>

<template>
  <div
    v-if="tabsStore.tabs.length > 0"
    class="flex items-stretch gap-1 px-2 h-[38px] shrink-0 border-b border-border/50 bg-sidebar overflow-x-auto"
  >
    <button
      v-for="tab in tabsStore.tabs"
      :key="tab.projectId"
      type="button"
      class="group relative flex items-center gap-2 pl-3 pr-2 my-[5px] rounded-md text-[13px] max-w-[200px] transition-colors cursor-pointer select-none"
      :class="tab.projectId === activeProjectId
        ? 'bg-accent text-foreground'
        : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'"
      :title="projectName(tab.projectId)"
      @click="select(tab.projectId)"
    >
      <!-- status dot: running / awaiting permission -->
      <span class="relative flex h-2 w-2 shrink-0">
        <span
          v-if="stateOf(tab.projectId).running"
          class="absolute inline-flex h-full w-full rounded-full bg-primary opacity-60 animate-ping"
        />
        <span
          class="relative inline-flex h-2 w-2 rounded-full"
          :class="stateOf(tab.projectId).pending > 0
            ? 'bg-amber-500'
            : stateOf(tab.projectId).running
              ? 'bg-primary'
              : 'bg-muted-foreground/40'"
        />
      </span>

      <FolderOpen class="h-[13px] w-[13px] shrink-0 opacity-70" :stroke-width="1.75" />
      <span class="truncate">{{ projectName(tab.projectId) }}</span>

      <!-- pending-permission badge (summed across the project's runs) -->
      <span
        v-if="stateOf(tab.projectId).pending > 0"
        class="flex h-4 min-w-[16px] items-center justify-center rounded-full bg-amber-500 px-1 text-[10px] font-medium text-white leading-none shrink-0"
      >{{ stateOf(tab.projectId).pending }}</span>

      <!-- close -->
      <span
        class="flex h-4 w-4 items-center justify-center rounded shrink-0 opacity-0 group-hover:opacity-100 hover:bg-foreground/10 transition-opacity"
        :class="{ 'opacity-60': tab.projectId === activeProjectId }"
        role="button"
        aria-label="Close tab"
        @click="closeTab(tab.projectId, $event)"
      >
        <X class="h-3 w-3" :stroke-width="2" />
      </span>
    </button>
  </div>
</template>
