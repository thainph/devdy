<script setup lang="ts">
// Work Dashboard — Projects (Delivery Radar boards, `projectHealth`): how many
// are at risk of slipping, need attention, are on track; one row per red /
// amber project with its reasons in words (latest issue, milestone behind by
// X% — an estimate; a failed last fetch or a cut board is said on the row,
// since the verdict then rests on older / partial data) and Open issue /
// Radar; the on-track ones fold into one line, those that cannot be judged
// into one line per reason (no board, GitLab, fetch error…). Neutral wording: "at risk of slipping", never "late project".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, ExternalLink, Radar } from 'lucide-vue-next'
import { Badge, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import { fitGroups, healthReasons, projectCard, unknownByReason } from '@/lib/dashboardLayout'
import type { ProjectHealth } from '@/lib/projectHealth'
import type { FittedGroup } from '@/lib/dashboardLayout'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import DashGroup from './DashGroup.vue'
import DashRow from './DashRow.vue'
import { HEALTH_LEVEL, LEVEL_BADGE, LEVEL_TEXT } from './levels'
import { useHealthWords } from './useHealthWords'

const props = defineProps<{
  healths: ProjectHealth[]
  state: SourceState
  now: number
}>()

const emit = defineEmits<{ openIssue: [url: string]; openRadar: [projectId?: string] }>()

const { t } = useI18n()
const { reasonText, grayReason } = useHealthWords()

const card = computed(() => projectCard(props.healths))

/** A folded line: every on-track project, or the projects not judged for one reason. */
type RestRow = { kind: 'ok', reason: null, projects: ProjectHealth[] } | { kind: 'unknown', reason: string, projects: ProjectHealth[] }
const restRows = computed<RestRow[]>(() => [
  ...(card.value.ok.length ? [{ kind: 'ok' as const, reason: null, projects: card.value.ok }] : []),
  ...unknownByReason(card.value.unknown).map((u) => ({ kind: 'unknown' as const, ...u })),
])
const groups = computed(() => fitGroups<'risky' | 'rest', ProjectHealth | RestRow>([
  { key: 'risky', items: [...card.value.risk, ...card.value.attention] },
  { key: 'rest', items: restRows.value },
], WIDGET_ROWS))
const riskyGroup = computed(() => groups.value.find((g) => g.key === 'risky') as FittedGroup<'risky', ProjectHealth> | undefined)
const restGroup = computed(() => groups.value.find((g) => g.key === 'rest') as FittedGroup<'rest', RestRow> | undefined)

function names(list: ProjectHealth[]): string {
  return list.map((h) => h.projectName).join(', ')
}

/** Reasons in words, plus the caveat when the verdict rests on old or partial data. */
function riskySub(h: ProjectHealth): string | null {
  const parts = healthReasons(h).map(reasonText)
  if (h.reason) parts.push(grayReason(h))
  return parts.join(' · ') || null
}

function restSub(row: RestRow): string {
  return row.kind === 'ok' ? t('dashboard.health.okSub') : t(`dashboard.health.reason.${row.reason}`)
}

function topIssueUrl(h: ProjectHealth): string | null {
  return h.topIssues[0]?.htmlUrl ?? null
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.health.title')"
    :icon="Radar"
    :scope="t('dashboard.health.scope')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.health.seeAll')"
    @see-all="emit('openRadar')"
  >
    <template #answer>
      <span class="text-3xl font-semibold leading-none tabular-nums" :class="LEVEL_TEXT[card.risk.length ? 'bad' : 'none']">{{ card.risk.length }}</span>
      <span class="text-[13px] font-medium">{{ t('dashboard.health.answer', card.risk.length) }}</span>
      <span class="text-[13px] text-muted-foreground">
        · {{ t('dashboard.health.answerRest', { attention: card.attention.length, ok: card.ok.length }) }}<template v-if="card.unknown.length">
          · {{ t('dashboard.health.answerUnknown', { n: card.unknown.length }) }}</template>
      </span>
    </template>

    <p v-if="healths.length === 0" class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.health.noProjects') }}
    </p>
    <div v-else class="border-t border-border/50">
      <DashGroup
        v-if="riskyGroup"
        :label="t('dashboard.health.groups.risky')"
        :count="riskyGroup.total"
        :hidden="riskyGroup.hidden"
        @more="emit('openRadar')"
      >
        <DashRow
          v-for="h in riskyGroup.shown"
          :key="h.projectId"
          :title="h.projectName"
          :sub="riskySub(h)"
          :hint="t('dashboard.health.openRadarAt', { name: h.projectName })"
          @open="emit('openRadar', h.projectId)"
        >
          <template #lead>
            <AlertCircle
              v-if="h.reason === 'error' || h.reason === 'truncated'"
              class="h-3 w-3 shrink-0 text-warning"
              :stroke-width="2"
              :aria-label="grayReason(h)"
            />
          </template>
          <template #aside>
            <Badge :tone="LEVEL_BADGE[HEALTH_LEVEL[h.level]]" size="xs">{{ t(`dashboard.byProject.level.${h.level}`) }}</Badge>
            <Button
              v-if="topIssueUrl(h)"
              variant="outline"
              size="xs"
              @click="emit('openIssue', topIssueUrl(h)!)"
            >
              <ExternalLink class="h-3 w-3" :stroke-width="1.75" />{{ t('dashboard.health.openIssue') }}
            </Button>
            <Button variant="ghost" size="xs" @click="emit('openRadar', h.projectId)">{{ t('dashboard.health.radar') }}</Button>
          </template>
        </DashRow>
      </DashGroup>

      <DashGroup
        v-if="restGroup"
        :label="t('dashboard.health.groups.rest')"
        :hidden="restGroup.hidden"
        @more="emit('openRadar')"
      >
        <DashRow
          v-for="row in restGroup.shown"
          :key="row.reason ?? 'ok'"
          :title="names(row.projects)"
          :sub="restSub(row)"
          @open="emit('openRadar', row.projects.length === 1 ? row.projects[0].projectId : undefined)"
        >
          <template #aside>
            <Badge :tone="row.kind === 'ok' ? 'success' : 'neutral'" size="xs">
              {{ t(`dashboard.byProject.level.${row.kind === 'ok' ? 'green' : 'gray'}`) }}
            </Badge>
          </template>
        </DashRow>
      </DashGroup>
    </div>
  </DashboardWidget>
</template>
