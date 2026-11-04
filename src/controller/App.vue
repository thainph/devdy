<script setup lang="ts">
/**
 * Devdy Remote Controller — root app. A standalone, Tauri-free web client that
 * drives the paired Host over the relay. Flow:
 *   1. A valid stored token → auto-reattach (skip entry) → the run it was on.
 *   2. Otherwise: LinkEntryView → connect + wait for the Host OTP gate →
 *      OtpEntryView → on success (session_token) → the link's run.
 *
 * Once paired, the device can drive EVERY run on the Host (SRS BR-005 v1.1).
 * Behind the session screen sit bottom tabs — Inbox (every prompt waiting, runs
 * finished since the last visit) · Runs · Todo · Note · Project. A fresh link
 * pairing lands on the link's run; re-opening the app lands on the Inbox.
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, shallowRef, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { useMarkdown } from '@/lib/markdown'
import { mergeContextModel } from '@/lib/contextLimits'
import LinkEntryView from './components/LinkEntryView.vue'
import OtpEntryView from './components/OtpEntryView.vue'
import SessionView from './components/SessionView.vue'
import RunBrowserView from './components/RunBrowserView.vue'
import InboxView from './components/InboxView.vue'
import TodoView from './components/TodoView.vue'
import FocusView from './components/FocusView.vue'
import TodoEditorView from './components/TodoEditorView.vue'
import FocusPlanView from './components/FocusPlanView.vue'
import FocusSettingsView from './components/FocusSettingsView.vue'
import NoteListView from './components/NoteListView.vue'
import NoteEditorView from './components/NoteEditorView.vue'
import ProjectTogglesView from './components/ProjectTogglesView.vue'
import ControllerHeader from './components/ControllerHeader.vue'
import HeaderButton from './components/HeaderButton.vue'
import ConnectionBadge from './components/ConnectionBadge.vue'
import SettingsMenuView from './components/SettingsMenuView.vue'
import PushSettingsView from './components/PushSettingsView.vue'
import ThemeSettingsView from './components/ThemeSettingsView.vue'
import { ChevronLeft } from 'lucide-vue-next'
import { isToggleGroup, type SettingsPage } from './settingsMenu'
import ProjectPickerSheet from './components/ProjectPickerSheet.vue'
import { concreteProject, rememberProject, validScope } from '@/lib/controllerProjectScope'
import { createDataStore } from './dataStore'
import { useControllerErrors } from './focusErrors'
import { setAppearance } from './appearance'
import { createPushStore } from './pushStore'
import { focusOpenFromMessage, focusOpenFromSearch, withoutFocusOpen, type FocusOpen } from '@/lib/remotePush'
import BottomTabBar, { type ControllerTab } from './components/BottomTabBar.vue'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import {
  aggregatePending,
  leaveVisit,
  markRead,
  parseVisitState,
  pendingCount,
  startVisit,
  type VisitState,
} from '@/lib/remoteInbox'
import type { ComposerTurn } from './components/SessionComposer.vue'
import { ControllerConnection, type AuthState, type ConnStatus, type SecretSource } from './connection'
import { createRunStore } from './runStore'
import * as tokenStore from './sessionTokenStore'
import type { LoginGrant } from './login'
import {
  cancelRunCmd,
  listPendingCmd,
  hostHas,
  listProjectFilesCmd,
  listProjectsCmd,
  listRunsCmd,
  newSessionCmd,
  parseSessionLink,
  respondPermissionCmd,
  sendTurnCmd,
  setRunMetaCmd,
  type NoteSummary,
  type QuestionAnswers,
  type RemoteDecision,
  type SessionLinkPayload,
  type TodoInfo,
} from './protocol'
import { localYmd } from '@/lib/todoDue'
import type { TodoTab } from '@/lib/remoteTodos'
import type { FocusTodo } from '@/lib/focus'
import { PLAN_KEY, todoAiKey } from '@/lib/remoteFocus'

const { t } = useI18n()
const { renderText, loadMarkdown } = useMarkdown()

const store = createRunStore()
const conn = shallowRef<ControllerConnection | null>(null)

// Todo / Note / project-switch screens (FEATURE_DATA), and Focus on top.
const errors = useControllerErrors()
const data = createDataStore((cmd, onAck) => conn.value?.sendCommand(cmd, onAck) ?? false, errors.describe)
// Web Push (host feature `push`): this phone's notification switch.
const push = createPushStore((cmd, onAck) => conn.value?.sendCommand(cmd, onAck) ?? false)
const noteEditorOpen = ref(false)
// The todo editor: closed (null), a new todo (`todo: null`) or an existing one.
// `focus` is the todo's board row as it was when the editor opened (the diff
// base for the Focus fields).
const todoEditor = ref<{ todo: TodoInfo | null; focus: FocusTodo | null; today: string } | null>(null)
// The "Plan today" screen, over the Focus tab.
const planOpen = ref(false)
// A todo to start a session on once it has a project (the project picker).
const aiProjectFor = ref<FocusTodo | null>(null)
// ── shared project scope ─────────────────────────────────────────────────────
// One project (null = all) that Sessions, Todo, Notes and Setting follow,
// picked from the header title or Setting's Project row, kept on this device
// with the recently used ones (`@/lib/controllerProjectScope`).
const SCOPE_KEY = 'devdy.controller.projectScope'
const RECENT_KEY = 'devdy.controller.recentProjects'
function readStored(key: string): unknown {
  try {
    const raw = localStorage.getItem(key)
    return raw ? JSON.parse(raw) : null
  } catch {
    return null
  }
}
function writeStored(key: string, value: unknown): void {
  try {
    localStorage.setItem(key, JSON.stringify(value))
  } catch {
    // Storage unavailable: the scope lasts for this visit only.
  }
}
const storedScope = ref<string | null>(((v) => (typeof v === 'string' ? v : null))(readStored(SCOPE_KEY)))
const recentProjects = ref<string[]>(
  ((v) => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []))(readStored(RECENT_KEY)),
)
const projectScope = computed(() => validScope(store.state.projectList, storedScope.value))
/** The project the Setting switches work on (they need one, never "all"). */
const settingsProjectId = computed(() =>
  concreteProject(store.state.projectList, projectScope.value, recentProjects.value),
)
const pickerOpen = ref(false)

