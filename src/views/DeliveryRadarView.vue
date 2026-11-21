<script setup lang="ts">
// Delivery Radar: every open GitHub issue of the selected Devdy projects (all
// their repos), classified against the Deadline of each project's linked
// GitHub Project board and rolled up per assignee — so a manager sees at a
// glance who is late. Rendered only in its own OS window (lib/deliveryRadarWindow).
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import DockWindowButton from '@/components/surfaces/DockWindowButton.vue'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { openUrl } from '@tauri-apps/plugin-opener'
import {
  BookMarked, CalendarDays, ChevronRight, Clock, CornerDownRight, CornerUpLeft, ExternalLink, EyeOff, FolderOpen, ListPlus, Radar, RefreshCw, Search, SearchX, Settings2, X,
} from 'lucide-vue-next'
import { useProjectsStore, type Project } from '@/stores/projects'
import { useDeliveryRadarStore } from '@/stores/deliveryRadar'
import {
  Banner, Button, Card, ContextMenu, DropdownItem, Input, SegmentedControl, Skeleton,
} from '@/components/ui'
import { DELIVERY_RADAR_OPEN_ROUTE, type DeliveryRadarOpenRoute } from '@/lib/deliveryRadarWindow'
import TodoCaptureModal from '@/components/focus/TodoCaptureModal.vue'
import DoneStatusesModal from '@/components/radar/DoneStatusesModal.vue'
import RadarProjectPicker from '@/components/radar/RadarProjectPicker.vue'
import RadarSettingsModal from '@/components/radar/RadarSettingsModal.vue'
import type { TodoPrefill } from '@/lib/focus'
import { sortProjectsByName } from '@/lib/projectOptions'
import {
  buildGroups,
  classifyAll,
  countByStatus,
  filterIssues,
  sectionByRepo,
  statusDotClass,
  swatchClass,
  visibleRows,
  type ClassifiedIssue,
  type RadarGroup,
  type RadarGroupMode,
  type RadarRepoSection,
  type RadarStatus,
} from '@/lib/deliveryRadar'

const props = defineProps<{
  /** Narrow to this project (a tab of the main window; the window reads `?project=`). */
  project?: string
}>()

const { t, locale } = useI18n()
const projectsStore = useProjectsStore()
const store = useDeliveryRadarStore()

const isRadarWindow = new URLSearchParams(window.location.search).get('radarWindow') === '1'
const PROJECTS_KEY = 'devdy:radarProjects'
const GROUP_KEY = 'devdy:radarGroupMode'
const BY_REPO_KEY = 'devdy:radarByRepo'

// ── Project scope ────────────────────────────────────────────────────────────
// Only projects with a linked GitHub Project board carry the Deadline field.
const boardProjects = computed(() => sortProjectsByName(projectsStore.projects.filter((p) => !!p.github_project_board_url)))
// Empty = all board projects.
const selectedProjects = ref<Set<string>>(new Set())
const scopeProjects = computed(() =>
  selectedProjects.value.size === 0
    ? boardProjects.value
    : boardProjects.value.filter((p) => selectedProjects.value.has(p.id)),
)
const scopeIds = computed(() => scopeProjects.value.map((p) => p.id))

function onlyProject(id: string) {
  if (boardProjects.value.some((p) => p.id === id)) selectedProjects.value = new Set([id])
}

// The tab re-pointed at another project (Projects screen → radar of that project).
watch(
  () => props.project,
  (id) => {
    if (id) onlyProject(id)
  },
)

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
  const urlProject = props.project ?? new URLSearchParams(window.location.search).get('project')
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
const settingsOpen = ref(false)
function openDoneRules() {
  settingsOpen.value = false
  doneStatusesOpen.value = true
}
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
// Status strip: what needs someone's attention, then what is fine.
const HOT: RadarStatus[] = ['overdue', 'due-soon', 'not-started', 'stale']
const CALM: RadarStatus[] = ['no-deadline', 'on-track']

