<script setup lang="ts">
/**
 * Todo screen of the phone Controller (spec §2.B1): a project filter, a
 * search, an Active / Done switch, open todos grouped Overdue / Today /
 * Upcoming / No due date and a circle that toggles done. Tapping a row (or the
 * floating +) asks the parent to open the full-height todo editor.
 *
 * When the Host ranks todos (Focus board) the Active list is the board
 * instead — by time, by project or this week — with today's load, a "Plan
 * today" entry and a sheet of quick actions per row (start, wait, snooze,
 * remind, AI, delete). Marking a started, estimated todo done asks how long
 * it really took.
 *
 * Backend-agnostic like the other controller views: the list comes in as a
 * prop; `toggle` / `refresh` / `open` go out as events. The filter, search and
 * Active / Done switch are models owned by the parent, so they survive a trip
 * to the editor and back.
 */
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Loader2, Sparkles, X } from 'lucide-vue-next'
import { Banner, Button } from '@/components/ui'
import { localYmd } from '@/lib/todoDue'
import {
  countTodos,
  doneTodos,
  filterByProject,
  groupOpenTodos,
  searchTodos,
  splitTodoText,
  todoGroup,
  type TodoTab,
} from '@/lib/remoteTodos'
import type { AiAnalysis, FocusBoard, FocusItem, FocusTodo } from '@/lib/focus'
import { PLAN_KEY, shouldAskActual, todoAiKey } from '@/lib/remoteFocus'
import FabButton from './FabButton.vue'
import FocusActionSheet from './FocusActionSheet.vue'
import FocusActualSheet from './FocusActualSheet.vue'
import FocusBoardList, { type FocusBoardView } from './FocusBoardList.vue'
import { useControllerErrors } from '../focusErrors'
import SearchField from './SearchField.vue'
import type { ProjectInfo, TodoInfo, TodoPatchWire } from '../protocol'

const props = defineProps<{
  /** null until the Host's first `todo_list`. */
  todos: TodoInfo[] | null
  projects: ProjectInfo[]
  /** The last refused write, already translated. */
  error: string | null
  /** The list request failed (refused / no answer). */
  failed?: boolean
  /** The Host ranks todos (Focus board). */
  focus?: boolean
  /** The Host's Focus board; null while loading. */
  board?: FocusBoard | null
  /** The board request failed: fall back to the plain list. */
  boardFailed?: boolean
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
  /** A quick Focus action on one todo. */
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
  open: [todo: TodoInfo | null]
  'clear-error': []
}>()

const { t, locale } = useI18n()
const errors = useControllerErrors()

/** Project filter ('' = every project), picked in the tab header. */
const projectFilter = defineModel<string>('projectId', { required: true })
const tab = defineModel<TodoTab>('tab', { required: true })
const query = defineModel<string>('query', { required: true })
// The phone's local calendar date. Re-read every minute and whenever the app
// comes back to the foreground, so a screen left open past midnight (or a
// phone woken the next morning) regroups Today / Overdue on its own.
const today = ref(localYmd())
/** Epoch ms for the board's relative labels ("start before 14:00"). */
const now = ref(Date.now())
function refreshToday(): void {
  now.value = Date.now()
  const ymd = localYmd()
  if (ymd !== today.value) today.value = ymd
}
function onVisibility(): void {
  if (document.visibilityState !== 'visible') return
  refreshToday()
  // The board is ranked at one instant and may have changed on the desktop
  // meanwhile: re-rank on coming back.
  if (props.board) {
    lastRank = Date.now()
    emit('refresh')
  }
}
let tick: ReturnType<typeof setInterval> | null = null

// Buckets and "Do now" move with the clock and with desktop edits the Host
// does not push: re-rank every few minutes while the screen is open.
const RERANK_MS = 5 * 60_000
let lastRank = Date.now()
function onTick(): void {
  refreshToday()
  if (props.board && document.visibilityState === 'visible' && Date.now() - lastRank >= RERANK_MS) {
    lastRank = Date.now()
    emit('refresh')
  }
}

onMounted(() => {
  refreshToday()
  tick = setInterval(onTick, 60_000)
  document.addEventListener('visibilitychange', onVisibility)
  emit('refresh')
})
onBeforeUnmount(() => {
  if (tick) clearInterval(tick)
  if (calendarTimer) clearTimeout(calendarTimer)
  document.removeEventListener('visibilitychange', onVisibility)
})

const projectNames = computed(() => new Map(props.projects.map((p) => [p.id, p.name])))

const searching = computed(() => query.value.trim().length > 0)
const scoped = computed(() =>
  searchTodos(filterByProject(props.todos ?? [], projectFilter.value || null), query.value),
)
const counts = computed(() => countTodos(scoped.value))
const groups = computed(() => groupOpenTodos(scoped.value, today.value))
const finished = computed(() => doneTodos(scoped.value))
/** The board's rows under the same project filter and search as the list. */
const boardItems = computed<FocusItem[]>(() => {
  const items = props.board?.items ?? []
  const keep = new Set(searchTodos(filterByProject(items.map((it) => it.todo), projectFilter.value || null), query.value).map((t) => t.id))
  return items.filter((it) => keep.has(it.todo.id))
})
const useBoard = computed(() => tab.value === 'active' && !!props.board)
/** Active tab of a Focus Host before its board arrived (the plain list would
 * flash, then reshuffle). */
