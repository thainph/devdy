<script setup lang="ts">
/**
 * Bottom sheet of one todo on the phone's Focus screen, sized to fit without
 * scrolling:
 *  - the title, its facts and why it sits where it does (the Host's reasons);
 *  - four big tiles: Done · Start / Pause · Snooze · Waiting / Back to to-do;
 *  - priority and estimate, set with one tap (the sheet stays open);
 *  - AI: Start with AI, Analyze (spinner + Cancel);
 *  - Remind me · Edit · Delete (two taps).
 * Snooze and Remind open a second page of the sheet (presets / a picked time).
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
  Check,
  ChevronLeft,
  Clock,
  Hourglass,
  Loader2,
  Pause,
  Pencil,
  Play,
  Rocket,
  Sparkles,
  Trash2,
  Undo2,
  X,
} from 'lucide-vue-next'
import { Button, Input } from '@/components/ui'
import {
  formatEstimate,
  fromLocalInput,
  snoozePresets,
  toLocalInput,
  type FocusItem,
  type TodoPriority,
} from '@/lib/focus'
import { splitTodoText } from '@/lib/remoteTodos'
import { useFocusText } from '@/components/focus/useFocusText'
import type { TodoPatchWire } from '../protocol'

const props = defineProps<{
  item: FocusItem
  projectNames: Map<string, string>
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
  /** A change that ends the visit (status, snooze, reminder): the sheet closes. */
  patch: [patch: TodoPatchWire]
  /** A field set in place (priority, estimate): the sheet stays. */
  set: [patch: TodoPatchWire]
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
const due = computed(() => ft.due(props.item, props.now))
const project = computed(() => (todo.value.project_id ? props.projectNames.get(todo.value.project_id) : null))

type Page = 'main' | 'snooze' | 'remind'
const page = ref<Page>('main')
const customAt = ref('')
const remindAt = ref('')
const confirmingDelete = ref(false)
watch(
  () => todo.value.id,
  () => {
    page.value = 'main'
    confirmingDelete.value = false
  },
)

function openSnooze(): void {
  // The picked time starts at tomorrow, start of the working day.
  const d = new Date(props.now)
  const [h, m] = workStart.value.split(':').map(Number)
  customAt.value = toLocalInput(new Date(d.getFullYear(), d.getMonth(), d.getDate() + 1, h || 9, m || 0).toISOString())
  page.value = 'snooze'
}
const customIso = computed(() => fromLocalInput(customAt.value))
function presetLabel(preset: string): string {
  return t(`focus.snooze.${preset}`, { time: workStart.value })
}

function openRemind(): void {
  remindAt.value = todo.value.remind_at
    ? toLocalInput(todo.value.remind_at)
    : toLocalInput(new Date(props.now + 3_600_000).toISOString())
  page.value = 'remind'
}
const remindIso = computed(() => fromLocalInput(remindAt.value))

function onDelete(): void {
  if (!confirmingDelete.value) {
    confirmingDelete.value = true
    return
  }
  emit('remove')
}

const PRIORITIES: TodoPriority[] = [1, 2, 3]
const ESTIMATES = [15, 30, 60, 120, 240]

const tile =
  'flex flex-col items-center justify-center gap-1 rounded-xl border border-border bg-background/40 py-3 text-[13px] font-semibold cursor-pointer active:bg-accent'
const pill = 'flex-1 rounded-lg py-2 text-[13px] font-medium cursor-pointer transition-colors'
const pillOn = 'bg-primary/20 text-foreground ring-1 ring-primary/50'
const pillOff = 'bg-secondary text-muted-foreground active:bg-accent'
const label = 'mb-1.5 text-[11px] font-semibold uppercase tracking-wider text-muted-foreground'
const linkBtn =
  'flex flex-1 items-center justify-center gap-1.5 rounded-lg py-2.5 text-[13px] font-medium cursor-pointer active:bg-accent'
const listRow =
  'flex w-full items-center gap-2.5 rounded-xl border border-border bg-background/40 px-3 py-3 text-left text-sm font-medium cursor-pointer active:bg-accent'
</script>

