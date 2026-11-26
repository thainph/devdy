<script setup lang="ts">
// Delivery Radar, mode B: every milestone of the selected projects with its
// health (late, behind schedule, at risk, …) and progress — closed issues,
// issues past development, and the share of the planned time already gone.
// Expanding a milestone lists its open work with the same rows as mode A.
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { CalendarDays, CheckCircle2, ChevronRight, ExternalLink, Flag, SearchX, X } from 'lucide-vue-next'
import { Card, ContextMenu, DropdownItem } from '@/components/ui'
import {
  compareMilestones,
  countMilestones,
  filterMilestones,
  swatchClass,
  visibleRows,
  type ClassifiedIssue,
  type MilestoneHealth,
  type MilestoneView,
  type RadarStatus,
} from '@/lib/deliveryRadar'
import RadarIssueRow from './RadarIssueRow.vue'
import { HEALTH_BG, HEALTH_CHIP, HEALTH_TEXT, IGRID, TONE_BG, TONE_TEXT } from './radarStyles'

const props = defineProps<{
  views: MilestoneView[]
  now: number
  query: string
  dueSoonDays: number
  lagPct: number
}>()

const emit = defineEmits<{
  'add-todo': [issue: ClassifiedIssue]
  'open-issue': [issue: ClassifiedIssue]
  'row-menu': [e: MouseEvent, issue: ClassifiedIssue]
}>()

const { t, locale } = useI18n()

const SHOW_DONE_KEY = 'devdy:radarShowDoneMilestones'

// ── Filtering ────────────────────────────────────────────────────────────────
const healthFilter = ref<MilestoneHealth | null>(null)
const showDone = ref(localStorage.getItem(SHOW_DONE_KEY) !== '0')
watch(showDone, (v) => {
  try { localStorage.setItem(SHOW_DONE_KEY, v ? '1' : '0') } catch { /* best-effort */ }
})

const matching = computed(() => filterMilestones(props.views, { query: props.query }))
const counts = computed(() => countMilestones(matching.value))
const shown = computed(() => filterMilestones(matching.value, { health: healthFilter.value }))

const HOT: MilestoneHealth[] = ['overdue', 'lag', 'at-risk', 'due-soon']
const CALM: MilestoneHealth[] = ['on-track', 'no-due']

function toggleHealth(h: MilestoneHealth) {
  healthFilter.value = healthFilter.value === h ? null : h
}
function healthHint(h: MilestoneHealth): string {
  return t(`radar.ms.healthHint.${h}`, { days: props.dueSoonDays, pct: props.lagPct })
}

// ── Projects → milestones ────────────────────────────────────────────────────
interface ProjectBlock {
  id: string
  name: string
  active: MilestoneView[]
  done: MilestoneView[]
  hot: { health: MilestoneHealth, n: number }[]
  milestoneCount: number
}
const projects = computed<ProjectBlock[]>(() => {
  const byId = new Map<string, ProjectBlock>()
  for (const v of [...shown.value].sort(compareMilestones)) {
    let p = byId.get(v.projectId)
    if (!p) {
      p = { id: v.projectId, name: v.projectName, active: [], done: [], hot: [], milestoneCount: 0 }
      byId.set(v.projectId, p)
    }
    if (v.health === 'done') p.done.push(v)
    else p.active.push(v)
    if (!v.noMilestone) p.milestoneCount++
  }
  for (const p of byId.values()) {
    p.hot = HOT.map((health) => ({ health, n: p.active.filter((v) => v.health === health).length })).filter((h) => h.n > 0)
  }
  return [...byId.values()]
})
const milestoneTotal = computed(() => shown.value.filter((v) => !v.noMilestone).length)

// ── Expansion ────────────────────────────────────────────────────────────────
const expanded = ref<Set<string>>(new Set())
const foldedProjects = ref<Set<string>>(new Set())
// Parent issues whose sub-issues are folded, per milestone.
const collapsed = ref<Set<string>>(new Set())