const boardLoading = computed(
  () => tab.value === 'active' && !!props.focus && !props.board && !props.boardFailed,
)
const isEmpty = computed(() =>
  tab.value === 'active'
    ? useBoard.value
      ? !boardItems.value.some((it) => it.bucket !== 'done')
      : groups.value.length === 0
    : finished.value.length === 0,
)

// Time / Project / Week, remembered on this device.
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

const aiRunning = computed(() => props.aiRunning ?? {})
const planning = computed(() => !!aiRunning.value[PLAN_KEY])

// Calendar refresh: spins until the Host's fresh board lands (or gives up).
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

// The quick-action sheet, by id so it follows the re-ranked board.
const actionsId = ref<string | null>(null)
const actionsItem = computed(() =>
  actionsId.value ? (props.board?.items.find((it) => it.todo.id === actionsId.value) ?? null) : null,
)
const actionsKey = computed(() => (actionsId.value ? todoAiKey(actionsId.value) : ''))
function onAction(patch: TodoPatchWire): void {
  if (actionsId.value) emit('patch', actionsId.value, patch)
  actionsId.value = null
}
function onActionToggle(): void {
  if (actionsId.value) onToggle(actionsId.value)
  actionsId.value = null
}
function onActionRemove(): void {
  if (actionsId.value) emit('remove', actionsId.value)
  actionsId.value = null
}
function onActionStartAi(): void {
  const it = actionsItem.value
  actionsId.value = null
  if (it) emit('start-ai', it.todo)
}

// Done → "how long did it take?" for a started, estimated todo (BR-11).
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
function onActionEdit(): void {
  const it = actionsItem.value
  actionsId.value = null
  if (it) emit('open', it.todo)
}

function formatDay(ymd: string): string {
  const [y, m, d] = ymd.split('-').map(Number)
  if (!y || !m || !d) return ymd
  return new Date(y, m - 1, d).toLocaleDateString(locale.value, { day: '2-digit', month: '2-digit' })
}

/** The due chip of a row: text + tone. */
function dueChip(todo: TodoInfo): { label: string; tone: string } | null {
  if (!todo.due_date) return null
  if (todo.done) return { label: formatDay(todo.due_date), tone: 'muted' }
  switch (todoGroup(todo, today.value)) {
    case 'overdue':
      return { label: t('controller.todo.dueOn', { date: formatDay(todo.due_date) }), tone: 'red' }
    case 'today':
      return { label: t('controller.todo.dueToday'), tone: 'amber' }
    default:
      return { label: formatDay(todo.due_date), tone: 'muted' }
  }
}

