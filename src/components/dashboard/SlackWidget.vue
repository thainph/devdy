<script setup lang="ts">
// Work Dashboard — Customers (Slack): how many customers wait on the user,
// then "Waiting on you" (customer · project, their last message, "waiting X",
// Open thread / Create todo / Mark seen) and "No reply needed now" (waiting on
// the customer; threads with new messages but no customer marked fold into
// one line). Customers are marked by hand in the Slack threads window.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlarmClock, CheckCheck, Clock, Hourglass, ListPlus, MessageCircleMore, MessageCircleWarning } from 'lucide-vue-next'
import { Badge, Banner, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import { fitGroups, slackCard } from '@/lib/dashboardLayout'
import type { SlackDashboardRow } from '@/lib/dashboardSlack'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import DashGroup from './DashGroup.vue'
import DashRow from './DashRow.vue'
import { CATEGORY_ICON, LEVEL_TEXT } from './levels'
import { useDurationLabel } from './useDurationLabel'

const props = defineProps<{
  rows: SlackDashboardRow[]
  errorCount: number
  /** Threads tracked overall (before the project filter). */
  trackedCount: number
  hasCustomers: boolean
  state: SourceState
  now: number
  projectName: (id: string | null) => string | null
}>()

const emit = defineEmits<{
  open: [id: string]
  openAll: []
  addTodo: [row: SlackDashboardRow]
  markSeen: [id: string]
}>()

const { t } = useI18n()
const { duration } = useDurationLabel()

const card = computed(() => slackCard(props.rows))
// The folded "new messages" line takes one of the rows.
const groups = computed(() => fitGroups([
  { key: 'waiting' as const, items: card.value.waiting },
  { key: 'later' as const, items: card.value.later },
], WIDGET_ROWS - (card.value.unassignedNew ? 1 : 0)))
const waitingGroup = computed(() => groups.value.find((g) => g.key === 'waiting') ?? null)
const laterGroup = computed(() => groups.value.find((g) => g.key === 'later') ?? null)

const WAIT_TONE = { ok: 'neutral', warn: 'warning', late: 'error' } as const
// Status icon of a customer waiting on the user, by how long.
const WAIT_ICON_TONE = { ok: 'info', warn: 'warn', late: 'bad' } as const

function who(row: SlackDashboardRow): string {
  const name = row.customer?.displayName || row.thread.title || t('slackThread.untitled')
  const project = props.projectName(row.thread.projectId)
  return project ? `${name} · ${project}` : name
}

function quote(row: SlackDashboardRow): string {
  const channel = row.thread.channel ? `#${row.thread.channel} · ` : ''
  return row.preview ? `${channel}“${row.preview}”` : channel.replace(/ · $/, '')
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.slack.title')"
    :icon="CATEGORY_ICON.slack"
    :level="card.level"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :stale-after="Infinity"
    :see-all="t('dashboard.slack.seeAll')"
    @see-all="emit('openAll')"
  >
    <template #answer>
      <span class="text-3xl font-semibold leading-none tabular-nums" :class="LEVEL_TEXT[card.level]">{{ card.waiting.length }}</span>
      <span class="text-[13px] font-medium">{{ t('dashboard.slack.answer', card.waiting.length) }}</span>
    </template>

    <Banner v-if="state.error" tone="destructive" class="mx-4 my-2">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <Banner v-if="errorCount > 0" tone="warning" class="mx-4 my-2">{{ t('dashboard.slack.fetchErrors', { n: errorCount }, errorCount) }}</Banner>
    <Banner v-if="!hasCustomers && trackedCount > 0" tone="info" class="mx-4 my-2">{{ t('dashboard.slack.noCustomers') }}</Banner>

    <p
      v-if="!state.error && !groups.length && !card.unassignedNew"
      class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground"
    >
      {{ trackedCount === 0 ? t('dashboard.slack.emptyNone') : t('dashboard.slack.calm') }}
    </p>

    <div v-else class="border-t border-border/50">
      <DashGroup
        v-if="waitingGroup"
        :label="t('dashboard.slack.groups.waiting')"
        :count="waitingGroup.total"
        :hidden="waitingGroup.hidden"
        @more="emit('openAll')"
      >
        <DashRow
          v-for="row in waitingGroup.shown"
          :key="row.thread.id"
          :title="who(row)"
          :sub="quote(row)"
          :hint="row.thread.title"
          :icon="MessageCircleWarning"
          :tone="WAIT_ICON_TONE[row.waitLevel]"
          @open="emit('open', row.thread.id)"
        >
          <template #lead>
            <span v-if="row.unread" class="h-1.5 w-1.5 shrink-0 rounded-full bg-primary" :title="t('dashboard.slack.unread')" />
          </template>
          <template #aside>
            <Badge v-if="row.waitingMs !== null" :tone="WAIT_TONE[row.waitLevel]" size="xs">
              <component :is="row.waitLevel === 'late' ? AlarmClock : Clock" class="h-2.5 w-2.5" :stroke-width="2.5" aria-hidden="true" />
              {{ t('dashboard.slack.waiting', { duration: duration(row.waitingMs) }) }}
            </Badge>
            <Button variant="outline" size="xs" @click="emit('open', row.thread.id)">{{ t('dashboard.slack.open') }}</Button>
            <Button
              variant="ghost"
              size="icon-sm"
              :title="t('dashboard.slack.addTodo')"
              :aria-label="t('dashboard.slack.addTodo')"
              @click="emit('addTodo', row)"
            >
              <ListPlus class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
            <Button
              v-if="row.unread"
              variant="ghost"
              size="icon-sm"
              :title="t('dashboard.slack.markSeen')"
              :aria-label="t('dashboard.slack.markSeen')"
              @click="emit('markSeen', row.thread.id)"
            >
              <CheckCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
        </DashRow>
      </DashGroup>

      <DashGroup
        v-if="laterGroup || card.unassignedNew"
        :label="t('dashboard.slack.groups.later')"
        :hidden="laterGroup?.hidden ?? 0"
        @more="emit('openAll')"
      >
        <DashRow
          v-for="row in laterGroup?.shown ?? []"
          :key="row.thread.id"
          :title="who(row)"
          :sub="t('dashboard.slack.youRepliedLast')"
          :hint="row.thread.title"
          :icon="Hourglass"
          tone="none"
          @open="emit('open', row.thread.id)"
        >
          <template #aside>
            <Badge tone="neutral" size="xs">{{ t('dashboard.slack.waitingOnCustomer') }}</Badge>
            <Button
              v-if="row.unread"
              variant="ghost"
              size="icon-sm"
              :title="t('dashboard.slack.markSeen')"
              :aria-label="t('dashboard.slack.markSeen')"
              @click="emit('markSeen', row.thread.id)"
            >
              <CheckCheck class="h-3.5 w-3.5" :stroke-width="1.75" />
            </Button>
          </template>
        </DashRow>
        <DashRow
          v-if="card.unassignedNew"
          :title="t('dashboard.slack.unassignedNew', { n: card.unassignedNew }, card.unassignedNew)"
          :sub="t('dashboard.slack.unassignedHint')"
          :icon="MessageCircleMore"
          tone="info"
          @open="emit('openAll')"
        >
          <template #aside>
            <Button variant="ghost" size="xs" @click="emit('openAll')">{{ t('dashboard.slack.view') }}</Button>
          </template>
        </DashRow>
      </DashGroup>
    </div>
  </DashboardWidget>
</template>