function toggleIn(set: typeof expanded, id: string) {
  const next = new Set(set.value)
  if (next.has(id)) next.delete(id)
  else next.add(id)
  set.value = next
}
const toggleMilestone = (key: string) => toggleIn(expanded, key)
const toggleProject = (id: string) => toggleIn(foldedProjects, id)
const toggleChildren = (v: MilestoneView, i: ClassifiedIssue) => toggleIn(collapsed, `${v.key}\u0001${i.key}`)
const expandable = computed(() => shown.value.filter((v) => v.issues.length > 0))
const allExpanded = computed(() => expandable.value.length > 0 && expandable.value.every((v) => expanded.value.has(v.key)))
function toggleAll() {
  expanded.value = allExpanded.value ? new Set() : new Set(expandable.value.map((v) => v.key))
}
function rowsOf(v: MilestoneView) {
  return visibleRows(v.rows, (key) => collapsed.value.has(`${v.key}\u0001${key}`))
}

// Narrowing to one health or a search means "show me those": open everything.
watch([healthFilter, () => props.query], ([h, q]) => {
  if (h || q.trim()) {
    expanded.value = new Set(expandable.value.map((v) => v.key))
    foldedProjects.value = new Set()
  }
})

// ── Cells ────────────────────────────────────────────────────────────────────
function shortDate(iso: string | null): string {
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(iso ?? '')
  if (!m) return ''
  const weekday = new Date(+m[1], +m[2] - 1, +m[3]).toLocaleDateString(locale.value, { weekday: 'short' })
  return `${m[3]}/${m[2]} · ${weekday}`
}
function dueText(v: MilestoneView): string {
  if (v.health === 'done') return t('radar.ms.finished')
  if (v.daysLeft === null) return t('radar.ms.noDue')
  if (v.daysLeft < 0) return t('radar.lateDays', { n: -v.daysLeft })
  if (v.daysLeft === 0) return t('radar.dueToday')
  return t('radar.daysLeft', { n: v.daysLeft })
}
function dueTone(v: MilestoneView): string {
  if (v.health === 'done') return 'text-muted-foreground'
  if (v.daysLeft === null) return 'text-muted-foreground/70 font-medium'
  if (v.daysLeft < 0) return 'text-destructive'
  if (v.daysLeft <= props.dueSoonDays) return 'text-info'
  return 'text-foreground'
}
const pct = (n: number | null) => Math.round(n ?? 0)

/** One line on why the milestone has its health. */
function healthWhy(v: MilestoneView): string {
  switch (v.health) {
    case 'overdue':
      return t('radar.ms.why.overdue', { n: -(v.daysLeft ?? 0), open: v.openCount })
    case 'at-risk': {
      const parts: string[] = []
      if (v.lateIssues) parts.push(t('radar.ms.why.lateIssues', { n: v.lateIssues }, v.lateIssues))
      if (v.afterDue.length) parts.push(t('radar.ms.why.afterDue', { n: v.afterDue.length }, v.afterDue.length))
      return parts.join(' · ')
    }
    case 'due-soon':
      return v.daysLeft === 0 ? t('radar.dueToday') : t('radar.daysLeft', { n: v.daysLeft ?? 0 })
    case 'no-due':
      return v.noMilestone ? t('radar.ms.why.noMilestone', { n: v.openCount }, v.openCount) : t('radar.ms.why.noDue')
    case 'done':
      return v.closedAt ? t('radar.ms.why.closedOn', { date: shortDate(v.closedAt) }) : t('radar.ms.why.allClosed', { n: v.closedCount }, v.closedCount)
    default:
      return v.elapsedPct !== null && v.closedPct !== null
        ? t('radar.ms.why.pace', { time: pct(v.elapsedPct), done: pct(v.closedPct) })
        : ''
  }
}
/** Issue statuses needing attention, as compact counts. */
const ISSUE_HOT: RadarStatus[] = ['overdue', 'due-soon', 'not-started', 'stale']
function issueHot(v: MilestoneView): RadarStatus[] {
  return ISSUE_HOT.filter((s) => v.counts[s] > 0)
}
function initial(name: string): string {
  return (name.replace(/^[^\p{L}\p{N}]+/u, '')[0] ?? '?').toUpperCase()
}

// ── GitHub links ─────────────────────────────────────────────────────────────
const linkMenu = ref<{ x: number, y: number, view: MilestoneView } | null>(null)
function openMilestone(e: MouseEvent, v: MilestoneView) {
  if (v.links.length === 1) openUrl(v.links[0].htmlUrl).catch(() => {})
  else linkMenu.value = { x: e.clientX, y: e.clientY, view: v }
}
function openLink(url: string) {
  linkMenu.value = null
  openUrl(url).catch(() => {})
}

