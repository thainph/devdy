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
import TodoEditorView from './components/TodoEditorView.vue'
import NoteListView from './components/NoteListView.vue'
import NoteEditorView from './components/NoteEditorView.vue'
import ProjectTogglesView from './components/ProjectTogglesView.vue'
import ControllerHeader from './components/ControllerHeader.vue'
import HeaderButton from './components/HeaderButton.vue'
import ConnectionBadge from './components/ConnectionBadge.vue'
import PushSheet from './components/PushSheet.vue'
import ThemeSheet from './components/ThemeSheet.vue'
import { AppSelect } from '@/components/ui'
import { Bell, BellOff, Palette, RefreshCw, X } from 'lucide-vue-next'
import { createDataStore } from './dataStore'
import { setAppearance } from './appearance'
import { createPushStore } from './pushStore'
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

const { t } = useI18n()
const { renderText, loadMarkdown } = useMarkdown()

const store = createRunStore()
const conn = shallowRef<ControllerConnection | null>(null)

// Todo / Note / project-switch screens (FEATURE_DATA).
const DATA_ERRORS = new Set([
  'not_connected', 'connection_lost', 'host_ack_timeout', 'not_found', 'malformed',
  'rate_limited', 'action_not_allowed', 'failed', 'load_timeout',
])
const data = createDataStore(
  (cmd, onAck) => conn.value?.sendCommand(cmd, onAck) ?? false,
  (reason) => {
    const key = reason === 'disconnected' ? 'connection_lost' : reason
    return key && DATA_ERRORS.has(key)
      ? t(`controller.data.errors.${key}`)
      : t('controller.data.errors.generic', { reason: reason ?? '?' })
  },
)
// Web Push (host feature `push`): this phone's notification switch.
const push = createPushStore((cmd, onAck) => conn.value?.sendCommand(cmd, onAck) ?? false)
const noteEditorOpen = ref(false)
// The todo editor: closed (null), a new todo (`todo: null`) or an existing one.
const todoEditor = ref<{ todo: TodoInfo | null; today: string } | null>(null)
// The Todo tab's filter, search and Active / Done switch, kept across the editor.
const todoProjectId = ref('')
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
const DATA_TABS: ControllerTab[] = ['todo', 'note', 'project']
const hiddenTabs = computed<ControllerTab[]>(() =>
  // Unknown until the token arrives: show everything rather than flicker.
  hostFeatures.value === null || hasData.value ? [] : DATA_TABS,
)
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
function openTodoEditor(todo: TodoInfo | null): void {
  data.clearError()
  todoEditor.value = { todo, today: localYmd() }
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
  // The browser rotated the push subscription: re-sync it with the Host.
  if (msg?.type === 'devdy:subscription-changed' && hasPush.value) void push.refresh()
}
navigator.serviceWorker?.addEventListener('message', onWorkerMessage)

function onTab(next: ControllerTab): void {
  if (next !== tab.value) data.clearError()
  tab.value = next
  closeNoteEditor()
  if (next === 'inbox') syncInbox()
  else if (next === 'runs') refreshRuns()
}

// ── tab header ──────────────────────────────────────────────────────────────
// One header for every tab: title, connection state, Refresh and Disconnect;
// Sessions / Todo / Notes add a project dropdown, the Inbox its push bell.
const TAB_TITLES: Record<ControllerTab, string> = {
  inbox: 'controller.inbox.title',
  runs: 'controller.browser.title',
  todo: 'controller.todo.title',
  note: 'controller.notes.title',
  project: 'controller.projectToggles.title',
}
const tabTitle = computed(() => t(TAB_TITLES[tab.value]))

