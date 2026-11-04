<script setup lang="ts">
/**
 * One ranked todo on the phone's Focus board: done circle, title with its
 * status icon, chips (P1, due, snoozed, estimate, project) and `⋯` for the
 * quick actions. Shared by the time buckets, the project and the week views.
 */
import { useI18n } from 'vue-i18n'
import { Hourglass, Loader2, MoreHorizontal, Play } from 'lucide-vue-next'
import { formatEstimate, type FocusItem, type FocusTodo } from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'

defineProps<{
  item: FocusItem
  projectNames: Map<string, string>
  /** Epoch ms, ticking, for the relative labels. */
  now: number
  /** Leave the project chip out (the project view's heading names it). */
  hideProject?: boolean
  /** An AI analysis of this todo is running. */
  analyzing?: boolean
}>()

const emit = defineEmits<{
  toggle: [id: string]
  open: [todo: FocusTodo]
  actions: [item: FocusItem]
}>()

const { t } = useI18n()
const ft = useFocusText()

const TONES: Record<string, string> = {
  danger: 'border-destructive/40 bg-destructive/10 text-destructive',
  warn: 'border-warning/40 bg-warning/10 text-warning',
  muted: 'border-border bg-secondary text-muted-foreground',
}
const chip = 'rounded-full border px-1.5 py-px text-[10px]'
</script>

<template>
  <div
    class="flex items-start gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 cursor-pointer hover:bg-accent/50"
    @click="emit('open', item.todo)"
  >
    <button
      class="mt-0.5 h-5 w-5 shrink-0 rounded-full border-2 border-foreground/30 transition-colors hover:border-success cursor-pointer"
      :title="t('controller.todo.markDone')"
      :aria-label="t('controller.todo.markDone')"
      @click.stop="emit('toggle', item.todo.id)"
    />
    <div class="min-w-0 flex-1">
      <p class="text-sm font-medium break-words">
        <Play v-if="item.todo.status === 'doing'" class="mr-0.5 inline h-3 w-3 text-primary" :stroke-width="2.5" />
        <Hourglass
          v-else-if="item.todo.status === 'waiting'"
          class="mr-0.5 inline h-3 w-3 text-muted-foreground"
          :stroke-width="2.5"
        />
        {{ splitTodoText(item.todo.text).title }}
      </p>
      <div class="mt-1 flex flex-wrap gap-1">
        <span v-if="item.todo.priority === 1" :class="[chip, TONES.danger]" class="font-semibold">P1</span>
        <span v-if="ft.due(item, now)" :class="[chip, TONES[ft.due(item, now)!.tone]]">
          {{ ft.due(item, now)!.text }}
        </span>
        <span v-if="item.bucket === 'snoozed' && item.todo.snoozed_until" :class="[chip, TONES.muted]">
          {{ t('focus.snooze.until', { time: ft.stamp(item.todo.snoozed_until) }) }}
        </span>
        <span :class="[chip, TONES.muted]">
          ~{{ formatEstimate(item.effective_estimate_min) }}<template v-if="item.estimate_missing">?</template>
        </span>
        <span
          v-if="!hideProject && item.todo.project_id && projectNames.get(item.todo.project_id)"
          :class="[chip, TONES.muted]"
        >
          {{ projectNames.get(item.todo.project_id) }}
        </span>
        <span v-if="analyzing" :class="[chip, TONES.muted]" class="inline-flex items-center gap-1">
          <Loader2 class="h-2.5 w-2.5 animate-spin" :stroke-width="2.5" />
          {{ t('focus.ai.analyzing') }}
        </span>
      </div>
    </div>
    <button
      class="-mr-1 grid h-7 w-7 shrink-0 place-items-center rounded-md text-muted-foreground cursor-pointer hover:bg-accent"
      :aria-label="t('focus.whyHere')"
      @click.stop="emit('actions', item)"
    >
      <MoreHorizontal class="h-4 w-4" :stroke-width="2" />
    </button>
  </div>
</template>
