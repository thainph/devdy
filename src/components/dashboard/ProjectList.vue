<script setup lang="ts">
// Work Dashboard — "By project": one traffic-light row per project, red →
// amber → green → gray ("not enough data" last). Each row: a severity stripe,
// the name and its level in words, two lines of reasons (the delivery risk,
// then what the other sources say: a customer waiting, a late todo…), the
// current milestone as "X% done · Y% of the time used" (an estimate; without
// a board, "Link board"), and count tags. Name / Radar open the Delivery
// Radar at the project; the top issue opens on GitHub.
import { useI18n } from 'vue-i18n'
import { ExternalLink, Link2 } from 'lucide-vue-next'
import { Badge, Button, Card } from '@/components/ui'
import { healthReasons, type ProjectRow } from '@/lib/dashboardLayout'
import type { SourceState } from '@/composables/useWorkDashboard'
import { HEALTH_LEVEL, LEVEL_BADGE, LEVEL_STRIPE } from './levels'
import { useDurationLabel } from './useDurationLabel'
import { useHealthWords } from './useHealthWords'

defineProps<{
  rows: ProjectRow[]
  state: SourceState
}>()

const emit = defineEmits<{ openRadar: [projectId: string]; openIssue: [url: string]; linkBoard: [projectId: string] }>()

const { t } = useI18n()
const { duration } = useDurationLabel()
const { reasonText, grayReason } = useHealthWords()

function firstLine(row: ProjectRow): string {
  const h = row.health
  const reasons = healthReasons(h).map(reasonText)
  if (reasons.length) return reasons.join(' · ')
  if (h.level === 'gray') return grayReason(h)
  return t('dashboard.byProject.noLateIssue')
}

function secondLine(row: ProjectRow): string | null {
  const s = row.signals
  const parts: string[] = []
  if (s.longestWait) {
    parts.push(s.longestWait.name
      ? t('dashboard.byProject.customerWaiting', { name: s.longestWait.name, duration: duration(s.longestWait.ms) })
      : t('dashboard.byProject.customerWaitingAnon', { duration: duration(s.longestWait.ms) }))
  }
  if (s.worstTodoLateDays !== null) {
    parts.push(s.worstTodoLateDays > 0
      ? t('dashboard.byProject.todoLate', { n: s.worstTodoLateDays }, s.worstTodoLateDays)
      : t('dashboard.byProject.todoLateToday'))
  }
  if (s.sessionsWaiting) parts.push(t('dashboard.byProject.sessionsWaiting', { n: s.sessionsWaiting }, s.sessionsWaiting))
  // A judged row with a caveat (last fetch failed, board cut short) says so.
  if (row.health.level !== 'gray' && row.health.reason) parts.push(grayReason(row.health))
  return parts.length ? parts.join(' · ') : null
}

function behind(row: ProjectRow): boolean {
  const p = row.health.progress
  return !!p && row.health.milestones.some((m) => m.title === p.title)
}

function progressLabel(row: ProjectRow): string {
  const p = row.health.progress!
  return p.elapsedPct === null
    ? t('dashboard.byProject.progressNoStart', { title: p.title, done: p.closedPct })
    : t('dashboard.byProject.progress', { title: p.title, done: p.closedPct, elapsed: p.elapsedPct })
}
</script>

