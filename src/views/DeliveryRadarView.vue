<script setup lang="ts">
// Delivery Radar: every open GitHub issue of the selected Devdy projects (all
// their repos), classified against the Deadline of each project's linked
// GitHub Project board and rolled up per assignee — so a manager sees at a
// glance who is late. Rendered only in its own OS window (lib/deliveryRadarWindow).
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { openUrl } from '@tauri-apps/plugin-opener'
import { CheckCheck, ChevronRight, CornerDownRight, ExternalLink, FolderOpen, ListPlus, Radar, RefreshCw, SearchX } from 'lucide-vue-next'
import { useProjectsStore, type Project } from '@/stores/projects'
import { useDeliveryRadarStore } from '@/stores/deliveryRadar'
import {
  Banner, Button, Card, ContextMenu, DropdownItem, Input, PageHeader, SegmentedControl, Skeleton,
} from '@/components/ui'
import { DELIVERY_RADAR_OPEN_ROUTE, type DeliveryRadarOpenRoute } from '@/lib/deliveryRadarWindow'
import TodoCaptureModal from '@/components/focus/TodoCaptureModal.vue'
import DoneStatusesModal from '@/components/radar/DoneStatusesModal.vue'
import type { TodoPrefill } from '@/lib/focus'
import {
  buildGroups,
  classifyAll,
  countByStatus,
  filterIssues,
  statusDotClass,
  type ClassifiedIssue,
  type RadarGroup,
  type RadarGroupMode,
  type RadarStatus,
} from '@/lib/deliveryRadar'

const { t } = useI18n()
const projectsStore = useProjectsStore()
const store = useDeliveryRadarStore()

const isRadarWindow = new URLSearchParams(window.location.search).get('radarWindow') === '1'
const PROJECTS_KEY = 'devdy:radarProjects'
const GROUP_KEY = 'devdy:radarGroupMode'

// ── Project scope ────────────────────────────────────────────────────────────
// Only projects with a linked GitHub Project board carry the Deadline field.
const boardProjects = computed(() => projectsStore.projects.filter((p) => !!p.github_project_board_url))
// Empty = all board projects.
const selectedProjects = ref<Set<string>>(new Set())
const scopeProjects = computed(() =>
  selectedProjects.value.size === 0
    ? boardProjects.value
    : boardProjects.value.filter((p) => selectedProjects.value.has(p.id)),
)
const scopeIds = computed(() => scopeProjects.value.map((p) => p.id))

