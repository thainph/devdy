<script setup lang="ts">
// Work Dashboard — Todos: how many are overdue (and due today), then the
// groups "Overdue" (red, "N days late") → "Today" → "Coming up" (due within
// 3 days / in progress / waiting); what does not fit folds into "+N more".
// Every row: status, project, priority; Done / Open.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, ListTodo } from 'lucide-vue-next'
import { Badge, Banner, Button } from '@/components/ui'
import { WIDGET_ROWS, type DashboardTodo } from '@/lib/workDashboard'
import { fitGroups, todoCardGroups, todoLevel } from '@/lib/dashboardLayout'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'
import DashGroup from './DashGroup.vue'
import DashRow from './DashRow.vue'
import { LEVEL_TEXT } from './levels'

const props = defineProps<{
  todos: DashboardTodo[]
  state: SourceState
  now: number
  projectName: (id: string | null) => string | null
}>()

const emit = defineEmits<{ open: [id: string]; openAll: []; done: [id: string] }>()

const { t } = useI18n()

const groups = computed(() => fitGroups(todoCardGroups(props.todos), WIDGET_ROWS))
const overdue = computed(() => props.todos.filter((x) => x.group === 'overdue').length)
const today = computed(() => props.todos.filter((x) => x.group === 'today').length)
const level = computed(() => todoLevel(props.todos))

const TONE = { overdue: 'error', today: 'warning', soon: 'neutral', active: 'neutral' } as const

function dueLabel(x: DashboardTodo): string {
  if (x.group === 'overdue') {
    const days = -(x.daysLeft ?? 0)
    return days > 0 ? t('dashboard.todos.lateDays', { n: days }, days) : t('dashboard.todos.lateToday')
  }
  if (x.group === 'today') return x.item.todo.due_time ?? t('dashboard.todos.today')
  if (x.group === 'soon') return t('dashboard.todos.inDays', { n: x.daysLeft ?? 0 }, x.daysLeft ?? 0)
  return t(`dashboard.todos.status.${x.status}`)
}

function sub(x: DashboardTodo): string {
  const parts: string[] = []
  if (x.group !== 'active') parts.push(t(`dashboard.todos.status.${x.status}`))
  if (x.item.todo.priority === 1) parts.push(t('dashboard.todos.priorityHigh'))
  else if (x.item.todo.priority === 3) parts.push(t('dashboard.todos.priorityLow'))
  const project = props.projectName(x.item.todo.project_id)
  if (project) parts.push(project)
  return parts.join(' · ')
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.todos.title')"
    :icon="ListTodo"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.todos.seeAll')"
    @see-all="emit('openAll')"
  >
    <template #answer>
      <span class="text-3xl font-semibold leading-none tabular-nums" :class="LEVEL_TEXT[overdue ? 'bad' : 'none']">{{ overdue }}</span>
      <span class="text-[13px] font-medium">{{ t('dashboard.todos.answer') }}</span>
      <span class="text-[13px]" :class="today ? 'text-warning' : 'text-muted-foreground'">· {{ t('dashboard.todos.answerToday', { n: today }) }}</span>
    </template>

    <Banner v-if="state.error" tone="destructive" class="mx-4 my-2">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="!state.error && level === 'none'" class="border-t border-border/50 px-4 py-5 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.todos.empty') }}
    </p>
    <div v-else class="border-t border-border/50">
      <DashGroup
        v-for="g in groups"
        :key="g.key"
        :label="t(`dashboard.todos.groups.${g.key}`)"
        :count="g.total"
        :hidden="g.hidden"
        @more="emit('openAll')"
      >
        <DashRow
          v-for="x in g.shown"
          :key="x.item.todo.id"
          :title="x.item.todo.text"
          :sub="sub(x)"
          @open="emit('open', x.item.todo.id)"
        >
          <template #aside>
            <Badge :tone="TONE[x.group]" size="xs">{{ dueLabel(x) }}</Badge>
            <Button variant="outline" size="xs" @click="emit('done', x.item.todo.id)">
              <Check class="h-3 w-3" :stroke-width="2" />{{ t('dashboard.todos.markDone') }}
            </Button>
          </template>
        </DashRow>
      </DashGroup>
    </div>
  </DashboardWidget>
</template>