// The Todo tab's search and Active / Done switch, kept across the editor; its
// project is the shared scope ('' = all).
const todoProjectId = computed<string>({
  get: () => projectScope.value ?? '',
  set: (id) => setScope(id || null),
})
const todoTab = ref<TodoTab>('active')
const todoQuery = ref('')
const toggleProjectId = ref<string | null>(null)
const dataBusy = computed(() => data.state.pending > 0)

// What the Host supports (`session_token.host_features`). An older Host sends
// none: hide what it would only refuse rather than let it fail on use.
const hostFeatures = computed(() => conn.value?.state.hostFeatures.value ?? null)
const hasData = computed(() => hostHas(hostFeatures.value, 'data'))
const hasInbox = computed(() => hostHas(hostFeatures.value, 'inbox'))
const hasRemember = computed(() => hostHas(hostFeatures.value, 'remember'))
const hasPush = computed(() => hostHas(hostFeatures.value, 'push'))
const hasFocus = computed(() => hostHas(hostFeatures.value, 'focus'))
// Setting stays: it also holds this phone's look, notifications and Disconnect.
const DATA_TABS: ControllerTab[] = ['todo', 'note']
const hiddenTabs = computed<ControllerTab[]>(() => {
  // Unknown until the token arrives: show the long-standing tabs rather than
  // flicker; Focus only appears once the Host says it ranks todos.
  const hidden: ControllerTab[] = hostFeatures.value === null || hasData.value ? [] : [...DATA_TABS]
  if (!(hasFocus.value && hasData.value)) hidden.push('focus')
  return hidden
})
watch(hiddenTabs, (hidden) => {
  if (hidden.includes(tab.value)) tab.value = 'inbox'
})

/** Open the editor on a list row (fetched in full) or, with null, a new note. */
function openNoteEditor(note: NoteSummary | null): void {
  data.clearError()
  if (note) data.loadNote(note.id)
  else data.closeNote()
  noteEditorOpen.value = true
}
/** The editor on a todo (or a new one). Opened from Focus it also edits the
 * Focus fields; from the Todo tab it is the plain editor. */
function openTodoEditor(todo: TodoInfo | null): void {
  data.clearError()
  const row =
    todo && tab.value === 'focus' ? data.state.board?.items.find((it) => it.todo.id === todo.id) : null
  todoEditor.value = { todo, focus: row ? { ...row.todo } : null, today: localYmd() }
}
/** Open the editor on a todo by id (from the day plan). */
function openTodoById(id: string): void {
  const todo =
    data.state.todos?.find((x) => x.id === id) ?? data.state.board?.items.find((it) => it.todo.id === id)?.todo
  if (todo) openTodoEditor(todo)
}
function loadTodoTab(): void {
  data.loadTodos()
}
/** Focus tab (re)load: the ranked board, plus the settings once (the working
 * day the snooze presets start at). */
function loadFocusTab(): void {
  if (!hasFocus.value) return
  data.loadBoard()
  if (!data.state.settings) data.loadSettings()
}
/** The editor's view of its todo as the board has it now (the AI plan
 * follows an analysis). */
const liveFocusTodo = computed(() => {
  const id = todoEditor.value?.todo?.id
  return id ? (data.state.board?.items.find((it) => it.todo.id === id)?.todo ?? null) : null
})

// ── Focus AI ──
/** 🚀 Start with AI: a new session on the todo, then shown in the run view. A
 * todo without a project gets one first (the picker on the Todo tab; the
 * editor has its own project field). */
async function startTodoAi(todo: FocusTodo): Promise<void> {
  const inEditor = !!todoEditor.value
  if (!todo.project_id && !inEditor) {
    aiProjectFor.value = todo
    return
  }
  const out = await data.startTodoAi(todo.id)
  if (out.ok && out.runId) {
    todoEditor.value = null
    planOpen.value = false
    openRun(out.runId)
    refreshRuns()
  } else if (out.code === 'project_required' && !inEditor) {
    data.clearError()
    aiProjectFor.value = todo
  }
}
/** The picker's answer: file the todo under `projectId`, then start. */
async function onAiProject(projectId: string | null): Promise<void> {
  const todo = aiProjectFor.value
  aiProjectFor.value = null
  if (!todo || !projectId) return
  if (await data.patchTodo(todo.id, { project_id: projectId })) void startTodoAi({ ...todo, project_id: projectId })
}
function onPlanToday(): void {
  void data.planToday()
}
function pushPlan(blockIds?: string[]) {
  return data.planToCalendar(data.state.dayPlan?.plan?.date ?? localYmd(), blockIds)
}
function closeTodoEditor(): void {
  todoEditor.value = null
}
function closeNoteEditor(): void {
  noteEditorOpen.value = false
  data.closeNote()
}
function selectToggleProject(projectId: string): void {
  toggleProjectId.value = projectId
  data.loadToggles(projectId)
}

/** The app phase drives which screen shows. `session` covers both the run
 * browser and an open run — `store.state.activeRunId` decides between them. */
type Phase = 'link' | 'otp' | 'session'
const phase = ref<Phase>('link')

const link = ref<SessionLinkPayload | null>(null)

// Bottom tab while no session is open. The Inbox is the home screen.
const tab = ref<ControllerTab>('inbox')
// Whether the first authentication opens the link's run. True for a link just
// scanned (that run is what the QR was for); false when re-opening the app on a
// stored token, which should land on the Inbox instead of an old run.
let landOnLinkRun = true