const projectFilterOptions = computed(() => [
  { value: '', label: t('controller.data.allProjects') },
  ...store.state.projectList.map((p) => ({ value: p.id, label: p.name })),
])
/** The header dropdown of the current tab ('' = every project). */
const headerProjectId = computed<string>({
  get: () => {
    if (tab.value === 'runs') return browserProjectId.value ?? ''
    if (tab.value === 'todo') return todoProjectId.value
    if (tab.value === 'note') return data.state.noteFilter.projectId ?? ''
    if (tab.value === 'project') return toggleProjectId.value ?? ''
    return ''
  },
  set: (id) => {
    if (tab.value === 'runs') browserProjectId.value = id || null
    else if (tab.value === 'todo') todoProjectId.value = id
    else if (tab.value === 'note') data.loadNotes(id || null, data.state.noteFilter.query)
    else if (tab.value === 'project' && id) selectToggleProject(id)
  },
})
const tabHasProjectFilter = computed(() => tab.value !== 'inbox')
/** Setting switches belong to one project: no "All projects" there. */
const headerProjectOptions = computed(() =>
  tab.value === 'project' ? projectFilterOptions.value.slice(1) : projectFilterOptions.value,
)

function refreshTab(): void {
  switch (tab.value) {
    case 'inbox':
      syncInbox()
      break
    case 'runs':
      refreshRuns()
      break
    case 'todo':
      data.loadTodos()
      break
    case 'note':
      data.loadNotes()
      break
    case 'project':
      refreshRuns()
      if (toggleProjectId.value) data.loadToggles(toggleProjectId.value)
      break
  }
}

const pushOpen = ref(false)
const pushOn = computed(() => push.ui() === 'on')
const themeOpen = ref(false)

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
    if (hasPush.value) void push.refresh()
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
    v-else-if="!runId && !draftProjectId && tab === 'todo' && todoEditor"
    :todo="todoEditor.todo"
    :projects="store.state.projectList"
    :default-project-id="todoProjectId"
    :today="todoEditor.today"
    :busy="dataBusy"
    :error="data.state.error"
    :save="data.saveTodo"
    :remove="data.deleteTodo"
    @back="closeTodoEditor"
    @clear-error="data.clearError()"
  />

  <div v-else-if="!runId && !draftProjectId" class="app-shell flex h-dvh min-h-0 flex-col bg-background text-foreground">
    <ControllerHeader :title="tabTitle">
      <template #badge>
        <ConnectionBadge :status="status" :live="live" :status-label="browserStatusLabel" :host-fingerprint="hostFp" />
      </template>
      <AppSelect
        v-if="tabHasProjectFilter"
        v-model="headerProjectId"
        size="xs"
        variant="outline"
        class="w-32 max-w-full"
        :options="headerProjectOptions"
      />
      <HeaderButton
        v-if="tab === 'inbox' && hasPush"
        :label="t('controller.push.title')"
        :active="pushOn"
        @click="pushOpen = true"
      >
        <Bell v-if="pushOn" class="h-4 w-4" :stroke-width="2" />
        <BellOff v-else class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
      <HeaderButton v-if="tab === 'project'" :label="t('controller.theme.title')" @click="themeOpen = true">
        <Palette class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
      <HeaderButton :label="t('controller.browser.refresh')" @click="refreshTab">
        <RefreshCw class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
      <HeaderButton :label="t('controller.session.disconnect')" @click="disconnect">
        <X class="h-4 w-4" :stroke-width="2" />
      </HeaderButton>
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
        @update:selected-project-id="browserProjectId = $event"
        @open="openRun"
        @new-session="startNewSession"
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
        @refresh="data.loadTodos()"
        @clear-error="data.clearError()"
      />
      <NoteListView
        v-else-if="tab === 'note'"
        :notes="data.state.notes"
        :projects="store.state.projectList"
        :project-id="data.state.noteFilter.projectId"
        :query="data.state.noteFilter.query"
        :error="data.state.error"
        :failed="data.state.failed.notes"
        @filter="(p, q) => data.loadNotes(p, q)"
        @open="openNoteEditor"
        @new="openNoteEditor(null)"
        @clear-error="data.clearError()"
      />
      <ProjectTogglesView
        v-else
        :projects="store.state.projectList"
        :toggles="data.state.toggles"
        :project-id="toggleProjectId"
        :error="data.state.error"
        :failed="data.state.failed.toggles"
        @select="selectToggleProject"
        @set="data.setToggle"
        @clear-error="data.clearError()"
      />
      <PushSheet v-if="hasPush && pushOpen" :push="push" @close="pushOpen = false" />
      <ThemeSheet v-if="themeOpen" @close="themeOpen = false" />
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
    @disconnect="disconnect"
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
