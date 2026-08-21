<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { openUrl } from '@tauri-apps/plugin-opener'
import { ZoomIn, ZoomOut, CalendarOff, User, Eye, EyeOff, Loader2 } from 'lucide-vue-next'
import {
  useProjectIssuesStore,
  type MilestoneGroup,
  type IssueRow,
} from '@/stores/projectIssues'

const props = defineProps<{
  milestones: MilestoneGroup[]
  /** Lane grouping: by milestone (default) or by assignee. */
  groupMode?: 'milestone' | 'assignee'
  /** Ordering of issues within each lane. */
  sortBy?: 'start' | 'assignee'
  /** Milestone titles (lowercased) currently showing their closed issues. */
  expanded?: Set<string>
  /** Milestone titles (lowercased) whose closed issues are being fetched. */
  loading?: Set<string>
}>()
const emit = defineEmits<{ (e: 'toggle-closed', title: string): void }>()
const store = useProjectIssuesStore()
const { t } = useI18n()

const NO_MILESTONE = 'No milestone'
function isExpanded(title: string): boolean {
  return props.expanded?.has(title.toLowerCase()) ?? false
}
function isLoadingClosed(title: string): boolean {
  return props.loading?.has(title.toLowerCase()) ?? false
}

const DAY = 86_400_000
const LEFT_WIDTH = 320
const ROW_H = 34

const pxPerDay = ref(28)
function zoom(delta: number) {
  pxPerDay.value = Math.max(8, Math.min(64, pxPerDay.value + delta))
}

interface Bar {
  issue: IssueRow
  start: number
  end: number
  inferredStart: boolean
}
interface Lane {
  group: MilestoneGroup
  bars: Bar[]
  due: number | null
  minStart: number
}

// The timeline end of an issue: a closed issue ends when it was closed (even if
// it never had a deadline); an open issue needs a deadline (board Deadline field
// OR milestone due date).
function barEnd(issue: IssueRow): string | null {
  if (issue.state === 'CLOSED' && issue.closedAt) return issue.closedAt
  return store.effectiveDeadline(issue)
}

// A bar needs an end (see barEnd). When the Start date is missing we infer it
// from the issue's creation date so milestone-based projects still render; such
// bars are flagged as inferred.
const lanes = computed<Lane[]>(() => {
  const built = props.milestones
    .map((group) => {
      const bars: Bar[] = []
      for (const issue of group.issues) {
        const dl = barEnd(issue)
        if (!dl) continue
        const startRaw = issue.startDate ?? issue.createdAt
        const start = new Date(startRaw).getTime()
        let end = new Date(dl).getTime()
        if (Number.isNaN(start) || Number.isNaN(end)) continue
        if (end < start) end = start
        bars.push({ issue, start, end, inferredStart: !issue.startDate })
      }
      // Unassigned sorts last (￿) when ordering by assignee.
      const assigneeKey = (b: Bar) => (b.issue.assignees[0]?.toLowerCase() ?? '￿')
      bars.sort((a, b) => {
        if (props.sortBy === 'assignee') {
          const ka = assigneeKey(a)
          const kb = assigneeKey(b)
          if (ka !== kb) return ka < kb ? -1 : 1
        }
        return a.start - b.start || a.end - b.end
      })
      const due = group.dueOn ? new Date(group.dueOn).getTime() : null
      const minStart = bars.length ? bars[0].start : Infinity
      return { group, bars, due: Number.isNaN(due as number) ? null : due, minStart }
    })
    .filter((l) => l.bars.length > 0)
  built.sort((a, b) => {
    const ad = a.due ?? Infinity
    const bd = b.due ?? Infinity
    if (ad !== bd) return ad - bd
    return a.minStart - b.minStart
  })
  return built
})

// Only issues with no deadline at all cannot be placed on the timeline.
const unscheduled = computed<IssueRow[]>(() => {
  const out: IssueRow[] = []
  for (const group of props.milestones) {
    for (const issue of group.issues) {
      if (!barEnd(issue)) out.push(issue)
    }
  }
  return out
})

const hasData = computed(() => lanes.value.length > 0)

const range = computed(() => {
  let min = Infinity
  let max = -Infinity
  for (const lane of lanes.value) {
    for (const b of lane.bars) {
      if (b.start < min) min = b.start
      if (b.end > max) max = b.end
    }
  }
  if (!Number.isFinite(min)) {
    const now = Date.now()
    return { start: now - 7 * DAY, end: now + 7 * DAY }
  }
  return { start: min - 2 * DAY, end: max + 2 * DAY }
})

const totalDays = computed(() => Math.max(1, Math.ceil((range.value.end - range.value.start) / DAY)))
const chartWidth = computed(() => totalDays.value * pxPerDay.value)

function x(t: number): number {
  return ((t - range.value.start) / DAY) * pxPerDay.value
}

