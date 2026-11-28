<script setup lang="ts">
// One issue row of the Delivery Radar, shared by the issue view (mode A) and
// the milestone view (mode B). Lays out on `IGRID`; the header row is the caller's.
// Reads like the inbox rows (InboxCard): title (click = open on
// GitHub) · meta (repo#num · project · people · parent). No buttons — + Todo,
// open and copy link live in the right-click menu the view owns (`menu`).
import { useI18n } from 'vue-i18n'
import {
  CalendarDays, ChevronRight, Clock, CornerDownRight, CornerUpLeft, ExternalLink, User, Users,
} from 'lucide-vue-next'
import { Badge } from '@/components/ui'
import { statusDotClass, type ClassifiedIssue } from '@/lib/deliveryRadar'
import { IGRID, TONE_CHIP } from './radarStyles'

const props = defineProps<{
  issue: ClassifiedIssue
  /** 0 = top level; 1 = sub-issue of the row above it, … */
  depth: number
  hasChildren: boolean
  /** Sub-issues folded under this row. */
  hidden: number
  now: number
  /** Show the repo in the `repo#n` reference (else only `#n`). */
  showRepo: boolean
  /** Show the project on the second line. */
  showProject: boolean
  /** People shown on the second line. */
  people: string[]
  /** `people` are the co-assignees of the group's person ("with …"). */
  coAssigned?: boolean
  /** Say "Unassigned" when `people` is empty. */
  showUnassigned?: boolean
  /** Extra note under the deadline (e.g. after the milestone's due date). */
  deadlineNote?: string
}>()

const emit = defineEmits<{
  'toggle-children': []
  open: []
  menu: [e: MouseEvent]
}>()

const { t, locale } = useI18n()

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
function deadlineTone(i: ClassifiedIssue): string {
  if (i.daysLeft !== null && i.daysLeft < 0) return 'text-destructive'
  if (i.radar === 'due-soon') return 'text-warning'
  return 'text-foreground font-medium'
}
function relativeTime(iso: string): string {
  const then = new Date(iso).getTime()
  if (Number.isNaN(then)) return ''
  const mins = Math.round((props.now - then) / 60000)
  if (mins < 1) return t('radar.justNow')
  if (mins < 60) return t('radar.minutesAgo', { mins })
  const hours = Math.round(mins / 60)
  if (hours < 24) return t('radar.hoursAgo', { hours })
  return t('radar.daysAgo', { days: Math.round(hours / 24) })
}
</script>

