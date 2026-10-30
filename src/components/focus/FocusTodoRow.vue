<script setup lang="ts">
// One todo on the Focus board (spec §3.2): ✓ checkbox · project chip · title ·
// relative due / start-by label · `~2h` estimate ("?" when missing) · P1 badge ·
// source icon (opens the link) · linked sessions; hovering shows the actions
// (▶ start / back to to-do, ⏸ snooze, ⏳ waiting, edit, delete). The tooltip
// explains why it ranks here (BR-05). Click → detail drawer.
//
// `row` is the full-width line of the "by time" view; `card` the narrow
// two-line form of the project / week columns.
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Hourglass, Link as LinkIcon, Pencil, Play, Trash2, Undo2 } from 'lucide-vue-next'
import { Button } from '@/components/ui'
import { useProjectsStore } from '@/stores/projects'
import { firstLine } from '@/lib/itemText'
import { formatEstimate, projectHue, sourceShortLabel, type FocusItem } from '@/lib/focus'
import FocusSnoozeMenu from './FocusSnoozeMenu.vue'
import { SOURCE_ICON } from './sourceIcon'
import { useFocusActions } from './useFocusActions'
import { useFocusText } from './useFocusText'

const props = withDefaults(
  defineProps<{
    item: FocusItem
    now: number
    layout?: 'row' | 'card'
    /** Rank shown in "Làm ngay". */
    rank?: number
    /** Hide the project chip (the column already says it). */
    hideProject?: boolean
  }>(),
  { layout: 'row', rank: undefined, hideProject: false },
)

const emit = defineEmits<{ open: [id: string] }>()

const { t } = useI18n()
const projects = useProjectsStore()
const actions = useFocusActions()
const text = useFocusText()

const todo = computed(() => props.item.todo)
const title = computed(() => firstLine(todo.value.text) || todo.value.text)
const project = computed(() =>
  todo.value.project_id ? projects.projects.find((p) => p.id === todo.value.project_id) ?? null : null,
)
const chipStyle = computed(() => {
  const h = projectHue(todo.value.project_id ?? '')
  return { backgroundColor: `hsl(${h} 70% 50% / 0.14)`, color: `hsl(${h} 60% 42%)`, borderColor: `hsl(${h} 70% 50% / 0.3)` }
})
const due = computed(() => text.due(props.item, props.now))
const why = computed(() => {
  const reasons = text.whyText(props.item)
  return reasons ? `${t('focus.whyHere')}: ${reasons}` : title.value
})
const estimate = computed(() => formatEstimate(props.item.effective_estimate_min))
const estimateTitle = computed(() =>
  props.item.estimate_missing
    ? t('focus.estimate.missing', { estimate: estimate.value })
    : todo.value.estimate_source === 'ai'
      ? t('focus.estimate.ai')
      : undefined,
)
const sourceLabel = computed(() => sourceShortLabel(todo.value.source_type, todo.value.source_ref, todo.value.source_url))
const sourceTitle = computed(() =>
  `${t('focus.actions.openSource')}${todo.value.source_type ? ` · ${t(`focus.sources.${todo.value.source_type}`)}` : ''}`,
)
const isDone = computed(() => todo.value.done)
const doing = computed(() => todo.value.status === 'doing')

const DUE_TONE = {
  danger: 'text-red-600 dark:text-red-400 font-medium',
  warn: 'text-amber-600 dark:text-amber-400 font-medium',
  muted: 'text-muted-foreground',
}
</script>