const todayX = computed(() => {
  const now = Date.now()
  if (now < range.value.start || now > range.value.end) return null
  return x(now)
})

interface Tick { x: number; label: string }
const ticks = computed<Tick[]>(() => {
  const step = pxPerDay.value >= 22 ? 1 : 7
  const out: Tick[] = []
  const startDate = new Date(range.value.start)
  startDate.setHours(0, 0, 0, 0)
  for (let t = startDate.getTime(); t <= range.value.end; t += step * DAY) {
    const d = new Date(t)
    out.push({ x: x(t), label: `${d.getDate()}/${d.getMonth() + 1}` })
  }
  return out
})

function barGeom(b: Bar) {
  const left = x(b.start)
  const width = Math.max(6, x(b.end) - left)
  return { left, width }
}
function barStyle(b: Bar) {
  const { left, width } = barGeom(b)
  return { left: `${left}px`, width: `${width}px` }
}

// Colour encodes lateness (see legend), independent of the board Status text.
// Closed issues read as "done" regardless of their former lateness.
function barClass(issue: IssueRow): string {
  if (issue.state === 'CLOSED') return 'bg-emerald-600/70 border-emerald-500'
  switch (store.issueStatus(issue)) {
    case 'overdue': return 'bg-red-500/85 border-red-400'
    case 'at-risk': return 'bg-amber-500/85 border-amber-400'
    case 'stale': return 'bg-yellow-600/75 border-yellow-500'
    case 'not-started': return 'bg-indigo-400/45 border-indigo-300/60'
    default: return 'bg-indigo-500/85 border-indigo-400'
  }
}

const LEGEND = computed<{ label: string; class: string }[]>(() => [
  { label: t('gantt.chart.legendOverdue'), class: 'bg-red-500/85' },
  { label: t('gantt.chart.legendAtRisk'), class: 'bg-amber-500/85' },
  { label: t('gantt.chart.legendStalled'), class: 'bg-yellow-600/75' },
  { label: t('gantt.chart.legendNotStarted'), class: 'bg-indigo-400/45' },
  { label: t('gantt.chart.legendOnTrack'), class: 'bg-indigo-500/85' },
  { label: t('gantt.chart.legendClosed'), class: 'bg-emerald-600/70' },
])

function statusText(issue: IssueRow): string {
  return issue.status ?? t('gantt.chart.noStatus')
}

// Tint the status badge to match the GitHub Projects option colour.
const GH_STATUS_CLASS: Record<string, string> = {
  GRAY: 'bg-slate-500/25 text-slate-700 dark:text-slate-100 border-slate-500/50',
  BLUE: 'bg-blue-500/25 text-blue-800 dark:text-blue-100 border-blue-500/50',
  GREEN: 'bg-emerald-500/25 text-emerald-800 dark:text-emerald-100 border-emerald-500/50',
  YELLOW: 'bg-amber-500/25 text-amber-800 dark:text-amber-100 border-amber-500/50',
  ORANGE: 'bg-orange-500/25 text-orange-800 dark:text-orange-100 border-orange-500/50',
  RED: 'bg-red-500/25 text-red-800 dark:text-red-100 border-red-500/50',
  PURPLE: 'bg-purple-500/25 text-purple-800 dark:text-purple-100 border-purple-500/50',
  PINK: 'bg-pink-500/25 text-pink-800 dark:text-pink-100 border-pink-500/50',
}
// Resolve a GitHub colour enum: prefer the real board option colour, otherwise
// infer from the status name so common workflows still read like GitHub.
function ghColor(issue: IssueRow): string | null {
  if (issue.statusColor) return issue.statusColor.toUpperCase()
  const s = (issue.status ?? '').toLowerCase().trim()
  if (!s) return null
  if (/(done|complete|closed|shipped|merged|resolved|hoàn thành)/.test(s)) return 'GREEN'
  if (/(review|qa|test|verify|kiểm thử)/.test(s)) return 'PURPLE'
  if (/(progress|doing|wip|develop|đang làm)/.test(s)) return 'YELLOW'
  if (/(ready|sẵn sàng)/.test(s)) return 'BLUE'
  if (/(block|hold|reject|fail|chặn)/.test(s)) return 'RED'
  return 'GRAY'
}
function statusClass(issue: IssueRow): string {
  const enum_ = ghColor(issue)
  const c = enum_ ? GH_STATUS_CLASS[enum_] : undefined
  return c ?? 'bg-muted/60 text-muted-foreground border-border/70'
}

function assigneeText(issue: IssueRow): string {
  if (!issue.assignees.length) return ''
  const [first, ...rest] = issue.assignees
  return rest.length ? `${first} +${rest.length}` : first
}

