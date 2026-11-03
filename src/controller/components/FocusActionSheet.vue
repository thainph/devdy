<script setup lang="ts">
/**
 * Bottom sheet of quick Focus actions for one todo on the phone: why it sits
 * where it does (the Host's reasons), then Done · Start / Back to to-do ·
 * Waiting on others · Snooze (presets, or wake a snoozed one) · Edit.
 *
 * Every action is one `todo_update` / `todo_toggle`; the Host answers with the
 * re-ranked board, so nothing is applied locally.
 */
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlarmClockOff, Check, Clock, Hourglass, Pencil, Play, Undo2 } from 'lucide-vue-next'
import { snoozePresets, type FocusItem } from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import type { TodoPatchWire } from '../protocol'

const props = defineProps<{
  item: FocusItem
  /** Epoch ms. */
  now: number
}>()

const emit = defineEmits<{
  patch: [patch: TodoPatchWire]
  toggle: []
  edit: []
  close: []
}>()

const { t } = useI18n()
const ft = useFocusText()

const todo = computed(() => props.item.todo)
const snoozed = computed(() => props.item.bucket === 'snoozed')
const presets = computed(() => snoozePresets(props.now))

function presetLabel(preset: string): string {
  return t(`focus.snooze.${preset}`, { time: '09:00' })
}

const row =
  'flex w-full cursor-pointer items-center gap-2.5 px-3 py-3 border-b border-border/60 last:border-b-0 text-left text-sm font-medium'
const heading = 'mt-3 mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground'
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
        <div class="ui-card mt-3 overflow-hidden rounded-xl border border-border bg-card">
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

        <h2 :class="heading">{{ t('focus.actions.snooze') }}</h2>
        <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
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
        </div>
      </div>
    </div>
  </div>
</template>
