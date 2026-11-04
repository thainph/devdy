<script setup lang="ts">
/**
 * Focus screen of the phone Controller (its own tab; the Todo tab stays the
 * plain list). Top to bottom:
 *  - Today: work needed vs free time on the Calendar as a bar (overload in
 *    red), the calibration hint, Calendar refresh and "Plan today";
 *  - the view switch — Time / Project / Week (remembered on this device);
 *  - the ranked list (`FocusBoardList`): "Do now" cards with Done / Start on
 *    them, then the buckets. Tapping a todo opens its action sheet.
 * Marking a started, estimated todo done asks how long it really took.
 *
 * Backend-agnostic like the other controller views: the board comes in as a
 * prop, every action goes out as an event. The project is the shared scope
 * picked in the header.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Gauge, Loader2, RefreshCw, Sparkles, X } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import {
  CAPACITY_LOADING,
  CAPACITY_NO_ACCOUNT,
  calibrationHint,
  type AiAnalysis,
  type FocusBoard,
  type FocusItem,
  type FocusTodo,
} from '@/lib/focus'
import { PLAN_KEY, shouldAskActual, todoAiKey } from '@/lib/remoteFocus'
import { useFocusText } from '@/components/focus/useFocusText'
import FabButton from './FabButton.vue'
import FocusActionSheet from './FocusActionSheet.vue'
import FocusActualSheet from './FocusActualSheet.vue'
import FocusBoardList, { type FocusBoardView } from './FocusBoardList.vue'
import { useControllerErrors } from '../focusErrors'
import type { ProjectInfo, TodoPatchWire } from '../protocol'

const props = defineProps<{
  /** null until the Host's first `focus_board`. */
  board: FocusBoard | null
  /** The board request failed. */
  failed?: boolean
  projects: ProjectInfo[]
  /** The shared project scope (null = every project). */
  projectId: string | null
  /** The last refused write, already translated. */
  error: string | null
  /** Running Focus AI requests by key (see `@/lib/remoteFocus`). */
  aiRunning?: Record<string, string>
  /** The last AI failure code per key. */
  aiErrors?: Record<string, string>
  /** Analyses finished this session, by todo id. */
  analyses?: Record<string, AiAnalysis>
  /** The Host's last "how long did it take?" suggestion. */
  actual?: { todoId: string; minutes: number | null } | null
  /** Start of the working day (`HH:MM`), for the snooze presets. */
  workStart?: string
}>()

const emit = defineEmits<{
  toggle: [id: string]
  patch: [id: string, patch: TodoPatchWire]
  remove: [id: string]
  analyze: [id: string]
  'cancel-analyze': [id: string]
  'start-ai': [todo: FocusTodo]
  /** Ask the Host for the BR-11 actual-time suggestion. */
  'suggest-actual': [id: string]
  'refresh-calendar': []
  /** Open the "Plan today" screen. */
  plan: []
  refresh: []
  /** Open the editor on this todo, or on a new one (null). */
  open: [todo: FocusTodo | null]
  'clear-error': []
}>()

const { t } = useI18n()
const ft = useFocusText()
const errors = useControllerErrors()

// ── clock & re-ranking ──
/** Epoch ms for the relative labels ("start before 14:00"). */
const now = ref(Date.now())
// Buckets and "Do now" move with the clock: re-rank every few minutes while
// the screen is open, and on coming back to it.
const RERANK_MS = 5 * 60_000
let lastRank = Date.now()
function rerank(): void {
  lastRank = Date.now()
  emit('refresh')
}
function onTick(): void {
  now.value = Date.now()
  if (props.board && document.visibilityState === 'visible' && Date.now() - lastRank >= RERANK_MS) rerank()
}
function onVisibility(): void {
  if (document.visibilityState !== 'visible') return
  now.value = Date.now()
  if (props.board) rerank()
}
let tick: ReturnType<typeof setInterval> | null = null
onMounted(() => {
  tick = setInterval(onTick, 60_000)
  document.addEventListener('visibilitychange', onVisibility)
  rerank()
})
onBeforeUnmount(() => {
  if (tick) clearInterval(tick)
  if (calendarTimer) clearTimeout(calendarTimer)
  document.removeEventListener('visibilitychange', onVisibility)
})

const projectNames = computed(() => new Map(props.projects.map((p) => [p.id, p.name])))
/** Open work in the scope. */
const items = computed<FocusItem[]>(() =>
  (props.board?.items ?? []).filter(
    (it) => it.bucket !== 'done' && (!props.projectId || it.todo.project_id === props.projectId),
  ),
)

