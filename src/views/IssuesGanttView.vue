<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useProjectsStore } from '@/stores/projects'
import { useProjectIssuesStore, type MilestoneGroup } from '@/stores/projectIssues'
import { Button, Card, AppSelect, Skeleton } from '@/components/ui'
import GanttChart from '@/components/GanttChart.vue'
import { openGanttWindow } from '@/lib/ganttWindow'
import { RefreshCw, CircleDot, AlertTriangle, Info, GanttChartSquare, FolderOpen, ExternalLink } from 'lucide-vue-next'

const route = useRoute()
const projectsStore = useProjectsStore()
const store = useProjectIssuesStore()
const { t } = useI18n()

// True when this view is rendered as the standalone pop-out window (App.vue
// renders it directly for `?ganttWindow=1`). Hides the "open in new window"
// button and lets the window respond to project-switch events instead of the
// router query.
const isGanttWindow = new URLSearchParams(window.location.search).get('ganttWindow') === '1'

const DAY = 86_400_000
const LAST_KEY = 'devdy:ganttProjectId'
const selectedProjectId = ref<string>('')
const selectedRepos = ref<Set<string>>(new Set())

// Only projects with a linked GitHub Project board (they're the ones with
// Start date / Deadline / Status data worth plotting on a Gantt).
const boardProjects = computed(() =>
  projectsStore.projects.filter((p) => !!p.github_project_board_url),
)
const projectOptions = computed(() =>
  boardProjects.value.map((p) => ({ value: p.id, label: p.name })),
)

function isBoardProject(id: string | null | undefined): boolean {
  return !!id && boardProjects.value.some((p) => p.id === id)
}

let setProjectUnlisten: UnlistenFn | null = null

onMounted(async () => {
  if (projectsStore.projects.length === 0) {
    try { await projectsStore.fetchProjects() } catch { /* ignore */ }
  }
  if (isGanttWindow) {
    document.title = t('gantt.windowTitle')
    // Reopening the window with a different project focuses it and switches here.
    setProjectUnlisten = await listen<{ projectId: string }>('gantt:set-project', (e) => {
      const id = e.payload?.projectId
      if (id && isBoardProject(id)) selectedProjectId.value = id
    })
  }
  // Priority: explicit ?project= (deep link from Run AI / project card, or the
  // pop-out window URL) → persisted choice → last-loaded in the Gantt store →
  // most recent board project.
  const urlProject = new URLSearchParams(window.location.search).get('project')
  const queried =
    (typeof route.query.project === 'string' ? route.query.project : null) ?? urlProject
  const remembered = localStorage.getItem(LAST_KEY)
  const initial =
    (isBoardProject(queried) ? queried : null) ??
    (isBoardProject(remembered) ? remembered : null) ??
    (isBoardProject(store.currentProjectId) ? store.currentProjectId : null) ??
    boardProjects.value[0]?.id ??
    ''
  selectedProjectId.value = initial
  if (initial && (store.currentProjectId !== initial || !store.board)) {
    store.refresh(initial)
  }
})

onBeforeUnmount(() => {
  setProjectUnlisten?.()
  setProjectUnlisten = null
})

// Honour deep-links that change the ?project= param while already on this screen.
watch(() => route.query.project, (p) => {
  if (typeof p === 'string' && isBoardProject(p) && p !== selectedProjectId.value) {
    selectedProjectId.value = p
  }
})

watch(selectedProjectId, (id) => {
  if (id) {
    localStorage.setItem(LAST_KEY, id)
    selectedRepos.value = new Set()
    selectedAssignees.value = new Set()
    expandedMilestones.value = new Set()
    store.refresh(id)
  }
})

// ── Closed-issue expansion (per milestone, loaded on demand) ─────────────────
// Set of milestone titles (lowercased) currently showing their closed issues.
const expandedMilestones = ref<Set<string>>(new Set())
const loadingMilestones = computed(
  () => new Set([...expandedMilestones.value].filter((k) => store.isLoadingClosed(selectedProjectId.value, k))),
)
async function toggleClosed(title: string) {
  const key = title.toLowerCase()
  const next = new Set(expandedMilestones.value)
  if (next.has(key)) {
    next.delete(key)
    expandedMilestones.value = next
    return
  }
  next.add(key)
  expandedMilestones.value = next
  if (!store.closedIssuesFor(selectedProjectId.value, title)) {
    await store.loadMilestoneIssues(selectedProjectId.value, title)
  }
}

