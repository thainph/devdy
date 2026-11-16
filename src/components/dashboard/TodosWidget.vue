<script setup lang="ts">
// Work Dashboard — todos that need attention: overdue (red, "N days late") →
// due today (amber) → due within 3 days → in progress / waiting. Each row shows
// its status, project and priority; actions: open it in the Todos window,
// mark it done.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, ListTodo } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { WIDGET_ROWS, type DashboardTodo } from '@/lib/workDashboard'
import type { SourceState } from '@/composables/useWorkDashboard'
import DashboardWidget from './DashboardWidget.vue'

const props = defineProps<{
  todos: DashboardTodo[]
  state: SourceState
  now: number
  projectName: (id: string | null) => string | null
}>()

const emit = defineEmits<{ open: [id: string]; openAll: []; done: [id: string] }>()

const { t } = useI18n()

const shown = computed(() => props.todos.slice(0, WIDGET_ROWS))
const overdue = computed(() => props.todos.filter((x) => x.group === 'overdue').length)

const GROUP_CLASS = {
  overdue: 'text-destructive',
  today: 'text-warning',
  soon: 'text-info',
  active: 'text-muted-foreground',
} as const

function dueLabel(x: DashboardTodo): string {
  if (x.group === 'overdue') {
    const days = -(x.daysLeft ?? 0)
    return days > 0 ? t('dashboard.todos.lateDays', { n: days }, days) : t('dashboard.todos.lateToday')
  }
  if (x.group === 'today') return t('dashboard.todos.today')
  if (x.group === 'soon') return t('dashboard.todos.inDays', { n: x.daysLeft ?? 0 }, x.daysLeft ?? 0)
  return t(`dashboard.todos.status.${x.status}`)
}
</script>

<template>
  <DashboardWidget
    :title="t('dashboard.todos.title')"
    :icon="ListTodo"
    :count="overdue"
    :count-title="t('dashboard.todos.countTitle')"
    :updated-at="state.updatedAt"
    :now="now"
    :loading="state.loading"
    :see-all="t('dashboard.seeAll', { n: todos.length })"
    @see-all="emit('openAll')"
  >
    <Banner v-if="state.error" tone="destructive" class="m-3">{{ t('dashboard.loadError', { error: state.error }) }}</Banner>
    <p v-if="!state.error && shown.length === 0" class="px-4 py-6 text-center text-[11px] text-muted-foreground">
      {{ t('dashboard.todos.empty') }}
    </p>
    <ul v-else class="divide-y divide-border/40">
      <li
        v-for="x in shown"
        :key="x.item.todo.id"
        class="group flex items-center gap-2 px-3 py-2 hover:bg-accent/50"
      >
        <button type="button" class="min-w-0 flex-1 cursor-pointer text-left" @click="emit('open', x.item.todo.id)">
          <p class="truncate text-[13px] text-foreground">{{ x.item.todo.text }}</p>
          <div class="mt-0.5 flex min-w-0 flex-wrap items-center gap-x-1.5 text-[11px] text-muted-foreground">
            <span class="font-medium" :class="GROUP_CLASS[x.group]">{{ dueLabel(x) }}</span>
            <span v-if="x.group !== 'active' && x.status !== 'todo'">· {{ t(`dashboard.todos.status.${x.status}`) }}</span>
            <span v-if="x.item.todo.priority === 1" class="text-destructive">· {{ t('dashboard.todos.priorityHigh') }}</span>
            <span v-else-if="x.item.todo.priority === 3">· {{ t('dashboard.todos.priorityLow') }}</span>
            <span v-if="projectName(x.item.todo.project_id)">· {{ projectName(x.item.todo.project_id) }}</span>
          </div>
        </button>
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0 opacity-0 transition-opacity group-hover:opacity-100 focus-visible:opacity-100"
          :title="t('dashboard.todos.markDone')"
          :aria-label="t('dashboard.todos.markDone')"
          @click="emit('done', x.item.todo.id)"
        >
          <Check class="h-3.5 w-3.5" :stroke-width="1.75" />
        </Button>
      </li>
    </ul>
  </DashboardWidget>
</template>