// ── today ──
const capacity = computed(() => props.board?.capacity ?? null)
const overMin = computed(() => (capacity.value ? capacity.value.need_min - capacity.value.free_min : 0))
/** Share of the free time the work needs, for the bar (capped at 100 %). */
const loadPct = computed(() => {
  const c = capacity.value
  if (!c) return 0
  if (c.free_min <= 0) return c.need_min > 0 ? 100 : 0
  return Math.min(100, Math.round((c.need_min / c.free_min) * 100))
})
const capacityHint = computed(() => {
  const e = props.board?.capacity_error
  if (capacity.value || !e || e === CAPACITY_LOADING) return null
  return e === CAPACITY_NO_ACCOUNT ? t('focus.capacity.connect') : t('focus.capacity.error')
})
const canRefresh = computed(() => props.board?.capacity_error !== CAPACITY_NO_ACCOUNT)
const calibration = computed(() => calibrationHint(props.board?.calibration))

const calendarRefreshing = ref(false)
let calendarTimer: ReturnType<typeof setTimeout> | null = null
function onRefreshCalendar(): void {
  calendarRefreshing.value = true
  if (calendarTimer) clearTimeout(calendarTimer)
  calendarTimer = setTimeout(() => (calendarRefreshing.value = false), 15_000)
  emit('refresh-calendar')
}
watch(
  () => props.board,
  () => (calendarRefreshing.value = false),
)

const aiRunning = computed(() => props.aiRunning ?? {})
const planning = computed(() => !!aiRunning.value[PLAN_KEY])

// ── view ──
const VIEW_KEY = 'devdy.controller.focusView'
const VIEWS: FocusBoardView[] = ['time', 'project', 'week']
function readView(): FocusBoardView {
  try {
    return VIEWS.find((v) => v === localStorage.getItem(VIEW_KEY)) ?? 'time'
  } catch {
    return 'time'
  }
}
const view = ref<FocusBoardView>(readView())
watch(view, (v) => {
  try {
    localStorage.setItem(VIEW_KEY, v)
  } catch {
    // Storage unavailable: the view lasts for this visit only.
  }
})

// ── the action sheet, by id so it follows the re-ranked board ──
const actionsId = ref<string | null>(null)
const actionsItem = computed(() =>
  actionsId.value ? (props.board?.items.find((it) => it.todo.id === actionsId.value) ?? null) : null,
)
// Gone from the board (done / deleted elsewhere): close.
watch(actionsItem, (it) => {
  if (actionsId.value && !it) actionsId.value = null
})
const actionsKey = computed(() => (actionsId.value ? todoAiKey(actionsId.value) : ''))
function closeAfter(fn: (id: string) => void): void {
  const id = actionsId.value
  actionsId.value = null
  if (id) fn(id)
}
function onActionEdit(): void {
  const it = actionsItem.value
  actionsId.value = null
  if (it) emit('open', it.todo)
}
function onActionStartAi(): void {
  const it = actionsItem.value
  actionsId.value = null
  if (it) emit('start-ai', it.todo)
}

// ── done → "how long did it take?" for a started, estimated todo (BR-11) ──
const actualFor = ref<FocusTodo | null>(null)
const actualSuggestion = computed(() =>
  actualFor.value && props.actual?.todoId === actualFor.value.id ? props.actual.minutes : undefined,
)
function onToggle(id: string): void {
  const it = props.board?.items.find((x) => x.todo.id === id)
  if (it && shouldAskActual(it.todo)) {
    actualFor.value = { ...it.todo }
    emit('suggest-actual', id)
  }
  emit('toggle', id)
}
function onActualSave(minutes: number): void {
  if (actualFor.value) emit('patch', actualFor.value.id, { actual_min: minutes })
  actualFor.value = null
}

const label = 'text-[11px] font-semibold uppercase tracking-wider text-muted-foreground'
</script>