<template>
  <div
    :class="IGRID"
    class="group/row pr-3 py-2 transition-colors hover:bg-accent/50"
    @contextmenu.prevent="emit('menu', $event)"
  >
    <div class="flex items-start gap-1.5 min-w-0" :style="{ paddingLeft: `${0.75 + depth * 1.4}rem` }">
      <!-- Fold slot: chevron on parents, an elbow on leaf sub-issues, else the issue icon. -->
      <span class="flex h-4.5 w-4.5 shrink-0 items-center justify-center">
        <button
          v-if="hasChildren"
          class="flex h-4.5 w-4.5 items-center justify-center rounded text-muted-foreground hover:bg-accent hover:text-foreground"
          :title="hidden ? t('radar.expandChildren') : t('radar.collapseChildren')"
          :aria-label="hidden ? t('radar.expandChildren') : t('radar.collapseChildren')"
          :aria-expanded="!hidden"
          @click="emit('toggle-children')"
        >
          <ChevronRight class="h-3 w-3 transition-transform" :class="{ 'rotate-90': !hidden }" :stroke-width="2" />
        </button>
        <CornerDownRight v-else-if="depth > 0" class="h-3 w-3 text-muted-foreground/60" :stroke-width="1.75" />
      </span>
      <span class="flex min-w-0 flex-1 flex-col gap-1">
        <!-- Line 1: title (opens on GitHub), radar status (on-track needs no chip), folded children -->
        <span class="flex min-w-0 items-center gap-1.5">
          <button
            type="button"
            class="min-w-0 truncate text-left text-[13px] font-semibold leading-tight text-foreground transition-colors hover:text-primary cursor-pointer"
            :title="issue.parent ? `${issue.title}\n${t('radar.parentHint', { title: `#${issue.parent.number} ${issue.parent.title}` })}` : issue.title"
            @click="emit('open')"
          >{{ issue.title }}</button>
          <ExternalLink class="h-3 w-3 shrink-0 text-muted-foreground/50 opacity-0 transition-opacity group-hover/row:opacity-100" :stroke-width="2" />
          <span
            v-if="issue.radar !== 'on-track'"
            class="shrink-0 rounded-[5px] px-1.5 py-0.5 text-[10px] font-medium"
            :class="TONE_CHIP[issue.radar]"
          >{{ t(`radar.status.${issue.radar}`) }}</span>
          <span
            v-if="hidden"
            class="shrink-0 text-[10px] font-medium text-muted-foreground"
            :title="t('radar.hiddenChildren', { n: hidden })"
          >+{{ hidden }}</span>
        </span>
        <!-- Line 2: repo#num · status (narrow) · project · people · parent -->
        <span class="flex min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap text-[11px] text-muted-foreground">
          <span class="shrink-0 font-mono"><template v-if="showRepo">{{ issue.repo }} </template><span class="text-muted-foreground/70">#{{ issue.number }}</span></span>
          <span class="@[36rem]/screen:hidden shrink-0">{{ issue.status || t('radar.noStatus') }}</span>
          <Badge v-if="showProject" tone="primary" size="xs" class="shrink-0 max-w-32 truncate" :title="issue.projectName">
            {{ issue.projectName }}
          </Badge>
          <span v-if="people.length" class="flex min-w-0 shrink-0 items-center gap-1" :title="people.join(', ')">
            <component :is="people.length > 1 ? Users : User" class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="truncate">{{ coAssigned ? t('radar.alsoAssigned', { names: people.join(', ') }) : people.join(', ') }}</span>
          </span>
          <span v-else-if="showUnassigned" class="flex shrink-0 items-center gap-1 italic">
            <User class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            {{ t('radar.unassigned') }}
          </span>
          <!-- Parent not in this list (other assignee, done, …): say where the task belongs. -->
          <span v-if="depth === 0 && issue.parent" class="flex min-w-0 items-center gap-1">
            <CornerUpLeft class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="shrink-0 font-mono">#{{ issue.parent.number }}</span>
            <span class="truncate italic">{{ issue.parent.title }}</span>
          </span>
        </span>
      </span>
    </div>
    <span class="hidden @[36rem]/screen:flex min-w-0 items-center gap-1.5 text-[11.5px]">
      <span v-if="issue.status" class="h-1.5 w-1.5 shrink-0 rounded-full" :class="statusDotClass(issue.statusColor)" />
      <span class="truncate" :class="issue.status ? 'text-foreground' : 'text-muted-foreground/70'">{{ issue.status || t('radar.noStatus') }}</span>
    </span>
    <span
      class="flex min-w-0 flex-col gap-px whitespace-nowrap leading-tight"
      :title="issue.deadlineInherited ? t('radar.inheritedDeadline') : undefined"
    >
      <span v-if="issue.daysLeft === null" class="text-[12px] font-medium text-muted-foreground/70">{{ t('radar.noDeadlineShort') }}</span>
      <span v-else class="truncate text-[12px] font-semibold" :class="deadlineTone(issue)">{{ deadlineText(issue) }}</span>
      <span v-if="deadlineDate(issue)" class="flex items-center gap-1 text-[11px] text-muted-foreground tabular-nums">
        <CalendarDays class="h-2.75 w-2.75 shrink-0" :stroke-width="2" />
        <span class="truncate">{{ deadlineDate(issue) }}</span>
      </span>
      <span v-if="issue.deadlineInherited" class="text-[10px] italic text-muted-foreground/70">{{ t('radar.inheritedShort') }}</span>
      <span v-if="deadlineNote" class="truncate text-[10px] font-semibold text-warning">{{ deadlineNote }}</span>
    </span>
    <span
      class="flex min-w-0 items-center justify-end gap-1 text-right text-[11px] tabular-nums"
      :class="issue.radar === 'stale' ? 'text-warning font-semibold' : 'text-muted-foreground/70'"
    >
      <Clock v-if="issue.radar === 'stale'" class="h-3 w-3 shrink-0" :stroke-width="2" />
      <span class="truncate">{{ relativeTime(issue.updatedAt) }}</span>
    </span>
  </div>
</template>
