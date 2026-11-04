<script setup lang="ts">
/**
 * One ranked todo on the phone's Focus screen: a done circle (a full-height
 * hit area), the title with its status icon and one line of facts — due
 * (coloured by urgency) · estimate · project. Tapping the row opens the todo's
 * action sheet. Shared by the time buckets, the project and the week views.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Hourglass, Loader2, Play } from 'lucide-vue-next'
import { formatEstimate, type FocusItem } from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'

const props = defineProps<{
  item: FocusItem
  projectNames: Map<string, string>
  /** Epoch ms, ticking, for the relative labels. */
  now: number
  /** Leave the project out (the project view's heading names it). */
  hideProject?: boolean
  /** An AI analysis of this todo is running. */
  analyzing?: boolean
}>()

const emit = defineEmits<{
  toggle: [id: string]
  actions: [item: FocusItem]
}>()

const { t } = useI18n()
const ft = useFocusText()

const todo = computed(() => props.item.todo)
const due = computed(() =>
  props.item.bucket === 'snoozed' && todo.value.snoozed_until
    ? { text: t('focus.snooze.until', { time: ft.stamp(todo.value.snoozed_until) }), tone: 'muted' as const }
    : ft.due(props.item, props.now),
)
const project = computed(() =>
  props.hideProject || !todo.value.project_id ? null : (props.projectNames.get(todo.value.project_id) ?? null),
)
const estimate = computed(
  () => `~${formatEstimate(props.item.effective_estimate_min)}${props.item.estimate_missing ? '?' : ''}`,
)

const TONE: Record<string, string> = {
  danger: 'text-destructive font-medium',
  warn: 'text-warning font-medium',
  muted: '',
}
</script>

<template>
  <div
    class="flex items-stretch border-b border-border/60 last:border-b-0 cursor-pointer active:bg-accent/60"
    @click="emit('actions', item)"
  >
    <button
      type="button"
      class="grid w-12 shrink-0 place-items-center cursor-pointer"
      :title="t('controller.todo.markDone')"
      :aria-label="t('controller.todo.markDone')"
      @click.stop="emit('toggle', todo.id)"
    >
      <span class="h-[22px] w-[22px] rounded-full border-2 border-foreground/30 transition-colors active:border-success" />
    </button>
    <div class="min-w-0 flex-1 py-2.5 pr-3">
      <p class="text-[15px] leading-snug font-medium break-words">
        <span
          v-if="todo.priority === 1"
          class="mr-1 rounded bg-destructive/15 px-1 align-[1px] text-[10px] font-bold text-destructive"
        >P1</span>
        <Play v-if="todo.status === 'doing'" class="mr-0.5 inline h-3.5 w-3.5 text-primary" :stroke-width="2.5" />
        <Hourglass
          v-else-if="todo.status === 'waiting'"
          class="mr-0.5 inline h-3.5 w-3.5 text-muted-foreground"
          :stroke-width="2.5"
        />
        {{ splitTodoText(todo.text).title }}
      </p>
      <p class="mt-0.5 flex flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
        <span v-if="due" :class="TONE[due.tone]">{{ due.text }}</span>
        <span v-if="due" aria-hidden="true">·</span>
        <span class="tabular-nums">{{ estimate }}</span>
        <template v-if="project">
          <span aria-hidden="true">·</span>
          <span class="truncate">{{ project }}</span>
        </template>
        <span v-if="analyzing" class="inline-flex items-center gap-1 text-primary">
          <Loader2 class="h-3 w-3 animate-spin" :stroke-width="2.5" />
          {{ t('focus.ai.analyzing') }}
        </span>
      </p>
    </div>
  </div>
</template>
