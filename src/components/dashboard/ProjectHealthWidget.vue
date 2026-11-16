<script setup lang="ts">
// Work Dashboard — Projects (Delivery Radar boards, `projectHealth`): how many
// are at risk of slipping, need attention, are on track; one row per red /
// amber project with its reasons in words (latest issue, milestone behind by
// X% — an estimate) and Open issue / Radar; the on-track ones fold into one
// line, those that cannot be judged (no board, GitLab, fetch error…) into
// another. Neutral wording: "at risk of slipping", never "late project".
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { ExternalLink, Radar } from 'lucide-vue-next'
import { Badge, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import { fitGroups, healthReasons, projectCard } from '@/lib/dashboardLayout'
import type { ProjectHealth } from '@/lib/projectHealth'
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
// The two folded lines (on track / not judged) take a row each.
const folded = computed(() => Number(card.value.ok.length > 0) + Number(card.value.unknown.length > 0))
const groups = computed(() => fitGroups([
  { key: 'risky' as const, items: [...card.value.risk, ...card.value.attention] },
], Math.max(1, WIDGET_ROWS - folded.value)))

function names(list: ProjectHealth[]): string {
  return list.map((h) => h.projectName).join(', ')
}

function unknownSub(list: ProjectHealth[]): string {
  return list.map((h) => `${h.projectName}: ${grayReason(h)}`).join(' · ')
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
        · {{ t('dashboard.health.answerRest', { attention: card.attention.length, ok: card.ok.length }) }}
      </span>
    </template>

    <p v-if="healths.length === 0" class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.health.noProjects') }}
    </p>
    <div v-else class="border-t border-border/50">
      <DashGroup
        v-for="g in groups"
        :key="g.key"
        :label="t('dashboard.health.groups.risky')"
        :count="g.total"
        :hidden="g.hidden"
        @more="emit('openRadar')"
      >
        <DashRow
          v-for="h in g.shown"
          :key="h.projectId"
          :title="h.projectName"
          :sub="healthReasons(h).map(reasonText).join(' · ') || null"
          :hint="t('dashboard.health.openRadarAt', { name: h.projectName })"
          @open="emit('openRadar', h.projectId)"
        >
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

      <DashGroup v-if="folded" :label="t('dashboard.health.groups.rest')">
        <DashRow
          v-if="card.ok.length"
          :title="names(card.ok)"
          :sub="t('dashboard.health.okSub')"
          @open="emit('openRadar')"
        >
          <template #aside>
            <Badge tone="success" size="xs">{{ t('dashboard.byProject.level.green') }}</Badge>
          </template>
        </DashRow>
        <DashRow
          v-if="card.unknown.length"
          :title="names(card.unknown)"
          :sub="unknownSub(card.unknown)"
          @open="emit('openRadar')"
        >
          <template #aside>
            <Badge tone="neutral" size="xs">{{ t('dashboard.byProject.level.gray') }}</Badge>
          </template>
        </DashRow>
      </DashGroup>
    </div>
  </DashboardWidget>
</template>
