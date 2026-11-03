<script setup lang="ts">
/**
 * Bottom sheet of quick Focus actions for one todo on the phone: why it sits
 * where it does (the Host's reasons), then Done · Start / Back to to-do ·
 * Waiting on others · Edit; the AI (Start with AI, Analyze with its spinner
 * and Cancel); Snooze (presets, a picked time, or wake a snoozed one); Remind
 * me at (a picked time, or clear); Delete (two taps).
 *
 * Every todo action is one `todo_update` / `todo_toggle` / `todo_delete`; the
 * Host answers with the re-ranked board, so nothing is applied locally.
 */
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import {
  AlarmClockOff,
  Bell,
  BellOff,
  CalendarClock,
  Check,
  Clock,
  Hourglass,
  Loader2,
  Pencil,
  Play,
  Rocket,
  Sparkles,
  Trash2,
  Undo2,
  X,
} from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import { fromLocalInput, snoozePresets, toLocalInput, type FocusItem } from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import type { TodoPatchWire } from '../protocol'

const props = defineProps<{
  item: FocusItem
  /** Epoch ms. */
  now: number
  /** Start of the working day (snooze presets), `HH:MM`. */
  workStart?: string
  /** An analysis of this todo is running. */
  analyzing?: boolean
  /** An analysis finished this session (its plan is in the editor). */
  analyzed?: boolean
  /** The last AI failure for this todo, translated. */
  aiError?: string | null
}>()

const emit = defineEmits<{
  patch: [patch: TodoPatchWire]
  toggle: []
  edit: []
  analyze: []
  'cancel-analyze': []
  'start-ai': []
  remove: []
  close: []
}>()

const { t } = useI18n()
const ft = useFocusText()

const todo = computed(() => props.item.todo)
const snoozed = computed(() => props.item.bucket === 'snoozed')
const workStart = computed(() => props.workStart || '09:00')
const presets = computed(() => snoozePresets(props.now, workStart.value))

function presetLabel(preset: string): string {
  return t(`focus.snooze.${preset}`, { time: workStart.value })
}

// Picked times: open inline under their row.
const customOpen = ref(false)
const customAt = ref('')
const remindOpen = ref(false)
const remindAt = ref('')
const confirmingDelete = ref(false)
watch(
  () => todo.value.id,
  () => {
    customOpen.value = false
    remindOpen.value = false
    confirmingDelete.value = false
  },
)

function openCustom(): void {
  // Default: tomorrow at the start of the working day.
  const d = new Date(props.now)
  const [h, m] = workStart.value.split(':').map(Number)
  customAt.value = toLocalInput(new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1, h || 9, m || 0).toISOString())
  customOpen.value = !customOpen.value
}
const customIso = computed(() => fromLocalInput(customAt.value))

function openRemind(): void {
  remindAt.value = todo.value.remind_at ? toLocalInput(todo.value.remind_at) : toLocalInput(new Date(props.now + 3_600_000).toISOString())
  remindOpen.value = !remindOpen.value
}
const remindIso = computed(() => fromLocalInput(remindAt.value))

function onDelete(): void {
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  emit('remove')
}

const row =
  'flex w-full cursor-pointer items-center gap-2.5 px-3 py-3 border-b border-border/60 last:border-b-0 text-left text-sm font-medium'
const heading = 'mt-3 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
const card = 'ui-card overflow-hidden rounded-xl border border-border bg-card'
const inline = 'flex items-center gap-2 border-b border-border/60 px-3 py-2.5 last:border-b-0'
</script>