<template>
  <div class="relative flex h-full min-h-0 flex-col text-foreground">
    <Banner
      v-if="error"
      tone="destructive"
      class="shrink-0 gap-2 rounded-none border-x-0 border-t-0 px-3 py-2 text-xs"
    >
      {{ error }}
      <template #actions>
        <button
          class="shrink-0 cursor-pointer text-destructive"
          :title="t('controller.data.dismiss')"
          :aria-label="t('controller.data.dismiss')"
          @click="emit('clear-error')"
        >
          <X class="h-3.5 w-3.5" :stroke-width="2" />
        </button>
      </template>
    </Banner>

    <main class="flex-1 min-h-0 overflow-auto px-3 pt-3 pb-24">
      <div v-if="!board && failed" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button variant="outline" @click="emit('refresh')">{{ t('controller.data.retry') }}</Button>
      </div>
      <p v-else-if="!board" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>

      <template v-else>
        <!-- Today -->
        <section class="ui-card mb-3 rounded-xl border border-border bg-card px-3 pt-2.5 pb-3">
          <div class="flex items-center gap-2">
            <p :class="label" class="flex-1">{{ t('focus.buckets.today') }}</p>
            <button
              v-if="canRefresh"
              type="button"
              class="-my-1 -mr-1.5 grid h-8 w-8 shrink-0 place-items-center rounded-md text-muted-foreground cursor-pointer active:bg-accent disabled:opacity-50"
              :disabled="calendarRefreshing"
              :title="t('focus.capacity.refresh')"
              :aria-label="t('focus.capacity.refresh')"
              @click="onRefreshCalendar"
            >
              <RefreshCw class="h-4 w-4" :class="calendarRefreshing ? 'animate-spin' : ''" :stroke-width="2" />
            </button>
          </div>
          <template v-if="capacity">
            <p class="mt-1 text-sm">
              <span class="font-medium">
                {{ t('controller.focus.today.load', { need: ft.span(capacity.need_min), free: ft.span(Math.max(0, capacity.free_min)) }) }}
              </span>
              <span v-if="overMin > 0" class="font-semibold text-destructive">
                · {{ t('focus.capacity.overloaded', { over: ft.span(overMin) }) }}
              </span>
            </p>
            <div class="mt-2 h-1.5 overflow-hidden rounded-full bg-secondary">
              <div
                class="h-full rounded-full transition-all"
                :class="overMin > 0 ? 'bg-destructive' : loadPct > 80 ? 'bg-warning' : 'bg-success'"
                :style="{ width: `${loadPct}%` }"
              />
            </div>
          </template>
          <p v-else-if="capacityHint" class="mt-1 text-xs text-muted-foreground">{{ capacityHint }}</p>
          <p v-if="calibration" class="mt-2 flex items-center gap-1.5 text-[11px] text-muted-foreground">
            <Gauge class="h-3.5 w-3.5 shrink-0" :stroke-width="2" />
            {{
              calibration > 1
                ? t('focus.calibration.slower', { factor: calibration })
                : t('focus.calibration.faster', { factor: calibration })
            }}
          </p>
          <button
            type="button"
            class="mt-3 flex h-10 w-full items-center justify-center gap-2 rounded-lg bg-primary/15 text-[13px] font-semibold text-primary cursor-pointer active:bg-primary/25"
            @click="emit('plan')"
          >
            <Loader2 v-if="planning" class="h-4 w-4 animate-spin" :stroke-width="2" />
            <Sparkles v-else class="h-4 w-4" :stroke-width="2" />
            {{ planning ? t('focus.plan.planning') : t('focus.plan.button') }}
          </button>
        </section>

        <!-- View -->
        <div class="ui-segmented mb-3 flex gap-1 rounded-lg border border-border bg-card p-0.5">
          <button
            v-for="v in VIEWS"
            :key="v"
            class="flex-1 rounded-md px-2 py-1.5 text-xs transition-colors cursor-pointer"
            :class="view === v ? 'is-active bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
            @click="view = v"
          >
            {{ t(`controller.focus.views.${v}`) }}
          </button>
        </div>

        <p v-if="!items.length" class="py-10 text-center text-sm text-foreground/50">
          {{ t('controller.focus.empty') }}
        </p>
        <FocusBoardList
          v-else
          :board="board"
          :items="items"
          :project-names="projectNames"
          :now="now"
          :view="view"
          :ai-running="aiRunning"
          @toggle="onToggle"
          @actions="actionsId = $event.todo.id"
          @patch="(id, patch) => emit('patch', id, patch)"
        />
      </template>
    </main>

    <FabButton :label="t('controller.todo.add')" @click="emit('open', null)" />

    <FocusActionSheet
      v-if="actionsItem"
      :item="actionsItem"
      :project-names="projectNames"
      :now="now"
      :work-start="workStart"
      :analyzing="!!aiRunning[actionsKey]"
      :analyzed="!!analyses?.[actionsItem.todo.id]"
      :ai-error="errors.aiError(aiErrors?.[actionsKey])"
      @patch="(patch) => closeAfter((id) => emit('patch', id, patch))"
      @set="(patch) => actionsId && emit('patch', actionsId, patch)"
      @toggle="closeAfter(onToggle)"
      @edit="onActionEdit"
      @analyze="actionsId && emit('analyze', actionsId)"
      @cancel-analyze="actionsId && emit('cancel-analyze', actionsId)"
      @start-ai="onActionStartAi"
      @remove="closeAfter((id) => emit('remove', id))"
      @close="actionsId = null"
    />

    <FocusActualSheet
      v-if="actualFor"
      :todo="actualFor"
      :suggestion="actualSuggestion"
      @save="onActualSave"
      @close="actualFor = null"
    />
  </div>
</template>
