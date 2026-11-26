<script setup lang="ts">
// One issue row of the Delivery Radar, shared by the issue view (mode A) and
// the milestone view (mode B). Lays out on `IGRID`; the header row is the caller's.
import { useI18n } from 'vue-i18n'
import {
  CalendarDays, ChevronRight, Clock, CornerDownRight, CornerUpLeft, ExternalLink, ListPlus,
} from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { statusDotClass, swatchClass, type ClassifiedIssue } from '@/lib/deliveryRadar'
import { IGRID, TONE_BG, TONE_CHIP } from './radarStyles'

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
  'add-todo': []
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
function initial(name: string): string {
  return (name.replace(/^[^\p{L}\p{N}]+/u, '')[0] ?? '?').toUpperCase()
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
    class="group/row mx-1.5 rounded-md pr-1.5 py-1.5 hover:bg-accent/50"
    :title="issue.parent ? `${issue.title}\n${t('radar.parentHint', { title: `#${issue.parent.number} ${issue.parent.title}` })}` : issue.title"
    @contextmenu.prevent="emit('menu', $event)"
  >
    <div class="flex items-start gap-2 min-w-0" :style="{ paddingLeft: `${0.5 + depth * 1.4}rem` }">
      <!-- Severity square, then the fold slot: chevron on parents, an elbow on leaf sub-issues. -->
      <span class="flex w-4.5 shrink-0 flex-col items-center gap-1 pt-1">
        <span class="h-2 w-2 rounded-[2px]" :class="TONE_BG[issue.radar]" :title="t(`radar.status.${issue.radar}`)" />
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
        <!-- Line 1: title, radar status (on-track needs no chip), folded children -->
        <span class="flex min-w-0 items-baseline gap-2">
          <span class="truncate text-[13px] font-medium leading-snug text-foreground">{{ issue.title }}</span>
          <span
            v-if="issue.radar !== 'on-track'"
            class="shrink-0 rounded-full px-1.75 py-px text-[10.5px] font-semibold"
            :class="TONE_CHIP[issue.radar]"
          >{{ t(`radar.status.${issue.radar}`) }}</span>
          <span
            v-if="hidden"
            class="shrink-0 rounded-full border border-dashed border-border px-1.5 text-[10px] font-semibold text-muted-foreground"
            :title="t('radar.hiddenChildren', { n: hidden })"
          >+{{ hidden }}</span>
        </span>
        <!-- Line 2: each kind of meta has its own form (code ref, project swatch, avatars, parent link) -->
        <span class="flex min-w-0 items-center gap-2 overflow-hidden whitespace-nowrap text-[11px] text-muted-foreground">
          <span class="shrink-0 rounded bg-muted px-1.25 py-px font-mono text-[10.5px]"><template v-if="showRepo">{{ issue.repo }}</template><span class="font-medium text-foreground">#{{ issue.number }}</span></span>
          <span class="@[36rem]/screen:hidden flex shrink-0 items-center gap-1.5">
            <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
            <span class="h-2 w-2 rounded-full" :class="statusDotClass(issue.statusColor)" />
            {{ issue.status || t('radar.noStatus') }}
          </span>
          <span v-if="showProject" class="flex shrink-0 items-center gap-1.5 font-medium text-foreground/80">
            <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
            <span class="h-1.75 w-1.75 rounded-[2px]" :class="swatchClass(issue.projectName)" />
            {{ issue.projectName }}
          </span>
          <span v-if="people.length" class="flex shrink-0 items-center gap-1.5">
            <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
            <span class="flex">
              <span
                v-for="(p, pi) in people.slice(0, 3)" :key="p"
                class="flex h-4 w-4 items-center justify-center rounded-full text-[8.5px] font-bold text-white ring-[1.5px] ring-card"
                :class="[swatchClass(p), pi > 0 ? '-ml-1' : '']"
              >{{ initial(p) }}</span>
            </span>
            {{ coAssigned ? t('radar.alsoAssigned', { names: people.join(', ') }) : people.join(', ') }}
          </span>
          <span v-else-if="showUnassigned" class="flex shrink-0 items-center gap-1.5 italic">
            <span class="h-1.5 w-1.5 rounded-full bg-muted-foreground/40" />
            {{ t('radar.unassigned') }}
          </span>
          <!-- Parent not in this list (other assignee, done, …): say where the task belongs. -->
          <span v-if="depth === 0 && issue.parent" class="flex min-w-0 items-center gap-1">
            <span class="mr-1 h-1.5 w-1.5 shrink-0 rounded-full bg-muted-foreground/40" />
            <CornerUpLeft class="h-3 w-3 shrink-0" :stroke-width="1.75" />
            <span class="shrink-0 font-mono text-[10.5px]">#{{ issue.parent.number }}</span>
            <span class="truncate italic">{{ issue.parent.title }}</span>
          </span>
        </span>
      </span>
    </div>
    <span class="hidden @[36rem]/screen:flex min-w-0">
      <span
        class="inline-flex max-w-full items-center gap-1.5 rounded-md border px-2 py-0.5 text-[11.5px]"
        :class="issue.status ? 'border-border font-medium text-foreground' : 'border-dashed border-border text-muted-foreground/70'"
      >
        <span v-if="issue.status" class="h-2 w-2 rounded-full shrink-0" :class="statusDotClass(issue.statusColor)" />
        <span class="truncate">{{ issue.status || t('radar.noStatus') }}</span>
      </span>
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
    <span class="flex justify-end gap-0.5 opacity-50 transition-opacity group-hover/row:opacity-100 focus-within:opacity-100">
      <Button
        variant="ghost"
        size="icon-xs"
        class="text-muted-foreground/70 hover:text-foreground"
        :title="t('focus.addTodoHint')"
        :aria-label="t('focus.addTodoHint')"
        @click="emit('add-todo')"
      >
        <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        variant="ghost"
        size="icon-xs"
        class="text-muted-foreground/70 hover:text-primary"
        :title="t('radar.openOnGithub')"
        :aria-label="t('radar.openOnGithub')"
        @click="emit('open')"
      >
        <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </span>
  </div>
</template>