<template>
  <div
    class="group relative flex cursor-pointer rounded-md transition-colors hover:bg-accent/50"
    :class="[
      layout === 'row' ? 'items-center gap-2 px-2 py-1.5' : 'flex-col gap-1 border border-border/60 bg-card px-2.5 py-2',
      doing && layout === 'card' ? 'border-l-2 border-l-primary' : '',
    ]"
    :title="why"
    role="button"
    tabindex="0"
    @click="emit('open', todo.id)"
    @keydown.enter.self="emit('open', todo.id)"
  >
    <div class="flex min-w-0 items-center gap-2" :class="layout === 'row' ? 'flex-1' : ''">
      <span v-if="rank" class="w-4 shrink-0 text-center text-xs font-semibold text-muted-foreground">{{ rank }}</span>
      <!-- ✓ / undo -->
      <button
        type="button"
        class="flex h-4 w-4 shrink-0 cursor-pointer items-center justify-center rounded border transition-colors"
        :class="isDone ? 'border-primary bg-primary text-primary-foreground' : 'border-muted-foreground/40 hover:border-primary'"
        :title="isDone ? t('focus.actions.undone') : t('focus.actions.done')"
        :aria-label="isDone ? t('focus.actions.undone') : t('focus.actions.done')"
        :aria-pressed="isDone"
        @click.stop="isDone ? actions.undone(todo) : actions.done(todo)"
      >
        <Check v-if="isDone" class="h-3 w-3" :stroke-width="2.5" />
      </button>
      <span
        v-if="!hideProject && project"
        class="max-w-28 shrink-0 truncate rounded border px-1.5 py-px text-[10px] font-medium"
        :style="chipStyle"
      >{{ project.name }}</span>
      <Play v-if="doing && layout === 'row'" class="h-3 w-3 shrink-0 text-primary" :stroke-width="2" :aria-label="t('focus.status.doing')" />
      <span
        class="min-w-0 truncate text-[13px]"
        :class="[isDone ? 'text-muted-foreground line-through' : '', layout === 'row' ? 'flex-1' : 'flex-1 font-medium']"
      >{{ title }}</span>
    </div>

    <!-- Meta: due · estimate · P1 · source · sessions -->
    <div class="flex shrink-0 items-center gap-2 text-[11px]" :class="layout === 'card' ? 'flex-wrap pl-6' : ''">
      <span v-if="due" class="whitespace-nowrap" :class="DUE_TONE[due.tone]">{{ due.text }}</span>
      <span
        class="whitespace-nowrap tabular-nums text-muted-foreground"
        :class="item.estimate_missing ? 'opacity-70' : ''"
        :title="estimateTitle"
      >~{{ estimate }}<span v-if="item.estimate_missing" class="text-amber-600 dark:text-amber-400">?</span></span>
      <span v-if="todo.priority === 1" class="rounded bg-red-600 px-1 py-px text-[9px] font-bold text-white">P1</span>
      <button
        v-if="todo.source_url"
        type="button"
        class="inline-flex cursor-pointer items-center gap-0.5 rounded px-1 text-muted-foreground transition-colors hover:bg-accent hover:text-foreground"
        :title="sourceTitle"
        :aria-label="sourceTitle"
        @click.stop="actions.openSource(todo)"
      >
        <component :is="SOURCE_ICON[todo.source_type ?? 'url']" class="h-3 w-3" :stroke-width="1.75" />
        <span v-if="sourceLabel" class="font-mono text-[10px]">{{ sourceLabel }}</span>
      </button>
      <span
        v-if="todo.linked_run_count > 0"
        class="inline-flex items-center gap-0.5 text-muted-foreground"
        :title="t('focus.actions.sessions', { n: todo.linked_run_count }, todo.linked_run_count)"
      >
        <LinkIcon class="h-3 w-3" :stroke-width="1.75" />{{ todo.linked_run_count }}
      </span>
    </div>

    <!-- Hover actions -->
    <div
      v-if="!isDone"
      class="flex shrink-0 items-center opacity-0 transition-opacity group-hover:opacity-100 focus-within:opacity-100"
      :class="layout === 'card' ? 'absolute right-1 top-1 rounded-md bg-popover shadow-sm' : ''"
      @click.stop
    >
      <Button
        v-if="!doing"
        variant="ghost"
        size="icon-sm"
        :title="t('focus.actions.start')"
        :aria-label="t('focus.actions.start')"
        @click="actions.start(todo)"
      >
        <Play class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        v-else
        variant="ghost"
        size="icon-sm"
        :title="t('focus.actions.backToTodo')"
        :aria-label="t('focus.actions.backToTodo')"
        @click="actions.backToTodo(todo)"
      >
        <Undo2 class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <FocusSnoozeMenu :todo="todo" />
      <Button
        v-if="todo.status !== 'waiting'"
        variant="ghost"
        size="icon-sm"
        :title="t('focus.actions.waiting')"
        :aria-label="t('focus.actions.waiting')"
        @click="actions.waiting(todo)"
      >
        <Hourglass class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        variant="ghost"
        size="icon-sm"
        :title="t('focus.actions.edit')"
        :aria-label="t('focus.actions.edit')"
        @click="emit('open', todo.id)"
      >
        <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
      <Button
        variant="destructive-ghost"
        size="icon-sm"
        :title="t('focus.actions.delete')"
        :aria-label="t('focus.actions.delete')"
        @click="actions.remove(todo)"
      >
        <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
      </Button>
    </div>
  </div>
</template>