<template>
  <Card>
    <p v-if="rows.length === 0" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.health.noProjects') }}
    </p>
    <ul v-else class="divide-y divide-border/50" :aria-busy="state.loading">
      <li
        v-for="row in rows"
        :key="row.health.projectId"
        class="relative grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1.5 py-3 pr-4 pl-5 lg:grid-cols-[minmax(8rem,12rem)_minmax(0,1fr)_minmax(10rem,15rem)_17rem]"
      >
        <span class="absolute inset-y-0 left-0 w-1" :class="LEVEL_STRIPE[HEALTH_LEVEL[row.health.level]]" aria-hidden="true" />

        <div class="min-w-0">
          <button
            type="button"
            class="block max-w-full cursor-pointer truncate text-left text-sm font-semibold text-foreground hover:underline"
            :title="t('dashboard.health.openRadarAt', { name: row.health.projectName })"
            @click="emit('openRadar', row.health.projectId)"
          >{{ row.health.projectName }}</button>
          <Badge :tone="LEVEL_BADGE[HEALTH_LEVEL[row.health.level]]" size="xs" class="mt-0.5">
            {{ t(`dashboard.byProject.level.${row.health.level}`) }}
          </Badge>
        </div>

        <div class="col-span-2 min-w-0 row-start-2 text-[13px] lg:col-span-1 lg:row-start-auto">
          <p class="truncate text-foreground" :title="firstLine(row)">{{ firstLine(row) }}</p>
          <p v-if="secondLine(row)" class="truncate text-[11px] text-muted-foreground" :title="secondLine(row) ?? undefined">{{ secondLine(row) }}</p>
        </div>

        <div class="col-span-2 row-start-3 min-w-0 lg:col-span-1 lg:row-start-auto">
          <div v-if="row.health.progress" class="grid gap-1" role="img" :aria-label="progressLabel(row)">
            <div class="relative h-1.5 rounded-full bg-muted" aria-hidden="true">
              <span
                class="absolute inset-y-0 left-0 rounded-full"
                :class="behind(row) ? LEVEL_STRIPE[HEALTH_LEVEL[row.health.level]] : 'bg-primary'"
                :style="{ width: `${row.health.progress.closedPct}%` }"
              />
              <span
                v-if="row.health.progress.elapsedPct !== null"
                class="absolute -inset-y-1 w-0.5 -translate-x-1/2 bg-foreground"
                :style="{ left: `${row.health.progress.elapsedPct}%` }"
              />
            </div>
            <span class="truncate text-[11px] text-muted-foreground" :title="progressLabel(row)">{{ progressLabel(row) }}</span>
          </div>
          <Button
            v-else-if="row.health.reason === 'no-board'"
            variant="outline"
            size="xs"
            @click="emit('linkBoard', row.health.projectId)"
          >
            <Link2 class="h-3 w-3" :stroke-width="1.75" />{{ t('dashboard.byProject.linkBoard') }}
          </Button>
          <span v-else-if="row.health.level !== 'gray'" class="text-[11px] text-muted-foreground">{{ t('dashboard.byProject.noMilestone') }}</span>
        </div>

        <div class="col-start-2 row-start-1 flex flex-wrap items-center justify-end gap-1 lg:col-start-auto lg:row-start-auto">
          <Badge v-if="row.signals.customersWaiting" tone="warning" size="xs">
            {{ t('dashboard.byProject.count.customers', { n: row.signals.customersWaiting }, row.signals.customersWaiting) }}
          </Badge>
          <Badge v-if="row.signals.prs" tone="neutral" size="xs">
            {{ t('dashboard.byProject.count.prs', { n: row.signals.prs }, row.signals.prs) }}
          </Badge>
          <Badge v-if="row.signals.overdueTodos" tone="error" size="xs">
            {{ t('dashboard.byProject.count.todos', { n: row.signals.overdueTodos }, row.signals.overdueTodos) }}
          </Badge>
          <Badge v-if="row.signals.sessions" tone="neutral" size="xs">
            {{ t('dashboard.byProject.count.sessions', { n: row.signals.sessions }, row.signals.sessions) }}
          </Badge>
          <Button
            v-if="row.health.topIssues[0]"
            variant="ghost"
            size="icon-sm"
            :title="t('dashboard.health.openIssue')"
            :aria-label="t('dashboard.health.openIssue')"
            @click="emit('openIssue', row.health.topIssues[0].htmlUrl)"
          >
            <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
          </Button>
          <Button variant="ghost" size="xs" @click="emit('openRadar', row.health.projectId)">{{ t('dashboard.health.radar') }}</Button>
        </div>
      </li>
    </ul>
  </Card>
</template>