function milestoneDueX(group: MilestoneGroup): number | null {
  if (!group.dueOn) return null
  const t = new Date(group.dueOn).getTime()
  if (Number.isNaN(t) || t < range.value.start || t > range.value.end) return null
  return x(t)
}

function open(issue: IssueRow) {
  openUrl(issue.htmlUrl).catch(() => {})
}

function fmt(iso: string | null): string {
  if (!iso) return '—'
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return '—'
  return `${d.getDate()}/${d.getMonth() + 1}/${d.getFullYear()}`
}
</script>

<template>
  <div class="flex flex-col gap-3 h-full min-h-0">
    <!-- Toolbar + legend -->
    <div class="flex items-center justify-between gap-4 flex-wrap shrink-0">
      <div class="flex items-center gap-3 flex-wrap text-[10px] text-muted-foreground">
        <span>{{ t('gantt.chart.colourLateness') }}</span>
        <span v-for="l in LEGEND" :key="l.label" class="flex items-center gap-1">
          <span class="h-2.5 w-2.5 rounded-sm" :class="l.class" />
          {{ l.label }}
        </span>
      </div>
      <div class="flex items-center gap-1">
        <button
          class="flex h-6 w-6 items-center justify-center rounded border border-border/60 text-muted-foreground hover:text-foreground hover:border-border"
          :title="t('gantt.chart.zoomOut')" @click="zoom(-6)"
        >
          <ZoomOut class="h-3.5 w-3.5" />
        </button>
        <button
          class="flex h-6 w-6 items-center justify-center rounded border border-border/60 text-muted-foreground hover:text-foreground hover:border-border"
          :title="t('gantt.chart.zoomIn')" @click="zoom(6)"
        >
          <ZoomIn class="h-3.5 w-3.5" />
        </button>
      </div>
    </div>

    <!-- Gantt -->
    <div v-if="hasData" class="flex-1 min-h-0 rounded-lg border border-border/60 overflow-auto bg-card/20">
      <div :style="{ width: `${LEFT_WIDTH + chartWidth}px` }">
        <!-- Header: date ticks -->
        <div class="flex sticky top-0 z-20 bg-card border-b border-border/60" :style="{ height: '28px' }">
          <div
            class="sticky left-0 z-30 shrink-0 bg-card border-r border-border/60 flex items-center px-3 text-[10px] font-medium text-muted-foreground"
            :style="{ width: `${LEFT_WIDTH}px` }"
          >
            {{ groupMode === 'assignee' ? t('gantt.chart.columnHeaderAssignee') : t('gantt.chart.columnHeader') }}
          </div>
          <div class="relative" :style="{ width: `${chartWidth}px` }">
            <div
              v-for="(tk, i) in ticks" :key="i"
              class="absolute top-0 bottom-0 flex items-center border-l border-border/30 pl-1 text-[9px] text-muted-foreground/70"
              :style="{ left: `${tk.x}px` }"
            >
              {{ tk.label }}
            </div>
          </div>
        </div>

        <!-- Lanes -->
        <template v-for="lane in lanes" :key="lane.group.title">
          <!-- Milestone header row -->
          <div class="flex border-b border-border/40 bg-muted/30" :style="{ height: `${ROW_H}px` }">
            <div
              class="sticky left-0 z-10 shrink-0 bg-muted/40 border-r border-border/60 flex items-center px-3 gap-2 text-[11px] font-semibold"
              :style="{ width: `${LEFT_WIDTH}px` }"
              :title="lane.group.title"
            >
              <span class="truncate">{{ lane.group.title }}</span>
              <span v-if="lane.group.dueOn" class="ml-auto shrink-0 text-[10px] font-normal text-muted-foreground">
                {{ t('gantt.chart.due', { date: fmt(lane.group.dueOn) }) }}
              </span>
              <button
                v-if="groupMode !== 'assignee' && lane.group.title !== NO_MILESTONE"
                class="shrink-0 flex items-center gap-1 rounded border border-border/60 px-1.5 py-0.5 text-[9px] font-normal text-muted-foreground hover:text-foreground hover:border-border transition-colors"
                :class="{ 'ml-auto': !lane.group.dueOn }"
                :title="isExpanded(lane.group.title) ? t('gantt.chart.hideClosed') : t('gantt.chart.showClosed')"
                @click="emit('toggle-closed', lane.group.title)"
              >
                <Loader2 v-if="isLoadingClosed(lane.group.title)" class="h-2.5 w-2.5 animate-spin" />
                <component :is="isExpanded(lane.group.title) ? EyeOff : Eye" v-else class="h-2.5 w-2.5" />
                {{ isExpanded(lane.group.title) ? t('gantt.chart.hideClosed') : t('gantt.chart.showClosed') }}
              </button>
            </div>
            <div class="relative" :style="{ width: `${chartWidth}px` }">
              <div
                v-if="todayX !== null"
                class="absolute top-0 bottom-0 w-px bg-red-500/60 z-0"
                :style="{ left: `${todayX}px` }"
              />
              <div
                v-if="milestoneDueX(lane.group) !== null"
                class="absolute top-1/2 -translate-y-1/2 h-2.5 w-2.5 rotate-45 bg-primary/80 border border-primary z-10"
                :style="{ left: `${milestoneDueX(lane.group)! - 5}px` }"
                :title="t('gantt.chart.milestoneDue', { date: fmt(lane.group.dueOn) })"
              />
            </div>
          </div>

          <!-- Issue rows -->
          <div
            v-for="b in lane.bars" :key="`${b.issue.repo}#${b.issue.number}`"
            class="flex border-b border-border/30 hover:bg-accent/20"
            :style="{ height: `${ROW_H}px` }"
          >
            <div
              class="sticky left-0 z-10 shrink-0 bg-card border-r border-border/60 flex items-center gap-1.5 px-3 text-[11px] cursor-pointer hover:text-primary"
              :style="{ width: `${LEFT_WIDTH}px` }"
              :title="`${b.issue.repo}#${b.issue.number} — ${b.issue.title}`"
              @click="open(b.issue)"
            >
              <span class="font-mono text-[10px] text-muted-foreground/70 shrink-0">#{{ b.issue.number }}</span>
              <span class="flex-1 min-w-0 truncate">{{ b.issue.title }}</span>
              <span
                v-if="assigneeText(b.issue)"
                class="shrink-0 flex items-center gap-0.5 text-[9px] text-muted-foreground max-w-24 truncate"
                :title="t('gantt.chart.assignees', { names: b.issue.assignees.join(', ') })"
              >
                <User class="h-2.5 w-2.5" />{{ assigneeText(b.issue) }}
              </span>
              <span
                class="shrink-0 rounded-full border px-1.5 py-0.5 text-[10px] font-medium max-w-24 truncate"
                :class="statusClass(b.issue)"
                :title="statusText(b.issue)"
              >
                {{ statusText(b.issue) }}
              </span>
            </div>
            <div class="relative" :style="{ width: `${chartWidth}px` }">
              <div
                v-if="todayX !== null"
                class="absolute top-0 bottom-0 w-px bg-red-500/60 z-0"
                :style="{ left: `${todayX}px` }"
              />
              <div
                class="absolute top-1/2 -translate-y-1/2 h-4.5 rounded border cursor-pointer z-10 hover:brightness-110 flex items-center overflow-hidden"
                :class="[barClass(b.issue), b.inferredStart ? 'border-dashed opacity-90' : '']"
                :style="barStyle(b)"
                :title="`${b.issue.repo}#${b.issue.number}\n${t('gantt.chart.barStatus', { status: statusText(b.issue) })}${b.issue.assignees.length ? '\n' + t('gantt.chart.assignees', { names: b.issue.assignees.join(', ') }) : ''}\n${b.inferredStart ? t('gantt.chart.startInferred') : ''}${fmt(b.inferredStart ? b.issue.createdAt : b.issue.startDate)} → ${fmt(store.effectiveDeadline(b.issue))}`"
                @click="open(b.issue)"
              >
                <span
                  v-if="barGeom(b).width > 46"
                  class="px-1.5 text-[9px] font-medium text-white/90 truncate pointer-events-none"
                >
                  {{ statusText(b.issue) }}
                </span>
              </div>
            </div>
          </div>
        </template>
      </div>
    </div>

    <div v-else class="rounded-lg border border-border/60 p-8 text-center text-[12px] text-muted-foreground">
      {{ t('gantt.chart.noDeadline') }}
    </div>

    <!-- Unscheduled -->
    <div v-if="unscheduled.length > 0" class="shrink-0 max-h-40 overflow-auto rounded-lg border border-border/60 bg-card/20 p-4 space-y-2">
      <div class="flex items-center gap-2 text-[11px] font-semibold text-muted-foreground">
        <CalendarOff class="h-3.5 w-3.5" />
        {{ t('gantt.chart.unscheduled', { count: unscheduled.length }) }}
      </div>
      <div class="flex flex-col gap-1">
        <button
          v-for="it in unscheduled" :key="`${it.repo}#${it.number}`"
          class="flex items-center gap-2 text-left text-[11px] hover:text-primary truncate"
          :title="it.title"
          @click="open(it)"
        >
          <span class="font-mono text-[10px] text-muted-foreground/70 shrink-0">{{ it.repo }}#{{ it.number }}</span>
          <span class="truncate">{{ it.title }}</span>
          <span class="shrink-0 rounded-full border px-1.5 py-0.5 text-[10px] font-medium" :class="statusClass(it)">
            {{ statusText(it) }}
          </span>
        </button>
      </div>
    </div>
  </div>
</template>