// Narrow panes keep milestone, due date and progress; counts and health join the title.
const MGRID = 'grid grid-cols-[minmax(0,1.2fr)_76px_minmax(96px,1fr)] @[52rem]/screen:grid-cols-[minmax(180px,1.5fr)_112px_minmax(170px,1.3fr)_minmax(130px,1fr)_minmax(110px,132px)] items-center gap-x-3'
const PAST_DEV_STRIPES = { backgroundImage: 'repeating-linear-gradient(135deg, hsl(var(--info) / 0.55) 0 3px, transparent 3px 6px)' }
</script>

<template>
  <div class="space-y-3">
    <!-- Health strip: needs attention (big, coloured) | fine (quiet). Click = filter. -->
    <Card body-class="grid grid-cols-1 @[36rem]/screen:grid-cols-[minmax(0,2fr)_minmax(0,1fr)]">
      <section v-for="(part, pi) in [HOT, CALM]" :key="pi" :class="pi === 1 ? 'border-t @[36rem]/screen:border-t-0 @[36rem]/screen:border-l border-border/60 bg-muted/30' : ''">
        <h3 class="px-3.5 pt-2 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground">
          {{ pi === 0 ? t('radar.ms.needsAttention') : t('radar.fine') }}
        </h3>
        <div class="grid" :style="{ gridTemplateColumns: `repeat(${part.length}, minmax(0, 1fr))` }">
          <button
            v-for="h in part" :key="h"
            class="relative flex min-w-0 flex-col items-start gap-0.5 px-3.5 pt-1 pb-2.5 text-left transition-[background-color,opacity] hover:bg-accent/50"
            :class="[HEALTH_TEXT[h], healthFilter && healthFilter !== h ? 'opacity-45' : '']"
            :title="`${healthHint(h)} — ${t('radar.filterHint')}`"
            :aria-pressed="healthFilter === h"
            @click="toggleHealth(h)"
          >
            <span
              class="font-semibold leading-tight tabular-nums"
              :class="[pi === 0 ? 'text-2xl' : 'text-xl', counts[h] > 0 ? '' : 'text-muted-foreground/40']"
            >{{ counts[h] }}</span>
            <span class="flex max-w-full items-center gap-1.5 text-[12px] text-muted-foreground">
              <span class="h-1.5 w-1.5 shrink-0 rounded-full" :class="HEALTH_BG[h]" />
              <span class="truncate">{{ t(`radar.ms.health.${h}`) }}</span>
            </span>
            <span v-if="healthFilter === h" class="absolute inset-x-2.5 bottom-0 h-0.5 rounded-t" :class="HEALTH_BG[h]" />
          </button>
        </div>
      </section>
    </Card>

    <!-- Filter state + table controls -->
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 min-h-6 text-[12px] text-muted-foreground">
      <template v-if="healthFilter">
        {{ t('radar.filtering') }}
        <span class="inline-flex items-center gap-1 rounded-full py-0.5 pl-2 pr-1 font-medium" :class="HEALTH_CHIP[healthFilter]">
          {{ t(`radar.ms.health.${healthFilter}`) }}
          <button class="rounded-full p-0.5 hover:bg-foreground/10" :title="t('radar.clearFilter')" :aria-label="t('radar.clearFilter')" @click="healthFilter = null">
            <X class="h-3 w-3" :stroke-width="2" />
          </button>
        </span>
      </template>
      <span v-else-if="projects.length">{{ t('radar.ms.summary', { projects: projects.length, milestones: milestoneTotal }) }}</span>
      <div class="ml-auto flex flex-wrap items-center gap-x-3 gap-y-1">
        <span class="hidden @[52rem]/screen:flex items-center gap-3 text-[11px]">
          <span class="flex items-center gap-1.5"><span class="h-2 w-4 rounded-sm bg-success" />{{ t('radar.ms.legendClosed') }}</span>
          <span class="flex items-center gap-1.5"><span class="h-2 w-4 rounded-sm border border-info/50" :style="PAST_DEV_STRIPES" />{{ t('radar.ms.legendPastDev') }}</span>
          <span class="flex items-center gap-1.5"><span class="h-3 w-0.5 rounded bg-foreground" />{{ t('radar.ms.legendElapsed') }}</span>
        </span>
        <button
          class="inline-flex items-center gap-1 rounded px-1 transition-colors"
          :class="showDone ? 'text-foreground font-medium' : 'hover:text-foreground'"
          :aria-pressed="showDone"
          @click="showDone = !showDone"
        >
          <CheckCircle2 class="h-3 w-3" :class="showDone ? 'text-primary' : ''" :stroke-width="1.75" />
          {{ t('radar.ms.showDone') }}
        </button>
        <button v-if="expandable.length" class="font-medium text-primary hover:underline" @click="toggleAll">
          {{ allExpanded ? t('radar.collapseAll') : t('radar.expandAll') }}
        </button>
      </div>
    </div>

    <!-- Empty -->
    <div v-if="projects.length === 0" class="flex flex-col items-center justify-center min-h-60 text-center">
      <div class="flex h-12 w-12 items-center justify-center rounded-xl bg-muted mb-4">
        <SearchX class="h-6 w-6 text-muted-foreground" :stroke-width="1.5" />
      </div>
      <p class="text-sm font-medium">{{ t('radar.ms.nothingTitle') }}</p>
      <p class="text-xs text-muted-foreground mt-1">{{ t('radar.ms.nothingHint') }}</p>
    </div>

    <Card v-else body-class="text-xs">
      <div :class="MGRID" class="pl-4 pr-3 h-8 border-b border-border/60 bg-muted/30 text-[11px] text-muted-foreground font-medium">
        <div class="truncate">{{ t('radar.ms.colMilestone') }}</div>
        <div class="truncate">{{ t('radar.ms.colDue') }}</div>
        <div class="truncate">{{ t('radar.ms.colProgress') }}</div>
        <div class="hidden @[52rem]/screen:block truncate">{{ t('radar.ms.colOpen') }}</div>
        <div class="hidden @[52rem]/screen:block truncate">{{ t('radar.ms.colHealth') }}</div>
      </div>

      <div v-for="p in projects" :key="p.id" class="border-b border-border/60 last:border-b-0">
        <!-- Project header. Click = fold. -->
        <button
          class="flex w-full items-center gap-2 bg-muted/40 px-3 py-1.5 text-left text-[12px] hover:bg-accent/50"
          :aria-expanded="!foldedProjects.has(p.id)"
          @click="toggleProject(p.id)"
        >
          <ChevronRight
            class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
            :class="{ 'rotate-90': !foldedProjects.has(p.id) }"
            :stroke-width="2"
          />
          <span class="h-2 w-2 shrink-0 rounded-[2px]" :class="swatchClass(p.name)" />
          <span class="truncate font-semibold">{{ p.name }}</span>
          <span class="shrink-0 text-[11px] text-muted-foreground">{{ t('radar.ms.milestoneCount', { n: p.milestoneCount }, p.milestoneCount) }}</span>
          <span class="ml-auto flex shrink-0 items-center gap-2.5 tabular-nums">
            <span v-for="h in p.hot" :key="h.health" class="flex items-center gap-1 text-[11px] font-semibold" :class="HEALTH_TEXT[h.health]" :title="t(`radar.ms.health.${h.health}`)">
              <span class="h-1.5 w-1.5 rounded-full" :class="HEALTH_BG[h.health]" />{{ h.n }}
            </span>
          </span>
        </button>

        <template v-if="!foldedProjects.has(p.id)">
          <template v-for="(list, li) in [p.active, showDone ? p.done : []]" :key="li">
            <div v-if="li === 1 && list.length" class="px-4 pt-2 pb-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
              {{ t('radar.ms.doneSection', { n: list.length }) }}
            </div>
            <div v-for="v in list" :key="v.key" class="border-t border-border/40">
              <div
                :class="[MGRID, v.issues.length ? 'cursor-pointer' : '']"
                class="relative w-full pl-4 pr-3 py-2.5 text-left hover:bg-accent/40 transition-colors focus-visible:outline-2 focus-visible:outline-ring"
                :role="v.issues.length ? 'button' : undefined"
                :tabindex="v.issues.length ? 0 : undefined"
                :aria-expanded="v.issues.length ? expanded.has(v.key) : undefined"
                @click="v.issues.length && toggleMilestone(v.key)"
                @keydown.enter.self.prevent="v.issues.length && toggleMilestone(v.key)"
                @keydown.space.self.prevent="v.issues.length && toggleMilestone(v.key)"
              >
                <span v-if="HOT.includes(v.health)" class="absolute left-0 inset-y-2 w-[3px] rounded-r" :class="HEALTH_BG[v.health]" />
                <!-- Milestone: title, repos (+ counts and health on narrow panes) -->
                <span class="flex min-w-0 flex-col gap-0.5">
                  <span class="flex min-w-0 items-center gap-2">
                    <ChevronRight
                      class="h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform"
                      :class="[{ 'rotate-90': expanded.has(v.key) }, v.issues.length ? '' : 'invisible']"
                      :stroke-width="2"
                    />
                    <Flag v-if="!v.noMilestone" class="h-3 w-3 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                    <span class="truncate text-[13px] font-semibold" :class="{ 'italic font-medium text-muted-foreground': v.noMilestone }">
                      {{ v.noMilestone ? t('radar.ms.noMilestone') : v.title }}
                    </span>
                    <button
                      v-if="v.links.length"
                      class="shrink-0 rounded p-0.5 text-muted-foreground/60 hover:bg-accent hover:text-primary"
                      :title="t('radar.openOnGithub')"
                      :aria-label="t('radar.openOnGithub')"
                      @click.stop="openMilestone($event, v)"
                    >
                      <ExternalLink class="h-3 w-3" :stroke-width="1.75" />
                    </button>
                  </span>
                  <span class="truncate pl-5.5 font-mono text-[10.5px] text-muted-foreground">{{ v.repos.join(' · ') }}</span>
                  <span class="flex items-center gap-2 pl-5.5 @[52rem]/screen:hidden">
                    <span class="shrink-0 rounded-full px-1.75 py-px text-[10.5px] font-semibold" :class="HEALTH_CHIP[v.health]">{{ t(`radar.ms.health.${v.health}`) }}</span>
                    <span class="truncate text-[11px] text-muted-foreground">{{ t('radar.ms.openCount', { n: v.openCount }) }}</span>
                  </span>
                </span>

                <!-- Due date -->
                <span class="flex min-w-0 flex-col gap-px whitespace-nowrap leading-tight">
                  <span class="truncate text-[12px] font-semibold" :class="dueTone(v)">{{ v.noMilestone ? '–' : dueText(v) }}</span>
                  <span v-if="v.dueOn" class="flex items-center gap-1 text-[11px] text-muted-foreground tabular-nums">
                    <CalendarDays class="h-2.75 w-2.75 shrink-0" :stroke-width="2" />
                    <span class="truncate">{{ shortDate(v.dueOn) }}</span>
                  </span>
                </span>

                <!-- Progress: closed | past dev (striped) | the rest, with a marker at the elapsed time -->
                <span class="flex min-w-0 flex-col gap-1.5">
                  <template v-if="v.closedPct !== null">
                    <span class="flex items-baseline justify-between gap-2 text-[11px] text-muted-foreground tabular-nums">
                      <span class="truncate">
                        <span class="text-[12px] font-semibold text-foreground">{{ pct(v.closedPct) }}%</span>
                        · {{ t('radar.ms.closedOf', { closed: v.closedCount, total: v.openCount + v.closedCount }) }}
                        <template v-if="v.pastDevCount"> · {{ t('radar.ms.pastDev', { n: v.pastDevCount }) }}</template>
                      </span>
                      <span v-if="v.elapsedPct !== null && v.health !== 'done'" class="hidden shrink-0 @[64rem]/screen:inline">
                        {{ t('radar.ms.elapsed', { n: pct(v.elapsedPct) }) }}
                      </span>
                    </span>
                    <span class="relative flex h-2 rounded-full bg-muted">
                      <span class="h-full rounded-l-full bg-success" :class="{ 'rounded-r-full': (v.closedPct ?? 0) >= 100 }" :style="{ width: `${v.closedPct}%` }" />
                      <span class="h-full border-y border-r border-info/50" :style="{ width: `${v.pastDevPct}%`, ...PAST_DEV_STRIPES }" />
                      <span
                        v-if="v.elapsedPct !== null && v.health !== 'done'"
                        class="absolute -inset-y-1 w-0.5 rounded bg-foreground"
                        :style="{ left: `calc(${v.elapsedPct}% - 1px)` }"
                        :title="t('radar.ms.elapsed', { n: pct(v.elapsedPct) })"
                      />
                    </span>
                  </template>
                  <span v-else class="text-[11px] italic text-muted-foreground/80">{{ t('radar.ms.noCounts') }}</span>
                </span>

                <!-- Open work: counts needing attention, people -->
                <span class="hidden @[52rem]/screen:flex min-w-0 flex-col gap-1">
                  <span class="flex flex-wrap items-center gap-x-2.5 text-[11px] font-semibold tabular-nums">
                    <span class="font-medium text-muted-foreground">{{ t('radar.ms.openCount', { n: v.openCount }) }}</span>
                    <span v-for="s in issueHot(v)" :key="s" class="flex items-center gap-1" :class="TONE_TEXT[s]" :title="t(`radar.status.${s}`)">
                      <span class="h-1.5 w-1.5 rounded-full" :class="TONE_BG[s]" />{{ v.counts[s] }}
                    </span>
                  </span>
                  <span v-if="v.people.length" class="flex min-w-0 items-center gap-1.5 text-[11px] text-muted-foreground" :title="v.people.join(', ')">
                    <span class="flex shrink-0">
                      <span
                        v-for="(name, ni) in v.people.slice(0, 4)" :key="name"
                        class="flex h-4 w-4 items-center justify-center rounded-full text-[8.5px] font-bold text-white ring-[1.5px] ring-card"
                        :class="[swatchClass(name), ni > 0 ? '-ml-1' : '']"
                      >{{ initial(name) }}</span>
                    </span>
                    <span class="truncate">{{ v.people.join(', ') }}</span>
                  </span>
                </span>

                <!-- Health + why -->
                <span class="hidden @[52rem]/screen:flex min-w-0 flex-col items-start gap-1">
                  <span class="shrink-0 rounded-full px-1.75 py-px text-[10.5px] font-semibold" :class="HEALTH_CHIP[v.health]" :title="healthHint(v.health)">
                    {{ t(`radar.ms.health.${v.health}`) }}
                  </span>
                  <span v-if="healthWhy(v)" class="line-clamp-2 text-[10.5px] leading-snug text-muted-foreground">{{ healthWhy(v) }}</span>
                </span>
              </div>

              <!-- Open work of the milestone -->
              <div v-if="expanded.has(v.key)" class="bg-muted/25 border-t border-border/40 pt-1 pb-1.5 pl-6">
                <div :class="IGRID" class="pl-4 pr-3 py-1 text-[10px] font-semibold uppercase tracking-wider text-muted-foreground/80">
                  <span>{{ t('radar.colIssue') }}</span>
                  <span class="hidden @[36rem]/screen:block truncate">{{ t('radar.colBoardStatus') }}</span>
                  <span class="truncate">{{ t('radar.colDeadline') }}</span>
                  <span class="truncate text-right">{{ t('radar.colUpdated') }}</span>
                  <span />
                </div>
                <div class="divide-y divide-border/40">
                  <RadarIssueRow
                    v-for="{ issue: i, depth, hasChildren, hidden } in rowsOf(v)" :key="i.key"
                    :issue="i"
                    :depth="depth"
                    :has-children="hasChildren"
                    :hidden="hidden"
                    :now="now"
                    :show-repo="v.repos.length > 1"
                    :show-project="false"
                    :people="i.assignees"
                    show-unassigned
                    :deadline-note="v.afterDue.includes(i.key) ? t('radar.ms.afterDueShort') : undefined"
                    @toggle-children="toggleChildren(v, i)"
                    @add-todo="emit('add-todo', i)"
                    @open="emit('open-issue', i)"
                    @menu="emit('row-menu', $event, i)"
                  />
                </div>
                <p v-if="v.hiddenCount" class="px-4 pt-1.5 text-[11px] italic text-muted-foreground">
                  {{ t('radar.ms.hiddenIssues', { n: v.hiddenCount }, v.hiddenCount) }}
                </p>
              </div>
            </div>
          </template>
        </template>
      </div>
    </Card>

    <ContextMenu :open="!!linkMenu" :x="linkMenu?.x ?? 0" :y="linkMenu?.y ?? 0" @close="linkMenu = null">
      <DropdownItem v-for="l in linkMenu?.view.links ?? []" :key="l.htmlUrl" @click="openLink(l.htmlUrl)">
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
        <span class="font-mono">{{ l.repo }}</span>
      </DropdownItem>
    </ContextMenu>
  </div>
</template>