function toggleProject(id: string) {
  const next = new Set(selectedProjects.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  // Every project ticked is the same as "All".
  selectedProjects.value = next.size === boardProjects.value.length ? new Set() : next
}
function onlyProject(id: string) {
  if (boardProjects.value.some((p) => p.id === id)) selectedProjects.value = new Set([id])
}

watch(selectedProjects, (v) => {
  try { localStorage.setItem(PROJECTS_KEY, JSON.stringify([...v])) } catch { /* best-effort */ }
})

// ── Loading ──────────────────────────────────────────────────────────────────
const now = ref(Date.now())
let tick: ReturnType<typeof setInterval> | null = null
let setProjectUnlisten: UnlistenFn | null = null

function refresh(force = false) {
  now.value = Date.now()
  return store.refresh(scopeIds.value, { force })
}

onMounted(async () => {
  if (isRadarWindow) {
    document.title = t('radar.windowTitle')
    // Reopening the window from a project card narrows it to that project.
    setProjectUnlisten = await listen<{ projectId: string }>('radar:set-project', (e) => {
      if (e.payload?.projectId) onlyProject(e.payload.projectId)
    })
  }
  if (projectsStore.projects.length === 0) {
    try { await projectsStore.fetchProjects() } catch { /* ignore */ }
  }
  const urlProject = new URLSearchParams(window.location.search).get('project')
  if (urlProject && boardProjects.value.some((p) => p.id === urlProject)) {
    selectedProjects.value = new Set([urlProject])
  } else {
    try {
      const saved = JSON.parse(localStorage.getItem(PROJECTS_KEY) ?? '[]') as string[]
      const known = saved.filter((id) => boardProjects.value.some((p) => p.id === id))
      selectedProjects.value = new Set(known)
    } catch { /* ignore */ }
  }
  void refresh()
  // Keep "overdue" / "updated … ago" honest while the window stays open.
  tick = setInterval(() => { now.value = Date.now() }, 60_000)
})

onBeforeUnmount(() => {
  setProjectUnlisten?.()
  if (tick) clearInterval(tick)
})

watch(scopeIds, (ids, prev) => {
  if (ids.join() !== prev?.join()) void refresh()
})

// ── Classification + grouping ────────────────────────────────────────────────
const classified = computed(() =>
  classifyAll(store.issuesFor(scopeProjects.value), now.value, store.thresholds, store.doneRules),
)
const counts = computed(() => countByStatus(classified.value))

const doneStatusesOpen = ref(false)
const statusFilter = ref<RadarStatus | null>(null)
const query = ref('')
const groupMode = ref<RadarGroupMode>(localStorage.getItem(GROUP_KEY) === 'project' ? 'project' : 'person')
watch(groupMode, (m) => {
  try { localStorage.setItem(GROUP_KEY, m) } catch { /* best-effort */ }
})
const groupModeOptions = computed(() => [
  { value: 'person' as const, label: t('radar.groupPerson') },
  { value: 'project' as const, label: t('radar.groupProject') },
])

const groups = computed(() =>
  buildGroups(filterIssues(classified.value, { status: statusFilter.value, query: query.value }), groupMode.value),
)

function toggleStatus(s: RadarStatus) {
  statusFilter.value = statusFilter.value === s ? null : s
}

// Columns of the summary table (on-track only counts towards "Open").
const COLUMNS: RadarStatus[] = ['overdue', 'due-soon', 'not-started', 'stale', 'no-deadline']
const CARDS: RadarStatus[] = [...COLUMNS, 'on-track']

const TONE_TEXT: Record<RadarStatus, string> = {
  overdue: 'text-destructive',
  'due-soon': 'text-warning',
  'not-started': 'text-info',
  stale: 'text-foreground',
  'no-deadline': 'text-muted-foreground',
  'on-track': 'text-success',
}
const TONE_PILL: Record<RadarStatus, string> = {
  overdue: 'bg-destructive/15 text-destructive border-destructive/30',
  'due-soon': 'bg-warning/15 text-warning border-warning/30',
  'not-started': 'bg-info/15 text-info border-info/30',
  stale: 'bg-muted text-foreground border-border',
  'no-deadline': 'bg-muted/60 text-muted-foreground border-border/70',
  'on-track': 'bg-success/15 text-success border-success/30',
}

function statusHint(s: RadarStatus): string {
  const days = s === 'due-soon' ? store.thresholds.dueSoonDays : store.thresholds.staleDays
  return t(`radar.statusHint.${s}`, { days })
}

// ── Expansion ────────────────────────────────────────────────────────────────
function groupId(g: RadarGroup): string {
  return g.key ?? '\u0000unassigned'
}
const expanded = ref<Set<string>>(new Set())
function toggleGroup(g: RadarGroup) {
  const next = new Set(expanded.value)
  const id = groupId(g)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  expanded.value = next
}
const allExpanded = computed(() => groups.value.length > 0 && groups.value.every((g) => expanded.value.has(groupId(g))))
function toggleAll() {
  expanded.value = allExpanded.value ? new Set() : new Set(groups.value.map(groupId))
}
// Narrowing to one status or a search means "show me those issues": open everything.
watch([statusFilter, query], ([s, q]) => {
  if (s || q.trim()) expanded.value = new Set(groups.value.map(groupId))
})
watch(groupMode, () => { expanded.value = new Set() })

function groupLabel(g: RadarGroup): string {
  return g.key === null ? t('radar.unassigned') : g.label
}
function isTesterGroup(g: RadarGroup): boolean {
  return groupMode.value === 'person' && g.key !== null && store.roleOf(g.key) === 'tester'
}

// ── Issue rows ───────────────────────────────────────────────────────────────
function deadlineText(i: ClassifiedIssue): string {
  if (i.daysLeft === null) return t('radar.noDeadline')
  if (i.daysLeft < 0) return t('radar.lateDays', { n: -i.daysLeft })
  if (i.daysLeft === 0) return t('radar.dueToday')
  return t('radar.daysLeft', { n: i.daysLeft })
}
function deadlineDate(i: ClassifiedIssue): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(i.deadline ?? '')
  return m ? `${m[3]}/${m[2]}` : ''
}
/** Project (person mode) or assignees (project mode) shown on an issue row. */
function issueContext(i: ClassifiedIssue, g: RadarGroup): string {
  if (groupMode.value === 'project') return i.assignees.join(', ') || t('radar.unassigned')
  const others = i.assignees.filter((a) => a !== g.key)
  return others.length && g.key !== null
    ? `${i.projectName} · ${t('radar.alsoAssigned', { names: others.join(', ') })}`
    : i.projectName
}