<template>
  <div class="absolute inset-0 z-20 flex flex-col justify-end">
    <div class="ui-backdrop absolute inset-0 bg-black/50" @click="emit('close')" />
    <div
      class="ctl-sheet relative flex max-h-[85%] flex-col rounded-t-2xl border-t border-border bg-card pt-2.5 pb-[max(1rem,env(safe-area-inset-bottom))] shadow-2xl"
    >
      <div class="mx-auto mb-2 h-1.5 w-10 shrink-0 rounded-full bg-foreground/20" />
      <p class="px-4 text-sm font-semibold break-words">{{ splitTodoText(todo.text).title }}</p>
      <p class="px-4 pt-1 text-[11px] leading-snug text-muted-foreground">
        {{ t('focus.whyHere') }}: {{ ft.whyText(item, now) }}
      </p>

      <div class="min-h-0 overflow-auto px-3 pb-1">
        <div class="mt-3" :class="card">
          <button type="button" :class="row" @click="emit('toggle')">
            <Check class="h-4 w-4 shrink-0 text-success" :stroke-width="2.5" />
            {{ t('focus.actions.done') }}
          </button>
          <button
            v-if="todo.status !== 'doing'"
            type="button"
            :class="row"
            @click="emit('patch', { status: 'doing', snoozed_until: null })"
          >
            <Play class="h-4 w-4 shrink-0 text-primary" :stroke-width="2.5" />
            {{ t('focus.actions.start') }}
          </button>
          <button v-if="todo.status !== 'todo'" type="button" :class="row" @click="emit('patch', { status: 'todo' })">
            <Undo2 class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('focus.actions.backToTodo') }}
          </button>
          <button
            v-if="todo.status !== 'waiting'"
            type="button"
            :class="row"
            @click="emit('patch', { status: 'waiting' })"
          >
            <Hourglass class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('focus.actions.waiting') }}
          </button>
          <button type="button" :class="row" @click="emit('edit')">
            <Pencil class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('focus.actions.edit') }}
          </button>
        </div>

        <h2 :class="heading">{{ t('controller.focus.ai.heading') }}</h2>
        <div :class="card">
          <button type="button" :class="row" @click="emit('start-ai')">
            <Rocket class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
            {{ t('focus.ai.start') }}
          </button>
          <div v-if="analyzing" :class="inline">
            <Loader2 class="h-4 w-4 shrink-0 animate-spin text-muted-foreground" :stroke-width="2" />
            <span class="min-w-0 flex-1 text-sm font-medium">{{ t('focus.ai.analyzing') }}</span>
            <Button variant="ghost" size="xs" @click="emit('cancel-analyze')">
              <X class="h-3 w-3" :stroke-width="2" />
              {{ t('focus.ai.cancel') }}
            </Button>
          </div>
          <template v-else>
            <button type="button" :class="row" @click="emit('analyze')">
              <Sparkles class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
              <span class="min-w-0 flex-1">
                {{ todo.ai_plan ? t('focus.ai.reanalyze') : t('controller.focus.ai.analyze') }}
              </span>
            </button>
            <button v-if="analyzed" type="button" :class="row" class="text-primary" @click="emit('edit')">
              <Check class="h-4 w-4 shrink-0" :stroke-width="2.5" />
              {{ t('controller.focus.ai.analyzedOpen') }}
            </button>
          </template>
          <p v-if="aiError && !analyzing" class="px-3 py-2 text-xs text-destructive">{{ aiError }}</p>
        </div>

        <h2 :class="heading">{{ t('focus.actions.snooze') }}</h2>
        <div :class="card">
          <button v-if="snoozed" type="button" :class="row" @click="emit('patch', { snoozed_until: null })">
            <AlarmClockOff class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('focus.snooze.wake') }}
          </button>
          <button
            v-for="p in presets"
            :key="p.preset"
            type="button"
            :class="row"
            @click="emit('patch', { snoozed_until: p.at })"
          >
            <Clock class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ presetLabel(p.preset) }}
          </button>
          <button type="button" :class="row" @click="openCustom">
            <CalendarClock class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('focus.snooze.custom') }}
          </button>
          <div v-if="customOpen" :class="inline">
            <Input v-model="customAt" type="datetime-local" size="md" class="min-w-0 flex-1" />
            <Button size="sm" :disabled="!customIso" @click="customIso && emit('patch', { snoozed_until: customIso })">
              {{ t('focus.snooze.apply') }}
            </Button>
          </div>
        </div>

        <h2 :class="heading">{{ t('focus.drawer.remindAt') }}</h2>
        <div :class="card">
          <button type="button" :class="row" @click="openRemind">
            <Bell class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            <span class="min-w-0 flex-1 truncate">
              {{ todo.remind_at ? ft.stamp(todo.remind_at) : t('controller.focus.remind.set') }}
            </span>
          </button>
          <div v-if="remindOpen" :class="inline">
            <Input v-model="remindAt" type="datetime-local" size="md" class="min-w-0 flex-1" />
            <Button size="sm" :disabled="!remindIso" @click="remindIso && emit('patch', { remind_at: remindIso })">
              {{ t('controller.focus.remind.apply') }}
            </Button>
          </div>
          <button v-if="todo.remind_at" type="button" :class="row" @click="emit('patch', { remind_at: null })">
            <BellOff class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('controller.focus.remind.clear') }}
          </button>
        </div>

        <div class="mt-3" :class="card">
          <button type="button" :class="row" class="text-destructive" @click="onDelete">
            <Trash2 class="h-4 w-4 shrink-0" :stroke-width="2" />
            {{ confirmingDelete ? t('controller.todo.sheet.confirmDelete') : t('focus.actions.delete') }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>
