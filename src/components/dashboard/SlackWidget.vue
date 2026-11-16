<script setup lang="ts">
// Work Dashboard — Slack: tracked threads that need the user (a customer is
// waiting on them, or new messages), with the customer, the wait, and the
// customer's latest message (Vietnamese when translated). Row actions: open
// the thread, turn it into a todo, mark it seen. Customers are marked by hand
// in the Slack threads window (Settings lists them).
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { CheckCheck, Clock, ListPlus, MessageSquare, UserCheck } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { WIDGET_ROWS } from '@/lib/workDashboard'
import type { SlackDashboardRow } from '@/lib/dashboardSlack'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import { useDurationLabel } from './useDurationLabel'

const props = defineProps<{
  rows: SlackDashboardRow[]
  attentionCount: number
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

const shown = computed(() => props.rows.slice(0, WIDGET_ROWS))

function timeOf(row: SlackDashboardRow): string | null {
  if (row.label === 'waitingOnMe' && row.waitingMs !== null) return t('dashboard.slack.waiting', { duration: duration(row.waitingMs) })
  const at = row.thread.lastReplyAt ? Date.parse(row.thread.lastReplyAt) : Number.NaN
  return Number.isNaN(at) ? null : t('dashboard.ago', { duration: duration(Math.max(0, props.now - at)) })
}

const WAIT_CLASS = { ok: 'text-info', warn: 'text-warning', late: 'text-destructive' } as const
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.slack.title')"
    :icon="MessageSquare"
    :count="attentionCount"
    :count-title="t('dashboard.slack.countTitle')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :stale-after="Infinity"
    :see-all="t('dashboard.seeAll', { n: rows.length })"
    @see-all="emit('openAll')"
  >
    <Banner v-if="state.error" tone="destructive" class="m-3">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <Banner v-if="errorCount > 0" tone="warning" class="m-3">{{ t('dashboard.slack.fetchErrors', { n: errorCount }, errorCount) }}</Banner>
    <Banner v-if="!hasCustomers && trackedCount > 0" tone="info" class="m-3">{{ t('dashboard.slack.noCustomers') }}</Banner>

    <p v-if="!state.error && shown.length === 0" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
      {{ trackedCount === 0 ? t('dashboard.slack.emptyNone') : t('dashboard.slack.empty') }}
    </p>

    <ul v-else class="divide-y divide-border/40">
      <li
        v-for="row in shown"
        :key="row.thread.id"
        class="group flex items-start gap-2 px-3 py-2 hover:bg-accent/50"
      >
        <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" @click="emit('open', row.thread.id)">
          <div class="flex min-w-0 items-center gap-1.5">
            <span
              v-if="row.unread"
              class="h-1.5 w-1.5 shrink-0 rounded-full bg-primary"
              :title="t('dashboard.slack.unread')"
            />
            <span class="truncate text-[13px] font-medium text-foreground">{{ row.thread.title || t('slackThread.untitled') }}</span>
          </div>
          <div class="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
            <span
              v-if="row.label === 'waitingOnMe'"
              class="inline-flex items-center gap-0.5 font-medium"
              :class="WAIT_CLASS[row.waitLevel]"
            >
              <Clock class="h-3 w-3" :stroke-width="2" />{{ t('dashboard.slack.waitingOnMe') }}
            </span>
            <span v-else-if="row.label === 'waitingOnCustomer'">{{ t('dashboard.slack.waitingOnCustomer') }}</span>
            <span v-else-if="row.unread" class="font-medium text-primary">{{ t('dashboard.slack.newMessages') }}</span>
            <span v-if="row.customer" class="inline-flex items-center gap-0.5">
              <UserCheck class="h-3 w-3" :stroke-width="1.75" />{{ row.customer.displayName }}
            </span>
            <span v-if="row.thread.channel">#{{ row.thread.channel }}</span>
            <span v-if="projectName(row.thread.projectId)">· {{ projectName(row.thread.projectId) }}</span>
            <span v-if="timeOf(row)">· {{ timeOf(row) }}</span>
          </div>
          <p v-if="row.preview" class="mt-0.5 line-clamp-1 text-[11px] text-foreground/80">{{ row.preview }}</p>
        </button>
        <div class="flex shrink-0 items-center gap-0.5 opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100">
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
        </div>
      </li>
    </ul>
  </DashboardWidget>
</template>