// ── Repo filter ────────────────────────────────────────────────────────────
const allRepos = computed(() => {
  const set = new Set<string>()
  for (const g of store.board?.milestones ?? []) for (const r of g.repos) set.add(r)
  return [...set].sort()
})
function toggleRepo(repo: string) {
  const next = new Set(selectedRepos.value)
  if (next.has(repo)) next.delete(repo)
  else next.add(repo)
  selectedRepos.value = next
}
function repoActive(repo: string) {
  return selectedRepos.value.size === 0 || selectedRepos.value.has(repo)
}

// ── Assignee filter + grouping ───────────────────────────────────────────────
type GroupMode = 'milestone' | 'assignee'
type SortMode = 'start' | 'assignee'
const UNASSIGNED = '__unassigned__'
const groupMode = ref<GroupMode>('milestone')
const sortMode = ref<SortMode>('start')
const selectedAssignees = ref<Set<string>>(new Set())

// The issue set currently in scope: board open issues, swapped to the full
// open+closed set for any milestone the user has expanded.
const scopedGroups = computed(() => {
  const src = store.board?.milestones ?? []
  const pid = selectedProjectId.value
  return src.map((g) => {
    const expanded = expandedMilestones.value.has(g.title.toLowerCase())
    const closed = expanded ? store.closedIssuesFor(pid, g.title) : null
    return { group: g, issues: closed ?? g.issues }
  })
})

const allAssignees = computed(() => {
  const set = new Set<string>()
  for (const { issues } of scopedGroups.value) for (const i of issues) for (const a of i.assignees) set.add(a)
  return [...set].sort()
})
// Whether any scoped issue has no assignee (drives the "Unassigned" chip).
const hasUnassigned = computed(() =>
  scopedGroups.value.some(({ issues }) => issues.some((i) => i.assignees.length === 0)),
)
function toggleAssignee(a: string) {
  const next = new Set(selectedAssignees.value)
  if (next.has(a)) next.delete(a)
  else next.add(a)
  selectedAssignees.value = next
}
function assigneePasses(issue: { assignees: string[] }): boolean {
  if (selectedAssignees.value.size === 0) return true
  if (issue.assignees.length === 0) return selectedAssignees.value.has(UNASSIGNED)
  return issue.assignees.some((a) => selectedAssignees.value.has(a))
}
function setGroupMode(mode: GroupMode) {
  groupMode.value = mode
}
function setSortMode(mode: SortMode) {
  sortMode.value = mode
}

// ── Milestone sorting (overdue → at-risk → on-track → none) ───────────────────
type MsStatus = 'overdue' | 'at-risk' | 'ontrack' | 'none'
function msStatus(g: MilestoneGroup): MsStatus {
  if (g.title === 'No milestone') return 'none'
  const due = g.dueOn ? new Date(g.dueOn).getTime() : null
  if (due !== null && due < Date.now() && g.openCount > 0) return 'overdue'
  if (due !== null && due - Date.now() <= store.atRiskDays * DAY && g.openCount > 0) return 'at-risk'
  return 'ontrack'
}
const MS_WEIGHT: Record<MsStatus, number> = { overdue: 0, 'at-risk': 1, ontrack: 2, none: 3 }

// Group scoped issues by their assignee login. An issue with multiple assignees
// shows in each of their lanes; unassigned issues fall into one bucket.
function groupByAssignee(groups: MilestoneGroup[]): MilestoneGroup[] {
  const unassignedLabel = t('gantt.unassigned')
  const byKey = new Map<string, MilestoneGroup>()
  for (const g of groups) {
    for (const issue of g.issues) {
      const keys = issue.assignees.length ? issue.assignees : [unassignedLabel]
      for (const key of keys) {
        let lane = byKey.get(key)
        if (!lane) {
          lane = { title: key, dueOn: null, repos: [], openCount: 0, closedCount: 0, issues: [] }
          byKey.set(key, lane)
        }
        lane.issues.push(issue)
        if (!lane.repos.includes(issue.repo)) lane.repos.push(issue.repo)
        lane.openCount = lane.issues.length
      }
    }
  }
  return [...byKey.values()].sort((a, b) => {
    const au = a.title === unassignedLabel ? 1 : 0
    const bu = b.title === unassignedLabel ? 1 : 0
    return au - bu || a.title.localeCompare(b.title)
  })
}

