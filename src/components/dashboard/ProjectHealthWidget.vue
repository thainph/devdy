<script setup lang="ts">
// Work Dashboard — projects at risk of slipping, from the Delivery Radar
// boards (`projectHealth`): red / amber rows with their 3 latest issues (and
// risky milestones, an estimate), gray rows that could not be judged (fetch
// error, truncated board, still loading) with the reason; projects without a
// board / GitLab-only and the on-track ones are summed up in one line.
// Neutral wording: "at risk of slipping", never "late project".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, CircleCheck, CircleDashed, ExternalLink, Flag, OctagonAlert, Radar } from 'lucide-vue-next'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import type { ProjectHealth } from '@/lib/projectHealth'
import type { ClassifiedIssue } from '@/lib/deliveryRadar'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'

const props = defineProps<{
  healths: ProjectHealth[]
  state: SourceState
  now: number
}>()

const emit = defineEmits<{ openIssue: [url: string]; openRadar: [projectId?: string] }>()

const { t } = useI18n()

const SUMMARY_REASONS = new Set(['no-board', 'gitlab'])
const listed = computed(() =>
  props.healths.filter((h) => h.level !== 'green' && !(h.level === 'gray' && SUMMARY_REASONS.has(h.reason ?? ''))),
)
const shown = computed(() => listed.value.slice(0, WIDGET_ROWS))
const atRisk = computed(() => props.healths.filter((h) => h.level === 'red' || h.level === 'orange').length)
const noBoard = computed(() => props.healths.filter((h) => h.level === 'gray' && h.reason === 'no-board').length)
const gitlab = computed(() => props.healths.filter((h) => h.level === 'gray' && h.reason === 'gitlab').length)
const green = computed(() => props.healths.filter((h) => h.level === 'green').length)

const LEVEL = {
  red: { icon: OctagonAlert, cls: 'text-destructive' },
  orange: { icon: AlertTriangle, cls: 'text-warning' },
  gray: { icon: CircleDashed, cls: 'text-muted-foreground' },
  green: { icon: CircleCheck, cls: 'text-success' },
} as const

function issueReason(i: ClassifiedIssue): string {
  const key = `dashboard.health.issue.${i.radar}`
  if (i.radar === 'overdue') return t(key, { n: -(i.daysLeft ?? 0) }, -(i.daysLeft ?? 0))
  if (i.radar === 'due-soon') return t(key, { n: i.daysLeft ?? 0 }, i.daysLeft ?? 0)
  return t(key)
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.health.title')"
    :icon="Radar"
    :count="atRisk"
    :count-title="t('dashboard.health.countTitle')"
    :scope="t('dashboard.health.scope')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.health.openRadar')"
    @see-all="emit('openRadar')"
  >
    <p v-if="healths.length === 0" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.health.noProjects') }}
    </p>
    <ul v-else class="divide-y divide-border/40">
      <li v-for="h in shown" :key="h.projectId" class="px-3 py-2">
        <div class="flex min-w-0 items-center gap-1.5">
          <component :is="LEVEL[h.level].icon" class="h-3.5 w-3.5 shrink-0" :class="LEVEL[h.level].cls" :stroke-width="2" />
          <button
            type="button"
            class="min-w-0 truncate text-left text-[13px] font-medium text-foreground hover:underline"
            :title="t('dashboard.health.openRadarAt', { name: h.projectName })"
            @click="emit('openRadar', h.projectId)"
          >{{ h.projectName }}</button>
          <span class="shrink-0 text-[11px]" :class="LEVEL[h.level].cls">{{ t(`dashboard.health.level.${h.level}`) }}</span>
          <span v-if="h.riskyIssueCount" class="shrink-0 text-[11px] text-muted-foreground">
            · {{ t('dashboard.health.riskyIssues', { n: h.riskyIssueCount }, h.riskyIssueCount) }}
          </span>
          <span v-if="h.reason" class="truncate text-[11px] text-muted-foreground">· {{ t(`dashboard.health.reason.${h.reason}`) }}</span>
        </div>
        <ul v-if="h.topIssues.length || h.milestones.length" class="mt-1 space-y-0.5 pl-5">
          <li v-for="m in h.milestones.slice(0, 2)" :key="`m:${m.title}`" class="flex min-w-0 items-center gap-1.5 text-[11px] text-muted-foreground">
            <Flag class="h-3 w-3 shrink-0" :class="m.kind === 'overdue' ? 'text-destructive' : 'text-warning'" :stroke-width="1.75" />
            <span class="truncate">{{ m.title }}</span>
            <span class="shrink-0" :class="m.kind === 'overdue' ? 'text-destructive' : 'text-warning'">
              {{ m.kind === 'overdue'
                ? t('dashboard.health.milestoneOverdue', { n: m.lateDays ?? 0, open: m.openCount }, m.lateDays ?? 0)
                : t('dashboard.health.milestoneLag', { elapsed: m.elapsedPct, closed: m.closedPct }) }}
            </span>
          </li>
          <li v-for="i in h.topIssues" :key="i.key" class="group flex min-w-0 items-center gap-1.5 text-[11px]">
            <button
              type="button"
              class="flex min-w-0 flex-1 cursor-pointer items-center gap-1.5 text-left text-muted-foreground hover:text-foreground"
              @click="emit('openIssue', i.htmlUrl)"
            >
              <span class="shrink-0 tabular-nums">#{{ i.number }}</span>
              <span class="truncate text-foreground/90">{{ i.title }}</span>
              <span v-if="i.activeAssignees.length" class="shrink-0">· {{ i.activeAssignees.join(', ') }}</span>
              <span class="shrink-0" :class="i.radar === 'overdue' ? 'text-destructive' : 'text-warning'">
                · {{ issueReason(i) }}<template v-if="i.deadlineInherited"> {{ t('dashboard.health.inherited') }}</template>
              </span>
              <ExternalLink class="h-3 w-3 shrink-0 opacity-0 group-hover:opacity-100" :stroke-width="1.75" />
            </button>
          </li>
        </ul>
      </li>
      <li
        v-if="noBoard || gitlab || green"
        class="flex flex-wrap items-center gap-x-2 px-3 py-2 text-[11px] text-muted-foreground"
      >
        <span v-if="green" class="text-success">{{ t('dashboard.health.greenCount', { n: green }, green) }}</span>
        <span v-if="noBoard">{{ t('dashboard.health.noBoardCount', { n: noBoard }, noBoard) }}</span>
        <span v-if="gitlab">{{ t('dashboard.health.gitlabCount', { n: gitlab }, gitlab) }}</span>
      </li>
    </ul>
  </DashboardWidget>
</template>