// Mirrored connection state (local refs for the template).
const status = ref<ConnStatus>('idle')
const hostFp = ref<string | null>(null)
const lastError = ref<string | null>(null)
const live = ref(false)
const decodeErrors = ref(0)
const auth = ref<AuthState | null>(null)

// Keep connection-state watchers in the root component scope. Creating these
// inside `onLink` would associate them with the LinkEntryView emit scope; they
// would then stop as soon as that view unmounts for the OTP screen.
watch(
  () => conn.value?.state.status.value ?? 'idle',
  (v) => (status.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.hostFingerprint.value ?? null,
  (v) => (hostFp.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.lastError.value ?? null,
  (v) => (lastError.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.live.value ?? false,
  (v) => (live.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.decodeErrors.value ?? 0,
  (v) => (decodeErrors.value = v),
  { immediate: true },
)
watch(
  () => conn.value?.state.auth.value ?? null,
  (v) => {
    auth.value = v
    if (v === 'authenticated') phase.value = 'session'
    // On failure stay on the OTP screen so the user can retry (unless we are
    // reconnecting with a token — then fall back to OTP entry).
    else if (v === 'failed') phase.value = 'otp'
  },
  { immediate: true },
)

// Composer selectors. These are kept in lock-step with the desktop: the Host
// pushes its live selection via `run_meta` (mirrored below) and a local edit is
// pushed straight back with `set_run_meta`. `applyingMeta` suppresses the echo so
// a mirrored value doesn't bounce back to the Host.
const engine = ref('')
const model = ref('')
const permissionMode = ref('')
let applyingMeta = false
watch(
  () => store.state.runMeta,
  (m) => {
    applyingMeta = true
    engine.value = m.engine
    model.value = m.model
    permissionMode.value = m.permissionMode
    void nextTick(() => {
      applyingMeta = false
    })
  },
  { deep: true, immediate: true },
)
function pushMeta(): void {
  if (applyingMeta) return
  const id = runId.value
  if (!id) return
  conn.value?.sendCommand(
    setRunMetaCmd(id, {
      engine: engine.value,
      model: model.value,
      permissionMode: permissionMode.value,
    }),
  )
}
function onEngine(v: string): void {
  engine.value = v
  pushMeta()
}
function onModel(v: string): void {
  model.value = v
  pushMeta()
}
function onPermissionMode(v: string): void {
  permissionMode.value = v
  pushMeta()
}
/** Switching account is a Host-side operation with real work behind it
 * (transcript mirroring), so we send it explicitly and let the Host's
 * `run_meta` push decide what actually landed rather than echoing optimistically. */
function onClaudeAccount(v: string): void {
  const id = runId.value
  if (!id) return
  conn.value?.sendCommand(setRunMetaCmd(id, { claudeAccountId: v }))
}

// Whether the bound run is currently executing. We flip it optimistically on
// send and clear it on a `done`.
const running = ref(false)
const sending = ref(false)

onMounted(() => {
  loadMarkdown()
  const token = tokenStore.load()
  const hashLink = parseHashLink()
  // A URL link for a DIFFERENT HOST is an explicit intent to connect THERE — it
  // must override a stale stored token, or we would auto-reattach to the old
  // host and fail with a confusing "fingerprint mismatch / MITM" error.
  //
  // A link for the same host but a different RUN is NOT a different session any
  // more: one paired device drives every run, so we keep the token and simply
  // open the run the link names.
  if (hashLink && token && hashLink.host_fingerprint !== token.host_fingerprint) {
    tokenStore.clear()
    onLink(hashLink)
    return
  }
  // Token reconnect: a still-fresh token (for the same host) skips the OTP. A
  // link for that same host still decides WHICH run we land on.
  if (token) {
    link.value = {
      relay_url: token.relay_url,
      host_fingerprint: token.host_fingerprint,
      rendezvous_code: token.rendezvous_code,
      run_id: hashLink?.run_id ?? token.run_id,
      link_secret: token.link_secret,
    }
    tokenStore.touch()
    landOnLinkRun = !!hashLink
    startConnection({ mode: 'session', token: token.token })
    phase.value = 'session'
  }
  // No token → stay on LinkEntryView (phase 'link'), which parses the hash itself.
})

/** Parse a session link from the URL hash (mirrors LinkEntryView), or null. */
function parseHashLink(): SessionLinkPayload | null {
  const hash = window.location.hash.replace(/^#/, '')
  if (!hash) return null
  try {
    return parseSessionLink(
      hash.includes('=') || hash.startsWith('{') ? hash : decodeURIComponent(hash),
    )
  } catch {
    return null
  }
}

onBeforeUnmount(() => {
  conn.value?.close()
  document.removeEventListener('visibilitychange', onVisibility)
  window.removeEventListener('pagehide', onPageHide)
  navigator.serviceWorker?.removeEventListener('message', onWorkerMessage)
})

/** Parsed a link (from hash or paste) — connect IMMEDIATELY (join + pubkey
 * handshake) so the desktop shows its OTP, then show the OTP entry screen. The
 * session key is derived later, once the user submits the OTP. */
function onLink(payload: SessionLinkPayload): void {
  link.value = payload
  landOnLinkRun = true
  phase.value = 'otp'
  startConnection(null)
}

/** Password login succeeded: keep the granted link + token exactly like a
 * token from a link session, then continue on the normal session path. */
function onGranted(grant: LoginGrant): void {
  tokenStore.save({ ...grant.link, token: grant.token })
  link.value = grant.link
  // A password login is "find my host", not "open this run".
  landOnLinkRun = false
  phase.value = 'session'
  startConnection({ mode: 'session', token: grant.token })
}

/** The user submitted an OTP — derive the key + fire the gate on the live
 * connection (safe to call again to retry a wrong code). */
function onOtp(otp: string): void {
  if (!link.value) return
  // A connection that stopped (e.g. the host signed out our stored token) is
  // closed for good; the code goes to a fresh one.
  if (!conn.value || conn.value.state.status.value === 'error') startConnection(null)
  conn.value?.submitOtp(otp)
}

/** Build a connection for the current link + secret source and wire its refs.
 * `secret` is null for a fresh OTP link (the OTP arrives later via submitOtp). */
function startConnection(secret: SecretSource | null): void {
  if (!link.value) return
  conn.value?.close()
  store.reset()
  data.reset()
  running.value = false
  sending.value = false
  const c = new ControllerConnection(link.value, secret, (p) => {
    store.apply(p)
    data.apply(p)
    push.apply(p)
    if (p.kind === 'appearance') setAppearance(p)
  })
  conn.value = c
  void c.connect()
}

function disconnect(): void {
  tokenStore.clear()
  conn.value?.close()
  conn.value = null
  store.reset()
  data.reset()
  noteEditorOpen.value = false
  todoEditor.value = null
  planOpen.value = false
  aiProjectFor.value = null
  link.value = null
  phase.value = 'link'
  status.value = 'idle'
  hostFp.value = null
  lastError.value = null
  live.value = false
  decodeErrors.value = 0
  auth.value = null
  running.value = false
}

// ── open run ────────────────────────────────────────────────────────────────
// The run in view. Starts at the link's run (what the QR was minted for) and
// moves as the user browses; null means the run browser is showing.
const runId = computed(() => store.state.activeRunId)
const runView = computed(() => (runId.value ? store.state.runs[runId.value] ?? null : null))
const pending = computed(() => store.pendingFor(runId.value))
const pendingElsewhere = computed(() => store.pendingElsewhere(runId.value))

// Which project the run browser is showing. Lives here (not inside the browser)
// so it survives opening a run and coming back — returning to the top-level
// project list every time would make moving between two runs tedious.
const browserProjectId = ref<string | null>(null)

// A new session being composed: the project it will be created in. Its run only
// exists once the Host accepts the first turn (the ack carries the new run id).
const draftProjectId = ref<string | null>(null)

function startNewSession(projectId: string): void {
  browserProjectId.value = projectId
  draftProjectId.value = projectId
  running.value = false
}

/** Open a run: tell the Host to stream it, then show it. */
function openRun(id: string): void {
  // Remember where this run lives so Back lands on its project, not the root.
  const info = store.state.runList.find((r) => r.id === id)
  if (info) browserProjectId.value = info.project_id
  saveVisit(markRead(visit.value, id))
  draftProjectId.value = null
  store.setActiveRun(id)
  conn.value?.openRun(id)
}

/** Back to the browser: stop streaming the run we were reading (its prompts and
 * status still arrive) and refresh the list so it is not stale. */
function backToBrowser(): void {
  draftProjectId.value = null
  const id = runId.value
  store.clearActiveRun()
  if (id) conn.value?.closeRun(id)
  refreshRuns()
}

function refreshRuns(): void {
  conn.value?.sendCommand(listRunsCmd())
  conn.value?.sendCommand(listProjectsCmd())
}

// ── inbox ───────────────────────────────────────────────────────────────────
/** Re-pull every open prompt (live ones that fired while this device was away
 * went nowhere) plus the run list the "since last visit" section reads. */
function syncInbox(): void {
  const c = conn.value
  if (!c) return
  refreshRuns()
  // An older Host has no `list_pending`: live prompts are all it can offer.
  if (!hasInbox.value) return
  // One sync at a time; the one in flight covers this request too.
  const token = store.beginPendingSync()
  if (token === null) return
  const sent = c.sendCommand(listPendingCmd(), (accepted) => store.endPendingSync(token, accepted))
  if (!sent) store.endPendingSync(token, false)
}

const inboxPending = computed(() =>
  aggregatePending(store.state.pendingQueues, store.state.pendingSeenAt),
)
const inboxCount = computed(() => pendingCount(store.state.pendingQueues))

// "Since last visit" bookkeeping, persisted across reloads.
const VISIT_KEY = 'devdy.controller.inbox.v1'
function loadVisit(): VisitState {
  try {
    return parseVisitState(localStorage.getItem(VISIT_KEY))
  } catch {
    return parseVisitState(null)
  }
}
const visit = ref<VisitState>(startVisit(loadVisit(), Date.now()))
function saveVisit(next: VisitState): void {
  visit.value = next
  try {
    localStorage.setItem(VISIT_KEY, JSON.stringify(next))
  } catch {
    // Storage unavailable: the Inbox still works for this visit.
  }
}
saveVisit(visit.value)
const sinceMs = computed(() => visit.value.anchor ?? Date.now())

function onVisibility(): void {
  // Pushes are held back while the app is on screen; say when that changes
  // (the socket may still carry this before the OS suspends the page).
  if (hasPush.value && auth.value === 'authenticated' && status.value === 'connected') {
    push.setVisible(document.visibilityState === 'visible')
  }
  if (document.visibilityState === 'hidden') {
    saveVisit(leaveVisit(visit.value, Date.now()))
    return
  }
  saveVisit(startVisit(visit.value, Date.now()))
  // The connection re-dials itself on wake; once live, catch up right away.
  if (auth.value === 'authenticated' && status.value === 'connected') syncInbox()
}
function onPageHide(): void {
  saveVisit(leaveVisit(visit.value, Date.now()))
}
document.addEventListener('visibilitychange', onVisibility)
window.addEventListener('pagehide', onPageHide)

// A tapped notification (service worker → open window): show the Inbox.
function onWorkerMessage(e: MessageEvent): void {
  const msg = e.data as { type?: unknown } | null
  if (msg?.type === 'devdy:open-inbox' && phase.value === 'session') openInbox()
  // A Focus reminder: its todo (handled once the session is usable).
  const open = focusOpenFromMessage(msg)
  if (open) focusOpen.value = open
  // The browser rotated the push subscription: re-sync it with the Host.
  if (msg?.type === 'devdy:subscription-changed' && hasPush.value) void push.refresh()
}
navigator.serviceWorker?.addEventListener('message', onWorkerMessage)

// ── tapped Focus reminder ───────────────────────────────────────────────────
// The worker posts `devdy:open-todo` to an open window, or opens one on
// `?open=todo&todo=<id>` (dropped from the address so a reload does not
// repeat it). Once the session is usable: the Todo tab, then that todo's
// editor as soon as a fresh list (and board) has it.
const focusOpen = ref<FocusOpen | null>(focusOpenFromSearch(window.location.search))
if (focusOpen.value) history.replaceState(history.state, '', withoutFocusOpen(window.location.href))
let focusOpenWait: { todoId: string; todos: unknown; board: unknown } | null = null
watch(
  () =>
    [focusOpen.value, status.value === 'connected' && auth.value === 'authenticated', hostFeatures.value] as const,
  ([target, ready, features]) => {
    if (!target || !ready || features === null) return
    focusOpen.value = null
    if (!hasData.value) return
    if (runId.value || draftProjectId.value) backToBrowser()
    onTab(hasFocus.value ? 'focus' : 'todo')
    closeTodoEditor()
    focusOpenWait = target.todoId ? { todoId: target.todoId, todos: data.state.todos, board: data.state.board } : null
    loadTodoTab()
    loadFocusTab()
  },
)
watch(
  () => [data.state.todos, data.state.board, data.state.failed.todos, data.state.failed.board] as const,
  ([todos, board, failed, boardFailed]) => {
    const wait = focusOpenWait
    if (!wait) return
    if (failed) {
      focusOpenWait = null
      return
    }
    // A board that failed to load is not waited for (the editor opens without it).
    if (todos === wait.todos || (hasFocus.value && board === wait.board && !boardFailed)) return
    focusOpenWait = null
    // Gone meanwhile (deleted / done elsewhere): the tab alone.
    const todo = todos?.find((x) => x.id === wait.todoId)
    if (todo && (tab.value === 'todo' || tab.value === 'focus')) openTodoEditor(todo)
  },
)

function onTab(next: ControllerTab): void {
  if (next !== tab.value) data.clearError()
  // Leaving the tab, or tapping Setting again, lands back on its menu.
  settingsPage.value = null
  tab.value = next
  // The run browser keeps its own project while a run is open; a tab switch
  // brings it back to the shared scope.
  if (next === 'runs') browserProjectId.value = projectScope.value
  closeNoteEditor()
  planOpen.value = false
  if (next === 'inbox') syncInbox()
  else if (next === 'runs') refreshRuns()
}

// ── tab header ──────────────────────────────────────────────────────────────
// One header for every tab: title and connection state; on Sessions / Todo /
// Notes and the Setting project screens the title also names the project and
// opens the project picker. A Setting screen adds Back to its menu.
const TAB_TITLES: Record<ControllerTab, string> = {
  inbox: 'controller.inbox.title',
  runs: 'controller.browser.title',
  focus: 'controller.tabs.focus',
  todo: 'controller.todo.title',
  note: 'controller.notes.title',
  project: 'controller.projectToggles.title',
}
const SETTINGS_TITLES: Record<SettingsPage, string> = {
  mcp: 'controller.projectToggles.mcp',
  skill: 'controller.projectToggles.skills',
  rule: 'controller.projectToggles.rules',
  server: 'controller.projectToggles.servers',
  account: 'controller.projectToggles.accounts',
  theme: 'controller.theme.title',
  push: 'controller.push.title',
  focus: 'controller.focus.settings.title',
}
/** The Setting screen open over its menu (null = the menu). */
const settingsPage = ref<SettingsPage | null>(null)
const onSettingsPage = computed(() => tab.value === 'project' && settingsPage.value !== null)
const tabTitle = computed(() =>
  t(onSettingsPage.value ? SETTINGS_TITLES[settingsPage.value!] : TAB_TITLES[tab.value]),
)
function openSettingsPage(page: SettingsPage): void {
  data.clearError()
  settingsPage.value = page
}

// The Setting screens follow the shared scope (or, on "all", the most
// recently used project): fetch its switches when they are not in.
watch(
  () => [tab.value === 'project' && hasData.value, settingsProjectId.value] as const,
  ([on, pid]) => {
    if (!on || !pid) return
    if (toggleProjectId.value !== pid || data.state.toggles?.project_id !== pid) selectToggleProject(pid)
  },
  { immediate: true },
)

/** Pick the shared project (null = all) and apply it to the open tab. */
function setScope(id: string | null): void {
  storedScope.value = id
  writeStored(SCOPE_KEY, id)
  if (id) {
    recentProjects.value = rememberProject(recentProjects.value, id)
    writeStored(RECENT_KEY, recentProjects.value)
  }
  if (tab.value === 'runs') browserProjectId.value = id
  else if (tab.value === 'note' && hasData.value) data.loadNotes(id, data.state.noteFilter.query)
}
function onPickProject(id: string | null): void {
  pickerOpen.value = false
  setScope(id)
}

/** The project the header names (each tab's own view of the scope). */
const headerProjectId = computed<string | null>(() => {
  if (tab.value === 'runs') return browserProjectId.value
  if (tab.value === 'project') return settingsProjectId.value
  return projectScope.value
})
/** The header's project label, or undefined where the screen has no project. */
const headerScope = computed<string | undefined>(() => {
  if (tab.value === 'inbox') return undefined
  if (tab.value !== 'runs' && !hasData.value) return undefined
  if (tab.value === 'project' && !isToggleGroup(settingsPage.value)) return undefined
  const id = headerProjectId.value
  return (id && store.state.projectList.find((p) => p.id === id)?.name) || t('controller.data.allProjects')
})

/** Setting → Refresh: re-fetch everything every tab shows. */
function refreshAll(): void {
  syncInbox()
  refreshRuns()
  if (hasData.value) {
    loadTodoTab()
    loadFocusTab()
    data.loadNotes()
    if (toggleProjectId.value) data.loadToggles(toggleProjectId.value)
  }
}

const pushOn = computed(() => push.ui() === 'on')

/** Open the Inbox from a session (the "another run needs you" banner). */
function openInbox(): void {
  tab.value = 'inbox'
  backToBrowser()
  syncInbox()
}

// Land on the link's run when the session first authenticates — the Host has
// already subscribed us to it, so this only decides what the UI shows.
//
// On a RE-authentication (reconnect) the Host resets its subscriptions to the
// link's run, so a user who had navigated elsewhere must re-open that run or
// their screen would silently stop streaming.
watch(
  () => auth.value,
  (v) => {
    if (v !== 'authenticated' || !link.value) return
    const active = store.state.activeRunId
    if (!active) {
      if (landOnLinkRun) store.setActiveRun(link.value.run_id)
      landOnLinkRun = false
    } else if (active !== link.value.run_id) {
      conn.value?.openRun(active)
    }
  },
)

// Each time the session becomes usable (first pairing AND every reconnect —
// `auth` stays `authenticated` across a re-dial, `status` does not): prompts
// that fired while the socket was down never arrived, and ones this device
// holds may have been answered at the desktop meanwhile.
watch(
  () => status.value === 'connected' && auth.value === 'authenticated',
  (ready) => {
    if (!ready) return
    syncInbox()
    if (hasData.value) data.reload()
    if (hasPush.value) {
      void push.refresh()
      // A "back on screen" sent while the socket was dying never arrived.
      if (document.visibilityState === 'visible') push.setVisible(true)
    }
  },
)
const entries = computed(() => runView.value?.entries ?? [])
// Usage / context metrics derived from the stream (desktop parity).
const contextTokens = computed(() => runView.value?.contextTokens ?? 0)
// `system.init` drops the `[1m]` suffix; re-attach it from the mirrored composer
// selection so 1M runs resolve to the 1M context limit rather than 200K.
const contextModel = computed(() =>
  mergeContextModel(runView.value?.model ?? null, store.state.runMeta.model),
)
const rateLimit = computed(() => runView.value?.rateLimit ?? null)
const usage = computed(() => runView.value?.usage ?? null)
// Subscription plan-usage badges (Claude + Codex), pushed by the Host.
const claudeBudget = computed(() => store.state.claudeBudget)
const codexBudget = computed(() => store.state.codexBudget)
const claudeAccount = computed(() => store.state.claudeAccount)
const claudeAccounts = computed(() => store.state.claudeAccounts)

// Clear the running flag when the run reports a terminal status.
watch(
  () => (runId.value ? store.state.runs[runId.value]?.status : null),
  (s) => {
    if (s === 'running') running.value = true
    else if (s && ['done', 'cancelled', 'failed', 'completed', 'error', 'fetched'].includes(s)) {
      running.value = false
    }
    sending.value = false
  },
  { immediate: true },
)

// Prefer the session's real title (from the Host's run-list snapshot, sent on
// pairing and refreshed when the run finishes). Fall back to a short run id when
// the run has not been titled yet.
const title = computed(() => {
  const id = runId.value
  if (!id) return draftProjectId.value ? t('controller.browser.newSession') : 'Run'
  const info = store.state.runList.find((r) => r.id === id)
  const runTitle = info?.title?.trim()
  return runTitle ? runTitle : `Run ${id.slice(0, 8)}`
})

// The OTP screen's inline error: only while auth actually failed (not a
// transient transport error), so we don't scare the user on a reconnect blip.
const otpError = computed(() => (auth.value === 'failed' ? lastError.value : null))
const otpConnecting = computed(
  () =>
    auth.value === 'pending' ||
    status.value === 'joining' ||
    (status.value === 'handshaking' && !hostFp.value),
)
const shortFp = computed(() => (link.value ? link.value.host_fingerprint.slice(0, 12) : ''))

// SessionView derives this itself; the browser takes it as a prop so both
// screens show the same wording for the same connection state.
const browserStatusLabel = computed(() => {
  switch (status.value) {
    case 'joining':
      return t('controller.session.joining')
    case 'handshaking':
      return t('controller.session.securing')
    case 'connected':
      return live.value
        ? t('controller.session.connected')
        : t('controller.session.connectedWaiting')
    case 'reconnecting':
      return t('controller.session.reconnecting')
    case 'error':
      return t('controller.session.error')
    case 'disconnected':
      return t('controller.session.disconnected')
    default:
      return t('controller.session.idle')
  }
})

// ── composer / permission wiring ────────────────────────────────────────────
function warnNotSent(): void {
  store.state.notice = t('controller.app.notConnected')
}

function onSend(turn: ComposerTurn): void {
  const id = runId.value
  if (!id) {
    if (draftProjectId.value) sendNewSession(draftProjectId.value, turn)
    return
  }
  const cmd = sendTurnCmd(
    id,
    {
      text: turn.text,
      engine: engine.value,
      model: model.value,
      permissionMode: permissionMode.value,
      mentions: turn.mentions,
      attachments: turn.attachments,
    },
    running.value,
  )
  const sent = conn.value?.sendCommand(cmd, (accepted, reason) => {
    sending.value = false
    if (!accepted) {
      store.state.notice =
        reason === 'connection_lost' || reason === 'disconnected'
          ? t('controller.app.connectionLost')
          : reason
            ? t('controller.app.hostRejectedReason', { reason })
            : t('controller.app.hostRejected')
      return
    }
    turn.accept()
    // Echo only after the Host confirms it accepted the command. This prevents
    // a reconnect race from showing (then losing) a turn the Host never saw.
    store.pushUser(
      id,
      turn.text,
      turn.attachments.map((a) => ({ media_type: a.mime, data: a.data_base64 })),
    )
    running.value = true
  })
  if (sent) {
    sending.value = true
  } else {
    sending.value = false
    warnNotSent()
  }
}

/** First turn of a new session: the Host creates the run, subscribes us to it
 * and returns its id, then we switch the view onto it. */
function sendNewSession(projectId: string, turn: ComposerTurn): void {
  const cmd = newSessionCmd(projectId, {
    text: turn.text,
    engine: engine.value,
    model: model.value,
    permissionMode: permissionMode.value,
    mentions: turn.mentions,
    attachments: turn.attachments,
  })
  const sent = conn.value?.sendCommand(cmd, (accepted, reason, newRunId) => {
    sending.value = false
    if (!accepted || !newRunId) {
      store.state.notice =
        reason === 'connection_lost' || reason === 'disconnected'
          ? t('controller.app.connectionLost')
          : reason
            ? t('controller.app.hostRejectedReason', { reason })
            : t('controller.app.hostRejected')
      return
    }
    turn.accept()
    // The user may have backed out while the Host was starting it.
    if (draftProjectId.value === projectId) {
      draftProjectId.value = null
      store.setActiveRun(newRunId)
    }
    store.pushUser(
      newRunId,
      turn.text,
      turn.attachments.map((a) => ({ media_type: a.mime, data: a.data_base64 })),
    )
    running.value = true
    refreshRuns()
  })
  if (sent) {
    sending.value = true
  } else {
    sending.value = false
    warnNotSent()
  }
}

function onCancel(): void {
  const id = runId.value
  if (!id) return
  if (!conn.value?.sendCommand(cancelRunCmd(id))) warnNotSent()
}

/** Answer one prompt. It stays on screen until the Host confirms the
 * sidecar/broker accepted it (`permission_resolved`). */
function respond(req: PermissionRequest, decision: RemoteDecision, answers?: QuestionAnswers): void {
  if (!conn.value?.sendCommand(respondPermissionCmd(req.run_id, req.request_id, decision, answers))) {
    warnNotSent()
  }
}

function onDecide(decision: RemoteDecision): void {
  if (pending.value) respond(pending.value, decision)
}

function onAnswer(answers: QuestionAnswers): void {
  if (pending.value) respond(pending.value, 'allow_once', answers)
}

function onRequestFiles(): void {
  const id = runId.value
  if (id) conn.value?.sendCommand(listProjectFilesCmd(id))
}
</script>

<template>
  <LinkEntryView v-if="phase === 'link'" @link="onLink" @granted="onGranted" />

  <OtpEntryView
    v-else-if="phase === 'otp'"
    :short-fingerprint="shortFp"
    :connecting="otpConnecting"
    :error="otpError"
    @otp="onOtp"
  />

  <!-- The note editor is full height, like a session: no tab bar. -->
  <NoteEditorView
    v-else-if="!runId && !draftProjectId && tab === 'note' && noteEditorOpen"
    :note="data.state.note"
    :note-id="data.state.noteId"
    :failed="data.state.failed.note"
    :projects="store.state.projectList"
    :default-project-id="data.state.noteFilter.projectId ?? ''"
    :busy="dataBusy"
    :error="data.state.error"
    :render-text="renderText"
    :save="data.saveNote"
    :remove="data.deleteNote"
    @retry="data.state.noteId && data.loadNote(data.state.noteId)"
    @back="closeNoteEditor"
    @clear-error="data.clearError()"
  />

  <!-- The todo editor too: full height, like the note editor. -->
  <TodoEditorView
    v-else-if="!runId && !draftProjectId && (tab === 'todo' || tab === 'focus') && todoEditor"
    :todo="todoEditor.todo"
    :projects="store.state.projectList"
    :default-project-id="todoProjectId"
    :today="todoEditor.today"
    :busy="dataBusy"
    :error="data.state.error"
    :save="data.saveTodo"
    :remove="data.deleteTodo"
    :focus="hasFocus && tab === 'focus'"
    :focus-todo="todoEditor.focus"
    :live-todo="liveFocusTodo"
    :render-text="renderText"
    :ai-running="data.state.aiRunning"
    :ai-errors="data.state.aiErrors"
    :analyses="data.state.analyses"
    :suggest="data.suggestTodo"
    @back="closeTodoEditor"
    @clear-error="data.clearError()"
    @analyze="(id) => void data.analyzeTodo(id)"
    @cancel-ai="data.cancelAi"
    @start-ai="startTodoAi"
  />

  <!-- Plan today: full height too; a todo opened from it comes back here. -->
  <FocusPlanView
    v-else-if="!runId && !draftProjectId && tab === 'focus' && planOpen && hasFocus"
    :day-plan="data.state.dayPlan"
    :failed="data.state.failed.plan"
    :planning="data.isAiRunning(PLAN_KEY)"
    :plan-error="errors.aiError(data.state.aiErrors[PLAN_KEY])"
    :items="data.state.board?.items ?? []"
    :error="data.state.error"
    :push="pushPlan"
    @back="planOpen = false"
    @load="data.loadDayPlan()"
    @make="onPlanToday"
    @cancel="data.cancelAi(PLAN_KEY)"
    @open="openTodoById"
    @clear-error="data.clearError(); data.clearAiError(PLAN_KEY)"
  />

  <div v-else-if="!runId && !draftProjectId" class="app-shell flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="tabTitle" :scope="headerScope" @pick-scope="pickerOpen = true">
      <template v-if="onSettingsPage" #leading>
        <HeaderButton class="-ml-1.5" :label="t('controller.data.back')" @click="settingsPage = null">
          <ChevronLeft class="h-4 w-4" :stroke-width="2" />
        </HeaderButton>
      </template>
      <template #badge>
        <ConnectionBadge :status="status" :live="live" :status-label="browserStatusLabel" :host-fingerprint="hostFp" />
      </template>
    </ControllerHeader>
    <div class="relative flex-1 min-h-0">
      <InboxView
        v-if="tab === 'inbox'"
        :status="status"
        :pending="inboxPending"
        :pending-seen-at="store.state.pendingSeenAt"
        :runs="store.state.runList"
        :projects="store.state.projectList"
        :since-ms="sinceMs"
        :read-ids="visit.read"
        :allow-always="hasRemember"
        @decide="respond"
        @answer="(req, answers) => respond(req, 'allow_once', answers)"
        @open="openRun"
      />
      <RunBrowserView
        v-else-if="tab === 'runs'"
        :status="status"
        :runs="store.state.runList"
        :projects="store.state.projectList"
        :pending-run-ids="Object.keys(store.state.pendingByRun)"
        :selected-project-id="browserProjectId"
        @update:selected-project-id="setScope"
        @open="openRun"
        @new-session="startNewSession"
      />
      <FocusView
        v-else-if="tab === 'focus'"
        :board="data.state.board"
        :failed="data.state.failed.board"
        :projects="store.state.projectList"
        :project-id="projectScope"
        :error="data.state.error"
        :ai-running="data.state.aiRunning"
        :ai-errors="data.state.aiErrors"
        :analyses="data.state.analyses"
        :actual="data.state.actual"
        :work-start="data.state.settings?.work_start"
        @open="openTodoEditor"
        @toggle="data.toggleTodo"
        @patch="(id, patch) => void data.patchTodo(id, patch)"
        @remove="(id) => void data.deleteTodo(id)"
        @analyze="(id) => void data.analyzeTodo(id)"
        @cancel-analyze="(id) => data.cancelAi(todoAiKey(id))"
        @start-ai="startTodoAi"
        @suggest-actual="data.suggestActual"
        @refresh-calendar="void data.refreshCalendar()"
        @plan="planOpen = true"
        @refresh="loadFocusTab"
        @clear-error="data.clearError()"
      />
      <TodoView
        v-else-if="tab === 'todo'"
        v-model:project-id="todoProjectId"
        v-model:tab="todoTab"
        v-model:query="todoQuery"
        :todos="data.state.todos"
        :projects="store.state.projectList"
        :error="data.state.error"
        :failed="data.state.failed.todos"
        @open="openTodoEditor"
        @toggle="data.toggleTodo"
        @refresh="loadTodoTab"
        @clear-error="data.clearError()"
      />
      <NoteListView
        v-else-if="tab === 'note'"
        :notes="data.state.notes"
        :projects="store.state.projectList"
        :project-id="projectScope"
        :query="data.state.noteFilter.query"
        :error="data.state.error"
        :failed="data.state.failed.notes"
        @filter="(p, q) => data.loadNotes(p, q)"
        @open="openNoteEditor"
        @new="openNoteEditor(null)"
        @clear-error="data.clearError()"
      />
      <SettingsMenuView
        v-else-if="settingsPage === null"
        :projects="hasData ? store.state.projectList : []"
        :project-id="settingsProjectId"
        :toggles="data.state.toggles"
        :push-on="hasPush ? pushOn : null"
        :focus="hasFocus && hasData"
        @open="openSettingsPage"
        @pick-project="pickerOpen = true"
        @refresh="refreshAll"
        @disconnect="disconnect"
      />
      <ThemeSettingsView v-else-if="settingsPage === 'theme'" />
      <PushSettingsView v-else-if="settingsPage === 'push'" :push="push" />
      <FocusSettingsView
        v-else-if="settingsPage === 'focus'"
        :settings="data.state.settings"
        :failed="data.state.failed.settings"
        :busy="dataBusy"
        :error="data.state.error"
        :save="data.saveSettings"
        @load="data.loadSettings()"
        @clear-error="data.clearError()"
      />
      <ProjectTogglesView
        v-else
        :group="settingsPage"
        :projects="store.state.projectList"
        :toggles="data.state.toggles"
        :project-id="toggleProjectId"
        :error="data.state.error"
        :failed="data.state.failed.toggles"
        @select="selectToggleProject"
        @set="data.setToggle"
        @clear-error="data.clearError()"
      />
      <ProjectPickerSheet
        v-if="pickerOpen"
        :projects="store.state.projectList"
        :selected="headerProjectId"
        :allow-all="tab !== 'project'"
        :recent="recentProjects"
        @pick="onPickProject"
        @close="pickerOpen = false"
      />
      <ProjectPickerSheet
        v-if="aiProjectFor"
        :projects="store.state.projectList"
        :selected="null"
        :allow-all="false"
        :recent="recentProjects"
        @pick="onAiProject"
        @close="aiProjectFor = null"
      />
    </div>
    <BottomTabBar :tab="tab" :inbox-count="inboxCount" :hidden="hiddenTabs" @update:tab="onTab" />
  </div>

  <SessionView
    v-else
    :allow-always="hasRemember"
    :run-key="runId ?? (draftProjectId ? `draft:${draftProjectId}` : null)"
    :title="title"
    :status="status"
    :live="live"
    :host-fingerprint="hostFp"
    :decode-errors="decodeErrors"
    :last-error="lastError"
    :notice="store.state.notice"
    :entries="entries"
    :running="running"
    :pending="pending"
    :pending-elsewhere="pendingElsewhere"
    :render-text="renderText"
    :slash-commands="store.state.slashCommands"
    :project-files="store.state.projectFiles"
    :discovered-models="store.state.discoveredModels"
    :claude-account-choices="store.state.claudeAccountChoices"
    :claude-account-id="store.state.runMeta.claudeAccountId"
    :engine="engine"
    :model="model"
    :permission-mode="permissionMode"
    :sending="sending"
    :context-tokens="contextTokens"
    :context-model="contextModel"
    :rate-limit="rateLimit"
    :usage="usage"
    :claude-budget="claudeBudget"
    :codex-budget="codexBudget"
    :claude-account="claudeAccount"
    :claude-accounts="claudeAccounts"
    @send="onSend"
    @cancel="onCancel"
    @decide="onDecide"
    @answer="onAnswer"
    @back="backToBrowser"
    @open-inbox="openInbox"
    @clear-notice="store.clearNotice()"
    @update:engine="onEngine"
    @update:model="onModel"
    @update:permission-mode="onPermissionMode"
    @update:claude-account-id="onClaudeAccount"
    @request-files="onRequestFiles"
  />
</template>