<template>
  <div class="absolute inset-0 z-20 flex flex-col justify-end">
    <div class="ui-backdrop absolute inset-0 bg-black/50" @click="emit('close')" />
    <div
      class="ctl-sheet relative flex max-h-[92%] flex-col rounded-t-2xl border-t border-border bg-card pt-2.5 pb-[max(1rem,env(safe-area-inset-bottom))] shadow-2xl"
    >
      <div class="mx-auto mb-2 h-1.5 w-10 shrink-0 rounded-full bg-foreground/20" />

      <!-- Header: what it is, and why it sits here. -->
      <div class="flex items-start gap-2 pl-4 pr-10">
        <button
          v-if="page !== 'main'"
          type="button"
          class="-ml-2 grid h-8 w-8 shrink-0 place-items-center rounded-md cursor-pointer active:bg-accent"
          :aria-label="t('controller.data.back')"
          @click="page = 'main'"
        >
          <ChevronLeft class="h-5 w-5" :stroke-width="2" />
        </button>
        <div class="min-w-0 flex-1">
          <p class="text-base leading-snug font-semibold break-words">{{ splitTodoText(todo.text).title }}</p>
          <p class="mt-0.5 text-xs text-muted-foreground">
            <template v-if="due">
              <span :class="due.tone === 'danger' ? 'text-destructive' : due.tone === 'warn' ? 'text-warning' : ''">
                {{ due.text }}
              </span>
              ·
            </template>
            ~{{ formatEstimate(item.effective_estimate_min) }}<template v-if="item.estimate_missing">?</template>
            <template v-if="project"> · {{ project }}</template>
          </p>
        </div>
      </div>

      <div class="min-h-0 overflow-auto px-4 pb-1">
        <!-- Main page -->
        <template v-if="page === 'main'">
          <p class="mt-1.5 text-[11px] leading-snug text-muted-foreground/80">
            {{ t('focus.whyHere') }}: {{ ft.whyText(item, now) }}
          </p>

          <div class="mt-3 grid grid-cols-4 gap-2">
            <button type="button" :class="tile" class="text-success" @click="emit('toggle')">
              <Check class="h-5 w-5" :stroke-width="2.5" />
              {{ t('focus.actions.done') }}
            </button>
            <button
              v-if="todo.status !== 'doing'"
              type="button"
              :class="tile"
              class="text-primary"
              @click="emit('patch', { status: 'doing', snoozed_until: null })"
            >
              <Play class="h-5 w-5" :stroke-width="2.5" />
              {{ t('focus.actions.start') }}
            </button>
            <button v-else type="button" :class="tile" @click="emit('patch', { status: 'todo' })">
              <Pause class="h-5 w-5" :stroke-width="2.5" />
              {{ t('controller.focus.pause') }}
            </button>
            <button type="button" :class="tile" @click="openSnooze">
              <Clock class="h-5 w-5" :stroke-width="2" />
              {{ t('focus.actions.snooze') }}
            </button>
            <button
              v-if="todo.status !== 'waiting'"
              type="button"
              :class="tile"
              @click="emit('patch', { status: 'waiting' })"
            >
              <Hourglass class="h-5 w-5" :stroke-width="2" />
              {{ t('controller.focus.wait') }}
            </button>
            <button v-else type="button" :class="tile" @click="emit('patch', { status: 'todo' })">
              <Undo2 class="h-5 w-5" :stroke-width="2" />
              {{ t('controller.focus.unwait') }}
            </button>
          </div>

          <p :class="label" class="mt-4">{{ t('focus.priority.label') }}</p>
          <div class="flex gap-1.5">
            <button
              v-for="p in PRIORITIES"
              :key="p"
              type="button"
              :class="[pill, todo.priority === p ? pillOn : pillOff]"
              @click="todo.priority !== p && emit('set', { priority: p })"
            >
              P{{ p }}
            </button>
          </div>

          <p :class="label" class="mt-3">{{ t('focus.drawer.estimate') }}</p>
          <div class="flex gap-1.5">
            <button
              v-for="m in ESTIMATES"
              :key="m"
              type="button"
              :class="[pill, todo.estimate_min === m ? pillOn : pillOff]"
              @click="todo.estimate_min !== m && emit('set', { estimate_min: m })"
            >
              {{ formatEstimate(m) }}
            </button>
          </div>

          <p :class="label" class="mt-3">{{ t('controller.focus.ai.heading') }}</p>
          <div class="flex gap-2">
            <button type="button" :class="listRow" class="flex-1 justify-center" @click="emit('start-ai')">
              <Rocket class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
              {{ t('focus.ai.start') }}
            </button>
            <button
              v-if="analyzing"
              type="button"
              :class="listRow"
              class="flex-1 justify-center"
              @click="emit('cancel-analyze')"
            >
              <Loader2 class="h-4 w-4 shrink-0 animate-spin" :stroke-width="2" />
              {{ t('focus.ai.cancel') }}
            </button>
            <button
              v-else-if="analyzed"
              type="button"
              :class="listRow"
              class="flex-1 justify-center text-primary"
              @click="emit('edit')"
            >
              <Check class="h-4 w-4 shrink-0" :stroke-width="2.5" />
              {{ t('controller.focus.ai.viewAnalysis') }}
            </button>
            <button v-else type="button" :class="listRow" class="flex-1 justify-center" @click="emit('analyze')">
              <Sparkles class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
              {{ todo.ai_plan ? t('focus.ai.reanalyze') : t('focus.ai.analyze') }}
            </button>
          </div>
          <p v-if="aiError && !analyzing" class="mt-1.5 text-xs text-destructive">{{ aiError }}</p>

          <div class="mt-3 flex gap-1 border-t border-border/60 pt-2">
            <button type="button" :class="linkBtn" @click="openRemind">
              <Bell class="h-4 w-4" :stroke-width="2" />
              <span class="truncate">{{ todo.remind_at ? ft.stamp(todo.remind_at) : t('controller.focus.remind.short') }}</span>
            </button>
            <button type="button" :class="linkBtn" @click="emit('edit')">
              <Pencil class="h-4 w-4" :stroke-width="2" />
              {{ t('focus.actions.edit') }}
            </button>
            <button type="button" :class="linkBtn" class="text-destructive" @click="onDelete">
              <Trash2 class="h-4 w-4" :stroke-width="2" />
              {{ confirmingDelete ? t('controller.focus.confirmDelete') : t('focus.actions.delete') }}
            </button>
          </div>
        </template>

        <!-- Snooze page -->
        <template v-else-if="page === 'snooze'">
          <p :class="label" class="mt-3">{{ t('focus.actions.snooze') }}</p>
          <div class="flex flex-col gap-2">
            <button
              v-if="snoozed"
              type="button"
              :class="listRow"
              @click="emit('patch', { snoozed_until: null })"
            >
              <AlarmClockOff class="h-4 w-4 shrink-0 text-primary" :stroke-width="2" />
              {{ t('focus.snooze.wake') }}
            </button>
            <button
              v-for="p in presets"
              :key="p.preset"
              type="button"
              :class="listRow"
              @click="emit('patch', { snoozed_until: p.at })"
            >
              <Clock class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
              {{ presetLabel(p.preset) }}
            </button>
          </div>
          <p :class="label" class="mt-4">{{ t('focus.snooze.custom') }}</p>
          <div class="flex items-center gap-2">
            <Input v-model="customAt" type="datetime-local" size="md" class="min-w-0 flex-1" />
            <Button size="md" :disabled="!customIso" @click="customIso && emit('patch', { snoozed_until: customIso })">
              {{ t('focus.snooze.apply') }}
            </Button>
          </div>
        </template>

        <!-- Remind page -->
        <template v-else>
          <p :class="label" class="mt-3">{{ t('focus.drawer.remindAt') }}</p>
          <div class="flex items-center gap-2">
            <Input v-model="remindAt" type="datetime-local" size="md" class="min-w-0 flex-1" />
            <Button size="md" :disabled="!remindIso" @click="remindIso && emit('patch', { remind_at: remindIso })">
              {{ t('controller.focus.remind.apply') }}
            </Button>
          </div>
          <button
            v-if="todo.remind_at"
            type="button"
            :class="listRow"
            class="mt-3"
            @click="emit('patch', { remind_at: null })"
          >
            <BellOff class="h-4 w-4 shrink-0 text-muted-foreground" :stroke-width="2" />
            {{ t('controller.focus.remind.clear') }}
          </button>
        </template>
      </div>

      <button
        v-if="page === 'main'"
        type="button"
        class="absolute top-2 right-2 grid h-8 w-8 place-items-center rounded-md text-muted-foreground cursor-pointer active:bg-accent"
        :aria-label="t('controller.data.dismiss')"
        @click="emit('close')"
      >
        <X class="h-4 w-4" :stroke-width="2" />
      </button>
    </div>
  </div>
</template>