function openIssue(i: ClassifiedIssue) {
  openUrl(i.htmlUrl).catch(() => {})
}

// Row menu (right-click): "+ Todo" from the issue (title, project, link and
// deadline prefilled), open it on GitHub.
const rowMenu = ref<{ x: number, y: number, issue: ClassifiedIssue } | null>(null)
const todoPrefill = ref<TodoPrefill | null>(null)
function openRowMenu(e: MouseEvent, issue: ClassifiedIssue) {
  rowMenu.value = { x: e.clientX, y: e.clientY, issue }
}
function addTodo(issue: ClassifiedIssue) {
  rowMenu.value = null
  todoPrefill.value = {
    text: `${issue.title} (${issue.repo}#${issue.number})`,
    projectId: issue.projectId,
    sourceUrl: issue.htmlUrl,
    dueDate: /^\d{4}-\d{2}-\d{2}/.exec(issue.deadline ?? '')?.[0] ?? null,
  }
}
function openFromMenu() {
  const issue = rowMenu.value?.issue
  rowMenu.value = null
  if (issue) openIssue(issue)
}

// ── Banners ──────────────────────────────────────────────────────────────────
function deadlineMapped(p: Project): boolean {
  try {
    return !!JSON.parse(p.github_project_field_mappings ?? 'null')?.deadlineFieldId
  } catch {
    return false
  }
}
const banners = computed(() => {
  const out: { id: string, tone: 'destructive' | 'warning' | 'info', text: string, project: Project, retry?: boolean, settings?: boolean }[] = []
  for (const p of scopeProjects.value) {
    const err = store.errors[p.id]
    if (err) out.push({ id: `${p.id}:err`, tone: 'destructive', text: t('radar.bannerError', { project: p.name, error: err }), project: p, retry: true })
    if (!deadlineMapped(p)) out.push({ id: `${p.id}:dl`, tone: 'warning', text: t('radar.bannerNoBoard', { project: p.name }), project: p, settings: true })
    const board = store.boards[p.id]?.board
    if (board?.skippedNonGithub) out.push({ id: `${p.id}:gl`, tone: 'info', text: t('radar.bannerSkippedNonGithub', { project: p.name }), project: p })
    if (board?.truncated) out.push({ id: `${p.id}:tr`, tone: 'warning', text: t('radar.bannerTruncated', { project: p.name }), project: p })
  }
  return out
})

const hasAnyData = computed(() => scopeIds.value.some((id) => !!store.boards[id]))
const updatedAt = computed(() => store.oldestSavedAt(scopeIds.value))

