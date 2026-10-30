import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import { ITEM_CHANGED } from '@/lib/itemWindow'
import {
  DEFAULT_FOCUS_SETTINGS,
  FOCUS_CHANGED,
  type AddTodoInput,
  type FocusBoard,
  type FocusSettings,
  type FocusTodo,
  type TodoPatch,
} from '@/lib/focus'

/**
 * Focus board (docs/focus-board-spec.md §3.2): the ranked todo board computed by
 * the backend (`focus_board`), the work-hour settings, and every todo write the
 * board, the drawer and the "+ Todo" forms make.
 *
 * The board is recomputed by the backend, so writes simply refetch it: right
 * after our own writes, on `focus:changed` / `item:changed` and every 60 s so
 * the relative times stay true. The backend emits both events after every
 * todo write made through its commands (any window, the AI), after sync
 * applies todo rows, and — for writes that bypass it, like the MCP sidecar
 * writing SQLite directly — on the next 60 s reminder tick that sees the
 * table changed.
 */
const REFRESH_MS = 60_000
/** The two events of one write arrive together: one refetch for both. */
const EVENT_DEBOUNCE_MS = 150

export const useFocusStore = defineStore('focus', () => {
  const board = ref<FocusBoard | null>(null)
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref<string | null>(null)
  const settings = ref<FocusSettings>({ ...DEFAULT_FOCUS_SETTINGS })
  const settingsLoaded = ref(false)

  const items = computed(() => board.value?.items ?? [])
  /** Backend clock of the last board, for relative labels. */
  const now = computed(() => (board.value ? Date.parse(board.value.now) : Date.now()))
  const overdueCount = computed(() => items.value.filter((it) => it.bucket === 'overdue').length)

  // Coalesce bursts (a write + its own echo events) into one fetch in flight + one queued.
  let inflight: Promise<void> | null = null
  let queued = false

  async function load(): Promise<void> {
    if (inflight) {
      queued = true
      return inflight
    }
    inflight = (async () => {
      loading.value = true
      try {
        board.value = await invoke<FocusBoard>('focus_board')
        error.value = null
        loaded.value = true
      } catch (e) {
        error.value = String(e)
      } finally {
        loading.value = false
      }
    })()
    try {
      await inflight
    } finally {
      inflight = null
    }
    if (queued) {
      queued = false
      await load()
    }
  }

  async function loadSettings() {
    try {
      const s = await invoke<FocusSettings>('focus_settings_get')
      settings.value = { ...DEFAULT_FOCUS_SETTINGS, ...s }
      settingsLoaded.value = true
    } catch (e) {
      error.value = String(e)
    }
  }

  /** Drop the cached Calendar events and reload the board (capacity bar). */
  async function refreshCalendar() {
    try {
      await invoke('focus_refresh_calendar')
    } catch {
      /* outside the Tauri shell */
    }
    await load()
  }

  /** Save; the backend returns the normalised settings, which become the state. */
  async function saveSettings(next: FocusSettings): Promise<FocusSettings> {
    const saved = await invoke<FocusSettings>('focus_settings_set', { settings: next })
    settings.value = { ...DEFAULT_FOCUS_SETTINGS, ...saved }
    void load()
    return settings.value
  }

  // Live updates while at least one screen uses the board.
  let users = 0
  let unlisten: UnlistenFn[] = []
  let timer: ReturnType<typeof setInterval> | null = null
  let debounce: ReturnType<typeof setTimeout> | null = null

  function scheduleLoad() {
    if (debounce) clearTimeout(debounce)
    debounce = setTimeout(() => {
      debounce = null
      void load()
    }, EVENT_DEBOUNCE_MS)
  }

  async function start() {
    users += 1
    if (users > 1) return
    void loadSettings()
    void load()
    timer = setInterval(() => void load(), REFRESH_MS)
    try {
      unlisten = await Promise.all([listen(FOCUS_CHANGED, scheduleLoad), listen(ITEM_CHANGED, scheduleLoad)])
    } catch {
      /* outside the Tauri shell */
    }
    // Stopped while the listeners were being registered.
    if (users === 0) teardown()
  }

  function teardown() {
    for (const u of unlisten) u()
    unlisten = []
    if (timer) clearInterval(timer)
    timer = null
    if (debounce) clearTimeout(debounce)
    debounce = null
  }

  function stop() {
    users = Math.max(0, users - 1)
    if (users === 0) teardown()
  }

  // ── Writes ──────────────────────────────────────────────────────────────

  async function add(input: AddTodoInput): Promise<FocusTodo> {
    const todo = await invoke<FocusTodo>('add_todo', { ...input, text: input.text.trim() })
    await load()
    return todo
  }

  async function update(id: string, patch: TodoPatch): Promise<FocusTodo> {
    const todo = await invoke<FocusTodo>('update_todo_fields', { id, patch })
    await load()
    return todo
  }

  const markDone = (id: string) => update(id, { done: true })
  /** Back to open work (§3.3); the backend resets status and `completed_at` on `done: false` anyway. */
  const markUndone = (id: string) => update(id, { done: false, status: 'todo' })
  /** `doing`; the backend stamps `started_at` the first time. */
  const startTodo = (id: string) => update(id, { status: 'doing' })
  const setWaiting = (id: string) => update(id, { status: 'waiting' })
  const backToTodo = (id: string) => update(id, { status: 'todo' })
  const snooze = (id: string, until: string | null) => update(id, { snoozed_until: until })
  const setActual = (id: string, minutes: number | null) => update(id, { actual_min: minutes })

  async function remove(id: string) {
    await invoke('delete_todo', { id })
    await load()
  }

  return {
    board,
    items,
    now,
    overdueCount,
    loading,
    loaded,
    error,
    settings,
    settingsLoaded,
    load,
    loadSettings,
    saveSettings,
    refreshCalendar,
    start,
    stop,
    add,
    update,
    markDone,
    markUndone,
    startTodo,
    setWaiting,
    backToTodo,
    snooze,
    setActual,
    remove,
  }
})