const filteredMilestones = computed<MilestoneGroup[]>(() => {
  const filtered = scopedGroups.value
    .map(({ group, issues }) => ({
      ...group,
      issues: issues.filter((i) => repoActive(i.repo) && assigneePasses(i)),
    }))
    .filter((g) => g.issues.length > 0)
  if (groupMode.value === 'assignee') return groupByAssignee(filtered)
  return filtered.sort((a, b) => MS_WEIGHT[msStatus(a)] - MS_WEIGHT[msStatus(b)])
})

// Fixed geometry for the loading skeleton's fake Gantt bars: [label width %,
// bar left offset %, bar width %]. Deterministic so it looks like a real
// timeline (staggered bars) without any randomness.
const GANTT_SKELETON_ROWS: [number, number, number][] = [
  [58, 4, 34],
  [72, 18, 28],
  [46, 30, 40],
  [64, 12, 24],
  [52, 40, 30],
  [68, 22, 46],
]

function relativeTime(iso: string | null): string {
  if (!iso) return ''
  const then = new Date(iso).getTime()
  const mins = Math.round((Date.now() - then) / 60000)
  if (mins < 1) return t('gantt.justNow')
  if (mins < 60) return t('gantt.minutesAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('gantt.hoursAgo', { hours })
  const days = Math.round(hours / 24)
  return t('gantt.daysAgo', { days })
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between px-6 h-13 border-b border-border/60 shrink-0 gap-4">
      <div class="flex items-center gap-2 min-w-0">
        <GanttChartSquare class="h-4 w-4 text-primary shrink-0" :stroke-width="2" />
        <h1 class="text-sm font-semibold shrink-0">{{ t('gantt.title') }}</h1>
        <span class="text-muted-foreground/40 shrink-0">/</span>
        <AppSelect
          v-model="selectedProjectId"
          size="sm"
          class="min-w-44 max-w-64"
          :options="projectOptions"
          :placeholder="t('gantt.selectProject')"
        />
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <label class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          {{ t('gantt.stalledLabel') }}
          <input
            v-model.number="store.staleDays"
            type="number"
            class="w-14 rounded border border-border/60 bg-transparent px-1.5 py-0.5 text-[11px]"
          />
          {{ t('gantt.days') }}
        </label>
        <Button variant="ghost" :disabled="store.loading || !selectedProjectId" @click="store.refresh(selectedProjectId, { force: true })">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.loading }" :stroke-width="2" />
          {{ t('common.refresh') }}
        </Button>
        <Button
          v-if="!isGanttWindow"
          variant="ghost"
          :title="t('gantt.openInNewWindow')"
          :disabled="!selectedProjectId"
          @click="openGanttWindow(selectedProjectId)"
        >
          <ExternalLink class="h-3.5 w-3.5" :stroke-width="2" />
          {{ t('gantt.newWindow') }}
        </Button>
      </div>
    </div>

    <!-- Content -->
    <div class="flex-1 flex flex-col min-h-0 overflow-hidden">
      <!-- No board-configured projects -->
      <div v-if="projectOptions.length === 0" class="flex flex-col items-center justify-center flex-1 min-h-0 p-6 text-center">
        <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
          <FolderOpen class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
        </div>
        <p class="text-sm font-medium">{{ t('gantt.noBoardTitle') }}</p>
        <p class="text-xs text-muted-foreground mt-1 max-w-72">
          {{ t('gantt.noBoardHint') }}
        </p>
      </div>

      <!-- Loading — mirrors the real layout (summary cards → repo chips →
           timeline) so the wait previews what's coming instead of blank boxes. -->
      <div v-else-if="store.loading && !store.board" class="flex-1 min-h-0 overflow-auto p-6 space-y-4">
        <!-- Summary cards -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3">
          <div
            v-for="i in 4" :key="i"
            class="animate-fade-rise rounded-lg border border-border/60 bg-card p-3 space-y-2.5"
            :style="{ animationDelay: `${i * 60}ms` }"
          >
            <Skeleton class="h-2.5 w-16 rounded" />
            <Skeleton class="h-6 w-10 rounded" />
          </div>
        </div>

        <!-- Repo chips -->
        <div class="flex items-center gap-1.5">
          <Skeleton v-for="i in 4" :key="i" class="h-5 w-16 rounded-full" />
        </div>

        <!-- Timeline frame -->
        <div class="animate-fade-rise rounded-lg border border-border/60 overflow-hidden" :style="{ animationDelay: '260ms' }">
          <div class="flex h-7 items-center border-b border-border/60 bg-card px-3">
            <Skeleton class="h-2.5 w-40 rounded" />
          </div>
          <div
            v-for="(row, i) in GANTT_SKELETON_ROWS" :key="i"
            class="flex items-center border-b border-border/30 last:border-b-0 px-3"
            :style="{ height: '34px' }"
          >
            <Skeleton class="h-2.5 rounded shrink-0" :style="{ width: `${row[0]}px` }" />
            <div class="relative ml-4 flex-1">
              <Skeleton class="h-4 rounded absolute" :style="{ left: `${row[1]}%`, width: `${row[2]}%` }" />
            </div>
          </div>
        </div>
      </div>

      <!-- Error -->
      <div v-else-if="store.error" class="m-6 p-4 bg-destructive/10 text-destructive rounded-lg text-sm border border-destructive/20">
        {{ store.error }}
      </div>

      <div v-else-if="store.board" class="flex-1 flex flex-col min-h-0">
        <!-- Fixed region: banners, summary cards, repo filter -->
        <div class="shrink-0 p-6 pb-4 space-y-4">
        <!-- Banners -->
        <div
          v-if="!store.board.boardLinked"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-primary/10 border border-primary/20 text-foreground"
        >
          <Info class="h-4 w-4 text-primary shrink-0" />
          {{ t('gantt.bannerNoBoardPrefix') }}
          <RouterLink :to="`/projects/${selectedProjectId}/settings`" class="text-primary hover:underline font-medium">{{ t('gantt.bannerNoBoardLink') }}</RouterLink>.
        </div>
        <div
          v-if="store.board.skippedNonGithub"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-amber-500/10 border border-amber-500/20"
        >
          <AlertTriangle class="h-4 w-4 text-amber-500 shrink-0" />
          {{ t('gantt.bannerSkippedNonGithub') }}
        </div>
        <div
          v-if="store.board.truncated"
          class="flex items-center gap-2 p-3 rounded-lg text-[12px] bg-amber-500/10 border border-amber-500/20"
        >
          <AlertTriangle class="h-4 w-4 text-amber-500 shrink-0" />
          {{ t('gantt.bannerTruncated') }}
        </div>

        <!-- Summary cards -->
        <div class="grid grid-cols-2 md:grid-cols-4 gap-3 animate-fade-rise">
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">{{ t('gantt.summaryOpen') }}</div>
            <div class="text-xl font-semibold mt-0.5">{{ store.summary.open }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">{{ t('gantt.summaryOverdue') }}</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.overdue > 0 ? 'text-red-500' : ''">{{ store.summary.overdue }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">{{ t('gantt.summaryAtRisk') }}</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.atRisk > 0 ? 'text-amber-500' : ''">{{ store.summary.atRisk }}</div>
          </Card>
          <Card body-class="p-3">
            <div class="text-[11px] text-muted-foreground">{{ t('gantt.summaryStalled') }}</div>
            <div class="text-xl font-semibold mt-0.5" :class="store.summary.stale > 0 ? 'text-yellow-500' : ''">{{ store.summary.stale }}</div>
          </Card>
        </div>

        <!-- Repo filter -->
        <div v-if="allRepos.length > 1" class="flex items-center gap-1.5 flex-wrap">
          <span class="text-[11px] text-muted-foreground mr-1">{{ t('gantt.repos') }}</span>
          <button
            v-for="repo in allRepos" :key="repo"
            class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
            :class="repoActive(repo)
              ? 'border-primary/40 bg-primary/10 text-primary'
              : 'border-border/60 text-muted-foreground hover:text-foreground'"
            @click="toggleRepo(repo)"
          >
            {{ repo }}
          </button>
        </div>

        <!-- Group mode + assignee filter -->
        <div class="flex items-center gap-x-4 gap-y-2 flex-wrap">
          <div class="flex items-center gap-1.5">
            <span class="text-[11px] text-muted-foreground mr-0.5">{{ t('gantt.groupBy') }}</span>
            <div class="inline-flex rounded-md border border-border/60 p-0.5">
              <button
                class="rounded px-2 py-0.5 text-[11px] transition-colors"
                :class="groupMode === 'milestone' ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:text-foreground'"
                @click="setGroupMode('milestone')"
              >
                {{ t('gantt.groupMilestone') }}
              </button>
              <button
                class="rounded px-2 py-0.5 text-[11px] transition-colors"
                :class="groupMode === 'assignee' ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:text-foreground'"
                @click="setGroupMode('assignee')"
              >
                {{ t('gantt.groupAssignee') }}
              </button>
            </div>
          </div>

          <div class="flex items-center gap-1.5">
            <span class="text-[11px] text-muted-foreground mr-0.5">{{ t('gantt.sortBy') }}</span>
            <div class="inline-flex rounded-md border border-border/60 p-0.5">
              <button
                class="rounded px-2 py-0.5 text-[11px] transition-colors"
                :class="sortMode === 'start' ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:text-foreground'"
                @click="setSortMode('start')"
              >
                {{ t('gantt.sortStart') }}
              </button>
              <button
                class="rounded px-2 py-0.5 text-[11px] transition-colors"
                :class="sortMode === 'assignee' ? 'bg-primary/10 text-primary' : 'text-muted-foreground hover:text-foreground'"
                @click="setSortMode('assignee')"
              >
                {{ t('gantt.groupAssignee') }}
              </button>
            </div>
          </div>

          <div v-if="allAssignees.length > 0 || hasUnassigned" class="flex items-center gap-1.5 flex-wrap">
            <span class="text-[11px] text-muted-foreground mr-1">{{ t('gantt.assigneesLabel') }}</span>
            <button
              v-for="a in allAssignees" :key="a"
              class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
              :class="selectedAssignees.has(a)
                ? 'border-primary/40 bg-primary/10 text-primary'
                : 'border-border/60 text-muted-foreground hover:text-foreground'"
              @click="toggleAssignee(a)"
            >
              {{ a }}
            </button>
            <button
              v-if="hasUnassigned"
              class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
              :class="selectedAssignees.has(UNASSIGNED)
                ? 'border-primary/40 bg-primary/10 text-primary'
                : 'border-border/60 text-muted-foreground hover:text-foreground'"
              @click="toggleAssignee(UNASSIGNED)"
            >
              {{ t('gantt.unassigned') }}
            </button>
          </div>
        </div>
        </div>
        <!-- end fixed region -->

        <!-- Scrollable region: only the Gantt issue rows scroll -->
        <div class="flex-1 flex flex-col min-h-0 px-6 pb-6">
        <!-- Empty -->
        <div v-if="filteredMilestones.length === 0" class="flex flex-1 flex-col items-center justify-center min-h-60 text-center">
          <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
            <CircleDot class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
          </div>
          <p class="text-sm font-medium">{{ t('gantt.noOpenIssues') }}</p>
          <p class="text-xs text-muted-foreground mt-1">{{ t('gantt.noOpenIssuesHint') }}</p>
        </div>

        <!-- Gantt -->
        <GanttChart
          v-else
          :milestones="filteredMilestones"
          :group-mode="groupMode"
          :sort-by="sortMode"
          :expanded="expandedMilestones"
          :loading="loadingMilestones"
          class="flex-1 min-h-0 animate-fade-rise"
          @toggle-closed="toggleClosed"
        />

        <div class="shrink-0 text-[10px] text-muted-foreground/60 text-right pt-2">
          {{ t('gantt.updated', { time: relativeTime(store.lastRefreshedAt) }) }}
        </div>
        </div>
        <!-- end scrollable region -->
      </div>
    </div>
  </div>
</template>