const TONE_TEXT: Record<RadarStatus, string> = {
  overdue: 'text-destructive',
  'due-soon': 'text-warning',
  'not-started': 'text-info',
  stale: 'text-foreground',
  'no-deadline': 'text-muted-foreground',
  'on-track': 'text-success',
}
const TONE_CHIP: Record<RadarStatus, string> = {
  overdue: 'bg-destructive/12 text-destructive',
  'due-soon': 'bg-warning/12 text-warning',
  'not-started': 'bg-info/12 text-info',
  stale: 'bg-muted text-foreground',
  'no-deadline': 'bg-muted/70 text-muted-foreground',
  'on-track': 'bg-success/12 text-success',
}
const TONE_BG: Record<RadarStatus, string> = {
  overdue: 'bg-destructive',
  'due-soon': 'bg-warning',
  'not-started': 'bg-info',
  stale: 'bg-foreground/70',
  'no-deadline': 'bg-muted-foreground/60',
  'on-track': 'bg-success',
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
// Parent issues whose sub-issues are folded, per group (the same issue can sit
// in several person groups). Everything starts unfolded.
const collapsed = ref<Set<string>>(new Set())
function treeId(g: RadarGroup, i: ClassifiedIssue): string {
  return `${groupId(g)}\u0001${i.key}`
}
function toggleChildren(g: RadarGroup, i: ClassifiedIssue) {
  const next = new Set(collapsed.value)
  const id = treeId(g, i)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  collapsed.value = next
}
function sectionRows(g: RadarGroup, rows: RadarGroup['rows']) {
  return visibleRows(rows, (key) => collapsed.value.has(`${groupId(g)}\u0001${key}`))
}

// Issues of a group split per repo (header only when the group spans several).
const byRepo = ref(localStorage.getItem(BY_REPO_KEY) !== '0')
watch(byRepo, (v) => {
  try { localStorage.setItem(BY_REPO_KEY, v ? '1' : '0') } catch { /* best-effort */ }
})
const foldedRepos = ref<Set<string>>(new Set())
function repoId(g: RadarGroup, s: RadarRepoSection): string {
  return `${groupId(g)}\u0002${s.key}`
}
function toggleRepo(g: RadarGroup, s: RadarRepoSection) {
  const next = new Set(foldedRepos.value)
  const id = repoId(g, s)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  foldedRepos.value = next
}
/** Sections to render; `null` header = one flat list (split off, or a single repo). */
function groupSections(g: RadarGroup): { section: RadarRepoSection | null, rows: RadarGroup['rows'] }[] {
  const sections = byRepo.value ? sectionByRepo(g.rows) : []
  if (sections.length < 2) return [{ section: null, rows: g.rows }]
  return sections.map((section) => ({ section, rows: section.rows }))
}
/** Statuses that need attention, as compact counts on a repo header. */
function sectionHot(s: RadarRepoSection): RadarStatus[] {
  return HOT.filter((st) => s.counts[st] > 0)
}

// Narrowing to one status or a search means "show me those issues": open everything.
watch([statusFilter, query], ([s, q]) => {
  if (s || q.trim()) {
    expanded.value = new Set(groups.value.map(groupId))
    collapsed.value = new Set()
    foldedRepos.value = new Set()
  }
})
watch(groupMode, () => {
  expanded.value = new Set()
  collapsed.value = new Set()
  foldedRepos.value = new Set()
})

function groupLabel(g: RadarGroup): string {
  return g.key === null ? t('radar.unassigned') : g.label
}
function groupInitial(g: RadarGroup): string {
  return g.key === null ? '?' : (g.label.replace(/^[^\p{L}\p{N}]+/u, '')[0] ?? '?').toUpperCase()
}
/** Worst status that needs attention in the group: the colour of its left stripe. */
function groupSeverity(g: RadarGroup): RadarStatus | null {
  return HOT.find((s) => g.counts[s] > 0) ?? null
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
/** "16/11 · Sat" — the weekday tells whether the deadline falls before a weekend. */
function deadlineDate(i: ClassifiedIssue): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(i.deadline ?? '')
  if (!m) return ''
  const weekday = new Date(+m[1], +m[2] - 1, +m[3]).toLocaleDateString(locale.value, { weekday: 'short' })
  return `${m[3]}/${m[2]} · ${weekday}`
}
/** People shown on an issue's second line: everyone (project mode) or the co-assignees (person mode). */
function issuePeople(i: ClassifiedIssue, g: RadarGroup): string[] {
  if (groupMode.value === 'project') return i.assignees
  return g.key === null ? [] : i.assignees.filter((a) => a !== g.key)
}
function initial(name: string): string {
  return (name.replace(/^[^\p{L}\p{N}]+/u, '')[0] ?? '?').toUpperCase()
}
function deadlineTone(i: ClassifiedIssue): string {
  if (i.daysLeft !== null && i.daysLeft < 0) return 'text-destructive'
  if (i.radar === 'due-soon') return 'text-warning'
  return 'text-foreground font-medium'
}


function openIssue(i: ClassifiedIssue) {
  openUrl(i.htmlUrl).catch(() => {})
}

// ── Followed people ──────────────────────────────────────────────────────────
// Group menu (right-click a person): stop following them.
const groupMenu = ref<{ x: number, y: number, group: RadarGroup } | null>(null)
function openGroupMenu(e: MouseEvent, g: RadarGroup) {
  if (groupMode.value === 'person') groupMenu.value = { x: e.clientX, y: e.clientY, group: g }
}
function hideGroup() {
  const g = groupMenu.value?.group
  groupMenu.value = null
  if (g) store.setHidden(g.key, true)
}
// Everyone the settings can switch on/off: assignees seen in scope (with their
// open work, hidden or not) plus anyone already hidden.
const members = computed(() => {
  const open = classifyAll(store.issuesFor(scopeProjects.value), now.value, store.thresholds, {
    stages: store.stages, testers: store.testers,
  })
  const counts = new Map<string, number>()
  let unassigned = 0
  for (const i of open) {
    if (!i.assignees.length) unassigned++
    for (const a of i.activeAssignees) counts.set(a.toLowerCase(), (counts.get(a.toLowerCase()) ?? 0) + 1)
  }
  const logins = new Map<string, string>()
  for (const a of store.assigneesFor(scopeIds.value)) logins.set(a.toLowerCase(), a)
  for (const h of store.hidden) if (h !== null && !logins.has(h)) logins.set(h, h)
  return {
    people: [...logins.entries()].map(([id, login]) => ({ login, count: counts.get(id) ?? 0 })),
    unassigned,
  }
})
const hiddenCount = computed(() => store.hidden.length)

// Project warnings fold into one line once there are several.
const warningsOpen = ref(false)

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

/** Docking the window back as a tab keeps a single-project scope. */
function dockParams(): Record<string, string> {
  const [only] = selectedProjects.value
  return selectedProjects.value.size === 1 && only ? { project: only } : {}
}

function openSettings(p: Project) {
  // The radar lives in its own window: ask the main window to show the project settings.
  emit(DELIVERY_RADAR_OPEN_ROUTE, { path: `/projects/${p.id}/settings` } satisfies DeliveryRadarOpenRoute).catch(() => {})
}

const GRID = 'grid grid-cols-[minmax(140px,1fr)_repeat(5,minmax(56px,92px))_minmax(48px,68px)_minmax(64px,84px)] items-center gap-x-1'
// Narrow windows drop the board-status column (it moves to the row's second line).
const IGRID = 'grid grid-cols-[minmax(0,1fr)_minmax(96px,124px)_64px_52px] md:grid-cols-[minmax(0,1fr)_minmax(104px,140px)_minmax(96px,124px)_minmax(64px,88px)_52px] items-center gap-x-4'
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- One toolbar: title, scope, grouping, search, freshness, actions -->
    <div class="shrink-0 border-b border-border/60 px-6 py-2.5 flex items-center gap-x-2 gap-y-2 flex-wrap">
      <div class="flex items-center gap-2 mr-2">
        <Radar class="h-4 w-4 text-primary shrink-0" :stroke-width="2" />
        <h1 class="text-sm font-semibold shrink-0">{{ t('radar.title') }}</h1>
        <span
          class="flex h-4 min-w-4 shrink-0 items-center justify-center rounded-full bg-muted px-1.5 text-[10px] font-medium text-muted-foreground"
          :title="t('radar.openIssues')"
        >{{ classified.length }}</span>
      </div>
      <template v-if="boardProjects.length > 0">
        <RadarProjectPicker v-model:selected="selectedProjects" :projects="boardProjects" :loading="store.loading" />
        <SegmentedControl v-model="groupMode" :options="groupModeOptions" :stretch="false" />
      </template>
      <div class="ml-auto flex items-center gap-2">
        <div v-if="boardProjects.length > 0" class="relative">
          <Search class="pointer-events-none absolute left-2 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" :stroke-width="1.75" />
          <Input v-model="query" size="sm" class="w-56 pl-7" :placeholder="t('radar.searchPlaceholder')" />
        </div>
        <span v-if="updatedAt" class="hidden lg:inline text-[11px] text-muted-foreground whitespace-nowrap">
          {{ t('radar.updated', { time: relativeTime(updatedAt) }) }}
        </span>
        <Button
          variant="outline"
          size="icon"
          :disabled="store.isLoading || scopeIds.length === 0"
          :title="updatedAt ? `${t('common.refresh')} · ${t('radar.updated', { time: relativeTime(updatedAt) })}` : t('common.refresh')"
          :aria-label="t('common.refresh')"
          @click="refresh(true)"
        >
          <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': store.isLoading }" :stroke-width="1.75" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          :title="t('radar.settings.title')"
          :aria-label="t('radar.settings.title')"
          @click="settingsOpen = true"
        >
          <Settings2 class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
        <DockWindowButton
          v-if="isRadarWindow"
          surface="radar"
          :params="dockParams"
        />
      </div>
    </div>

    <!-- No board-configured projects -->
    <div v-if="boardProjects.length === 0" class="flex flex-col items-center justify-center flex-1 min-h-0 p-6 text-center">
      <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
        <FolderOpen class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('radar.noBoardTitle') }}</p>
      <p class="text-xs text-muted-foreground mt-1 max-w-80">{{ t('radar.noBoardHint') }}</p>
    </div>

    <!-- First load: skeleton mirroring the strip + table -->
    <div v-else-if="!hasAnyData && store.isLoading" class="flex-1 min-h-0 overflow-auto p-6 space-y-4">
      <div class="grid grid-cols-3 lg:grid-cols-6 gap-3">
        <div v-for="i in 6" :key="i" class="rounded-lg border border-border/60 bg-card p-3 space-y-2.5">
          <Skeleton class="h-6 w-10 rounded" />
          <Skeleton class="h-2.5 w-16 rounded" />
        </div>
      </div>
      <div class="rounded-lg border border-border/60 overflow-hidden">
        <div v-for="i in 6" :key="i" class="flex items-center gap-4 border-b border-border/30 last:border-b-0 px-3 h-12">
          <Skeleton class="h-6 w-6 rounded-full" />
          <Skeleton class="h-2.5 w-28 rounded" />
          <Skeleton v-for="j in 5" :key="j" class="h-2.5 w-8 rounded" />
        </div>
      </div>
    </div>

    <div v-else class="flex-1 min-h-0 overflow-auto">
      <div class="p-6 space-y-3">
        <!-- Per-project problems: one line when there are several -->
        <template v-if="banners.length">
          <button
            v-if="banners.length > 1"
            class="flex w-full items-center gap-2 rounded-md border px-3 py-1.5 text-left text-[12px] font-medium transition-colors"
            :class="banners.some((b) => b.tone === 'destructive')
              ? 'border-destructive/35 bg-destructive/8 text-destructive hover:bg-destructive/12'
              : 'border-warning/35 bg-warning/8 text-warning hover:bg-warning/12'"
            :aria-expanded="warningsOpen"
            @click="warningsOpen = !warningsOpen"
          >
            {{ t('radar.warningsCount', { n: banners.length }, banners.length) }}
            <ChevronRight class="ml-auto h-3.5 w-3.5 transition-transform" :class="{ 'rotate-90': warningsOpen }" :stroke-width="2" />
          </button>
          <template v-if="banners.length === 1 || warningsOpen">
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
          </template>
        </template>

        <!-- Status strip: needs attention (big, coloured) | fine (quiet). Click = filter. -->
        <Card body-class="grid grid-cols-1 md:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
          <section v-for="(part, pi) in [HOT, CALM]" :key="pi" :class="pi === 1 ? 'border-t md:border-t-0 md:border-l border-border/60 bg-muted/30' : ''">
            <h3 class="px-3.5 pt-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
              {{ pi === 0 ? t('radar.needsAttention') : t('radar.fine') }}
            </h3>
            <div class="grid" :style="{ gridTemplateColumns: `repeat(${part.length}, minmax(0, 1fr))` }">
              <button
                v-for="s in part" :key="s"
                class="relative flex flex-col items-start gap-0.5 px-3.5 pt-1 pb-2.5 text-left transition-[background-color,opacity] hover:bg-accent/50"
                :class="[TONE_TEXT[s], statusFilter && statusFilter !== s ? 'opacity-45' : '']"
                :title="`${statusHint(s)} — ${t('radar.filterHint')}`"
                :aria-pressed="statusFilter === s"
                @click="toggleStatus(s)"
              >
                <span
                  class="font-semibold leading-tight tabular-nums"
                  :class="[pi === 0 ? 'text-2xl' : 'text-xl', counts[s] > 0 ? '' : 'text-muted-foreground/40']"
                >{{ counts[s] }}</span>
                <span class="flex items-center gap-1.5 text-[12px] text-muted-foreground">
                  <span class="h-1.5 w-1.5 rounded-full" :class="TONE_BG[s]" />
                  {{ t(`radar.status.${s}`) }}
                </span>
                <span v-if="statusFilter === s" class="absolute inset-x-2.5 bottom-0 h-0.5 rounded-t" :class="TONE_BG[s]" />
              </button>
            </div>
          </section>
        </Card>

        <!-- Filter state + table controls -->
        <div class="flex items-center gap-2 min-h-6 text-[12px] text-muted-foreground">
          <template v-if="statusFilter">
            {{ t('radar.filtering') }}
            <span class="inline-flex items-center gap-1 rounded-full py-0.5 pl-2 pr-1 font-medium" :class="TONE_CHIP[statusFilter]">
              {{ t(`radar.status.${statusFilter}`) }}
              <button class="rounded-full p-0.5 hover:bg-foreground/10" :title="t('radar.clearFilter')" :aria-label="t('radar.clearFilter')" @click="statusFilter = null">
                <X class="h-3 w-3" :stroke-width="2" />
              </button>
            </span>
          </template>
          <span v-else-if="groups.length">
            {{ groupMode === 'person' ? t('radar.groupsByPerson', { n: groups.length }, groups.length) : t('radar.groupsByProject', { n: groups.length }, groups.length) }}
          </span>
          <div class="ml-auto flex items-center gap-3">
            <button
              v-if="hiddenCount"
              class="inline-flex items-center gap-1 hover:text-foreground"
              :title="t('radar.settings.membersTitle')"
              @click="settingsOpen = true"
            >
              <EyeOff class="h-3 w-3" :stroke-width="1.75" />
              {{ t('radar.hiddenCount', { n: hiddenCount }) }}
            </button>
            <button
              v-if="groups.length"
              class="inline-flex items-center gap-1 rounded px-1 transition-colors"
              :class="byRepo ? 'text-foreground font-medium' : 'hover:text-foreground'"
              :aria-pressed="byRepo"
              :title="t('radar.byRepoHint')"
              @click="byRepo = !byRepo"
            >
              <BookMarked class="h-3 w-3" :class="byRepo ? 'text-primary' : ''" :stroke-width="1.75" />
              {{ t('radar.byRepo') }}
            </button>
            <button v-if="groups.length" class="font-medium text-primary hover:underline" @click="toggleAll">
              {{ allExpanded ? t('radar.collapseAll') : t('radar.expandAll') }}
            </button>
          </div>
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
          <div :class="GRID" class="pl-4 pr-3 h-8 border-b border-border/60 bg-muted/30 text-[11px] text-muted-foreground font-medium">
            <div class="truncate">{{ groupMode === 'person' ? t('radar.colPerson') : t('radar.colProject') }}</div>
            <div v-for="s in COLUMNS" :key="s" class="text-center truncate" :title="statusHint(s)">{{ t(`radar.status.${s}`) }}</div>
            <div class="text-center truncate">{{ t('radar.colTotal') }}</div>
            <div class="text-center truncate">{{ t('radar.colWorst') }}</div>
          </div>

          <div v-for="g in groups" :key="groupId(g)" class="border-b border-border/50 last:border-b-0">
            <button
              :class="GRID"
              class="relative w-full pl-4 pr-3 min-h-13 py-1.5 text-left hover:bg-accent/40 transition-colors"
              :aria-expanded="expanded.has(groupId(g))"
              @click="toggleGroup(g)"
              @contextmenu.prevent="openGroupMenu($event, g)"
            >
              <!-- Severity stripe: worst status needing attention -->
              <span v-if="groupSeverity(g)" class="absolute left-0 inset-y-2 w-[3px] rounded-r" :class="TONE_BG[groupSeverity(g)!]" />
              <span class="flex items-center gap-2 min-w-0">
                <ChevronRight
                  class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
                  :class="{ 'rotate-90': expanded.has(groupId(g)) }"
                  :stroke-width="2"
                />
                <span class="flex h-6.5 w-6.5 shrink-0 items-center justify-center rounded-full bg-muted text-[11px] font-semibold text-muted-foreground">
                  {{ groupInitial(g) }}
                </span>
                <span class="flex min-w-0 flex-col">
                  <span class="flex items-center gap-1.5 min-w-0">
                    <span class="truncate text-[13px] font-semibold" :class="{ 'italic font-medium text-muted-foreground': g.key === null }">{{ groupLabel(g) }}</span>
                    <span
                      v-if="isTesterGroup(g)"
                      class="shrink-0 rounded border border-info/30 bg-info/15 px-1 text-[10px] font-medium text-info"
                      :title="t('radar.testerHint')"
                    >{{ t('radar.tester') }}</span>
                  </span>
                  <span class="truncate text-[11px] text-muted-foreground" :title="g.related.join(', ')">{{ g.related.join(' · ') }}</span>
                </span>
              </span>
              <span v-for="s in COLUMNS" :key="s" class="text-center tabular-nums">
                <span v-if="g.counts[s] > 0" class="text-[15px] font-semibold" :class="TONE_TEXT[s]">{{ g.counts[s] }}</span>
                <span v-else class="text-muted-foreground/35">·</span>
              </span>
              <span class="text-center font-semibold tabular-nums">{{ g.total }}</span>
              <span class="text-center tabular-nums" :class="g.worstLateDays ? 'text-destructive font-semibold' : 'text-muted-foreground/40'">
                {{ g.worstLateDays ? t('radar.nDays', { n: g.worstLateDays }, g.worstLateDays) : '–' }}
              </span>
            </button>

            <!-- Issues of the group -->
            <div v-if="expanded.has(groupId(g))" class="bg-muted/25 border-t border-border/40 pt-1 pb-1.5">
              <div :class="IGRID" class="pl-10 pr-3 py-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
                <span>{{ t('radar.colIssue') }}</span>
                <span class="hidden md:block truncate">{{ t('radar.colBoardStatus') }}</span>
                <span class="truncate">{{ t('radar.colDeadline') }}</span>
                <span class="truncate text-right">{{ t('radar.colUpdated') }}</span>
                <span />
              </div>
              <template v-for="{ section, rows } in groupSections(g)" :key="section?.key ?? 'all'">
              <!-- Repo header: project (person mode), repo, open count, what needs attention. Click = fold. -->
              <button
                v-if="section"
                class="mx-1.5 mt-1.5 flex w-[calc(100%-0.75rem)] items-center gap-2 rounded-md px-2 py-1 text-left text-[11.5px] hover:bg-accent/50"
                :aria-expanded="!foldedRepos.has(repoId(g, section))"
                @click="toggleRepo(g, section)"
              >
                <ChevronRight
                  class="h-3 w-3 shrink-0 text-muted-foreground transition-transform"
                  :class="{ 'rotate-90': !foldedRepos.has(repoId(g, section)) }"
                  :stroke-width="2"
                />
                <template v-if="groupMode === 'person'">
                  <span class="h-1.75 w-1.75 shrink-0 rounded-[2px]" :class="swatchClass(section.projectName)" />
                  <span class="shrink-0 font-medium text-muted-foreground">{{ section.projectName }}</span>
                  <span class="shrink-0 text-muted-foreground/50">/</span>
                </template>
                <BookMarked v-else class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <span class="truncate font-mono text-[11.5px] font-semibold text-foreground">{{ section.repo }}</span>
                <span class="shrink-0 rounded-full bg-muted px-1.5 text-[10px] font-medium tabular-nums text-muted-foreground">{{ section.total }}</span>
                <span class="ml-auto flex shrink-0 items-center gap-2.5 tabular-nums">
                  <span
                    v-for="st in sectionHot(section)" :key="st"
                    class="flex items-center gap-1 text-[11px] font-semibold"
                    :class="TONE_TEXT[st]"
                    :title="t(`radar.status.${st}`)"
                  >
                    <span class="h-1.5 w-1.5 rounded-full" :class="TONE_BG[st]" />{{ section.counts[st] }}
                  </span>
                </span>
              </button>
              <div v-if="!section || !foldedRepos.has(repoId(g, section))" class="divide-y divide-border/40" :class="{ 'ml-3 border-l border-border/50': section }">
              <div
                v-for="{ issue: i, depth, hasChildren, hidden } in sectionRows(g, rows)" :key="i.key"
                :class="IGRID"
                class="group/row mx-1.5 rounded-md pr-1.5 py-1.5 hover:bg-accent/50"
                :title="i.parent ? `${i.title}\n${t('radar.parentHint', { title: `#${i.parent.number} ${i.parent.title}` })}` : i.title"
                @contextmenu.prevent="openRowMenu($event, i)"
              >
                <div class="flex items-start gap-2 min-w-0" :style="{ paddingLeft: `${0.5 + depth * 1.4}rem` }">
                  <!-- Severity square, then the fold slot: chevron on parents, an elbow on leaf sub-issues. -->
                  <span class="flex w-4.5 shrink-0 flex-col items-center gap-1 pt-1">
                    <span class="h-2 w-2 rounded-[2px]" :class="TONE_BG[i.radar]" :title="t(`radar.status.${i.radar}`)" />
                    <button
                      v-if="hasChildren"
                      class="flex h-4.5 w-4.5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
                      :title="hidden ? t('radar.expandChildren') : t('radar.collapseChildren')"
                      :aria-label="hidden ? t('radar.expandChildren') : t('radar.collapseChildren')"
                      :aria-expanded="!hidden"
                      @click="toggleChildren(g, i)"
                    >
                      <ChevronRight class="h-3 w-3 transition-transform" :class="{ 'rotate-90': !hidden }" :stroke-width="2" />
                    </button>
                    <CornerDownRight v-else-if="depth > 0" class="h-3 w-3 text-muted-foreground/60" :stroke-width="1.75" />
                  </span>
                  <span class="flex min-w-0 flex-1 flex-col gap-1">
                    <!-- Line 1: title, radar status (on-track needs no chip), folded children -->
                    <span class="flex min-w-0 items-baseline gap-2">
                      <span class="truncate text-[13px] font-medium leading-snug text-foreground">{{ i.title }}</span>
                      <span
                        v-if="i.radar !== 'on-track'"
                        class="shrink-0 rounded-full px-1.75 py-px text-[10.5px] font-semibold"
                        :class="TONE_CHIP[i.radar]"
                      >{{ t(`radar.status.${i.radar}`) }}</span>
                      <span
                        v-if="hidden"
                        class="shrink-0 rounded-full border border-dashed border-border px-1.5 text-[10px] font-semibold text-muted-foreground"
                        :title="t('radar.hiddenChildren', { n: hidden })"
                      >+{{ hidden }}</span>
                    </span>
                    <!-- Line 2: each kind of meta has its own form (code ref, project swatch, avatars, parent link) -->
                    <span class="flex min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap text-[11px] text-muted-foreground">
                      <span class="shrink-0 rounded bg-muted px-1.25 py-px font-mono text-[10.5px]"><template v-if="!section || section.repo !== i.repo">{{ i.repo }}</template><span class="font-medium text-foreground">#{{ i.number }}</span></span>
                      <span class="md:hidden flex shrink-0 items-center gap-1.5">
                        <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
                        <span class="h-2 w-2 rounded-full" :class="statusDotClass(i.statusColor)" />
                        {{ i.status || t('radar.noStatus') }}
                      </span>
                      <span v-if="groupMode === 'person' && (!section || section.projectId !== i.projectId)" class="flex shrink-0 items-center gap-1.5 font-medium text-foreground/80">
                        <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
                        <span class="h-1.75 w-1.75 rounded-[2px]" :class="swatchClass(i.projectName)" />
                        {{ i.projectName }}
                      </span>
                      <span v-if="issuePeople(i, g).length" class="flex shrink-0 items-center gap-1.5">
                        <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
                        <span class="flex">
                          <span
                            v-for="(p, pi) in issuePeople(i, g).slice(0, 3)" :key="p"
                            class="flex h-4 w-4 items-center justify-center rounded-full text-[8.5px] font-bold text-white ring-[1.5px] ring-card"
                            :class="[swatchClass(p), pi > 0 ? '-ml-1' : '']"
                          >{{ initial(p) }}</span>
                        </span>
                        {{ groupMode === 'person' ? t('radar.alsoAssigned', { names: issuePeople(i, g).join(', ') }) : issuePeople(i, g).join(', ') }}
                      </span>
                      <span v-else-if="groupMode === 'project'" class="flex shrink-0 items-center gap-1.5 italic">
                        <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
                        {{ t('radar.unassigned') }}
                      </span>
                      <!-- Parent not in this list (other assignee, done, …): say where the task belongs. -->
                      <span v-if="depth === 0 && i.parent" class="flex min-w-0 items-center gap-1">
                        <span class="mr-1 h-1.5 w-1.5 shrink-0 rounded-full bg-muted-foreground/40" />
                        <CornerUpLeft class="h-3 w-3 shrink-0" :stroke-width="1.75" />
                        <span class="shrink-0 font-mono text-[10.5px]">#{{ i.parent.number }}</span>
                        <span class="truncate italic">{{ i.parent.title }}</span>
                      </span>
                    </span>
                  </span>
                </div>
                <span class="hidden md:flex min-w-0">
                  <span
                    class="inline-flex max-w-full items-center gap-1.5 rounded-md border px-2 py-0.5 text-[11.5px]"
                    :class="i.status ? 'border-border font-medium text-foreground' : 'border-dashed border-border text-muted-foreground/70'"
                  >
                    <span v-if="i.status" class="h-2 w-2 rounded-full shrink-0" :class="statusDotClass(i.statusColor)" />
                    <span class="truncate">{{ i.status || t('radar.noStatus') }}</span>
                  </span>
                </span>
                <span
                  class="flex min-w-0 flex-col gap-px whitespace-nowrap leading-tight"
                  :title="i.deadlineInherited ? t('radar.inheritedDeadline') : undefined"
                >
                  <span v-if="i.daysLeft === null" class="text-[12px] font-medium text-muted-foreground/70">{{ t('radar.noDeadlineShort') }}</span>
                  <span v-else class="truncate text-[12px] font-semibold" :class="deadlineTone(i)">{{ deadlineText(i) }}</span>
                  <span v-if="deadlineDate(i)" class="flex items-center gap-1 text-[11px] text-muted-foreground tabular-nums">
                    <CalendarDays class="h-2.75 w-2.75 shrink-0" :stroke-width="2" />
                    <span class="truncate">{{ deadlineDate(i) }}</span>
                  </span>
                  <span v-if="i.deadlineInherited" class="text-[10px] italic text-muted-foreground/70">{{ t('radar.inheritedShort') }}</span>
                </span>
                <span
                  class="flex min-w-0 items-center justify-end gap-1 text-right text-[11px] tabular-nums"
                  :class="i.radar === 'stale' ? 'text-warning font-semibold' : 'text-muted-foreground/70'"
                >
                  <Clock v-if="i.radar === 'stale'" class="h-3 w-3 shrink-0" :stroke-width="2" />
                  <span class="truncate">{{ relativeTime(i.updatedAt) }}</span>
                </span>
                <span class="flex justify-end gap-0.5 opacity-50 transition-opacity group-hover/row:opacity-100 focus-within:opacity-100">
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    class="text-muted-foreground/70 hover:text-foreground"
                    :title="t('focus.addTodoHint')"
                    :aria-label="t('focus.addTodoHint')"
                    @click="addTodo(i)"
                  >
                    <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon-xs"
                    class="text-muted-foreground/70 hover:text-primary"
                    :title="t('radar.openOnGithub')"
                    :aria-label="t('radar.openOnGithub')"
                    @click="openIssue(i)"
                  >
                    <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
                  </Button>
                </span>
              </div>
              </div>
              </template>
            </div>
          </div>
        </Card>
      </div>
    </div>

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
    <ContextMenu :open="!!groupMenu" :x="groupMenu?.x ?? 0" :y="groupMenu?.y ?? 0" @close="groupMenu = null">
      <DropdownItem @click="hideGroup">
        <EyeOff class="h-3.5 w-3.5" :stroke-width="1.75" />
        {{ t('radar.hideMember', { name: groupMenu ? groupLabel(groupMenu.group) : '' }) }}
      </DropdownItem>
    </ContextMenu>
    <TodoCaptureModal :open="!!todoPrefill" :prefill="todoPrefill" @close="todoPrefill = null" />
    <RadarSettingsModal
      :open="settingsOpen"
      :can-edit-done-rules="scopeIds.length > 0"
      :members="members"
      @close="settingsOpen = false"
      @done-rules="openDoneRules"
    />
    <DoneStatusesModal :open="doneStatusesOpen" :projects="scopeProjects" @close="doneStatusesOpen = false" />
  </div>
</template>