function relativeTime(ms: number | null | string): string {
  if (ms === null || ms === '') return ''
  const then = typeof ms === 'string' ? new Date(ms).getTime() : ms
  if (Number.isNaN(then)) return ''
  const mins = Math.round((now.value - then) / 60000)
  if (mins < 1) return t('radar.justNow')
  if (mins < 60) return t('radar.minutesAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('radar.hoursAgo', { hours })
  return t('radar.daysAgo', { days: Math.round(hours / 24) })
}

function openSettings(p: Project) {
  // The radar lives in its own window: ask the main window to show the project settings.
  emit(DELIVERY_RADAR_OPEN_ROUTE, { path: `/projects/${p.id}/settings` } satisfies DeliveryRadarOpenRoute).catch(() => {})
}

const GRID = 'grid grid-cols-[minmax(150px,1.2fr)_repeat(5,minmax(64px,80px))_56px_76px_minmax(120px,1fr)] items-center gap-x-2'
</script>

<template>
  <div class="flex flex-col h-full">
    <PageHeader :title="t('radar.title')" :count="classified.length" :count-title="t('radar.openIssues')">
      <template #leading>
        <Radar class="h-4 w-4 text-primary shrink-0" :stroke-width="2" />
      </template>
      <template #actions>
        <label class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          {{ t('radar.dueSoonLabel') }}
          <Input v-model.number="store.thresholds.dueSoonDays" type="number" size="sm" class="w-14" />
          {{ t('radar.days') }}
        </label>
        <label class="flex items-center gap-1.5 text-[11px] text-muted-foreground">
          {{ t('radar.staleLabel') }}
          <Input v-model.number="store.thresholds.staleDays" type="number" size="sm" class="w-14" />
          {{ t('radar.days') }}
        </label>
        <Button
          variant="outline"
          :disabled="scopeIds.length === 0"
          :title="t('radar.doneStatuses.buttonHint')"
          @click="doneStatusesOpen = true"
        >
          <CheckCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
          {{ t('radar.doneStatuses.button') }}
        </Button>
        <Button variant="outline" :disabled="store.isLoading || scopeIds.length === 0" @click="refresh(true)">
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.isLoading }" :stroke-width="1.75" />
          {{ t('common.refresh') }}
        </Button>
      </template>
    </PageHeader>

    <!-- No board-configured projects -->
    <div v-if="boardProjects.length === 0" class="flex flex-col items-center justify-center flex-1 min-h-0 p-6 text-center">
      <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
        <FolderOpen class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('radar.noBoardTitle') }}</p>
      <p class="text-xs text-muted-foreground mt-1 max-w-80">{{ t('radar.noBoardHint') }}</p>
    </div>

    <template v-else>
      <!-- Toolbar: project scope, grouping, search -->
      <div class="shrink-0 border-b border-border/60 px-6 py-2 flex items-center gap-x-4 gap-y-2 flex-wrap">
        <div class="flex items-center gap-1.5 flex-wrap min-w-0">
          <span class="text-[11px] text-muted-foreground mr-0.5">{{ t('radar.projectsLabel') }}</span>
          <button
            class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
            :class="selectedProjects.size === 0
              ? 'border-primary/40 bg-primary/10 text-primary'
              : 'border-border/60 text-muted-foreground hover:text-foreground'"
            @click="selectedProjects = new Set()"
          >
            {{ t('radar.allProjects') }}
          </button>
          <button
            v-for="p in boardProjects" :key="p.id"
            class="rounded-full border px-2.5 py-0.5 text-[11px] transition-colors"
            :class="selectedProjects.has(p.id)
              ? 'border-primary/40 bg-primary/10 text-primary'
              : 'border-border/60 text-muted-foreground hover:text-foreground'"
            @click="toggleProject(p.id)"
          >
            {{ p.name }}
            <RefreshCw v-if="store.loading.has(p.id)" class="inline h-2.5 w-2.5 ml-0.5 animate-spin" :stroke-width="2" />
          </button>
        </div>
        <div class="ml-auto flex items-center gap-2">
          <SegmentedControl v-model="groupMode" :options="groupModeOptions" :stretch="false" />
          <Input v-model="query" size="sm" class="w-56" :placeholder="t('radar.searchPlaceholder')" />
        </div>
      </div>

      <!-- First load: skeleton mirroring cards + table -->
      <div v-if="!hasAnyData && store.isLoading" class="flex-1 min-h-0 overflow-auto p-6 space-y-4">
        <div class="grid grid-cols-3 lg:grid-cols-6 gap-3">
          <div v-for="i in 6" :key="i" class="rounded-lg border border-border/60 bg-card p-3 space-y-2.5">
            <Skeleton class="h-2.5 w-16 rounded" />
            <Skeleton class="h-6 w-10 rounded" />
          </div>
        </div>
        <div class="rounded-lg border border-border/60 overflow-hidden">
          <div v-for="i in 6" :key="i" class="flex items-center gap-4 border-b border-border/30 last:border-b-0 px-3 h-10">
            <Skeleton class="h-2.5 w-28 rounded" />
            <Skeleton v-for="j in 5" :key="j" class="h-2.5 w-8 rounded" />
          </div>
        </div>
      </div>

      <div v-else class="flex-1 min-h-0 overflow-auto">
        <div class="p-6 space-y-4">
          <!-- Per-project problems -->
          <Banner v-for="b in banners" :key="b.id" :tone="b.tone">
            {{ b.text }}
            <template v-if="b.retry || b.settings" #actions>
              <Button v-if="b.retry" size="xs" variant="outline" @click="store.refresh([b.project.id], { force: true })">
                {{ t('radar.retry') }}
              </Button>
              <Button v-if="b.settings" size="xs" variant="outline" @click="openSettings(b.project)">
                {{ t('radar.bannerNoBoardLink') }}
              </Button>
            </template>
          </Banner>

          <!-- Summary cards (click = filter to that status) -->
          <div class="grid grid-cols-3 lg:grid-cols-6 gap-3">
            <Card
              v-for="s in CARDS" :key="s"
              body-class="p-3"
              role="button"
              tabindex="0"
              class="cursor-pointer transition-colors hover:bg-accent/50"
              :class="statusFilter === s ? 'ring-2 ring-primary/60' : ''"
              :title="`${statusHint(s)} — ${t('radar.filterHint')}`"
              :aria-pressed="statusFilter === s"
              @click="toggleStatus(s)"
              @keydown.enter.prevent="toggleStatus(s)"
            >
              <div class="text-[11px] text-muted-foreground">{{ t(`radar.status.${s}`) }}</div>
              <div class="text-xl font-semibold mt-0.5" :class="counts[s] > 0 ? TONE_TEXT[s] : ''">{{ counts[s] }}</div>
            </Card>
          </div>

          <!-- Empty -->
          <div v-if="groups.length === 0" class="flex flex-col items-center justify-center min-h-60 text-center">
            <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
              <SearchX class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
            </div>
            <p class="text-sm font-medium">{{ t('radar.nothingTitle') }}</p>
            <p class="text-xs text-muted-foreground mt-1">{{ t('radar.nothingHint') }}</p>
          </div>

          <!-- Rollup table -->
          <Card v-else body-class="text-xs">
            <div :class="GRID" class="px-3 h-8 border-b border-border/60 bg-muted/30 text-[11px] text-muted-foreground font-medium sticky top-0 z-10">
              <button class="flex items-center gap-1 text-left hover:text-foreground" @click="toggleAll">
                <ChevronRight class="h-3 w-3 transition-transform" :class="{ 'rotate-90': allExpanded }" :stroke-width="2" />
                {{ groupMode === 'person' ? t('radar.colPerson') : t('radar.colProject') }}
              </button>
              <div v-for="s in COLUMNS" :key="s" class="text-center truncate" :title="statusHint(s)">{{ t(`radar.status.${s}`) }}</div>
              <div class="text-center">{{ t('radar.colTotal') }}</div>
              <div class="text-center">{{ t('radar.colWorst') }}</div>
              <div class="truncate">{{ groupMode === 'person' ? t('radar.colRelatedProjects') : t('radar.colRelatedPeople') }}</div>
            </div>

            <div v-for="g in groups" :key="groupId(g)" class="border-b border-border/40 last:border-b-0">
              <button
                :class="GRID"
                class="w-full px-3 h-10 text-left hover:bg-accent/50 transition-colors"
                :aria-expanded="expanded.has(groupId(g))"
                @click="toggleGroup(g)"
              >
                <span class="flex items-center gap-1.5 min-w-0">
                  <ChevronRight class="h-3 w-3 shrink-0 text-muted-foreground transition-transform" :class="{ 'rotate-90': expanded.has(groupId(g)) }" :stroke-width="2" />
                  <span class="truncate text-[13px] font-medium" :class="{ 'italic text-muted-foreground': g.key === null }">{{ groupLabel(g) }}</span>
                  <span
                    v-if="isTesterGroup(g)"
                    class="shrink-0 rounded border border-info/30 bg-info/15 px-1 text-[10px] font-medium text-info"
                    :title="t('radar.testerHint')"
                  >{{ t('radar.tester') }}</span>
                </span>
                <span v-for="s in COLUMNS" :key="s" class="flex justify-center">
                  <span
                    v-if="g.counts[s] > 0"
                    class="min-w-7 rounded-full border px-1.5 py-0.5 text-center text-[11px] font-semibold"
                    :class="TONE_PILL[s]"
                  >{{ g.counts[s] }}</span>
                  <span v-else class="text-muted-foreground/40">–</span>
                </span>
                <span class="text-center font-medium">{{ g.total }}</span>
                <span class="text-center" :class="g.worstLateDays ? 'text-destructive font-medium' : 'text-muted-foreground/40'">
                  {{ g.worstLateDays ? t('radar.lateDays', { n: g.worstLateDays }) : '–' }}
                </span>
                <span class="truncate text-[11px] text-muted-foreground" :title="g.related.join(', ')">{{ g.related.join(', ') }}</span>
              </button>

              <!-- Issues of the group -->
              <div v-if="expanded.has(groupId(g))" class="bg-muted/20 border-t border-border/30 py-1">
                <div
                  v-for="{ issue: i, depth } in g.rows" :key="i.key"
                  class="group flex items-center gap-2 pr-3 h-8 hover:bg-accent/50 cursor-pointer"
                  :style="{ paddingLeft: `${2 + depth * 1.25}rem` }"
                  :title="i.parent ? `${i.title}\n${t('radar.parentHint', { title: `#${i.parent.number} ${i.parent.title}` })}` : i.title"
                  @click="openIssue(i)"
                  @contextmenu.prevent="openRowMenu($event, i)"
                >
                  <CornerDownRight v-if="depth > 0" class="h-3 w-3 shrink-0 -ml-1 text-muted-foreground/60" :stroke-width="1.75" />
                  <span class="w-24 shrink-0 rounded border px-1.5 py-0.5 text-center text-[10px] font-medium truncate" :class="TONE_PILL[i.radar]">
                    {{ t(`radar.status.${i.radar}`) }}
                  </span>
                  <span class="font-mono text-[10px] text-muted-foreground/70 shrink-0">{{ i.repo }}#{{ i.number }}</span>
                  <span class="flex min-w-0 flex-1 items-baseline gap-1.5">
                    <span class="truncate">{{ i.title }}</span>
                    <!-- Parent not in this list (other assignee, done, …): say where the task belongs. -->
                    <span v-if="depth === 0 && i.parent" class="hidden md:inline truncate shrink-[2] text-[10px] text-muted-foreground/70">
                      ↑ #{{ i.parent.number }} {{ i.parent.title }}
                    </span>
                  </span>
                  <span class="hidden md:inline truncate max-w-48 text-[11px] text-muted-foreground">{{ issueContext(i, g) }}</span>
                  <span class="flex items-center gap-1 shrink-0 w-28 text-[11px] text-muted-foreground truncate">
                    <span class="h-2 w-2 rounded-full shrink-0" :class="statusDotClass(i.statusColor)" />
                    <span class="truncate">{{ i.status || t('radar.noStatus') }}</span>
                  </span>
                  <span
                    class="shrink-0 w-28 text-right text-[11px]"
                    :class="[i.daysLeft !== null && i.daysLeft < 0 ? 'text-destructive font-medium' : 'text-muted-foreground', { italic: i.deadlineInherited }]"
                    :title="i.deadlineInherited ? t('radar.inheritedDeadline') : undefined"
                  >
                    {{ deadlineText(i) }}<span v-if="deadlineDate(i)" class="text-muted-foreground/60"> · {{ deadlineDate(i) }}</span>
                  </span>
                  <span class="hidden lg:inline shrink-0 w-28 text-right text-[10px] text-muted-foreground/70">
                    {{ t('radar.updatedAgo', { time: relativeTime(i.updatedAt) }) }}
                  </span>
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    class="opacity-0 group-hover:opacity-100 shrink-0"
                    :title="t('focus.addTodoHint')"
                    :aria-label="t('focus.addTodoHint')"
                    @click.stop="addTodo(i)"
                  >
                    <ListPlus class="h-3 w-3" :stroke-width="1.75" />
                  </Button>
                </div>
              </div>
            </div>
          </Card>

          <div v-if="updatedAt" class="text-[10px] text-muted-foreground/60 text-right">
            {{ t('radar.updated', { time: relativeTime(updatedAt) }) }}
          </div>
        </div>
      </div>
    </template>

    <ContextMenu :open="!!rowMenu" :x="rowMenu?.x ?? 0" :y="rowMenu?.y ?? 0" @close="rowMenu = null">
      <DropdownItem @click="rowMenu && addTodo(rowMenu.issue)">
        <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('focus.addTodoHint') }}
      </DropdownItem>
      <DropdownItem @click="openFromMenu">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('radar.openOnGithub') }}
      </DropdownItem>
    </ContextMenu>
    <TodoCaptureModal :open="!!todoPrefill" :prefill="todoPrefill" @close="todoPrefill = null" />
    <DoneStatusesModal :open="doneStatusesOpen" :projects="scopeProjects" @close="doneStatusesOpen = false" />
  </div>
</template>