const CHIP_TONES: Record<string, string> = {
  red: 'border-destructive/40 bg-destructive/10 text-destructive',
  amber: 'border-warning/40 bg-warning/10 text-warning',
  muted: 'border-border bg-secondary text-muted-foreground',
}

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

    <SearchField
      v-model="query"
      :placeholder="t('controller.todo.searchPlaceholder')"
      :clear-label="t('controller.todo.clearSearch')"
    />

    <main class="flex-1 min-h-0 overflow-auto px-3 pt-3 pb-24">
      <!-- Active / Done -->
      <div class="ui-segmented mb-3 flex gap-1 rounded-lg border border-border bg-card p-0.5">
        <button
          v-for="key in (['active', 'done'] as const)"
          :key="key"
          class="flex-1 rounded-md px-2 py-1.5 text-xs transition-colors cursor-pointer"
          :class="tab === key ? 'is-active bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
          @click="tab = key"
        >
          {{ t(`controller.todo.${key}`, { count: counts[key] }) }}
        </button>
      </div>

      <div v-if="useBoard" class="mb-3 flex items-center gap-2">
        <div class="ui-segmented flex min-w-0 flex-1 gap-1 rounded-lg border border-border bg-card p-0.5">
          <button
            v-for="v in VIEWS"
            :key="v"
            class="flex-1 truncate rounded-md px-2 py-1.5 text-xs transition-colors cursor-pointer"
            :class="view === v ? 'is-active bg-secondary font-semibold text-foreground' : 'text-muted-foreground'"
            @click="view = v"
          >
            {{ t(`controller.focus.views.${v}`) }}
          </button>
        </div>
        <button
          type="button"
          class="flex h-8 shrink-0 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-card px-2.5 text-xs font-medium"
          :title="t('focus.plan.hint')"
          @click="emit('plan')"
        >
          <Loader2 v-if="planning" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          <Sparkles v-else class="h-3.5 w-3.5 text-primary" :stroke-width="2" />
          {{ t('focus.plan.button') }}
        </button>
      </div>

      <div v-if="todos === null && failed" class="flex flex-col items-center gap-3 py-10 text-sm text-foreground/50">
        {{ t('controller.data.loadFailed') }}
        <Button
          variant="outline"
          @click="emit('refresh')"
        >
          {{ t('controller.data.retry') }}
        </Button>
      </div>
      <p v-else-if="todos === null || boardLoading" class="flex items-center justify-center gap-2 py-10 text-sm text-foreground/50">
        <Loader2 class="h-4 w-4 animate-spin" :stroke-width="2" /> {{ t('controller.data.loading') }}
      </p>
      <p v-else-if="isEmpty" class="py-10 text-center text-sm text-foreground/50">
        {{
          searching
            ? t('controller.todo.noResults')
            : tab === 'active'
              ? t('controller.todo.empty')
              : t('controller.todo.emptyDone')
        }}
      </p>

      <FocusBoardList
        v-else-if="useBoard && board"
        :board="board"
        :items="boardItems"
        :project-names="projectNames"
        :now="now"
        :view="view"
        :ai-running="aiRunning"
        :calendar-refreshing="calendarRefreshing"
        @toggle="onToggle"
        @open="emit('open', $event)"
        @actions="actionsId = $event.todo.id"
        @refresh-calendar="onRefreshCalendar"
      />

      <template v-else-if="tab === 'active'">
        <section v-for="g in groups" :key="g.key" class="mb-3">
          <h2 class="mb-1.5 px-0.5 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
            {{ t(`controller.todo.groups.${g.key}`) }}
          </h2>
          <div class="ui-card overflow-hidden rounded-xl border border-border bg-card">
            <div
              v-for="todo in g.todos"
              :key="todo.id"
              class="flex items-start gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 cursor-pointer hover:bg-accent/50"
              @click="emit('open', todo)"
            >
              <button
                class="mt-0.5 h-5 w-5 shrink-0 rounded-full border-2 border-foreground/30 transition-colors hover:border-success cursor-pointer"
                :title="t('controller.todo.markDone')"
                :aria-label="t('controller.todo.markDone')"
                @click.stop="emit('toggle', todo.id)"
              />
              <div class="min-w-0 flex-1">
                <p class="text-sm font-medium break-words">{{ splitTodoText(todo.text).title }}</p>
                <p v-if="splitTodoText(todo.text).note" class="mt-0.5 truncate text-xs text-muted-foreground">
                  {{ splitTodoText(todo.text).note }}
                </p>
                <div
                  v-if="dueChip(todo) || (todo.project_id && projectNames.get(todo.project_id))"
                  class="mt-1 flex flex-wrap gap-1"
                >
                  <span
                    v-if="dueChip(todo)"
                    class="rounded-full border px-1.5 py-px text-[10px]"
                    :class="CHIP_TONES[dueChip(todo)!.tone]"
                  >
                    {{ dueChip(todo)!.label }}
                  </span>
                  <span
                    v-if="todo.project_id && projectNames.get(todo.project_id)"
                    class="rounded-full border px-1.5 py-px text-[10px]"
                    :class="CHIP_TONES.muted"
                  >
                    {{ projectNames.get(todo.project_id) }}
                  </span>
                </div>
              </div>
            </div>
          </div>
        </section>
      </template>

      <div v-else class="ui-card overflow-hidden rounded-xl border border-border bg-card">
        <div
          v-for="todo in finished"
          :key="todo.id"
          class="flex items-start gap-2.5 px-3 py-2.5 border-b border-border/60 last:border-b-0 cursor-pointer hover:bg-accent/50"
          @click="emit('open', todo)"
        >
          <button
            class="mt-0.5 grid h-5 w-5 shrink-0 place-items-center rounded-full border-2 border-success bg-success text-[11px] font-black text-success-foreground cursor-pointer"
            :title="t('controller.todo.markOpen')"
            :aria-label="t('controller.todo.markOpen')"
            @click.stop="emit('toggle', todo.id)"
          >
            ✓
          </button>
          <div class="min-w-0 flex-1">
            <p class="text-sm text-muted-foreground line-through break-words">{{ splitTodoText(todo.text).title }}</p>
            <div v-if="todo.project_id && projectNames.get(todo.project_id)" class="mt-1 flex flex-wrap gap-1">
              <span class="rounded-full border px-1.5 py-px text-[10px]" :class="CHIP_TONES.muted">
                {{ projectNames.get(todo.project_id) }}
              </span>
            </div>
          </div>
        </div>
      </div>
    </main>

    <FabButton
      :label="t('controller.todo.add')"
      @click="emit('open', null)"
    />

    <FocusActionSheet
      v-if="actionsItem"
      :item="actionsItem"
      :now="now"
      :work-start="workStart"
      :analyzing="!!aiRunning[actionsKey]"
      :analyzed="!!analyses?.[actionsItem.todo.id]"
      :ai-error="errors.aiError(aiErrors?.[actionsKey])"
      @patch="onAction"
      @toggle="onActionToggle"
      @edit="onActionEdit"
      @analyze="emit('analyze', actionsItem.todo.id)"
      @cancel-analyze="emit('cancel-analyze', actionsItem.todo.id)"
      @start-ai="onActionStartAi"
      @remove="onActionRemove"
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
