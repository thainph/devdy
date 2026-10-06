<script setup lang="ts">
import { ref, reactive, shallowRef, computed, onMounted, onUnmounted, watch, nextTick, type ComponentPublicInstance } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { useProjectsStore, type Repo } from '@/stores/projects'
import {
  useRunsStore,
  sameRunLogRevision,
  runActivityAt,
  type RunRecord,
  type ProjectEntry,
  type RunLogRevision,
} from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useChatDraftsStore } from '@/stores/chatDrafts'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { useGitlabAccountsStore } from '@/stores/gitlabAccounts'
import { useServersStore, type ProjectServer } from '@/stores/servers'
import { useAwsAccountsStore } from '@/stores/awsAccounts'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useModelCatalogStore } from '@/stores/modelCatalog'
import { useUILayoutStore } from '@/stores/uiLayout'
import { registerMenuAction } from '@/lib/appMenu'
import { invoke } from '@/lib/tauri'
import { openUrl } from '@tauri-apps/plugin-opener'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
import {
  Play, Square, GitPullRequest, Bug,
  Clock, Cpu, Terminal, FileText, RotateCcw, RefreshCw,
  Send, MessageSquare, Trash2, Settings, Code2, FolderClosed, FolderOpen, Sparkles, ExternalLink,
  ChevronDown, ChevronUp, Maximize2, Minimize2, AppWindow,
  ImagePlus, X, Paperclip,
  ShieldQuestion, MessageCircleQuestion,
  Pin, PinOff, Shield, ShieldOff, Pencil, Check, Github, Gitlab, UserCircle,
  ClipboardCopy, ScrollText, HardDrive, Cloud, Radio, Languages, StickyNote, ListTodo, FolderTree, Loader2,
  MoreHorizontal, BookMarked, Search, Network, ChevronLeft, ChevronRight, Hash
} from 'lucide-vue-next'
import AppSelect from '@/components/AppSelect.vue'
import { type SavedPrompt, parseSavedPrompts, promptLabel } from '@/lib/savedPrompts'
import StreamLog from '@/components/StreamLog.vue'
import TranslatePopover from '@/components/TranslatePopover.vue'
import MentionedFiles from '@/components/MentionedFiles.vue'
import ContextMeter from '@/components/ContextMeter.vue'
import BudgetBadge from '@/components/BudgetBadge.vue'
import { mergeContextModel } from '@/lib/contextLimits'
import FileTree from '@/components/FileTree.vue'
import ConductorWorkerPanel from '@/components/conductor/ConductorWorkerPanel.vue'
import { useConductorStore } from '@/stores/conductor'
import { Button, Input, Badge, Modal, DropdownMenu, DropdownItem, DropdownSeparator } from '@/components/ui'
import { useTurnNavigator, turnLabel } from '@/composables/useTurnNavigator'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { openItemCreateWindow, type ItemKind } from '@/lib/itemWindow'
import { vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { matchProjectFile, parseLineRef, decorateFileLinks } from '@/lib/fileLinks'
import { openFileWindow } from '@/lib/fileWindow'
import { openSessionWindow } from '@/lib/sessionWindow'
import { IS_SESSION_WINDOW } from '@/lib/windowMode'
import { FILE_MENTION_EVENT } from '@/lib/fileEvents'
import { useMarkdown } from '@/lib/markdown'
import {
  entriesToPlainText,
  parseStreamLogWindow,
  modelFromPreamble,
  createStreamState,
  applyStreamEvent,
  type StreamEntry,
  type ImageAttachment,
} from '@/lib/streamEvents'
import { MODEL_OPTIONS, PERMISSION_MODE_OPTIONS, withCurrentModel, combinedModelOptions, engineForModel } from '@/lib/engineOptions'
import RemoteSessionModal from '@/components/remote/RemoteSessionModal.vue'
import { useRemoteControlStore } from '@/stores/remoteControl'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const projectStore = useProjectsStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const conductorStore = useConductorStore()
const tabsStore = useWorkspaceTabsStore()
const draftsStore = useChatDraftsStore()
const remoteControlStore = useRemoteControlStore()
const ghStore = useGithubAccountsStore()
const glStore = useGitlabAccountsStore()
const serversStore = useServersStore()
const awsStore = useAwsAccountsStore()
const claudeStore = useClaudeAccountsStore()
const appSettings = useAppSettingsStore()
const modelCatalog = useModelCatalogStore()
const uiLayout = useUILayoutStore()
const { confirm } = useConfirm()
const { toast } = useToast()

const projectId = computed(() => route.params.projectId as string)
const project = computed(() => projectStore.projects.find(p => p.id === projectId.value))

// True when THIS webview is a standalone session pop-out (see lib/sessionWindow).
// Such a window is locked to its one run: the session-switching chrome (left rail
// + the navigation buttons in the toolbar) is hidden so it stays a single
// conversation, and its own "pop out" button is suppressed. Read from windowMode
// (captured at module load) rather than the live URL, which the router rewrites
// once it navigates this window to the run.
const isSessionWindow = IS_SESSION_WINDOW

// GĐ6 (AC3): the project's active git account(s). Shows one badge per linked
// provider (GitHub first, then GitLab) so both attached accounts are visible.
const activeAccounts = computed(() => {
  // Tone per provider is kept in sync with the Projects list: GitHub=info,
  // GitLab=warning (server chips use status tones, AWS uses primary).
  const badges: { key: string; icon: typeof Github; tone: 'info' | 'warning'; username: string; title: string }[] = []
  const ghId = project.value?.github_account_id
  if (ghId) {
    const acc = ghStore.accounts.find(a => a.id === ghId)
    if (acc) {
      badges.push({
        key: 'github',
        icon: Github,
        tone: 'info',
        username: acc.username ? `@${acc.username}` : acc.label,
        title: `GitHub account: ${acc.label}${acc.username ? ` (@${acc.username})` : ''}`,
      })
    }
  }
  const glId = project.value?.gitlab_account_id
  if (glId) {
    const acc = glStore.accounts.find(a => a.id === glId)
    if (acc) {
      badges.push({
        key: 'gitlab',
        icon: Gitlab,
        tone: 'warning',
        username: acc.username ? `@${acc.username}` : acc.label,
        title: `GitLab account: ${acc.label}${acc.username ? ` (@${acc.username})` : ''}${acc.host ? ` on ${acc.host}` : ''}`,
      })
    }
  }
  return badges
})

// VPS servers mapped to this project — pre-wired for transparent SSH on each run.
// Loaded per-project (list_project_servers); the header surfaces which hosts the
// AI can reach, mirroring the git-account badges above.
const projectServers = ref<ProjectServer[]>([])
async function loadProjectServers() {
  try {
    projectServers.value = await serversStore.listForProject(projectId.value)
  } catch {
    projectServers.value = []
  }
}

// Max distinct server chips before collapsing the rest into a single "+N" chip,
// keeping the (narrow) header from overflowing when many servers are mapped.
const MAX_SERVER_CHIPS = 2
// Tone follows connection status; the shared Badge renders the solid high-contrast fill.
type ServerTone = 'success' | 'error' | 'neutral'
type ServerChip = { key: string; label: string; tone: ServerTone; title: string }
const serverChips = computed<ServerChip[]>(() =>
  projectServers.value.map((s) => {
    const tone: ServerTone = s.status === 'online' ? 'success' : s.status === 'offline' ? 'error' : 'neutral'
    const auth = s.auth_method === 'key' ? 'key' : 'ssh-agent'
    const status = s.status ? ` · ${s.status}` : ''
    return {
      key: `${s.id}-${s.role}`,
      label: s.label,
      tone,
      title: `${s.role}: ${s.username}@${s.host}:${s.port} · auth ${auth}${status}`,
    }
  }),
)
const visibleServerChips = computed(() => serverChips.value.slice(0, MAX_SERVER_CHIPS))
const hiddenServerChips = computed(() => serverChips.value.slice(MAX_SERVER_CHIPS))
const hiddenServerTitle = computed(() =>
  hiddenServerChips.value.map((c) => c.label).join('\n'),
)

// The project's linked AWS account (credentials brokered per run). Label + region
// on the chip; account id + ARN in the tooltip.
const awsChip = computed(() => {
  const id = project.value?.aws_account_id
  if (!id) return null
  const acc = awsStore.accounts.find((a) => a.id === id)
  if (!acc) return null
  const parts = [`AWS: ${acc.label}`]
  if (acc.account_id) parts.push(`account ${acc.account_id}`)
  parts.push(`region ${acc.region}`)
  const title = parts.join(' · ') + (acc.arn ? `\n${acc.arn}` : '')
  return { label: acc.label, region: acc.region, title }
})

const activeRunId = computed(() => route.params.runId as string | undefined)

const repos = ref<Repo[]>([])

// Fetch takes a URL and nothing else: the URL is the only input that states all
// three things a fetch needs — which repo, issue vs PR, and the number. A bare
// number is ambiguous on both the repo and the kind (GitHub shares one number
// space between issues and PRs), so it is rejected rather than guessed at.
const refInput = ref('')
const detectedRef = ref<
  { kind: 'issue' | 'pr'; number: number; repoId: string; repoName: string } | null
>(null)
const fetching = ref(false)
// The fetch form is collapsed by default so the run History gets the space.
const fetchOpen = ref(false)
// Left rail view: session controls/history, or the project file tree.
const leftTab = ref<'session' | 'files'>('session')

// Open a worker session (from the conductor sidebar) IN-PLACE, replacing the main
// view in the current window — the worker panel stays visible (anchored to the
// conductor, see conductorAnchorId) so the user can hop between workers or back to
// the conductor without a forest of OS windows.
async function openSession(runId: string) {
  if (!runId) return
  await loadRunLog(runId)
}
// Secondary action: pop a worker out into its OWN OS window (one window per run),
// for watching several workers side by side. Clicking the same worker again just
// focuses the window already showing it.
async function popoutSession(runId: string) {
  if (!runId) return
  await openSessionWindow(projectId.value, runId)
}
const fetchError = ref<string | null>(null)
const needsLinkedIssue = ref(false)
const linkedIssueInput = ref('')

const engineOverride = ref('')
const modelOverride = ref('')
// Engine that produced the currently loaded conversation. The selector can
// temporarily hold a not-yet-started override, so keep the persisted run engine
// explicit for labels, resume, and handoff decisions.
const loadedRunEngine = ref<string>('')
function resolveEngineChoice(value: string | null | undefined): string {
  return value || appSettings.settings?.default_engine || 'claude'
}
function syncLoadedRunEngine(engine: string) {
  engineOverride.value = engine
  loadedRunEngine.value = engine
}
// Effective engine for the next start/handoff (selector value, else default).
const effectiveEngine = computed(() => resolveEngineChoice(engineOverride.value))
const runSettingsOpen = ref(false)
const runSettingsRoot = ref<HTMLElement | null>(null)
const engineBadgeLabel = computed(() => effectiveEngine.value === 'codex' ? 'Codex' : 'Claude')
const engineBadgeIcon = computed(() => effectiveEngine.value === 'codex' ? Terminal : Cpu)
const runSettingsTitle = computed(() => t('run.runSettingsTitle', { engine: engineBadgeLabel.value }))
function toggleRunSettingsMenu() {
  runSettingsOpen.value = !runSettingsOpen.value
}
function closeRunSettingsMenu() {
  runSettingsOpen.value = false
}
function onRunSettingsPointerDown(e: MouseEvent) {
  if (!runSettingsOpen.value) return
  const target = e.target
  if (!(target instanceof Element)) return
  if (runSettingsRoot.value?.contains(target)) return
  if (target.closest('[data-app-select-dropdown]')) return
  closeRunSettingsMenu()
}
// One unified picker: Claude + Codex models in a single grouped list, so the
// chosen model implies the engine and no separate engine selector is needed.
// Option tables live in @/lib/engineOptions (shared with the remote controller):
// Claude's is curated + annotated with the last validation sweep; Codex's is
// whatever `codex debug models` last reported.
//
// The model that an empty (Default) pick resolves to, read reactively from the
// global settings so changing the default in Settings is reflected here at once.
const globalDefaultModel = computed(() =>
  (effectiveEngine.value === 'codex'
    ? appSettings.settings?.codex_model
    : appSettings.settings?.claude_model) || '',
)
const modelOptions = computed(() => {
  let claude = modelCatalog.withValidation(MODEL_OPTIONS.claude)
  let codex = modelCatalog.codexOptions(MODEL_OPTIONS.codex)
  // Keep an off-table saved pick selectable (a dropped/aliased id) by appending
  // it to the group matching the loaded run's engine.
  const cur = modelOverride.value
  if (cur && !claude.some(o => o.value === cur) && !codex.some(o => o.value === cur)) {
    const eng = loadedRunEngine.value || currentRun.value?.engine
    if (eng === 'codex') codex = withCurrentModel(codex, cur)
    else claude = withCurrentModel(claude, cur)
  }
  const def = globalDefaultModel.value
  return combinedModelOptions({
    claude,
    codex,
    defaultLabel: def ? t('run.modelDefaultWith', { model: def }) : t('run.modelDefault'),
    claudeLabel: 'Claude',
    codexLabel: 'Codex',
  })
})
// Picking a model drives the engine. Switching engine mid-conversation routes
// through the handoff flow (which remembers the model to apply after the switch).
const pendingEngineModel = ref<string | null>(null)
function onModelSelect(value: string) {
  const nextEngine = engineForModel(value, modelOptions.value) // '' = Default → global
  const currentEngine = loadedRunEngine.value || currentRun.value?.engine || effectiveEngine.value
  const resolvedNext = nextEngine || appSettings.settings?.default_engine || 'claude'
  const hasConversation =
    !!currentRunId.value &&
    currentStatus.value !== 'fetched' &&
    sourceText.value.trim().length > 0
  if (hasConversation && resolvedNext !== currentEngine) {
    pendingEngineModel.value = value
    closeRunSettingsMenu()
    handoffTarget.value = resolvedNext
    return
  }
  engineOverride.value = nextEngine
  modelOverride.value = value
}
const permissionMode = ref('')

// ── Run selectors: two levels only — global default, or per-session ────────
// Engine, model and permission have exactly two tiers: an empty selector value
// means "follow the global setting" (reactive), and any explicit pick applies
// to THIS run only. Per-run choices are persisted in the run's own meta
// (`set_run_meta` / the `run:meta:<id>` listener below), never project-wide, so
// one run's override never leaks into another session.

const outputEl = ref<HTMLDivElement | null>(null)
const historyEl = ref<HTMLDivElement | null>(null)
const sessionListEl = ref<HTMLDivElement | null>(null)
const currentRunId = ref<string | null>(null)

// Scroll the session-history list so the currently open session is visible.
// Called whenever the active run changes (click, deep link, back/forward).
function scrollActiveSessionIntoView() {
  const id = currentRunId.value
  if (!id) return
  nextTick(() => {
    const container = sessionListEl.value
    if (!container) return
    const el = container.querySelector<HTMLElement>(`[data-run-id="${CSS.escape(id)}"]`)
    if (el) el.scrollIntoView({ block: 'nearest' })
  })
}
// Remote-control modal: create a per-run link + OTP to drive this run from a phone.
const remoteModalOpen = ref(false)
// Remote state for the CURRENTLY viewed run, derived reactively from the global
// status. A paired phone drives EVERY run, so "is a phone attached" is a
// property of the session while "is it looking at this run" is per run.
// Deriving with computeds — instead of a ref refreshed imperatively — means
// switching sessions re-evaluates against THIS run's id immediately.
const remoteStatus = computed(() => remoteControlStore.status)
/** A phone has completed auth, so it can drive this (and any other) run. */
const remotePaired = computed(() => !!remoteStatus.value?.session_authenticated)
/** A pairing link exists but no phone has completed auth yet. */
const remoteWaiting = computed(
  () => !!remoteStatus.value?.bound_run_id && !remotePaired.value,
)
/** The paired phone is looking at THIS run right now. */
const remoteActive = computed(
  () => remotePaired.value && remoteStatus.value?.focus_run_id === currentRunId.value,
)
/**
 * Remote-state marker for the History list, so the user can tell AT A GLANCE
 * where the phone is without opening anything:
 *   success → the phone is looking at that run right now;
 *   warning → a pairing link exists but no phone has connected yet.
 */
const remoteRowMarker = computed<
  { runId: string; tone: 'success' | 'warning'; title: string } | null
>(() => {
  const st = remoteStatus.value
  if (!st) return null
  if (st.session_authenticated && st.focus_run_id) {
    return { runId: st.focus_run_id, tone: 'success', title: t('run.remoteRowDriving') }
  }
  if (st.bound_run_id) {
    return { runId: st.bound_run_id, tone: 'warning', title: t('run.remoteRowWaiting') }
  }
  return null
})
/** State-aware tooltip for the Remote button. */
const remoteButtonTitle = computed(() => {
  if (!currentRunId.value) return t('run.remoteBtnSelectRun')
  if (remoteActive.value) return t('run.remoteBtnActive')
  if (remotePaired.value) return t('run.remoteBtnPaired')
  if (remoteWaiting.value) return t('run.remoteBtnBoundHere')
  return t('run.remoteBtnDefault')
})
const remoteUnlisteners: UnlistenFn[] = []

// ── Remote meta sync ────────────────────────────────────────────────────────
// The composer's engine / model / permission-mode selection is the single source
// of truth shared with a remote controller. Local edits are pushed to the backend
// store (which fans them out to the controller); controller-originated edits are
// applied back onto these selectors. `applyingRemoteMeta` guards the echo so an
// applied value doesn't bounce straight back out.
let applyingRemoteMeta = false
let metaUnlisten: UnlistenFn | null = null
function pushRunMeta(): void {
  if (applyingRemoteMeta) return
  const id = currentRunId.value
  if (!id) return
  void invoke('set_run_meta', {
    runId: id,
    engine: effectiveEngine.value || null,
    model: modelOverride.value || null,
    permissionMode: permissionMode.value || null,
  })
}
watch([engineOverride, modelOverride, permissionMode], () => pushRunMeta())
watch(
  currentRunId,
  async (id) => {
    if (metaUnlisten) {
      metaUnlisten()
      metaUnlisten = null
    }
    if (!id) return
    metaUnlisten = await listen<{
      engine?: string | null
      model?: string | null
      permission_mode?: string | null
    }>(`run:meta:${id}`, (e) => {
      const m = e.payload
      applyingRemoteMeta = true
      if (m.engine) engineOverride.value = m.engine
      if (typeof m.model === 'string') modelOverride.value = m.model
      if (typeof m.permission_mode === 'string') permissionMode.value = m.permission_mode
      void nextTick(() => {
        applyingRemoteMeta = false
      })
    })
    // Seed the store with the current selection so a controller pairing right now
    // lands on the correct engine instead of a blank/default.
    pushRunMeta()
  },
  { immediate: true },
)

async function refreshRemoteActive(): Promise<void> {
  // Pull the latest global status into the store; the computeds above re-derive
  // this run's remote state from it (button dot + chip).
  try {
    await remoteControlStore.refreshStatus()
  } catch {
    // ignore — a stale status just means no remote indicator
  }
}
const viewingLogRunId = ref<string | null>(null)
const viewingLog = ref<string>('')

// History (on-disk log) view state — only used when viewing a finished run that
// has no in-memory live session. Kept separate from the live session so a
// background run can keep streaming into its own buffer.
// shallowRef: a persisted log is immutable once parsed, so we never mutate an
// entry in place. Skipping Vue's deep-reactive proxy over the whole (possibly
// large) array makes parsing/assigning + the first render markedly cheaper.
const historyEntries = shallowRef<StreamEntry[]>([])
const historyHasStream = ref(false)
// True while reading + parsing a persisted log from disk, so the viewer can show
// a loading animation instead of a blank/"Loading…" flash. Skipped on a focus
// refresh (we keep the current content on screen and swap it in silently).
const historyLoading = ref(false)
const historyToolIndex = new Map<string, number>()
// Context-window state reconstructed from a persisted log (history view).
const historyContextTokens = ref(0)
const historyModel = ref<string | null>(null)

// ── History paging ──────────────────────────────────────────────────────────
// Opening a run used to read the WHOLE log (up to ~29MB) over IPC and parse all
// of it, only to render the last 80 entries. Now the backend returns one window
// of records at a time and `historyEntries` holds exactly what has been loaded —
// it is no longer a full-log array with a slice on top.
//
// `historyCursor` is the byte offset the oldest loaded record starts at; feeding
// it back as `before` fetches the window just older than it.
const HISTORY_PAGE_INITIAL = 80
const HISTORY_PAGE_STEP = 120
const historyCursor = ref<number | null>(null)
const historyHasMore = ref(false)
const historyLoadingMore = ref(false)
// The window can sit mid-file after a jump (not just the tail), so we also track
// the BOTTOM edge: `historyBottomCursor` is the byte offset just past the last
// loaded record, `historyHasMoreBelow` is true when newer content remains on
// disk below it, and `historyFileSize` is the log's size (to compute the above).
const historyBottomCursor = ref<number | null>(null)
const historyHasMoreBelow = ref(false)
const historyFileSize = ref(0)
// Session-prompt index of the FIRST loaded turn, tracked only while the window is
// mid-file (after a jump). At the tail it is derived as a suffix instead — see
// `loadedTurnOffset`. Lets the navigator map loaded turns back to the full list
// even when the window isn't anchored to the end.
const firstLoadedSeq = ref(0)
// Briefly after a jump, ignore scroll-driven auto-loading: the programmatic
// scroll-to-top would otherwise instantly trigger a prepend and shove the landing
// out of view.
let historyAutoLoadBlockedUntil = 0
// Model id recovered from the page preamble (the `system.init` record at the top
// of the log). A tail window cannot contain it, and without it the context-window
// limit is unknown.
const historyPreambleModel = ref<string | null>(null)
// Fingerprint of the log currently on screen, so a refocus can skip the read
// entirely when nothing changed.
const historyRevision = ref<RunLogRevision | null>(null)
// Tool entries covering the WHOLE persisted run (not just the loaded window),
// used only to build the "files changed" list. shallowRef + a pruned payload
// keeps this small: the backend strips file bodies and tool output before
// sending, so this is orders of magnitude smaller than the log it came from.
const historyToolEntries = shallowRef<StreamEntry[]>([])

/**
 * Populate `historyToolEntries` for a persisted run.
 *
 * Fire-and-forget on purpose: the run view paints from its own page load, and
 * the "files changed" button simply appears once this resolves. Failing is
 * non-fatal — the button stays hidden rather than blocking the view.
 */
async function loadHistoryToolEntries(runId: string) {
  try {
    const records = await runsStore.getRunToolRecords(runId)
    if (viewingLogRunId.value !== runId) return
    historyToolEntries.value = parseStreamLogWindow(records).entries
  } catch {
    historyToolEntries.value = []
  }
}

// Load the window just older than what's on screen and prepend it, preserving
// the viewport: older entries push everything down, so bump scrollTop by exactly
// the height they added (measured across the next frame) and the content the
// user was reading stays put.
async function showEarlierHistory() {
  const runId = viewingLogRunId.value
  if (!runId || !historyHasMore.value || historyLoadingMore.value) return
  if (historyCursor.value === null) return
  historyLoadingMore.value = true
  const el = historyEl.value
  const beforeHeight = el?.scrollHeight ?? 0
  const beforeTop = el?.scrollTop ?? 0
  try {
    const page = await runsStore.getRunLogPage(runId, historyCursor.value, HISTORY_PAGE_STEP)
    // The run may have been switched while this was in flight.
    if (viewingLogRunId.value !== runId) return
    const parsed = parseStreamLogWindow(page.records, { seedModel: historyPreambleModel.value })
    historyCursor.value = page.cursor
    historyHasMore.value = page.has_more
    if (parsed.entries.length) {
      historyEntries.value = [...parsed.entries, ...historyEntries.value]
      // Older turns now sit before the window's first, so the first-loaded index
      // moves back by however many prompts we just prepended.
      const added = parsed.entries.reduce((n, e) => n + (e.kind === 'user' ? 1 : 0), 0)
      firstLoadedSeq.value = Math.max(0, firstLoadedSeq.value - added)
    }
    await nextTick()
    requestAnimationFrame(() => {
      const e = historyEl.value
      if (!e) return
      e.scrollTop = beforeTop + (e.scrollHeight - beforeHeight)
    })
  } catch {
    /* leave what's already loaded on screen */
  } finally {
    historyLoadingMore.value = false
  }
}

// Append the next window of newer records below the loaded ones — the mirror of
// `showEarlierHistory`. Needed once the window can sit mid-file (after a jump),
// so scrolling down past it keeps reading toward the end. New content lands below
// the viewport, so no scroll compensation is required.
async function showLaterHistory() {
  const runId = viewingLogRunId.value
  if (!runId || !historyHasMoreBelow.value || historyLoadingMore.value) return
  if (historyBottomCursor.value === null) return
  historyLoadingMore.value = true
  try {
    const page = await runsStore.getRunLogForward(runId, historyBottomCursor.value, HISTORY_PAGE_STEP)
    if (viewingLogRunId.value !== runId) return
    const parsed = parseStreamLogWindow(page.records, { seedModel: historyPreambleModel.value })
    historyBottomCursor.value = page.cursor
    historyHasMoreBelow.value = page.has_more
    if (parsed.entries.length) {
      historyEntries.value = [...historyEntries.value, ...parsed.entries]
    }
  } catch {
    /* leave what's already loaded on screen */
  } finally {
    historyLoadingMore.value = false
  }
}

// Replace the loaded window with a fresh one starting at a byte offset (the start
// of a prompt record, from the whole-session scan). One forward read instead of
// paging back from the tail — the heavy part of jumping to an old prompt.
async function jumpToHistoryOffset(offset: number) {
  const runId = viewingLogRunId.value
  if (!runId) return
  historyLoadingMore.value = true
  try {
    const page = await runsStore.getRunLogForward(runId, offset, HISTORY_PAGE_STEP)
    if (viewingLogRunId.value !== runId) return
    const parsed = parseStreamLogWindow(page.records, { seedModel: historyPreambleModel.value })
    historyEntries.value = parsed.entries
    historyCursor.value = offset
    historyHasMore.value = offset > 0
    historyBottomCursor.value = page.cursor
    historyHasMoreBelow.value = page.has_more
    historyFileSize.value = page.revision.size
    // The programmatic scroll that follows sits at the very top → would trip the
    // scroll-up auto-loader; hold it off briefly.
    historyAutoLoadBlockedUntil = Date.now() + 500
  } catch {
    /* leave what's already loaded on screen */
  } finally {
    historyLoadingMore.value = false
  }
}

const inputContent = ref<string>('')
const inputContentRunId = ref<string | null>(null)
const inputContentError = ref<string | null>(null)
const inputContentLoading = ref(false)

const currentRun = computed(() => runsStore.runs.find(r => r.id === currentRunId.value))
const currentIsSession = computed(() => currentRun.value?.run_type === 'session')

watch(
  () => [currentRun.value?.id, currentRun.value?.engine] as const,
  ([id, engine]) => {
    if (id && id === currentRunId.value && engine) syncLoadedRunEngine(engine)
  },
)

// Live streaming state for the focused run is owned by the liveRuns store so it
// survives navigation and lets multiple runs stream at once.
const session = computed(() => currentRunId.value ? live.get(currentRunId.value) : undefined)
// Viewing a run acknowledges its "run finished" notification in the Active runs
// dock. Fires when the focused run changes and when it finishes while on screen.
watch(
  [currentRunId, () => session.value?.notifyDone],
  () => {
    if (currentRunId.value && session.value?.notifyDone) live.markSeen(currentRunId.value)
  },
  { immediate: true },
)
const liveEntries = computed(() => session.value?.entries ?? [])
const liveOutputLines = computed(() => session.value?.outputLines ?? [])
const liveHasStream = computed(() => session.value?.hasStreamEvents ?? false)
// Context-window meter state for the focused run. Prefer the live session; for
// a past run with no live session, use the figures reconstructed from its log.
const contextTokens = computed(() => session.value?.contextTokens ?? historyContextTokens.value)
const contextModel = computed(() =>
  // `system.init` drops the `[1m]` suffix; re-attach it from the composer pick so
  // 1M runs resolve to the 1M context limit rather than 200K.
  mergeContextModel(
    session.value?.model ?? historyModel.value ?? currentRun.value?.engine ?? null,
    modelOverride.value,
  ),
)
// Model shown on the composer's run-settings badge: the live session's actual
// model when available, else the run's persisted model, else the pending pick
// or the resolved global default. Empty only when nothing is known yet.
const composerModelLabel = computed(() =>
  session.value?.model
  || historyModel.value
  || modelOverride.value
  || currentRun.value?.model
  || globalDefaultModel.value
  || '',
)
const contextRateLimit = computed(() => session.value?.rateLimit ?? null)
// Prefer the live session's status, fall back to the persisted run row.
const currentStatus = computed(() => session.value?.status ?? currentRun.value?.status ?? 'idle')
const currentSessionId = computed(() => session.value?.sessionId ?? currentRun.value?.session_id ?? null)

// ── Conductor ──────────────────────────────────────────────────────────────
// A conductor is just this session that also drives worker runs. When the open
// run is one, we show the worker sidebar. The conductor store tracks the worker
// tree event-driven (membership + timeline); per-worker *status* comes live from
// the `liveRuns` store, so a worker flipping running→done updates instantly
// without any backend round-trip.
const isConductor = computed(() => !!conductorStore.detail?.session)
const conductorWorkers = computed(() =>
  (conductorStore.detail?.workers ?? []).map((w) => ({
    ...w,
    // Prefer the live status (from run:activated/run:done events); fall back to
    // the DB snapshot for workers liveRuns isn't tracking (e.g. already finished).
    status: live.get(w.worker_id)?.status ?? w.status,
  })),
)
// The conductor's concurrent-worker cap (set at launch, editable live from the
// sidebar header). Shown next to the live worker count so the user sees headroom
// at a glance. (Distinct from `conductorMaxWorkers`, the launch default applied
// when a "New conductor" session sends its first message.)
const conductorWorkerCap = computed(() => conductorStore.detail?.session?.max_workers ?? 0)
const conductorEvents = computed(() => conductorStore.detail?.events ?? [])
// The conductor the worker panel is anchored to for the open run: the run itself
// when it's a conductor, otherwise its parent conductor when the open run is a
// worker (so viewing a worker in-place keeps the panel + its sibling list alive).
// Falls back to the run's own id for ordinary sessions (store self-clears those).
const conductorAnchorId = computed<string | null>(() => {
  const id = currentRunId.value
  if (!id) return null
  const meta = runsStore.runMeta.get(id) ?? runsStore.runs.find((r) => r.id === id)
  return meta?.conductor_run_id ?? id
})
const workerPanelOpen = ref(true)

// Conductor ids that currently have at least one worker streaming. A conductor's
// own status can be 'done' (it finished planning) while its workers keep running,
// so we surface the running state on the parent row too — its icon stays the live
// spinner until every worker settles. Worker status comes live from liveRuns,
// falling back to the DB snapshot for workers liveRuns isn't tracking.
const conductorIdsWithRunningWorker = computed(() => {
  const ids = new Set<string>()
  for (const r of runsStore.runs) {
    if (!r.conductor_run_id) continue
    const status = live.get(r.id)?.status ?? r.status
    if (status === 'running') ids.add(r.conductor_run_id)
  }
  return ids
})

// ── Worker-cap editing (sidebar header) ─────────────────────────────────────
// Two states share one editor:
//  • pre-launch (a "New conductor" session not yet started) — the cap edits the
//    local `conductorMaxWorkers` ref that seeds the launch; no backend row exists.
//  • running/finished conductor — the cap is raised/lowered live: the backend
//    clamps to [1,100], persists it, and the next session_spawn enforces it.
// The draft is a separate ref so the 2s detail poll can't clobber it mid-edit.
// Emitted value from <Input> is a string, so we parse on commit.
const editingWorkerCap = ref(false)
const workerCapDraft = ref<string | number>(0)
const savingWorkerCap = ref(false)
function beginEditWorkerCap() {
  workerCapDraft.value = effectiveWorkerCap.value || 1
  editingWorkerCap.value = true
}
function cancelEditWorkerCap() {
  editingWorkerCap.value = false
}
async function commitWorkerCap() {
  if (!editingWorkerCap.value) return
  editingWorkerCap.value = false
  const parsed = Math.round(Number(workerCapDraft.value))
  if (!Number.isFinite(parsed)) return
  const next = Math.min(100, Math.max(1, parsed))
  // Pre-launch conductor: no session row yet, so just remember the launch cap.
  if (!isConductor.value) {
    conductorMaxWorkers.value = next
    return
  }
  // Target the anchored conductor — the open run may be one of its workers.
  const id = conductorStore.selectedId
  if (!id || next === conductorWorkerCap.value) return
  savingWorkerCap.value = true
  try {
    await conductorStore.setMaxWorkers(id, next)
  } catch (e) {
    toast.error(t('conductor.setMaxWorkersFailed', { error: String(e) }))
  } finally {
    savingWorkerCap.value = false
  }
}

// Run ids created via "New conductor" whose goal has NOT been sent yet (so they
// have no role/conductor row in the DB). Tracked by id — not a single global
// bool — so the intent survives switching to another session: the History icon
// keeps flagging the run as a conductor, and returning to it still launches as a
// conductor. An id is dropped when its goal is sent (it becomes a real
// conductor) or the run is deleted. `conductorMaxWorkers` is the launch-time cap,
// editable from the sidebar before the goal is sent.
const pendingConductorIds = reactive(new Set<string>())
const conductorMaxWorkers = ref(10)
function isPendingConductor(runId: string | null | undefined): boolean {
  return !!runId && pendingConductorIds.has(runId)
}

// The OPEN run is a pending (pre-launch) conductor. We surface the worker sidebar
// in this state too so the user can set the worker limit BEFORE sending the goal.
const pendingConductor = computed(
  () => isPendingConductor(currentRunId.value) && !isConductor.value && currentStatus.value === 'fetched',
)
// Show the worker sidebar for a live conductor OR a pending (pre-launch) one.
const showWorkerPanel = computed(() => isConductor.value || pendingConductor.value)
// Cap shown/edited in the header: the persisted live cap once launched, else the
// local launch-time ref.
const effectiveWorkerCap = computed(() =>
  isConductor.value ? conductorWorkerCap.value : conductorMaxWorkers.value,
)

// Point the conductor store at the conductor anchoring the open run (itself if it
// IS a conductor, else its parent conductor when viewing a worker in-place); it
// self-clears when the run is a normal session. Watching the anchor — not the raw
// run id — means hopping between a conductor's workers keeps the same panel.
watch(conductorAnchorId, (id) => {
  void conductorStore.select(id ?? null)
  // Arriving at a new conductor resets the worker sidebar to expanded, so it
  // always reveals its workers instead of inheriting a previous manual collapse.
  workerPanelOpen.value = true
}, { immediate: true })
// Keep worker permission prompts flowing: listen to each running worker so its
// queue populates (and the sidebar's ⚠ flag lights up) even before it's opened.
watch(conductorWorkers, (ws) => {
  for (const w of ws) {
    if (w.status === 'running') void live.startListening(w.worker_id, projectId.value)
  }
}, { immediate: true })

// Sidebar/label text for a run: sessions show their title; issue/PR show "#N".
function runLabel(run: RunRecord): string {
  if (run.run_type === 'session') return run.title || t('run.labelSession')
  return `${run.run_type === 'analyze_issue' ? t('run.labelIssue') : t('run.labelPr')} #${run.ref_number}`
}

// A conductor is an ordinary session run tagged with role='conductor' (see
// start_conductor). The History list uses this to give it a distinct icon/tone.
// A freshly created "New conductor" session has no role in the DB until its goal
// is sent, so we also treat the open, pending-conductor run as a conductor — the
// History row then flags it the moment it is created, before launch.
function isConductorRun(run: RunRecord): boolean {
  if (run.run_type === 'session' && run.role === 'conductor') return true
  return isPendingConductor(run.id)
}

// History-row icon: conductor (Network) → plain session (MessageSquare) →
// issue (Bug) → PR (GitPullRequest). Kept as a function so the conductor case
// short-circuits before the plain-session one.
// A run is "running" for display when it is streaming itself, or — for a
// conductor — when any of its workers is still streaming.
function runIsRunning(run: RunRecord): boolean {
  if (run.status === 'running') return true
  return conductorIdsWithRunningWorker.value.has(run.id)
}

function runIcon(run: RunRecord) {
  if (runIsRunning(run)) return Loader2
  if (isConductorRun(run)) return Network
  if (run.run_type === 'session') return MessageSquare
  if (run.run_type === 'analyze_issue') return Bug
  return GitPullRequest
}

// History row: tint the session icon by run status so the list conveys state
// at a glance without a separate status pill. Mirrors StatusBadge's palette.
// A freshly-created chat session ('fetched' + session) is a draft → stays muted.
function runIconColorClass(run: RunRecord): string {
  if (runIsRunning(run)) return 'text-blue-500'
  if (run.status === 'fetched' && run.run_type === 'session') return 'text-muted-foreground'
  switch (run.status) {
    case 'running': return 'text-blue-500'
    case 'done': return 'text-emerald-500'
    case 'failed': return 'text-red-500'
    case 'cancelled': return 'text-amber-500'
    case 'fetched': return 'text-violet-500'
    default: return 'text-muted-foreground'
  }
}

// History row: which Claude account this run used. Only Claude runs record an
// account id (Codex has no per-run account), so we return a label just for
// engine 'claude' with a matching, known account — otherwise '' (no badge).
// Legacy runs with no run-level account fall back to the global ~/.claude, so
// they intentionally show nothing rather than a guessed account.
function runClaudeAccountLabel(run: RunRecord): string {
  if (run.engine !== 'claude' || !run.claude_account_id) return ''
  const acc = claudeStore.accounts.find((a) => a.id === run.claude_account_id)
  return acc ? acc.label : ''
}

// History row: the model this run runs on. Prefer the persisted per-run model
// (resolved at start/resume); fall back to the engine name for runs that never
// started or predate model persistence, so the badge is never empty.
function runModelLabel(run: RunRecord): string {
  return run.model || run.engine
}

// A freshly created session that was never run (status 'fetched', `started_at`
// still null) has no real model/account/activity yet — only `created_at`.
// Showing the engine name as a model and a "last activity" that is really the
// creation time reads as wrong. So for an unstarted session the meta block is
// made `invisible` (kept in the layout, so the row stays the same height as a
// full one) rather than removed. `started_at` is set the moment a run begins,
// so it is the clean signal that there is real info worth showing.
function runStarted(run: RunRecord): boolean {
  return !!run.started_at
}

// History row timestamp: date + time, so runs from the same day stay
// distinguishable. Falls back to the raw string if the date can't be parsed.
// Fed with `runActivityAt(run)` — the same value the list sorts by, so the
// printed time always explains the row's position.
function runTimestamp(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString(undefined, {
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  })
}

// ── History search ────────────────────────────────────────────────────────
// Free-text filter over the History list. Matching is done on what the row
// actually shows (title/label, engine, status, issue/PR number) so whatever the
// user reads in the list is also what they can type to find it again.
const sessionSearch = ref('')
const filteredRuns = computed(() => {
  const q = sessionSearch.value.trim().toLowerCase()
  if (!q) return runsStore.runs
  return runsStore.runs.filter((run) => {
    const haystack = [
      runLabel(run),
      run.title ?? '',
      run.engine,
      run.status,
      run.run_type,
      run.ref_number != null ? `#${run.ref_number}` : '',
    ]
      .join(' ')
      .toLowerCase()
    return haystack.includes(q)
  })
})

// ── History list ───────────────────────────────────────────────────────────
// Worker sessions (run.conductor_run_id set) NEVER appear in the History list —
// they live solely in the conductor's right-side worker panel. The History list
// therefore shows only conductors and ordinary sessions/issues/PRs, each row
// acting on its own run.id.
interface DisplayRun {
  run: RunRecord
}

// Flat view of `filteredRuns` with every worker filtered out (both workers
// nested under a listed conductor and orphan workers whose conductor isn't
// listed). Top-level order follows the store's sort.
const displayRuns = computed<DisplayRun[]>(() => {
  const out: DisplayRun[] = []
  for (const run of filteredRuns.value) {
    if (run.conductor_run_id) continue
    out.push({ run })
  }
  return out
})

// Ids of conductors the user has protected. Protection cascades to their
// workers for "Clear all" (the bulk cleanup keeps them), so we badge those
// workers too even though their own `protected` flag stays false — they remain
// individually deletable.
const protectedConductorIds = computed(
  () =>
    new Set(
      runsStore.runs
        .filter((r) => r.role === 'conductor' && r.protected)
        .map((r) => r.id),
    ),
)
function isProtectedByConductor(run: RunRecord): boolean {
  return (
    !run.protected &&
    run.conductor_run_id != null &&
    protectedConductorIds.value.has(run.conductor_run_id)
  )
}

// A run awaiting a permission / question response (front of its live queue), or
// undefined. Drives the animated attention icon in the History list so the user
// knows which run needs them without a floating toast.
function pendingRequest(runId: string) {
  return live.get(runId)?.permissionQueue[0]
}

// Build the GitHub URL for an issue/PR run, or null if we can't resolve the repo.
function runGithubUrl(run: RunRecord): string | null {
  if (run.run_type === 'session' || run.ref_number == null) return null
  const repo = repos.value.find(r => r.id === run.repo_id)
  if (!repo?.github_owner || !repo?.github_repo) return null
  const kind = run.run_type === 'analyze_issue' ? 'issues' : 'pull'
  return `https://github.com/${repo.github_owner}/${repo.github_repo}/${kind}/${run.ref_number}`
}

async function openRunInBrowser(run: RunRecord) {
  const url = runGithubUrl(run)
  if (url) await openUrl(url)
}

const { renderText, loadMarkdown } = useMarkdown()

// `viewingLogRunId` is set only while showing an on-disk log (it's cleared the
// moment a run goes live), so its presence alone means we're in history view.
// This also surfaces a partial log recovered from disk for a run left as
// 'running' by a previous app session that died mid-run.
const isViewingHistory = computed(() => !!viewingLogRunId.value)
// Entries currently shown in the AI Result column — drives the "files changed"
// quick-access list so it matches whatever the user is looking at.
// Entries the "files changed" list is derived from.
//
// It must reflect the WHOLE run, not the window currently on screen — otherwise
// a long conversation silently under-reports what it changed. For a live session
// the in-memory entries already are the whole run. For a persisted one they are
// only a window, so the backend scans the log and returns the (pruned) tool
// records instead; see `loadHistoryToolEntries`.
const mentionedFileEntries = computed(() =>
  isViewingHistory.value ? historyToolEntries.value : liveEntries.value,
)
const hasLiveOutput = computed(
  () => liveOutputLines.value.length > 0 || liveEntries.value.length > 0 || currentStatus.value === 'running'
)
// Split-pane state: percentage of the left (Content) column.
const splitContainerEl = ref<HTMLElement | null>(null)
const leftWidthPct = ref(50)
const isResizing = ref(false)

// Container for the AI Result column (left: AI result, right: optional content).
const resultSplitEl = ref<HTMLElement | null>(null)

// Panel visibility: the AI Result column is always on — issue/PR runs can pull
// the fetched Content in as a left split column via the header toggle. Sessions
// have no issue/PR content, so Content is never available for them.
const showContent = ref(false)
const canToggleContent = computed(() => !uiLayout.focusMode && !currentIsSession.value)
const contentVisible = computed(() => canToggleContent.value && showContent.value)
const showResizeHandle = computed(() => contentVisible.value)
// No run is selected AND the project has none — hide the Content / AI Result
// split entirely and show a single centered "No sessions yet" empty state.
const noSession = computed(() => !currentRunId.value && runsStore.runs.length === 0)
const contentWidth = computed(() => (showResizeHandle.value ? leftWidthPct.value + '%' : '100%'))
const resultWidth = computed(() => (showResizeHandle.value ? 100 - leftWidthPct.value + '%' : '100%'))

function startResize(e: MouseEvent) {
  e.preventDefault()
  isResizing.value = true
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
  window.addEventListener('mousemove', onResize)
  window.addEventListener('mouseup', stopResize)
}

function onResize(e: MouseEvent) {
  if (!splitContainerEl.value) return
  const rect = splitContainerEl.value.getBoundingClientRect()
  const pct = ((e.clientX - rect.left) / rect.width) * 100
  leftWidthPct.value = Math.max(20, Math.min(80, pct))
}

function stopResize() {
  isResizing.value = false
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  window.removeEventListener('mousemove', onResize)
  window.removeEventListener('mouseup', stopResize)
}

const sourceText = computed(() => {
  if (isViewingHistory.value) {
    if (historyHasStream.value) return entriesToPlainText(historyEntries.value)
    return viewingLog.value
  }
  if (liveHasStream.value) return entriesToPlainText(liveEntries.value)
  return liveOutputLines.value.map(l => l.text).join('\n')
})

// Legacy (non-stream) markdown for each view column.
const liveLegacyHtml = computed(() => liveHasStream.value ? '' : renderText(
  liveOutputLines.value.map(l => l.text).join('\n')
))
const historyLegacyHtml = computed(() => historyHasStream.value ? '' : renderText(viewingLog.value))

// Whether the live output is "pinned" to the bottom. We only auto-scroll when
// the user is already near the bottom; if they scroll up to read earlier
// output, we leave their position alone until they come back down.
const stickToBottom = ref(true)
const SCROLL_PIN_THRESHOLD = 80 // px from the bottom counted as "at bottom"

function isNearBottom(el: HTMLElement) {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= SCROLL_PIN_THRESHOLD
}

function scrollOutputToBottom() {
  nextTick(() => {
    if (outputEl.value) outputEl.value.scrollTop = outputEl.value.scrollHeight
  })
}

// Re-evaluate the pin whenever the user scrolls the live output.
function onOutputScroll() {
  // The floating translate button is anchored to a viewport rect that scrolling
  // invalidates — hide it (the popover, once open, dismisses on its own).
  clearTranslateTrigger()
  scheduleScrollSettle()
}

// --- Scroll anchoring across layout resizes ---------------------------------
// When the question panel opens/closes (or the split is dragged), the output
// column changes width, text re-wraps and the total scrollHeight changes. A
// pixel-based scrollTop then points at different content, so the reader jumps.
// We fix this by anchoring on an *element* near the top of the viewport and
// restoring scrollTop after the reflow so that element stays put.
type ScrollAnchor = { bottom: true } | { el: Element; offset: number } | null
let scrollAnchor: ScrollAnchor = null

// The scrollable output may wrap its content in a single container (StreamLog's
// `.space-y-3` root, or the legacy markdown div). Descend through single-child
// wrappers to reach the actual list of block entries we can anchor on.
function outputBlocks(root: HTMLElement): Element[] {
  let node: Element = root
  while (node.children.length === 1) node = node.children[0]
  return Array.from(node.children)
}

function captureScrollAnchor() {
  const el = outputEl.value
  if (!el) return
  // If pinned to the bottom, keep it pinned — no element anchor needed.
  if (stickToBottom.value) { scrollAnchor = { bottom: true }; return }
  const top = el.getBoundingClientRect().top
  for (const node of outputBlocks(el)) {
    const r = node.getBoundingClientRect()
    if (r.height > 0 && r.bottom >= top) {
      scrollAnchor = { el: node, offset: r.top - top }
      return
    }
  }
  scrollAnchor = null
}

// Everything a scroll has to recompute reads layout: `scrollHeight` for the
// bottom pin, entry rects for the anchor. A trackpad emits several scroll events
// per frame, and each of those reads forces a synchronous layout mid-scroll —
// that is the stutter. Coalesce into one read per frame; both values only have
// to be current by the time the frame is painted.
let scrollSettleFrame = 0
function scheduleScrollSettle() {
  if (scrollSettleFrame) return
  scrollSettleFrame = requestAnimationFrame(() => {
    scrollSettleFrame = 0
    // Pointer interaction pauses auto-follow. Do not silently re-enable it while
    // the pointer is still down (for example while drag-selecting text).
    if (outputEl.value && !pointerDownInOutput.value) {
      stickToBottom.value = isNearBottom(outputEl.value)
    }
    captureScrollAnchor()
  })
}

function restoreScrollAnchor() {
  const el = outputEl.value
  const a = scrollAnchor
  if (!el || !a) return
  if ('bottom' in a) { el.scrollTop = el.scrollHeight; return }
  const top = el.getBoundingClientRect().top
  const delta = (a.el.getBoundingClientRect().top - top) - a.offset
  if (delta) el.scrollTop += delta
}

// Attach a ResizeObserver to the output element whenever it mounts, so any
// width/height change restores the reading position. The output appears and
// disappears via v-if, so we track the ref rather than observing once at mount.
let outputRO: ResizeObserver | null = null
watch(outputEl, (el) => {
  outputRO?.disconnect()
  outputRO = null
  if (el) {
    outputRO = new ResizeObserver(() => restoreScrollAnchor())
    outputRO.observe(el)
  }
})

// ── Jump between user prompts ───────────────────────────────────────────────
// In a long run, scrolling back to "where did I ask for this?" is a long drag
// past tool calls and assistant output. The user's own messages are the
// landmarks worth landing on, so they get their own navigation. It drives
// whichever pane is showing — the live output or a history log.
const turnScrollEl = computed(() => (isViewingHistory.value ? historyEl.value : outputEl.value))
const turnEntries = computed(() => (isViewingHistory.value ? historyEntries.value : liveEntries.value))
const {
  turns: promptTurns,
  scrollToTurn,
  activeEntryIndex,
} = useTurnNavigator(turnScrollEl, turnEntries)

// The navigator must list EVERY prompt in the session, but the viewer only
// holds a window (the tail of a history log), so `promptTurns` — derived from
// what's on screen — under-reports. `sessionTurns` is the whole-session list,
// scanned from the log in Rust and parsed with the SAME parser (see
// `loadSessionTurns`); empty until that resolves, when we fall back to the
// loaded turns.
type SessionTurn = { label: string; offset: number }
const sessionTurns = shallowRef<SessionTurn[]>([])

async function loadSessionTurns(runId: string) {
  try {
    const records = await runsStore.getRunUserRecords(runId)
    if (currentRunId.value !== runId) return
    // Feed the pruned user records through the SAME parser the main view uses,
    // one at a time, so compaction summaries / slash-command messages are
    // filtered identically — and tag each resulting prompt with the byte offset
    // of the record that produced it (for jump-to-offset). A real user prompt is
    // always the last entry pushed by its record, so identity-compare the tail.
    const state = createStreamState()
    const turns: SessionTurn[] = []
    let lastUser: StreamEntry | null = null
    for (const r of records) {
      try {
        applyStreamEvent(state, JSON.parse(r.json))
      } catch {
        continue
      }
      const last = state.entries[state.entries.length - 1]
      if (last && last.kind === 'user' && last !== lastUser) {
        lastUser = last
        turns.push({ label: turnLabel(last.text), offset: r.offset })
      }
    }
    sessionTurns.value = turns
  } catch {
    if (currentRunId.value === runId) sessionTurns.value = []
  }
}

// Rescan whenever the focused run changes. The scan reads the persisted log that
// both the live and history panes share, so it covers either mode.
watch(
  currentRunId,
  (id) => {
    sessionTurns.value = []
    if (id) void loadSessionTurns(id)
  },
  { immediate: true },
)

// The list that actually drives the navigator: the whole-session scan when it's
// at least as complete as what's loaded, otherwise the loaded turns (covers a
// live run that just produced a prompt the scan predates, and the pre-scan gap).
const navTurns = computed<{ label: string; offset?: number }[]>(() =>
  sessionTurns.value.length >= promptTurns.value.length && sessionTurns.value.length > 0
    ? sessionTurns.value
    : promptTurns.value.map((t) => ({ label: t.label })),
)

// Session-prompt index of the loaded window's first turn. While more content
// remains BELOW (a mid-file window, i.e. after a jump), that index is tracked
// explicitly in `firstLoadedSeq`. Otherwise the window reaches the end, so the
// loaded turns are the tail — a plain suffix of the full list — which also stays
// correct for the live pane.
const loadedTurnOffset = computed(() =>
  historyHasMoreBelow.value
    ? firstLoadedSeq.value
    : Math.max(0, navTurns.value.length - promptTurns.value.length),
)

// One landmark is nothing to navigate between, and the legacy (non-stream)
// renderer has no turn markers to walk.
const showTurnNav = computed(
  () =>
    navTurns.value.length > 1 &&
    !!turnScrollEl.value &&
    (isViewingHistory.value ? historyHasStream.value : liveHasStream.value),
)

// Which prompt (as an index into the full `navTurns`) the reader is parked on,
// marked in the jump list. Resolved when the menu opens: keeping it live would
// mean reading every turn's layout on every scroll, which is exactly the cost
// the output pane works to avoid.
const activeTurnSeq = ref(-1)
function loadedTurnIndexOf(entryIndex: number): number {
  return promptTurns.value.findIndex((t) => t.entryIndex === entryIndex)
}
// Full-list index of the turn currently at the top, or -1 when above the first
// loaded one.
function activeSessionSeq(): number {
  const li = loadedTurnIndexOf(activeEntryIndex())
  return li < 0 ? -1 : loadedTurnOffset.value + li
}
function onTurnMenuToggle(open: boolean) {
  if (open) activeTurnSeq.value = activeSessionSeq()
}

// Jumping away from the bottom has to release the auto-follow pin, or the next
// streamed chunk yanks the reader straight back down.
function afterTurnJump() {
  const el = outputEl.value
  if (el && !isViewingHistory.value) stickToBottom.value = isNearBottom(el)
}

function goToTurn(entryIndex: number, smooth = false) {
  scrollToTurn(entryIndex, smooth)
  afterTurnJump()
}

// Jump to a prompt by its position in the WHOLE session. `smooth` glides to the
// landing — used for an explicit pick from the list; prev/next step instantly so
// rapid presses stay snappy.
async function jumpToSessionTurn(seq: number, smooth = false) {
  if (seq < 0 || seq >= navTurns.value.length) return

  // Already in the loaded window → just scroll, no reload.
  const localIfLoaded = seq - loadedTurnOffset.value
  if (localIfLoaded >= 0 && localIfLoaded < promptTurns.value.length) {
    goToTurn(promptTurns.value[localIfLoaded].entryIndex, smooth)
    return
  }

  // History + we know the prompt's byte offset → load a fresh window right there
  // in one read, then land on it (now the window's first turn).
  const offset = navTurns.value[seq]?.offset
  if (isViewingHistory.value && typeof offset === 'number') {
    firstLoadedSeq.value = seq
    await jumpToHistoryOffset(offset)
    await nextTick()
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
    const turn = promptTurns.value[seq - loadedTurnOffset.value]
    if (turn) goToTurn(turn.entryIndex, smooth)
    return
  }

  // Fallback (live pane, or offset not scanned yet): page backwards until the
  // target is in, then scroll.
  if (isViewingHistory.value) {
    let guard = 0
    while (seq < loadedTurnOffset.value && historyHasMore.value && !historyLoadingMore.value && guard < 500) {
      await showEarlierHistory()
      guard++
    }
    await nextTick()
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()))
  }
  const turn = promptTurns.value[seq - loadedTurnOffset.value]
  if (turn) goToTurn(turn.entryIndex, smooth)
}

function prevTurn() {
  const cur = activeSessionSeq()
  // -1 means the reader is above the first loaded turn — step to the one just
  // before the loaded window.
  const target = cur < 0 ? loadedTurnOffset.value - 1 : cur - 1
  if (target >= 0) void jumpToSessionTurn(target)
}

function nextTurn() {
  const cur = activeSessionSeq()
  const target = cur < 0 ? loadedTurnOffset.value : cur + 1
  if (target < navTurns.value.length) void jumpToSessionTurn(target)
}

// ⌘/Ctrl+⌥+↑/↓. The bare arrows — and their plain ⌘ or ⌥ variants — all move the
// caret inside the composer's textarea, so the shortcut takes the one
// combination both macOS and the text field leave free.
function onTurnNavKey(e: KeyboardEvent) {
  if (!(e.metaKey || e.ctrlKey) || !e.altKey) return
  if (e.key !== 'ArrowUp' && e.key !== 'ArrowDown') return
  if (!showTurnNav.value) return
  e.preventDefault()
  if (e.key === 'ArrowUp') prevTurn()
  else nextTurn()
}

// While the user is actively interacting with the output — holding the mouse
// down to drag-select, or with text already selected inside it — we must NOT
// yank the view to the bottom when new streamed content arrives. Otherwise a
// click/selection near the bottom gets interrupted the moment a new chunk lands.
const pointerDownInOutput = ref(false)
function onOutputPointerDown() {
  pointerDownInOutput.value = true
  // A click means the user is reading/interacting with the current content.
  // Pause auto-follow until they deliberately scroll back to the bottom.
  stickToBottom.value = false
}
function onWindowPointerUp() {
  pointerDownInOutput.value = false
}
function hasOutputSelection() {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed || !outputEl.value) return false
  return (
    (sel.anchorNode != null && outputEl.value.contains(sel.anchorNode)) ||
    (sel.focusNode != null && outputEl.value.contains(sel.focusNode))
  )
}

// ── Translate selection ─────────────────────────────────────────────────────
// Drag-selecting text inside the AI-result output (live or history) surfaces a
// small floating "Dịch" button; clicking it opens a TranslatePopover that calls
// the one-shot `translate_text` backend command. Detection is a window mouseup
// listener that checks the selection lands inside the output/history container.
const translateTrigger = ref<{ text: string; x: number; y: number } | null>(null)
const activeTranslation = ref<{ text: string; x: number; y: number } | null>(null)
const defaultTranslateLang = computed(() => appSettings.settings?.translate_target_lang || 'vi')

function clearTranslateTrigger() {
  translateTrigger.value = null
}

// Auto-load the history window as the reader approaches either edge: older
// records when near the top, newer ones when near the bottom. Held off briefly
// right after a jump (the programmatic scroll would otherwise instantly prepend).
const HISTORY_SCROLL_MARGIN = 400
function onHistoryScroll() {
  clearTranslateTrigger()
  const el = historyEl.value
  if (!el || historyLoadingMore.value || Date.now() < historyAutoLoadBlockedUntil) return
  if (el.scrollTop < HISTORY_SCROLL_MARGIN && historyHasMore.value) {
    void showEarlierHistory()
  } else if (
    el.scrollHeight - el.scrollTop - el.clientHeight < HISTORY_SCROLL_MARGIN &&
    historyHasMoreBelow.value
  ) {
    void showLaterHistory()
  }
}

function nodeInOutput(node: Node | null): boolean {
  if (!node) return false
  return !!outputEl.value?.contains(node) || !!historyEl.value?.contains(node)
}

function onSelectionMouseUp() {
  const sel = window.getSelection()
  if (!sel || sel.isCollapsed) { translateTrigger.value = null; return }
  const text = sel.toString().trim()
  if (text.length < 2) { translateTrigger.value = null; return }
  if (!nodeInOutput(sel.anchorNode) && !nodeInOutput(sel.focusNode)) {
    translateTrigger.value = null
    return
  }
  let rect: DOMRect | null = null
  try { rect = sel.getRangeAt(0).getBoundingClientRect() } catch { rect = null }
  if (!rect || (rect.width === 0 && rect.height === 0)) { translateTrigger.value = null; return }
  translateTrigger.value = { text, x: rect.left, y: rect.bottom + 6 }
  // Warm the translation sidecar now, while the user reaches for the button, so
  // the first translation isn't paying Node/SDK cold-start.
  void invoke('prewarm_translate').catch(() => {})
}

function openTranslate() {
  const t = translateTrigger.value
  if (!t) return
  activeTranslation.value = { ...t }
  translateTrigger.value = null
}

function closeTranslate() {
  activeTranslation.value = null
}

// ── Quick capture from a run ────────────────────────────────────────────────
// Jotting a thought down mid-run must not cost the user this screen: the capture
// opens in the standalone item window (its own OS window, so no route change and
// no overlay — the stream keeps its scroll and every open panel survives),
// pre-filed under this project and run.
function captureContext() {
  return { projectId: projectId.value, runId: currentRunId.value }
}

function openQuickCapture(kind: ItemKind) {
  openItemCreateWindow(kind, captureContext())
}

/** Turn the current output selection into a pre-filled note. */
function captureSelectionAsNote() {
  const trigger = translateTrigger.value
  if (!trigger) return
  openItemCreateWindow('note', captureContext(), {
    title: currentRun.value?.title ?? project.value?.name ?? '',
    content: trigger.text,
  })
  translateTrigger.value = null
}

/** Turn the current output selection into a pre-filled todo. */
function captureSelectionAsTodo() {
  const trigger = translateTrigger.value
  if (!trigger) return
  openItemCreateWindow('todo', captureContext(), { content: trigger.text })
  translateTrigger.value = null
}

// Keep the live output pinned to the bottom as new entries/lines arrive for
// the focused, running run — but only while the user is parked at the bottom
// and not in the middle of selecting/clicking inside the output.
// Whether we should currently keep the output pinned to the bottom: the run is
// live, we're not in history view, the user is parked at the bottom, and isn't
// mid-selection/click inside the output.
function shouldPinToBottom() {
  return (
    currentStatus.value === 'running' &&
    !isViewingHistory.value &&
    stickToBottom.value &&
    !pointerDownInOutput.value &&
    !hasOutputSelection()
  )
}

// Pin in a single requestAnimationFrame — this runs AFTER Vue patches the stream
// and the browser has computed layout, so we read `scrollHeight` exactly once
// per frame instead of forcing a reflow on every streamed event (the old nested
// nextTicks + repeated scrollHeight reads were the layout-thrash source). The
// guards are re-checked inside the frame: a pointer interaction may begin after
// the watcher fires but before this callback runs.
function keepPinnedToBottom() {
  if (!shouldPinToBottom()) return
  requestAnimationFrame(() => {
    if (shouldPinToBottom() && outputEl.value) {
      outputEl.value.scrollTop = outputEl.value.scrollHeight
    }
  })
}

watch(
  () => [liveEntries.value.length, liveOutputLines.value.length, currentStatus.value] as const,
  keepPinnedToBottom
)

// When switching to another run's live output, jump to its latest output.
watch(currentRunId, (id) => {
  stickToBottom.value = true
  scrollOutputToBottom()
  scrollActiveSessionIntoView()
  closeRunSettingsMenu()
  clearTranslateTrigger()
  closeTranslate()
  // Keep the URL in sync with the selected run. Selecting a run from the History
  // list sets `currentRunId` locally but the route would otherwise stay at
  // `/projects/:projectId` with no runId — so a reload/remount (app backgrounding
  // for a notification, focus/resume, HMR) lands on the "no run selected" empty
  // state, and PermissionNotifier mistakes the on-screen run for a background one.
  // Mirroring it into the URL makes the selection survive remounts and lets
  // app-wide components tell which run is actually open.
  const current = typeof route.params.runId === 'string' ? route.params.runId : undefined
  if (current === (id ?? undefined)) return
  const target = id
    ? { name: 'project-run-detail', params: { projectId: projectId.value, runId: id } }
    : { name: 'project-run', params: { projectId: projectId.value } }
  router.replace(target).catch(() => {})
})

// Follow route-driven run changes (the OS-notification deep link in
// PermissionNotifier, browser back/forward). onMounted handles the first load;
// this keeps the view in sync for later route changes without a remount.
watch(activeRunId, (id) => {
  if (id && id !== currentRunId.value) loadRunLog(id)
})

// Keep the server chips in sync if the active project changes without a remount.
watch(projectId, () => {
  loadProjectServers()
})

onMounted(async () => {
  loadMarkdown()
  nextTick(autoResizeComposer)
  document.addEventListener('mousedown', onRunSettingsPointerDown)
  // File → New Session (⌘N). Only this screen can serve it: starting a session
  // needs the project the user is currently in.
  unbindNewSessionMenu = registerMenuAction('file.newSession', () => {
    void handleNewSession()
  })
  // Hydrate the persisted model caches so the composer's picker shows the last
  // refreshed lists (no discovery here — refresh is manual in Settings).
  modelCatalog.ensureLoaded().catch(() => {})
  // Needed for the composer's saved-prompt dropdown (and the engine default).
  appSettings.ensureLoaded().catch(() => {})
  if (projectStore.projects.length === 0) {
    await projectStore.fetchProjects()
  }
  // GĐ6 (AC3): ensure account metadata is available for the header badge.
  if (ghStore.accounts.length === 0) ghStore.fetch().catch(() => {})
  if (glStore.accounts.length === 0) glStore.fetch().catch(() => {})
  if (claudeStore.accounts.length === 0) claudeStore.fetch().catch(() => {})
  // Header chips: which VPS servers + AWS account this project is wired to.
  if (awsStore.accounts.length === 0) awsStore.fetch().catch(() => {})
  loadProjectServers()
  repos.value = await projectStore.listRepos(projectId.value)
  await runsStore.fetchRuns(projectId.value)
  // Preload the project file list so inline-code mentions resolve to links.
  ensureProjectFiles().catch(() => {})
  if (activeRunId.value) {
    await loadRunLog(activeRunId.value)
  } else {
    // Bare project route (no runId): reopen the most recent session so the user
    // lands back on their last conversation instead of an empty screen. Runs are
    // sorted pinned-first then last-activity desc, so the first one is the one
    // the user worked on most recently.
    // With no runs at all, the "chưa có session nào" empty state is shown.
    const latest = runsStore.runs[0]
    if (latest) await loadRunLog(latest.id)
  }
  window.addEventListener('focus', onAppFocus)
  window.addEventListener('pointerup', onWindowPointerUp)
  window.addEventListener('mouseup', onSelectionMouseUp)
  window.addEventListener('keydown', onTurnNavKey)
  setupMentionBridge()

  // Remote-control live indicator: reflect whether a phone is actively driving
  // THIS run, updated by the host agent events (and an initial status read).
  refreshRemoteActive()
  for (const evt of [
    'remote://connection-authenticated',
    'remote://disconnected',
    'remote://auth-failed',
    'remote://session-expired',
  ]) {
    remoteUnlisteners.push(await listen(evt, () => refreshRemoteActive()))
  }

  // Drag-and-drop files into the composer. Tauri intercepts OS file drops at the
  // native layer (dragDropEnabled defaults on), so the webview's HTML5 `drop`
  // carries no paths — we must use the native event, which gives absolute paths.
  dragDropUnlisten = await getCurrentWebview().onDragDropEvent((event) => {
    if (!composerVisible.value) return
    const type = event.payload.type
    if (type === 'enter' || type === 'over') {
      isDraggingFile.value = true
    } else if (type === 'leave') {
      isDraggingFile.value = false
    } else if (type === 'drop') {
      isDraggingFile.value = false
      void addDroppedPaths(event.payload.paths)
    }
  })

  // Mirror in any sessions created/continued outside Devdy while it was closed
  // (both engines), then surface them in the sidebar.
  Promise.all([
    runsStore.reconcileClaudeSessions(projectId.value).catch(() => 0),
    runsStore.reconcileCodexSessions(projectId.value).catch(() => 0),
  ]).then(([a, b]) => { if (a + b > 0) runsStore.fetchRuns(projectId.value) })

  // Live mirror: the backend watches the shared Claude transcript store and
  // emits this when a session changes (new turns from the CLI / VS Code, or a
  // brand-new external session). Refetch the list and reload the viewed run.
  sessionsChangedUnlisten = await listen<{ project_id: string; run_id?: string }>(
    'sessions:changed',
    (event) => {
      if (event.payload?.project_id !== projectId.value) return
      // Always refresh the sidebar list. Only reload the on-screen log when the
      // run that changed is the one being viewed — other sessions in the project
      // changing shouldn't force a reload of the open conversation.
      runsStore.fetchRuns(projectId.value)
      if (event.payload.run_id && event.payload.run_id === currentRunId.value) {
        refreshViewedRunOnFocus(true)
      }
    },
  )
})

// When the window regains focus, the run on screen may have been continued
// outside Devdy (claude CLI / VS Code extension write to the same session
// transcript). Re-read its on-disk log so those external turns show up.
// Skip live sessions — those stream their own updates in real time.
let sessionsChangedUnlisten: UnlistenFn | null = null
let dragDropUnlisten: UnlistenFn | null = null

// `force` = the on-disk transcript actually changed (the sessions:changed file
// watcher fired), so we must re-read even a run we hold live in memory. A plain
// window refocus passes force=false: it must NOT blank + re-render the viewed
// run (that was the visible "flash" on every refocus). loadRunLog then keeps a
// live session on screen as-is, and for a history view re-reads but no-ops when
// the content is byte-identical.
async function refreshViewedRunOnFocus(force = false) {
  const id = currentRunId.value
  if (!id) return
  const ls = live.get(id)
  if (ls && ls.status === 'running') return
  await loadRunLog(id, { preferDisk: true, force })
}

// Wrapper for the window 'focus' event: the DOM passes an Event as the first
// arg, which must not be read as `force` (it's truthy).
function onAppFocus() {
  void refreshViewedRunOnFocus(false)
  // Safety-net for the event-driven conductor panel: catch any `conductor:changed`
  // event that may have been missed while the window was in the background.
  if (conductorStore.selectedId) void conductorStore.refreshDetail()
}

onUnmounted(() => {
  // Run event listeners live in the liveRuns store, so they intentionally
  // survive this component unmounting — that's what lets runs keep streaming
  // while the user is on another screen.
  unbindNewSessionMenu?.()
  unbindNewSessionMenu = null
  conductorStore.stopPolling()
  if (isResizing.value) stopResize()
  outputRO?.disconnect()
  outputRO = null
  if (scrollSettleFrame) {
    cancelAnimationFrame(scrollSettleFrame)
    scrollSettleFrame = 0
  }
  window.removeEventListener('focus', onAppFocus)
  document.removeEventListener('mousedown', onRunSettingsPointerDown)
  window.removeEventListener('pointerup', onWindowPointerUp)
  window.removeEventListener('mouseup', onSelectionMouseUp)
  window.removeEventListener('keydown', onTurnNavKey)
  sessionsChangedUnlisten?.()
  sessionsChangedUnlisten = null
  dragDropUnlisten?.()
  dragDropUnlisten = null
  remoteUnlisteners.forEach((fn) => fn())
  remoteUnlisteners.length = 0
  metaUnlisten?.()
  metaUnlisten = null
  mentionUnlisten?.()
  mentionUnlisten = null
})

async function loadInputContent(runId: string) {
  if (inputContentRunId.value === runId && inputContent.value) return
  inputContentRunId.value = runId
  inputContentLoading.value = true
  inputContentError.value = null
  inputContent.value = ''
  try {
    inputContent.value = await runsStore.readRunInput(runId)
  } catch (e) {
    inputContentError.value = String(e)
  } finally {
    inputContentLoading.value = false
  }
}

// Clear the history (on-disk log) view; used when switching to live output.
function clearHistoryView() {
  viewingLogRunId.value = null
  viewingLog.value = ''
  historyEntries.value = []
  historyHasStream.value = false
  historyToolIndex.clear()
  historyContextTokens.value = 0
  historyModel.value = null
  historyCursor.value = null
  historyHasMore.value = false
  historyBottomCursor.value = null
  historyHasMoreBelow.value = false
  historyFileSize.value = 0
  firstLoadedSeq.value = 0
  historyPreambleModel.value = null
  historyRevision.value = null
  historyToolEntries.value = []
}

async function handleFetch(linkedIssueOverride?: number) {
  const target = detectedRef.value
  if (!target) {
    // fetchError may already name the unmapped repo — don't overwrite that.
    if (!fetchError.value) fetchError.value = t('run.refMustBeUrl')
    return
  }

  fetchError.value = null
  fetching.value = true
  try {
    let run: RunRecord
    if (target.kind === 'issue') {
      run = await runsStore.fetchIssue(projectId.value, target.repoId, target.number)
    } else {
      run = await runsStore.fetchPr(projectId.value, target.repoId, target.number, linkedIssueOverride)
    }
    await runsStore.fetchRuns(projectId.value)
    currentRunId.value = run.id
    clearHistoryView()
    needsLinkedIssue.value = false
    linkedIssueInput.value = ''
    await loadInputContent(run.id)
  } catch (e) {
    const msg = String(e)
    if (target.kind === 'pr' && msg.includes('NO_LINKED_ISSUE')) {
      needsLinkedIssue.value = true
      fetchError.value = t('run.prNoLinkedIssue')
    } else {
      fetchError.value = msg
    }
  } finally {
    fetching.value = false
  }
}

async function handleSubmitLinkedIssue() {
  const n = parseInt(linkedIssueInput.value.trim())
  if (isNaN(n) || n <= 0) { fetchError.value = t('run.enterValidIssueNumber'); return }
  await handleFetch(n)
}

function setLocalRunStatus(runId: string, status: RunRecord['status'], engine?: string) {
  const run = runsStore.runs.find(r => r.id === runId)
  if (run) {
    run.status = status
    if (engine) run.engine = engine
  }
}

// Preflight the run-blocking guardrail for `engine` BEFORE the UI echoes the
// user's message or flips a run into "running". Returns the override flag to
// pass to the backend (false = under budget, true = user chose to proceed
// anyway), or `null` when the user declines — in which case the caller must
// abort without touching the timeline.
async function preflightBudget(engine: string): Promise<boolean | null> {
  let isOver = false
  try {
    const status = await invoke<{ is_over: boolean }>('get_run_budget', {
      engine,
      runId: currentRunId.value,
      projectId: projectId.value,
    })
    isOver = status.is_over
  } catch {
    return false // can't determine the verdict → don't block the user
  }
  if (!isOver) return false
  const proceed = await confirm({
    title: t('run.budgetExceededTitle'),
    message: t('run.budgetConfirmMessage'),
    confirmLabel: t('run.continue'),
    variant: 'primary',
  })
  return proceed ? true : null
}

async function handleStartRun() {
  if (!currentRunId.value) return
  const id = currentRunId.value
  const engine = effectiveEngine.value

  const override = await preflightBudget(engine)
  if (override === null) return // over budget and declined — start nothing

  clearHistoryView()
  live.reset(id, projectId.value)
  syncLoadedRunEngine(engine)
  setLocalRunStatus(id, 'running', engine)
  await live.startListening(id, projectId.value)

  try {
    await runsStore.startRun(
      id,
      engine,
      permissionMode.value || undefined,
      undefined,
      modelOverride.value || undefined,
      undefined,
      override,
    )
  } catch (e) {
    live.setStatus(id, 'failed')
    setLocalRunStatus(id, 'failed')
    live.appendOutput(id, `Error: ${String(e)}`)
  }
}

// Create a standalone work session (not tied to a GitHub issue/PR) and focus
// the composer so the user can type the first instruction right away.
const creatingSession = ref(false)
async function createSessionWithEngine(engine?: string) {
  if (creatingSession.value) return
  creatingSession.value = true
  try {
    const run = await runsStore.createSessionRun(projectId.value, engine)
    await runsStore.fetchRuns(projectId.value)
    await loadRunLog(run.id)
    focusComposer()
  } catch (e) {
    toast.error(t('run.createSessionFailed', { error: String(e) }))
  } finally {
    creatingSession.value = false
  }
}

async function handleNewSession() {
  await createSessionWithEngine(engineOverride.value || undefined)
}

// Create a fresh session already primed as a conductor: mark the new run id as a
// pending conductor so the History icon flags it and the worker sidebar opens.
// Its first message becomes the goal and launches it as a conductor (see the send
// path, which promotes the run via conductorStore.start and clears the mark).
async function handleNewConductor() {
  await createSessionWithEngine(engineOverride.value || undefined)
  if (currentRunId.value) pendingConductorIds.add(currentRunId.value)
}

// Unbound on unmount so the menu item stops firing into a dead screen.
let unbindNewSessionMenu: (() => void) | null = null

// Start a brand-new turn on `runId` with `text` as the prompt. Shared by the
// "fetched" first-run path and by engine handoffs.
// Callers MUST preflight the budget (see `preflightBudget`) and pass the
// resulting `overrideBudget` — this executor echoes the message immediately.
async function launchFreshRun(
  runId: string,
  engine: string | undefined,
  text: string,
  images: ImageAttachment[] = [],
  overrideBudget = false,
) {
  const selectedEngine = engine || effectiveEngine.value
  currentRunId.value = runId
  clearHistoryView()
  live.reset(runId, projectId.value)
  live.pushUser(runId, projectId.value, text, images)
  stickToBottom.value = true // re-pin: the user just sent a message
  syncLoadedRunEngine(selectedEngine)
  setLocalRunStatus(runId, 'running', selectedEngine)
  await live.startListening(runId, projectId.value)
  await runsStore.startRun(runId, selectedEngine, permissionMode.value || undefined, text, modelOverride.value || undefined, images, overrideBudget)
}

// ── Cross-engine handoff (Claude ↔ Codex) ────────────────────────────────
const handoffTarget = ref<string | null>(null)
const handingOff = ref(false)
const startingEngineSession = ref(false)
const engineSwitchBusy = computed(() => handingOff.value || startingEngineSession.value)

// Dismiss the handoff prompt without switching (also drops any pending model).
function closeHandoffDialog() {
  handoffTarget.value = null
  pendingEngineModel.value = null
}

async function doHandoff(targetEngine: string) {
  if (!currentRunId.value || engineSwitchBusy.value) return
  handingOff.value = true
  const sourceId = currentRunId.value
  const sourceEngine = loadedRunEngine.value || currentRun.value?.engine || 'claude'
  try {
    // Snapshot the transcript before any state reset wipes streamEntries.
    const transcript = sourceText.value
    // A live session can't be forked — stop it first, then continue on the new engine.
    if (currentStatus.value === 'running') {
      try { await runsStore.cancelRun(sourceId) } catch { /* best effort */ }
    }
    const override = await preflightBudget(targetEngine)
    if (override === null) return // over budget on the target engine — abort handoff start
    const { run, context_path } = await runsStore.createHandoffRun(sourceId, targetEngine, transcript)
    await runsStore.fetchRuns(projectId.value) // surface the new run in the sidebar
    syncLoadedRunEngine(targetEngine)
    // Carry the model the user picked when they triggered the switch.
    if (pendingEngineModel.value !== null) modelOverride.value = pendingEngineModel.value
    const seed = t('run.handoffSeed', { engine: sourceEngine, path: context_path })
    await launchFreshRun(run.id, targetEngine, seed, [], override)
  } catch (e) {
    toast.error(t('run.handoffFailed', { error: String(e) }))
  } finally {
    handingOff.value = false
    handoffTarget.value = null
    pendingEngineModel.value = null
  }
}

async function startNewEngineSession(targetEngine: string) {
  if (engineSwitchBusy.value) return
  startingEngineSession.value = true
  try {
    handoffTarget.value = null
    if (pendingEngineModel.value !== null) modelOverride.value = pendingEngineModel.value
    await createSessionWithEngine(targetEngine)
  } finally {
    startingEngineSession.value = false
    pendingEngineModel.value = null
  }
}

const followUpInput = ref('')
const sendingFollowUp = ref(false)
const savingClaudeAccount = ref(false)

const defaultClaudeAccount = computed(() =>
  claudeStore.accounts.find((acc) => acc.is_default) ?? null,
)
const effectiveProjectClaudeAccountId = computed(() =>
  project.value?.claude_account_id || defaultClaudeAccount.value?.id || '',
)
const selectedClaudeAccountId = computed(() => {
  const run = currentRun.value
  if (!run) return ''
  // Empty = the Default tier (follows project/global). Only a run-level
  // override pins a concrete account for this session.
  return run.claude_account_id || ''
})
// Label for the Default account entry: resolves to the project/global default
// account so the choice is never opaque. Falls back to "Global ~/.claude" only
// when no default account is configured at all.
const defaultClaudeAccountLabel = computed(() => {
  const acc = claudeStore.accounts.find((a) => a.id === effectiveProjectClaudeAccountId.value)
  return acc ? t('run.claudeAccountDefaultWith', { account: acc.label }) : t('run.globalClaudeAccount')
})
const claudeAccountOptions = computed(() => {
  const opts = claudeStore.accounts.map((acc) => ({
    value: acc.id,
    label: `${acc.label}${acc.email ? ` · ${acc.email}` : ''}`,
  }))
  const currentId = currentRun.value?.claude_account_id
  if (currentId && !opts.some((opt) => opt.value === currentId)) {
    opts.unshift({ value: currentId, label: t('run.missingClaudeAccount') })
  }
  // Always offer the Default tier so a per-session pick can be cleared back to
  // "follow project/global".
  opts.unshift({ value: '', label: defaultClaudeAccountLabel.value })
  return opts
})
const showClaudeAccountSelect = computed(() =>
  !!currentRun.value
  && effectiveEngine.value === 'claude'
  && (claudeStore.accounts.length > 0 || !!currentRun.value.claude_account_id),
)
const claudeAccountSelectDisabled = computed(() =>
  currentStatus.value === 'running' || sendingFollowUp.value || savingClaudeAccount.value,
)
const claudeAccountSelectTitle = computed(() =>
  currentStatus.value === 'running'
    ? t('run.claudeAccountRunningTitle')
    : t('run.claudeAccountTitle'),
)

async function handleClaudeAccountChange(value: string) {
  const run = currentRun.value
  if (!run || currentStatus.value === 'running' || savingClaudeAccount.value) return
  const next = value.trim() || null
  if ((run.claude_account_id ?? null) === next) return
  savingClaudeAccount.value = true
  try {
    await runsStore.setRunClaudeAccount(run.id, next)
    toast.success(t('run.claudeAccountUpdated'))
  } catch (e) {
    toast.error(t('run.claudeAccountUpdateFailed', { error: String(e) }))
  } finally {
    savingClaudeAccount.value = false
  }
}

// ── Pasted / attached images in the composer ──────────────────────────────
interface PendingImage extends ImageAttachment {
  id: string
  url: string // data URL for the thumbnail preview
}
const pendingImages = ref<PendingImage[]>([])
const imageInputEl = ref<HTMLInputElement | null>(null)
let imageSeq = 0

const MAX_IMAGE_BYTES = 10 * 1024 * 1024 // ~10MB per image, matches engine limits

// ── Dragged-in files referenced by path ───────────────────────────────────
// Non-image files are attached by absolute path (not embedded); the agent opens
// them via its Read/Edit tools. See SRS FR-002/FR-004.
interface PendingFile {
  id: string
  name: string
  path: string
}
const pendingFiles = ref<PendingFile[]>([])
let fileSeq = 0
// Whether an OS file is being dragged over the window (drives the dropzone hint).
const isDraggingFile = ref(false)

// ── Draft persistence ─────────────────────────────────────────────────────
// The composer's unsent draft (text, pasted images and attached files) is kept
// PER SESSION, keyed by run id: switching between sessions — or leaving the
// screen and coming back (the run reopens from the route / most-recent) —
// restores exactly that session's draft instead of a shared, easily-clobbered
// project-wide one. Mirrored to localStorage so it also survives app reloads.
// `restoringDraft` guards the persist watcher while we swap a session's draft in.
let restoringDraft = false

function restoreDraftFor(runId: string | null) {
  restoringDraft = true
  pendingImages.value = []
  pendingFiles.value = []
  followUpInput.value = ''
  if (runId) {
    const draft = draftsStore.get(runId)
    followUpInput.value = draft.text
    for (const img of draft.images) pushPendingImage(img.media_type, img.data)
    for (const f of draft.files) addPendingFile(f.path)
  }
  nextTick(() => { restoringDraft = false })
}

// Load the focused session's draft whenever it changes (initial open, session
// switch, new session). The persist watcher below then tracks edits for it.
watch(currentRunId, (id) => restoreDraftFor(id), { immediate: true })

watch(
  [followUpInput, pendingImages, pendingFiles],
  () => {
    if (restoringDraft || !currentRunId.value) return
    draftsStore.set(currentRunId.value, {
      text: followUpInput.value,
      images: pendingImages.value.map(({ media_type, data }) => ({ media_type, data })),
      files: pendingFiles.value.map(({ name, path }) => ({ name, path })),
    })
  },
  { deep: true },
)

// Extensions we treat as inline images (base64 + thumbnail). Everything else is
// attached by path.
const IMAGE_EXT_RE = /\.(png|jpe?g|gif|webp)$/i

// Push a raw-base64 image onto the pending strip, rebuilding the data URL used
// for the thumbnail preview. Shared by paste, file-picker and drag-drop paths.
function pushPendingImage(media_type: string, data: string) {
  pendingImages.value.push({
    id: `img-${imageSeq++}`,
    media_type,
    data,
    url: `data:${media_type};base64,${data}`,
  })
}

// Read a File/Blob into a base64 attachment and add it to the pending strip.
function addImageFile(file: File | null) {
  if (!file || !file.type.startsWith('image/')) return
  if (file.size > MAX_IMAGE_BYTES) {
    toast.error(t('run.imageTooLarge', { size: MAX_IMAGE_BYTES / 1024 / 1024 }))
    return
  }
  const reader = new FileReader()
  reader.onload = () => {
    const url = String(reader.result || '')
    const comma = url.indexOf(',')
    if (comma < 0) return
    pushPendingImage(file.type, url.slice(comma + 1))
  }
  reader.readAsDataURL(file)
}

// Handle a batch of absolute paths from a drag-drop (or the file picker): images
// are read to base64 through the backend (frontend fs scope can't reach arbitrary
// paths); other files are attached by reference.
async function addDroppedPaths(paths: string[]) {
  for (const path of paths) {
    if (IMAGE_EXT_RE.test(path)) {
      try {
        const img = await invoke<{ media_type: string; data: string }>('read_file_base64', { path })
        pushPendingImage(img.media_type, img.data)
      } catch (e) {
        toast.error(String(e))
      }
    } else {
      addPendingFile(path)
    }
  }
}

function addPendingFile(path: string) {
  if (pendingFiles.value.some((f) => f.path === path)) return // dedupe (BR-004)
  const name = path.split(/[/\\]/).pop() || path
  pendingFiles.value.push({ id: `file-${fileSeq++}`, name, path })
}

function removePendingFile(id: string) {
  pendingFiles.value = pendingFiles.value.filter((f) => f.id !== id)
}

// Attach files via the native picker (second entry point alongside drag-drop).
// Returns absolute paths, consistent with the drag-drop flow.
async function onPickFiles() {
  const selected = await openFileDialog({ multiple: true, directory: false })
  if (!selected) return
  const paths = Array.isArray(selected) ? selected : [selected]
  await addDroppedPaths(paths)
}

function takePendingFilePaths(): string[] {
  const paths = pendingFiles.value.map((f) => f.path)
  pendingFiles.value = []
  return paths
}

// Append dropped file references to the outgoing prompt as absolute paths so the
// agent can Read/Edit them. Literal backticked paths (not `@mentions`) — the
// sidecar stream mode doesn't expand `@`, and both engines accept absolute paths.
function composePrompt(text: string, paths: string[]): string {
  if (!paths.length) return text
  const list = paths.map((p) => `- \`${p}\``).join('\n')
  const block = `${t('run.attachFileLabel')}\n${list}`
  return text ? `${text}\n\n${block}` : block
}

// Capture images pasted into the textarea (Cmd+V from a screenshot, etc.).
function onComposerPaste(e: ClipboardEvent) {
  const items = e.clipboardData?.items
  if (!items) return
  let handled = false
  for (const item of items) {
    if (item.kind === 'file' && item.type.startsWith('image/')) {
      const file = item.getAsFile()
      if (file) { addImageFile(file); handled = true }
    }
  }
  // Only swallow the paste when it actually carried an image, so text pastes
  // (and mixed paste) still land in the textarea.
  if (handled) e.preventDefault()
}

function onPickImages(e: Event) {
  const input = e.target as HTMLInputElement
  for (const f of Array.from(input.files ?? [])) addImageFile(f)
  input.value = '' // allow re-picking the same file
}

function removePendingImage(id: string) {
  pendingImages.value = pendingImages.value.filter((img) => img.id !== id)
}

function takePendingImages(): ImageAttachment[] {
  const imgs = pendingImages.value.map(({ media_type, data }) => ({ media_type, data }))
  pendingImages.value = []
  return imgs
}
const chatEligible = computed(() => {
  if (!currentRunId.value) return false
  // Both Claude and Codex run through a sidecar now (streaming + permission +
  // multi-turn), so the composer is available for both engines.
  if (currentStatus.value === 'running') return true
  // Allow starting the run with a custom prompt when nothing has run yet.
  if (currentStatus.value === 'fetched') return true
  // Allow resume when this run captured a session id (thread id for Codex).
  if (!currentSessionId.value) return false
  return ['done', 'cancelled', 'failed'].includes(currentStatus.value)
})

const engineLabel = computed(() => loadedRunEngine.value || currentRun.value?.engine || effectiveEngine.value || t('run.engineFallback'))

const chatPlaceholder = computed(() => {
  if (currentStatus.value === 'running') {
    return t('run.chatPlaceholderRunning', { engine: engineLabel.value })
  }
  if (currentStatus.value === 'fetched') {
    return currentIsSession.value
      ? t('run.chatPlaceholderFetchedSession')
      : t('run.chatPlaceholderFetchedIssue')
  }
  if (currentSessionId.value) {
    return t('run.chatPlaceholderResume', { engine: engineLabel.value })
  }
  return t('run.chatPlaceholderUnavailable')
})

const chatSendLabel = computed(() => {
  if (sendingFollowUp.value) return t('run.sendLabelSending')
  if (currentStatus.value === 'running') return t('run.sendLabelSend')
  if (currentStatus.value === 'fetched') return t('run.sendLabelRun')
  if (!chatEligible.value) return t('run.sendLabelRun')
  return t('run.sendLabelResume')
})

// Whether the composer footer should be shown at all — any loaded run, so the
// engine/model/permission controls and the primary action live in one place.
const composerVisible = computed(() => !!currentRunId.value)

// The primary footer button is disabled when there's nothing to do. A default
// "Run" (fetched record, or a finished run we can't resume) needs no input;
// sending a follow-up / resuming does.
const primaryDisabled = computed(() => {
  if (sendingFollowUp.value) return true
  if (currentStatus.value === 'fetched' || !chatEligible.value) return false
  return !followUpInput.value.trim() && !pendingImages.value.length && !pendingFiles.value.length
})

// One entry point for the footer's primary button: cancel while running is a
// separate button, so here we either send a typed message/resume, or kick off
// the default-prompt run when nothing was typed.
function handlePrimaryAction() {
  if (currentStatus.value === 'running') { handleSendFollowUp(); return }
  const hasInput = followUpInput.value.trim().length > 0 || pendingImages.value.length > 0 || pendingFiles.value.length > 0
  if (!hasInput && (currentStatus.value === 'fetched' || !chatEligible.value)) {
    handleStartRun()
    return
  }
  handleSendFollowUp()
}

// Send a slash command (e.g. /compact, /clear) as a message. Routes through the
// normal primary action so it respects running / resume / start state.
function sendSlashCommand(cmd: string) {
  followUpInput.value = cmd
  handlePrimaryAction()
}

// ── Composer auto-resize / expand ─────────────────────────────────────────
// The composer grows with its content (great for pasted long text) up to a
// cap, then scrolls internally. "Expanded" raises that cap to most of the
// viewport so long text is comfortable to read and edit.
const composerExpanded = ref(false)

function autoResizeComposer() {
  const el = composerEl.value
  if (!el) return
  // Expanded: pin the textarea to a large fixed height so "maximize" is
  // immediately visible even when there's little/no content to grow into.
  if (composerExpanded.value) {
    const expandedPx = Math.round(window.innerHeight * 0.6)
    el.style.height = `${expandedPx}px`
    el.style.overflowY = 'auto'
    return
  }
  // Collapsed: grow with content up to a cap, then scroll internally.
  const maxPx = 180
  el.style.height = 'auto'
  const next = Math.min(el.scrollHeight, maxPx)
  el.style.height = `${next}px`
  el.style.overflowY = el.scrollHeight > maxPx ? 'auto' : 'hidden'
}

function toggleComposerExpand() {
  composerExpanded.value = !composerExpanded.value
  nextTick(() => {
    autoResizeComposer()
    composerEl.value?.focus()
  })
}

// Reliably move focus into the composer. A single `nextTick` is not enough when
// the composer is mounting for the first time (e.g. right after a new session is
// created, `currentRunId` also drives a concurrent `router.replace`): the
// textarea may not be in the DOM yet, or focus gets stolen as the route settles.
// Retry across a few animation frames until it takes.
function focusComposer(attempts = 8) {
  nextTick(() => {
    const el = composerEl.value
    if (el) {
      el.focus({ preventScroll: true })
      if (document.activeElement === el) return
    }
    if (attempts > 0) requestAnimationFrame(() => focusComposer(attempts - 1))
  })
}

// Resize on every value change: typing, paste, programmatic edits
// (slash/mention insert), and clearing after a send.
watch(followUpInput, () => autoResizeComposer(), { flush: 'post' })

// ── @-mention file/folder autocomplete ───────────────────────────────────
const composerEl = ref<HTMLTextAreaElement | null>(null)
const projectFiles = ref<ProjectEntry[]>([])
const projectFilesPath = ref<string | null>(null)
const mentionOpen = ref(false)
const mentionQuery = ref('')
const mentionStart = ref(0) // index of the `@` in followUpInput
const mentionIndex = ref(0)
const MENTION_LIMIT = 50

// ── Slash-command palette (Claude advertises commands on system.init) ─────
// Prefer the live session's list; when viewing a finished run from disk the
// live session is gone, so fall back to the commands captured in the history.
const slashCommands = computed(() => {
  const liveCmds = session.value?.slashCommands
  if (liveCmds && liveCmds.length) return liveCmds
  for (let i = historyEntries.value.length - 1; i >= 0; i--) {
    const e = historyEntries.value[i]
    if (e.kind === 'system' && e.slashCommands?.length) return e.slashCommands
  }
  // Brand-new session (no init event yet): fall back to the last commands this
  // engine advertised in any prior session.
  return live.cachedSlashCommands(effectiveEngine.value)
})
const slashOpen = ref(false)
const slashQuery = ref('')
const slashIndex = ref(0)
const slashItems = computed(() => {
  const q = slashQuery.value.toLowerCase()
  return slashCommands.value.filter((c) => c.toLowerCase().startsWith(q)).slice(0, MENTION_LIMIT)
})

// Open the palette while the whole composer is a bare `/command` token.
function detectSlash() {
  // Read straight from the DOM element rather than the v-model ref: while an IME
  // (e.g. Vietnamese Telex) is composing, Vue defers updating `followUpInput`
  // until `compositionend`, so the ref is stale on every `input` event. The
  // element's own `.value` is always current. Fall back to the ref pre-mount.
  const text = composerEl.value?.value ?? followUpInput.value
  const m = /^\/([a-zA-Z0-9_-]*)$/.exec(text)
  if (!m || !slashCommands.value.length) { slashOpen.value = false; return }
  slashQuery.value = m[1]
  slashIndex.value = 0
  slashOpen.value = true
}

function selectSlash(cmd: string) {
  followUpInput.value = `/${cmd}`
  slashOpen.value = false
  nextTick(() => {
    const el = composerEl.value
    if (el) { el.focus(); const pos = followUpInput.value.length; el.setSelectionRange(pos, pos) }
  })
}

function onComposerInput() {
  detectMention()
  detectSlash()
}

async function ensureProjectFiles(force = false) {
  const path = project.value?.path
  if (!path) return
  // Skip the walk only when the cache is warm AND no refresh was requested.
  // `force` is passed when a new @-mention session starts so newly-added files
  // (e.g. a file just copied into the folder) show up without a reload.
  if (!force && projectFilesPath.value === path && projectFiles.value.length) return
  try {
    projectFiles.value = await runsStore.listProjectFiles(path)
    projectFilesPath.value = path
  } catch {
    projectFiles.value = []
  }
}

// ── Opening a file ────────────────────────────────────────────────────────
// Every file opens in its own OS window (FileViewerWindow), never in a modal
// over the conversation: reading a file is something you do ALONGSIDE the run,
// and a modal covered the very output that made you click it. One window per
// file, so several can sit side by side; reopening the same file focuses the
// window already showing it.

// Set of known project file paths, for resolving inline-code mentions.
const projectFileSet = computed(() => new Set(projectFiles.value.map(e => !e.is_dir && e.path).filter(Boolean) as string[]))

// Resolve a raw inline-code token (e.g. `src/foo.ts` or just `foo.ts`) to a
// project file path, or null when it doesn't name a real file.
function fileMatcher(raw: string): string | null {
  return matchProjectFile(raw, projectFileSet.value)
}

/** Show a file (optionally scrolled to `line`) in its own window. */
function openFile(path: string, line?: number | null) {
  const projectPath = project.value?.path
  if (!projectPath || !path) return
  openFileWindow(projectPath, path, line ?? null)
}

// Directive for RunView's own markdown containers (issue/PR Content, legacy
// HTML). Marks inline `<code>` that names a project file as a clickable link.
const vFileLinks = {
  mounted: (el: HTMLElement) => decorateFileLinks(el, fileMatcher),
  updated: (el: HTMLElement) => decorateFileLinks(el, fileMatcher),
}
function onProseClick(e: MouseEvent) {
  const target = e.target as HTMLElement
  const code = target.closest('code.file-link') as HTMLElement | null
  const path = code?.getAttribute('data-file-path')
  if (path) { e.preventDefault(); openFile(path); return }
  // Markdown link: prevent the webview from navigating (relative hrefs blank
  // the SPA). Open project files in the viewer, external URLs in the browser.
  const anchor = target.closest('a') as HTMLAnchorElement | null
  if (anchor) {
    e.preventDefault()
    const href = anchor.getAttribute('href') ?? ''
    if (!href) return
    if (/^(https?:|mailto:)/i.test(href)) { onOpenUrl(href); return }
    const fp = fileMatcher(href.replace(/#.*$/, ''))
    if (fp) openFile(fp, parseLineRef(href))
  }
}

function onOpenUrl(url: string) {
  openUrl(url).catch(() => { /* opener unavailable */ })
}

// Score an entry against the query. Higher = more relevant; -1 = no match.
// Order of preference: exact basename > basename prefix > basename substring >
// path substring > fuzzy subsequence over the path. This is what lets the file
// you're actually typing surface ahead of alphabetically-earlier paths that
// merely contain the same characters (the old code just took the first 50 in
// alphabetical order, so deep/late matches never showed up).
function scoreMention(path: string, q: string): number {
  const lp = path.toLowerCase()
  const slash = lp.lastIndexOf('/')
  const base = slash >= 0 ? lp.slice(slash + 1) : lp
  if (base === q) return 1000
  if (base.startsWith(q)) return 900 - base.length
  const bi = base.indexOf(q)
  if (bi >= 0) return 700 - bi - base.length * 0.01
  const pi = lp.indexOf(q)
  if (pi >= 0) return 500 - pi - lp.length * 0.01
  // Fuzzy: every query char appears in order somewhere in the path.
  let qi = 0
  for (let i = 0; i < lp.length && qi < q.length; i++) {
    if (lp[i] === q[qi]) qi++
  }
  if (qi === q.length) return 200 - lp.length * 0.01
  return -1
}

const mentionItems = computed<ProjectEntry[]>(() => {
  const q = mentionQuery.value.toLowerCase()
  const src = projectFiles.value
  if (!q) return src.slice(0, MENTION_LIMIT)
  const scored: Array<{ e: ProjectEntry; s: number }> = []
  for (const e of src) {
    const s = scoreMention(e.path, q)
    if (s >= 0) scored.push({ e, s })
  }
  // Highest score first; ties keep the (alphabetical) source order.
  scored.sort((a, b) => b.s - a.s)
  return scored.slice(0, MENTION_LIMIT).map((x) => x.e)
})

// Detect an active `@token` immediately before the caret (no whitespace inside).
function detectMention() {
  const el = composerEl.value
  if (!el) { mentionOpen.value = false; return }
  const caret = el.selectionStart ?? 0
  // Read from the DOM element, not the v-model ref: during IME composition Vue
  // keeps `followUpInput` stale until `compositionend`, which would make the
  // query empty and surface the first-50 (alphabetical) files instead.
  const text = el.value
  let i = caret - 1
  while (i >= 0) {
    const ch = text[i]
    if (ch === '@') break
    if (ch === ' ' || ch === '\n' || ch === '\t') { mentionOpen.value = false; return }
    i--
  }
  if (i < 0 || text[i] !== '@') { mentionOpen.value = false; return }
  // `@` must start a token (preceded by whitespace or be at the very start).
  const prev = i > 0 ? text[i - 1] : ''
  if (prev && !/\s/.test(prev)) { mentionOpen.value = false; return }
  const wasOpen = mentionOpen.value
  mentionStart.value = i
  mentionQuery.value = text.slice(i + 1, caret)
  mentionIndex.value = 0
  mentionOpen.value = true
  // Refresh the file list when a mention session first opens (not on every
  // keystroke) so files added to the folder since the last session appear.
  ensureProjectFiles(!wasOpen)
}

function selectMention(entry: ProjectEntry) {
  const el = composerEl.value
  const caret = el?.selectionStart ?? followUpInput.value.length
  const text = followUpInput.value
  const before = text.slice(0, mentionStart.value)
  const after = text.slice(caret)
  const insert = `@${entry.path}${entry.is_dir ? '/' : ''} `
  followUpInput.value = before + insert + after
  mentionOpen.value = false
  nextTick(() => {
    const pos = before.length + insert.length
    if (el) { el.focus(); el.setSelectionRange(pos, pos) }
  })
}

// Insert a file mention (`@path`) into the composer. Reached from the shared
// file menu — the explorer's right-click AND a file window's ⋯ menu — which
// broadcasts it, because the composer only exists here in the main window.
// Inserts at the caret when the composer is focused, otherwise appends.
function mentionFileInComposer(path: string) {
  const el = composerEl.value
  const cur = followUpInput.value
  const focused = el && document.activeElement === el
  const caret = focused ? (el!.selectionStart ?? cur.length) : cur.length
  const before = cur.slice(0, caret)
  const after = cur.slice(caret)
  const sep = before.length && !/\s$/.test(before) ? ' ' : ''
  const insert = `${sep}@${path} `
  followUpInput.value = before + insert + after
  mentionOpen.value = false
  nextTick(() => {
    const pos = (before + insert).length
    if (el) { el.focus(); el.setSelectionRange(pos, pos) }
  })
}

// ── Saved prompt library (edited in Settings → Prompt Templates) ──────────
const savedPrompts = computed(() => parseSavedPrompts(appSettings.settings?.saved_prompts))

// Insert a saved prompt at the caret, appending when the composer isn't
// focused — same contract as `mentionFileInComposer`. Deliberately does NOT
// send: the user reviews/edits first.
function insertSavedPrompt(p: SavedPrompt) {
  const el = composerEl.value
  const cur = followUpInput.value
  const focused = el && document.activeElement === el
  const caret = focused ? (el!.selectionStart ?? cur.length) : cur.length
  const before = cur.slice(0, caret)
  const after = cur.slice(caret)
  // Keep an existing draft readable by dropping the prompt onto its own line.
  const sep = before.length && !/\n$/.test(before) ? '\n' : ''
  const insert = `${sep}${p.body}`
  followUpInput.value = before + insert + after
  nextTick(() => {
    const pos = (before + insert).length
    if (el) { el.focus(); el.setSelectionRange(pos, pos) }
  })
}

function onComposerKeydown(e: KeyboardEvent) {
  if (slashOpen.value && slashItems.value.length) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      slashIndex.value = (slashIndex.value + 1) % slashItems.value.length
      return
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      slashIndex.value = (slashIndex.value - 1 + slashItems.value.length) % slashItems.value.length
      return
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault()
      selectSlash(slashItems.value[slashIndex.value])
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      slashOpen.value = false
      return
    }
  }
  if (mentionOpen.value && mentionItems.value.length) {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      mentionIndex.value = (mentionIndex.value + 1) % mentionItems.value.length
      return
    }
    if (e.key === 'ArrowUp') {
      e.preventDefault()
      mentionIndex.value = (mentionIndex.value - 1 + mentionItems.value.length) % mentionItems.value.length
      return
    }
    if (e.key === 'Enter' || e.key === 'Tab') {
      e.preventDefault()
      selectMention(mentionItems.value[mentionIndex.value])
      return
    }
    if (e.key === 'Escape') {
      e.preventDefault()
      mentionOpen.value = false
      return
    }
  }
  // Enter (no Shift) sends; Shift+Enter inserts a newline.
  if (e.key === 'Enter' && !e.shiftKey) {
    e.preventDefault()
    handleSendFollowUp()
  }
}

async function handleSendFollowUp() {
  mentionOpen.value = false
  const text = followUpInput.value.trim()
  const hasImages = pendingImages.value.length > 0
  const hasFiles = pendingFiles.value.length > 0
  if ((!text && !hasImages && !hasFiles) || !currentRunId.value || sendingFollowUp.value) return
  const id = currentRunId.value
  const isFetched = currentStatus.value === 'fetched'
  const wasRunning = currentStatus.value === 'running'
  const runEngine =
    (!isFetched && (loadedRunEngine.value || currentRun.value?.engine)) || effectiveEngine.value
  sendingFollowUp.value = true
  try {
    // Always confirm the budget BEFORE echoing anything into the timeline. On
    // decline nothing is pushed and no "thinking" indicator appears.
    const override = await preflightBudget(runEngine)
    if (override === null) return

    const images = takePendingImages()
    const prompt = composePrompt(text, takePendingFilePaths())

    if (isFetched) {
      // Conductor mode: promote this fresh session into a conductor and launch
      // it with the typed text as its goal. It then streams here like any
      // session, and the worker sidebar appears once its tree is polled.
      if (isPendingConductor(id)) {
        await conductorStore.start(id, prompt, { maxWorkers: conductorMaxWorkers.value })
        // start_conductor has marked the run role='conductor' in the DB. Refresh
        // the list so that role is present locally BEFORE dropping the pending
        // mark — otherwise the History icon flickers back to a plain session.
        await runsStore.fetchRuns(projectId.value)
        pendingConductorIds.delete(id)
        live.setStatus(id, 'running')
        setLocalRunStatus(id, 'running')
        await live.startListening(id, projectId.value)
        live.pushUser(id, projectId.value, prompt, images)
        void conductorStore.select(id)
        followUpInput.value = ''
        return
      }
      // First run on this fetched record — use the typed text as the prompt.
      await launchFreshRun(id, engineOverride.value || undefined, prompt, images, override)
      // start_run has derived the sidebar title from this first prompt. Refresh in
      // place (no `loading` flash) so History shows that title immediately instead
      // of the "Session" placeholder until a later `sessions:changed` arrives.
      await runsStore.refreshMeta(projectId.value)
      followUpInput.value = ''
      return
    }

    // This run becomes the live conversation. If we were viewing its on-disk
    // history, seed the live session with that transcript so prior context
    // stays visible, then leave history mode.
    const s = live.ensure(id, projectId.value)
    if (s.entries.length === 0 && viewingLogRunId.value === id && historyHasStream.value) {
      s.entries.push(...historyEntries.value)
      s.hasStreamEvents = true
      for (const e of historyEntries.value) {
        if (e.kind === 'system' && e.sessionId) s.sessionId = e.sessionId
        if (e.kind === 'system' && e.slashCommands?.length) s.slashCommands = e.slashCommands
      }
    }
    clearHistoryView()

    // Show the user message right away in the timeline (running / resume paths).
    live.pushUser(id, projectId.value, prompt, images)
    stickToBottom.value = true // re-pin: the user just sent a message
    scrollOutputToBottom()

    if (!wasRunning) {
      live.setStatus(id, 'running')
      setLocalRunStatus(id, 'running')
      await live.startListening(id, projectId.value)
      await runsStore.resumeRun(id, permissionMode.value || undefined, modelOverride.value || undefined, override)
    }

    await runsStore.sendUserMessage(id, prompt, images, override)
    followUpInput.value = ''
  } catch (e) {
    live.appendOutput(id, t('run.sendFailed', { error: String(e) }))
  } finally {
    sendingFollowUp.value = false
  }
}

// Permission / question prompts no longer live in this view at all: the global
// PermissionWindowManager opens one standalone window per run that is asking (this
// run, a popped-out session, or a background run) and owns the resolve logic.

// A file menu anywhere (including another webview) asking for an @mention here.
let mentionUnlisten: UnlistenFn | null = null
async function setupMentionBridge() {
  try {
    mentionUnlisten = await listen<{ projectPath: string; path: string }>(
      FILE_MENTION_EVENT,
      (e) => {
        const payload = e.payload
        // Ignore files from a project this screen isn't showing.
        if (!payload?.path || (payload.projectPath && payload.projectPath !== project.value?.path)) return
        mentionFileInComposer(payload.path)
      },
    )
  } catch {
    /* running outside the Tauri shell */
  }
}

// Re-fetch fresh PR/issue content from GitHub and overwrite this run's input
// markdown IN PLACE. The existing AI result (output/session) is preserved so
// the user can keep working on it with the refreshed context.
async function handleRefetch(sourceRunId: string) {
  fetchError.value = null
  refetchingRunId.value = sourceRunId
  try {
    await runsStore.refetchRun(sourceRunId)
    // Force-refresh the input markdown shown in the view (output untouched).
    if (inputContentRunId.value === sourceRunId) {
      inputContentRunId.value = null
      await loadInputContent(sourceRunId)
    }
  } catch (e) {
    fetchError.value = String(e)
  } finally {
    refetchingRunId.value = null
  }
}

async function handleCancel() {
  if (!currentRunId.value) return
  const id = currentRunId.value
  try {
    await runsStore.cancelRun(id)
    live.setStatus(id, 'cancelled')
    setLocalRunStatus(id, 'cancelled')
  } catch (e) {
    toast.error(String(e))
  }
}

const deletingRunId = ref<string | null>(null)
const refetchingRunId = ref<string | null>(null)
const clearingAll = ref(false)
const pinningRunId = ref<string | null>(null)
const protectingRunId = ref<string | null>(null)
const convertingRunId = ref<string | null>(null)
// Which History row currently has its actions (⋯) menu open. Kept so the
// trigger stays visible even when the pointer leaves the row while open.
const actionsMenuRunId = ref<string | null>(null)

// Inline rename state for the History list.
const renamingRunId = ref<string | null>(null)
const renameDraft = ref('')
const renameInputEl = ref<HTMLInputElement | null>(null)
// Only one rename overlay is rendered at a time (v-if on the matching run id),
// so this ref always points at the active input (or null when none is open).
function setRenameInputRef(el: Element | ComponentPublicInstance | null) {
  renameInputEl.value = (el as HTMLInputElement | null) ?? null
}

function startRename(run: RunRecord) {
  renamingRunId.value = run.id
  renameDraft.value = run.title ?? runLabel(run)
  nextTick(() => {
    renameInputEl.value?.focus()
    renameInputEl.value?.select()
  })
}

// Copy the absolute path of this run/session's log file to the clipboard.
async function copyLogPath(run: RunRecord) {
  try {
    const path = await runsStore.getRunLogPath(run.id)
    if (!path) { toast.info(t('run.noLogFileYet')); return }
    await navigator.clipboard.writeText(path)
    toast.success(t('run.logPathCopied'))
  } catch (e) {
    toast.error(String(e))
  }
}

// Copy this run/session's Devdy run ID to the clipboard — the id the devdy MCP
// (session_read) uses to load the session.
async function copySessionId(run: RunRecord) {
  try {
    await navigator.clipboard.writeText(run.id)
    toast.success(t('run.sessionIdCopied'))
  } catch (e) {
    toast.error(String(e))
  }
}

// Open this run/session's log file in a standalone file viewer window.
async function viewLogFile(run: RunRecord) {
  const p = project.value
  if (!p) return
  try {
    const path = await runsStore.getRunLogPath(run.id)
    if (!path) { toast.info(t('run.noLogFileYet')); return }
    await openFileWindow(p.path, path)
  } catch (e) {
    toast.error(String(e))
  }
}

function cancelRename() {
  renamingRunId.value = null
  renameDraft.value = ''
}

async function commitRename(runId: string) {
  if (renamingRunId.value !== runId) return
  const title = renameDraft.value.trim()
  const run = runsStore.runs.find(r => r.id === runId)
  // No change (or empty for a run that already has no custom title) → just close.
  if (run && title === (run.title ?? '')) {
    cancelRename()
    return
  }
  try {
    await runsStore.renameRun(runId, title)
    toast.success(t('run.runRenamed'))
  } catch (err) {
    toast.error(String(err))
  } finally {
    cancelRename()
  }
}

async function handleTogglePin(run: RunRecord) {
  pinningRunId.value = run.id
  const willPin = !run.pinned
  try {
    await runsStore.setRunPinned(run.id, willPin)
    toast.success(willPin ? t('run.runPinned') : t('run.runUnpinned'))
  } catch (err) {
    toast.error(String(err))
  } finally {
    pinningRunId.value = null
  }
}

async function handleToggleProtect(run: RunRecord) {
  protectingRunId.value = run.id
  const willProtect = !run.protected
  try {
    await runsStore.setRunProtected(run.id, willProtect)
    toast.success(willProtect ? t('run.runProtected') : t('run.runUnprotected'))
  } catch (err) {
    toast.error(String(err))
  } finally {
    protectingRunId.value = null
  }
}

function clearActiveRunState() {
  if (currentRunId.value) live.discard(currentRunId.value)
  currentRunId.value = null
  // A fresh run starts on the global default (empty = follow settings).
  engineOverride.value = ''
  loadedRunEngine.value = ''
  clearHistoryView()
  inputContent.value = ''
  inputContentRunId.value = null
  inputContentError.value = null
}

// Convert a stopped normal session into a conductor: the backend marks the role,
// creates the conductor session, then resumes the existing conversation with the
// conductor tools + framing injected. We then open it live and start polling its
// worker tree, seeding the viewer from the on-disk transcript so the prior
// conversation stays visible while new conductor turns stream in.
async function handleConvertToConductor(run: RunRecord) {
  const label = runLabel(run)
  if (!(await confirm({
    title: t('run.convertToConductorTitle'),
    message: t('run.convertToConductorMessage', { label }),
    confirmLabel: t('run.convertToConductorConfirm'),
  }))) return
  convertingRunId.value = run.id
  try {
    await conductorStore.convertToConductor(run.id)
    // Pure DB upgrade: the session is NOT resumed and stays stopped. Just refresh
    // History so the new 'conductor' role shows. The conductor tools come online
    // the next time the user resumes the session with their own prompt.
    await runsStore.fetchRuns(projectId.value)
    toast.success(t('run.convertedToConductor'))
  } catch (e) {
    toast.error(String(e))
  } finally {
    convertingRunId.value = null
  }
}

async function handleDeleteRun(runId: string) {
  const run = runsStore.runs.find(r => r.id === runId)
  const label = run ? runLabel(run) : t('run.thisRun')
  if (!(await confirm({
    title: t('run.deleteRunTitle'),
    message: t('run.deleteRunMessage', { label }),
    confirmLabel: t('common.delete'),
  }))) return
  deletingRunId.value = runId
  try {
    await runsStore.deleteRun(runId)
    // Drop any pending-conductor mark for the removed run.
    pendingConductorIds.delete(runId)
    if (currentRunId.value === runId || viewingLogRunId.value === runId) {
      clearActiveRunState()
    } else {
      live.discard(runId)
    }
    // Forget the deleted run so its project tab won't resume onto it, and
    // navigate away from a deep-linked deleted run so the URL stays consistent.
    tabsStore.forgetRun(runId)
    if (activeRunId.value === runId) {
      router.replace(`/projects/${projectId.value}`)
    }
    toast.success(t('run.runDeleted'))
  } catch (err) {
    toast.error(String(err))
  } finally {
    deletingRunId.value = null
  }
}

async function handleClearAllRuns() {
  if (runsStore.runs.length === 0) return
  if (!(await confirm({
    title: t('run.deleteAllRunsTitle'),
    message: t('run.deleteAllRunsMessage'),
    confirmLabel: t('run.deleteAll'),
  }))) return
  clearingAll.value = true
  try {
    await runsStore.deleteAllRuns(projectId.value)
    clearActiveRunState()
    if (activeRunId.value) {
      router.replace(`/projects/${projectId.value}`)
    }
    toast.success(t('run.allRunsDeleted'))
  } catch (err) {
    toast.error(String(err))
  } finally {
    clearingAll.value = false
  }
}

async function loadRunLog(runId: string, opts: { preferDisk?: boolean; force?: boolean } = {}) {
  currentRunId.value = runId
  // Reflect this run's metadata so the chat composer can decide eligibility.
  const meta = runsStore.runs.find(r => r.id === runId)
  if (meta?.engine) {
    syncLoadedRunEngine(meta.engine)
  }
  // Kick off loading PR/Issue content in parallel so the Content tab is ready.
  // Standalone sessions have no input markdown — skip.
  if (meta?.run_type !== 'session') loadInputContent(runId).catch(() => {})

  // If we still hold a live in-memory session for this run (it's running, or it
  // finished during this app session), show that live instead of the on-disk
  // log — and re-attach listeners if it's still streaming.
  // Only `force` (the transcript actually changed on disk — the sessions:changed
  // watcher fired) overrides this for a finished session to re-read turns added
  // outside Devdy. A plain window refocus (preferDisk without force) keeps the
  // live view untouched so it doesn't blank + re-render (the "flash").
  const liveSession = live.get(runId)
  if (liveSession && !(opts.force && liveSession.status !== 'running')) {
    clearHistoryView()
    if (liveSession.status === 'running' && !live.isListening(runId)) {
      await live.startListening(runId, projectId.value)
    }
    return
  }

  // 'fetched' runs have no AI output yet — show Content first and let the
  // AI Result tab fall through to the "chưa run" empty state.
  if (meta?.status === 'fetched') {
    clearHistoryView()
    return
  }

  // On a focus refresh we're re-reading a log the user is already viewing, so
  // preserve their scroll position instead of yanking them to the bottom. Only
  // re-pin to the bottom if they were already near it.
  const isFocusRefresh = !!opts.preferDisk && viewingLogRunId.value === runId
  const prevScrollTop = historyEl.value?.scrollTop ?? 0
  const wasNearBottom = historyEl.value ? isNearBottom(historyEl.value) : true

  // Fresh open: clear immediately and show the loading animation. Focus refresh:
  // keep the current content on screen (no clear, no spinner) — we only touch
  // reactive state below IF the log actually changed, so an unchanged refocus
  // causes zero re-render (no flash).
  if (!isFocusRefresh) {
    historyEntries.value = []
    historyHasStream.value = false
    historyToolIndex.clear()
    historyContextTokens.value = 0
    historyModel.value = null
    historyCursor.value = null
    historyHasMore.value = false
    historyBottomCursor.value = null
    historyHasMoreBelow.value = false
    historyFileSize.value = 0
    firstLoadedSeq.value = 0
    historyPreambleModel.value = null
    historyRevision.value = null
    historyToolEntries.value = []
    viewingLog.value = ''
    historyLoading.value = true
  }
  viewingLogRunId.value = runId
  try {
    // A refocus on an unchanged log must cost nothing. The fingerprint is a
    // couple of stat() calls; before this, every alt-tab back into the app
    // re-read the entire file, re-encoded it as JSON across IPC and re-parsed
    // it, just to discover the bytes were identical.
    if (isFocusRefresh && historyRevision.value) {
      const rev = await runsStore.getRunLogRevision(runId)
      if (sameRunLogRevision(rev, historyRevision.value)) return
    }

    const page = await runsStore.getRunLogPage(runId, undefined, HISTORY_PAGE_INITIAL)
    // The user may have switched runs while this was in flight.
    if (viewingLogRunId.value !== runId) return
    const seedModel = modelFromPreamble(page.preamble)
    const parsed = parseStreamLogWindow(page.records, { seedModel })
    if (parsed.entries.length > 0) {
      historyToolIndex.clear()
      historyEntries.value = parsed.entries
      for (const [k, v] of parsed.toolIndex) historyToolIndex.set(k, v)
      historyHasStream.value = true
      historyContextTokens.value = parsed.contextTokens
      historyModel.value = parsed.model
      historyCursor.value = page.cursor
      historyHasMore.value = page.has_more
      // The initial window is the tail, so it already reaches the end of the log.
      historyBottomCursor.value = page.revision.size
      historyHasMoreBelow.value = false
      historyFileSize.value = page.revision.size
      firstLoadedSeq.value = 0
      historyPreambleModel.value = seedModel
      historyRevision.value = page.revision
      viewingLog.value = ''
      // Whole-run scan for the "files changed" list, off the critical path.
      void loadHistoryToolEntries(runId)
      nextTick(() => {
        if (!historyEl.value) return
        if (isFocusRefresh && !wasNearBottom) {
          // Keep the user where they were reading — don't jump on refocus.
          historyEl.value.scrollTop = prevScrollTop
        } else {
          // Fresh load / switch run: jump to the end so the latest AI result is visible.
          historyEl.value.scrollTop = historyEl.value.scrollHeight
        }
      })
    } else {
      // Nothing parsed as stream records. Either the run produced no output, or
      // this is a legacy/plain-markdown log — the only case that still needs the
      // whole file, so it's fetched here rather than on the hot path.
      const content = await runsStore.getRunLog(runId)
      if (viewingLogRunId.value !== runId) return
      if (content.trim()) {
        historyHasStream.value = false
        historyRevision.value = page.revision
        viewingLog.value = content
      } else {
        viewingLog.value = ''
        viewingLogRunId.value = null
      }
    }
  } catch (e) {
    viewingLog.value = String(e)
  } finally {
    historyLoading.value = false
  }
}

/** What a pasted URL resolved to. `slug` is the repo it points at — `owner/repo`
 * for GitHub, the full project path for GitLab. */
interface ParsedRef {
  number: number
  kind: 'issue' | 'pr'
  slug: string
}

/** Parse an issue/PR/MR URL. Anything else — including a bare number — is null. */
function parseIssueOrPrUrl(input: string): ParsedRef | null {
  const val = input.trim()

  // GitLab puts the ref under a `/-/` separator, and the project path may have
  // any number of subgroup segments: host/group/sub/project/-/merge_requests/42
  const gitlab = val.match(/^(?:https?:\/\/)?[^/\s]+\/(.+?)\/-\/(merge_requests|issues)\/(\d+)/i)
  if (gitlab) {
    return {
      number: parseInt(gitlab[3]),
      kind: gitlab[2].toLowerCase() === 'merge_requests' ? 'pr' : 'issue',
      slug: gitlab[1],
    }
  }

  // GitHub (and GHE — the host is ignored): host/owner/repo/pull/123
  const github = val.match(/^(?:https?:\/\/)?[^/\s]+\/([^/\s]+\/[^/\s]+)\/(pull|issues)\/(\d+)/i)
  if (github) {
    return {
      number: parseInt(github[3]),
      kind: github[2].toLowerCase() === 'pull' ? 'pr' : 'issue',
      slug: github[1],
    }
  }

  return null
}

/** The repo slug as the pasted URL would spell it, for matching. */
function repoSlug(r: Repo): string | null {
  if (r.provider === 'gitlab') return r.gitlab_project_path
  if (r.github_owner && r.github_repo) return `${r.github_owner}/${r.github_repo}`
  return null
}

async function handleOpenInVscode() {
  if (!project.value) return
  try {
    // Reveal the most recently loaded issue/PR markdown alongside the project folder.
    const run = currentRunId.value
      ? runsStore.runs.find(r => r.id === currentRunId.value)
      : null
    await projectStore.openInVscode(project.value.path, run?.input_path ?? undefined)
  } catch (e) {
    toast.error(String(e))
  }
}

async function handleOpenInFolder() {
  if (!project.value) return
  try {
    await projectStore.openInFolder(project.value.path)
  } catch (e) {
    toast.error(String(e))
  }
}

async function handleOpenInTerminal() {
  if (!project.value) return
  try {
    const settings = await invoke<{ terminal_app: string }>('get_settings')
    await projectStore.openInTerminal(project.value.path, settings.terminal_app)
  } catch (e) {
    toast.error(String(e))
  }
}

function handleRefInput(val: string) {
  refInput.value = val
  needsLinkedIssue.value = false
  linkedIssueInput.value = ''
  fetchError.value = null
  detectedRef.value = null
  if (!val.trim()) return

  // Stay quiet while a URL is still half-typed — `handleFetch` reports the
  // "not a URL" case on submit. An unmapped repo, by contrast, comes from a
  // fully parsed URL, so it is safe to flag live.
  const parsed = parseIssueOrPrUrl(val)
  if (!parsed) return
  // The URL names its own repo — resolve it rather than trusting a selection,
  // so #N is never pulled from a different repo than the one that was pasted.
  const match = repos.value.find(
    r => repoSlug(r)?.toLowerCase() === parsed.slug.toLowerCase(),
  )
  if (!match) {
    fetchError.value = t('run.repoNotInProject', { slug: parsed.slug })
    return
  }
  detectedRef.value = {
    kind: parsed.kind,
    number: parsed.number,
    repoId: match.id,
    repoName: match.name,
  }
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between gap-3 px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2 min-w-0">
        <h1 class="text-sm font-semibold truncate">{{ project?.name ?? t('run.projectFallback') }}</h1>
        <Badge
          v-for="acc in activeAccounts"
          :key="acc.key"
          :tone="acc.tone"
          size="sm"
          class="font-mono shrink-0 inline-flex items-center gap-1"
          :title="acc.title"
        >
          <component :is="acc.icon" class="h-3 w-3" :stroke-width="1.75" />
          {{ acc.username }}
        </Badge>
        <!-- VPS servers wired to this project (transparent SSH per run). -->
        <Badge
          v-for="chip in visibleServerChips"
          :key="chip.key"
          :tone="chip.tone"
          size="sm"
          class="shrink-0 inline-flex items-center gap-1"
          :title="chip.title"
        >
          <HardDrive class="h-3 w-3" :stroke-width="2" />
          {{ chip.label }}
        </Badge>
        <Badge
          v-if="hiddenServerChips.length"
          tone="neutral"
          size="sm"
          class="shrink-0 inline-flex items-center gap-1"
          :title="hiddenServerTitle"
        >
          <HardDrive class="h-3 w-3" :stroke-width="2" />
          +{{ hiddenServerChips.length }}
        </Badge>
        <!-- Linked AWS account (credentials brokered per run). -->
        <Badge
          v-if="awsChip"
          tone="primary"
          size="sm"
          class="shrink-0 inline-flex items-center gap-1"
          :title="awsChip.title"
        >
          <Cloud class="h-3 w-3" :stroke-width="2" />
          {{ awsChip.label }}
          <span class="opacity-90 font-mono">{{ awsChip.region }}</span>
        </Badge>
      </div>
      <!-- Icon-only actions, split into three clusters: capture, open-elsewhere,
           then the session/project controls. Labels live in the tooltips so the
           header stays short no matter how narrow the pane gets.
           Hidden entirely in a session pop-out for now — that window is a bare
           conversation view. -->
      <div v-if="!isSessionWindow" class="flex items-center gap-1 shrink-0">
        <!-- Capture a thought without leaving the run (⌘K / ⌘⇧N do the same).
             A todo and a note sit side by side so either is one click away. -->
        <Button
          variant="outline"
          size="icon"
          :title="t('run.quickCaptureTodoTitle')"
          @click="openQuickCapture('todo')"
        >
          <ListTodo class="h-4 w-4" :stroke-width="2" />
        </Button>
        <Button
          variant="outline"
          size="icon"
          :title="t('run.quickCaptureNoteTitle')"
          @click="openQuickCapture('note')"
        >
          <StickyNote class="h-4 w-4" :stroke-width="2" />
        </Button>

        <div class="h-4 w-px bg-border shrink-0 mx-1" aria-hidden="true" />

        <!-- VS Code / Finder / terminal are the same intent — open this project
             somewhere else — so they collapse into one trigger. -->
        <DropdownMenu align="right">
          <template #trigger>
            <Button
              variant="outline"
              size="icon"
              :disabled="!project"
              :title="t('run.openProjectIn')"
            >
              <ExternalLink class="h-4 w-4" :stroke-width="2" />
            </Button>
          </template>
          <DropdownItem :disabled="!project" @click="handleOpenInVscode">
            <Code2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
            VS Code
          </DropdownItem>
          <DropdownItem :disabled="!project" @click="handleOpenInFolder">
            <FolderOpen class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
            {{ t('run.folder') }}
          </DropdownItem>
          <DropdownItem :disabled="!project" @click="handleOpenInTerminal">
            <Terminal class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
            {{ t('run.terminal') }}
          </DropdownItem>
        </DropdownMenu>

        <div class="h-4 w-px bg-border shrink-0 mx-1" aria-hidden="true" />

        <Button
          variant="outline"
          size="icon"
          :disabled="!currentRunId"
          :title="remoteButtonTitle"
          class="relative"
          @click="remoteModalOpen = true"
        >
          <Radio
            class="h-4 w-4"
            :class="remoteActive ? 'text-emerald-500' : remotePaired ? 'text-emerald-500/50' : ''"
            :stroke-width="2"
          />
          <!-- Live pulse ONLY when the phone is looking at THIS run. -->
          <span
            v-if="remoteActive"
            class="absolute -right-0.5 -top-0.5 flex h-2.5 w-2.5"
            :aria-label="t('run.remoteConnecting')"
          >
            <span class="absolute inline-flex h-full w-full rounded-full bg-emerald-500 opacity-75 animate-ping" />
            <span class="relative inline-flex h-2.5 w-2.5 rounded-full bg-emerald-500 ring-2 ring-background" />
          </span>
        </Button>
        <Button
          v-if="!isSessionWindow"
          variant="outline"
          size="icon"
          :title="t('run.projectSettingsTitle')"
          @click="router.push(`/projects/${projectId}/settings`)"
        >
          <Settings class="h-4 w-4" :stroke-width="2" />
        </Button>
        <Button
          v-if="!isSessionWindow"
          variant="outline"
          size="icon"
          :title="uiLayout.focusMode ? t('run.exitFocusMode') : t('run.enterFocusMode')"
          @click="uiLayout.toggleFocus()"
        >
          <component :is="uiLayout.focusMode ? Minimize2 : Maximize2" class="h-4 w-4" :stroke-width="2" />
        </Button>
      </div>
    </div>

    <div class="flex flex-1 overflow-hidden">
      <!-- Left panel: controls + history (hidden in focus mode, and in a session
           pop-out — that window is locked to its one run, no switcher). -->
      <div v-if="!uiLayout.focusMode && !isSessionWindow" class="w-72 shrink-0 border-r border-border/60 flex flex-col overflow-hidden bg-card/20">

        <!-- Rail tab switcher: Session (controls + history) vs Files (tree). -->
        <div class="flex shrink-0 border-b border-border/60 text-xs font-medium">
          <button
            class="flex-1 flex items-center justify-center gap-1.5 py-2 transition-colors cursor-pointer"
            :class="leftTab === 'session' ? 'text-foreground border-b-2 border-primary' : 'text-muted-foreground hover:text-foreground'"
            @click="leftTab = 'session'"
          >
            <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('run.session') }}
          </button>
          <button
            class="flex-1 flex items-center justify-center gap-1.5 py-2 transition-colors cursor-pointer"
            :class="leftTab === 'files' ? 'text-foreground border-b-2 border-primary' : 'text-muted-foreground hover:text-foreground'"
            @click="leftTab = 'files'"
          >
            <FolderTree class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('run.files') }}
          </button>
        </div>

        <!-- Compact top toolbar: New session, then a collapsible Fetch -->
        <div v-show="leftTab === 'session'" class="p-3 border-b border-border/60 space-y-2">
          <!-- Session type is chosen right here at the entry point: a plain
               conversation, or a conductor that orchestrates worker sessions.
               Both create a fresh session; "New conductor" pre-arms the composer's
               conductor toggle so the first message becomes the goal. -->
          <div class="flex gap-1.5">
            <Button class="flex-1" :disabled="creatingSession" @click="handleNewSession">
              <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
              {{ creatingSession ? t('run.creating') : t('run.newSession') }}
            </Button>
            <Button
              variant="outline"
              class="flex-1"
              :disabled="creatingSession"
              :title="t('conductor.newConductorHint')"
              @click="handleNewConductor"
            >
              <Network class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('conductor.newConductor') }}
            </Button>
          </div>

          <!-- Fetch toggle -->
          <Button
            variant="outline"
            class="w-full justify-between"
            :class="{ 'bg-accent/50': fetchOpen }"
            @click="fetchOpen = !fetchOpen"
          >
            <span class="flex items-center gap-1.5">
              <GitPullRequest class="h-3.5 w-3.5" :stroke-width="1.75" />
              {{ t('run.fetchIssuePr') }}
            </span>
            <ChevronDown class="h-3.5 w-3.5 transition-transform" :class="{ 'rotate-180': fetchOpen }" :stroke-width="1.75" />
          </Button>

          <!-- Fetch form (collapsible) -->
          <div v-if="fetchOpen" class="space-y-3 pt-1">

          <div v-if="repos.length === 0" class="text-[10px] text-amber-500/80 leading-relaxed">
            {{ t('run.noReposConfigured') }}
          </div>

          <!-- The URL is the whole form: it names the repo, the kind and the
               number, so there is nothing left to pick by hand. -->
          <div>
            <Input
              :model-value="refInput"
              @update:model-value="handleRefInput"
              :placeholder="t('run.refPlaceholder')"
              @keyup.enter="handleFetch()"
            />
            <p v-if="fetchError" class="mt-1 text-[10px] text-destructive">{{ fetchError }}</p>
          </div>

          <!-- Echo back what the URL was understood to mean. -->
          <div
            v-if="detectedRef"
            class="flex items-center gap-1.5 px-2 py-1 text-[11px] rounded-md bg-muted text-muted-foreground"
          >
            <component
              :is="detectedRef.kind === 'pr' ? GitPullRequest : Bug"
              class="h-3 w-3 shrink-0"
              :stroke-width="1.75"
            />
            <span class="truncate">
              {{ detectedRef.kind === 'pr' ? t('run.labelPr') : t('run.issue') }}
              #{{ detectedRef.number }} · {{ detectedRef.repoName }}
            </span>
          </div>

          <Button
            v-if="!needsLinkedIssue"
            variant="outline"
            class="w-full"
            :disabled="fetching || !detectedRef"
            @click="handleFetch()"
          >
            {{ fetching
              ? t('run.fetching')
              : detectedRef?.kind === 'pr' ? t('run.fetchPr')
              : detectedRef?.kind === 'issue' ? t('run.fetchIssue')
              : t('run.fetchIssuePr') }}
          </Button>

          <!-- Linked issue prompt (shown when PR body has no Fixes/Closes/Resolves #N) -->
          <div v-if="needsLinkedIssue" class="space-y-2">
            <Input
              v-model="linkedIssueInput"
              :placeholder="t('run.linkedIssuePlaceholder')"
              @keyup.enter="handleSubmitLinkedIssue"
            />
            <Button
              variant="outline"
              class="w-full"
              :disabled="fetching || !linkedIssueInput"
              @click="handleSubmitLinkedIssue"
            >
              {{ fetching ? t('run.fetching') : t('run.fetchWithLinkedIssue') }}
            </Button>
          </div>
          </div>
        </div>

        <!-- Run history -->
        <!-- The header (title + Clear all + search) is pinned: only the rows
             below it scroll, so the filter and the bulk action stay reachable
             no matter how far down the list the user is. -->
        <div v-show="leftTab === 'session'" class="flex-1 min-h-0 flex flex-col">
          <div class="shrink-0 border-b border-border/40">
            <div class="flex items-center justify-between px-4 py-2.5">
              <p class="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider">{{ t('run.history') }}</p>
              <button
                v-if="runsStore.runs.length > 0"
                class="flex items-center gap-1 text-[10px] text-muted-foreground/70 hover:text-destructive transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
                :disabled="clearingAll"
                :title="clearingAll ? t('run.clearing') : t('run.clearAllRunsTitle')"
                @click="handleClearAllRuns"
              >
                <Trash2 class="h-3 w-3" :stroke-width="1.75" />
                {{ clearingAll ? t('run.clearing') : t('run.clearAll') }}
              </button>
            </div>
            <div v-if="runsStore.runs.length > 0" class="px-3 pb-2.5">
              <div class="relative">
                <Search class="pointer-events-none absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-muted-foreground" :stroke-width="1.75" />
                <Input
                  v-model="sessionSearch"
                  :placeholder="t('run.searchSessionsPlaceholder')"
                  class="h-8 pl-8 pr-8 text-xs"
                />
                <button
                  v-if="sessionSearch"
                  type="button"
                  class="absolute right-2 top-1/2 -translate-y-1/2 text-muted-foreground hover:text-foreground cursor-pointer"
                  :title="t('run.clearSearch')"
                  @click="sessionSearch = ''"
                >
                  <X class="h-3.5 w-3.5" :stroke-width="2" />
                </button>
              </div>
            </div>
          </div>
          <div ref="sessionListEl" class="flex-1 min-h-0 overflow-auto">
            <div v-if="runsStore.loading" class="px-4 py-3 text-xs text-muted-foreground">{{ t('common.loading') }}</div>
            <div v-else-if="runsStore.runs.length === 0" class="px-4 py-6 text-center text-xs text-muted-foreground">
              {{ t('run.noSessionsYet') }}
            </div>
            <div v-else-if="filteredRuns.length === 0" class="px-4 py-6 text-center text-xs text-muted-foreground">
              {{ t('run.noMatchingSessions') }}
            </div>
            <div
              v-else
              v-for="{ run } in displayRuns"
              :key="run.id"
              :data-run-id="run.id"
              class="group relative border-b border-border/30 transition-colors hover:bg-accent/40 focus-within:bg-accent/40"
              :class="{ 'bg-accent/60': currentRunId === run.id }"
            >
              <!-- Selected indicator bar -->
              <span
                v-if="currentRunId === run.id"
                class="absolute left-0 inset-y-0 w-0.5 bg-primary"
              />
              <button
                class="w-full text-left px-3 py-3 cursor-pointer rounded-sm focus:outline-none focus-visible:ring-1 focus-visible:ring-ring focus-visible:ring-inset"
                @click="loadRunLog(run.id)"
              >
                <!-- Title + status -->
                <div class="flex items-center gap-2">
                  <component
                    :is="runIcon(run)"
                    class="h-4 w-4 shrink-0"
                    :class="[runIconColorClass(run), { 'animate-spin': runIsRunning(run) }]"
                    :stroke-width="1.75"
                    :title="isConductorRun(run)
                      ? t('conductor.modeToggle')
                      : (run.status === 'fetched' && run.run_type === 'session'
                        ? t('common.status.draft')
                        : t(`common.status.${run.status}`))"
                  />
                  <!-- Pin / protect badges grouped so they stay tightly aligned
                       when both are shown (own gap, not the row's gap-2). -->
                  <span
                    v-if="run.pinned || run.protected || isProtectedByConductor(run)"
                    class="flex shrink-0 items-center gap-1"
                  >
                    <Pin
                      v-if="run.pinned"
                      class="h-3 w-3 text-primary"
                      :stroke-width="2"
                      :aria-label="t('run.pinned')"
                    />
                    <Shield
                      v-if="run.protected"
                      class="h-3 w-3 text-emerald-500"
                      :stroke-width="2"
                      :aria-label="t('run.protected')"
                    />
                    <Shield
                      v-else-if="isProtectedByConductor(run)"
                      class="h-3 w-3 text-emerald-500/50"
                      :stroke-width="2"
                      :aria-label="t('run.protectedByConductor')"
                      :title="t('run.protectedByConductor')"
                    />
                  </span>
                  <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight" :title="runLabel(run)">{{ runLabel(run) }}</span>
                  <!-- Remote-control marker: a phone is driving (green, pulsing) or
                       a link is waiting for one (amber) on THIS session. Lets the
                       user spot the remotely-controlled session in the list. -->
                  <span
                    v-if="remoteRowMarker && remoteRowMarker.runId === run.id"
                    class="relative flex h-4 w-4 shrink-0 items-center justify-center"
                    :class="remoteRowMarker.tone === 'success' ? 'text-emerald-500' : 'text-amber-500'"
                    :title="remoteRowMarker.title"
                  >
                    <span
                      v-if="remoteRowMarker.tone === 'success'"
                      class="absolute inset-0 animate-ping rounded-full bg-emerald-500/30"
                    />
                    <Radio class="relative h-3.5 w-3.5" :stroke-width="2" />
                  </span>
                  <!-- Animated attention marker: this run is waiting for a
                       permission / question answer (replaces the floating toast). -->
                  <span
                    v-if="pendingRequest(run.id)"
                    class="relative flex h-4 w-4 shrink-0 items-center justify-center text-primary"
                    :title="
                      pendingRequest(run.id)?.tool_name === 'AskUserQuestion'
                        ? t('run.waitingForAnswer')
                        : t('run.waitingForPermission')
                    "
                  >
                    <span class="absolute inset-0 animate-ping rounded-full bg-primary/30" />
                    <component
                      :is="pendingRequest(run.id)?.tool_name === 'AskUserQuestion' ? MessageCircleQuestion : ShieldQuestion"
                      class="relative h-4 w-4"
                      :stroke-width="2"
                    />
                  </span>
                </div>
                <!-- Meta: the AI badges (account + model) on top so a long model
                     id gets the full row width, with the timestamp on its own line
                     underneath. Always visible so hovering never hides the session
                     info. -->
                <div
                  class="mt-2 pl-6 pr-3 text-[10px] text-muted-foreground/70"
                  :class="{ invisible: !runStarted(run) }"
                >
                  <!-- Account badge shown alongside the model when the run has a
                       known Claude account (the name implies Claude); Codex and
                       legacy/account-less runs show just the model. `pr-6` keeps
                       the badges clear of the hover actions (⋯) at the row's end. -->
                  <div class="flex flex-wrap items-center gap-1.5 pr-6">
                    <span
                      v-if="runClaudeAccountLabel(run)"
                      class="shrink-0 truncate max-w-[10rem] px-1.5 py-0.5 rounded bg-primary/10 text-[9px] font-medium tracking-wide text-primary"
                      :title="runClaudeAccountLabel(run)"
                    >
                      {{ runClaudeAccountLabel(run) }}
                    </span>
                    <!-- The model this run actually runs on (falls back to engine). -->
                    <span
                      class="min-w-0 max-w-full truncate px-1.5 py-0.5 rounded bg-muted/60 font-mono text-[9px] tracking-wide text-muted-foreground"
                      :title="runModelLabel(run)"
                    >
                      {{ runModelLabel(run) }}
                    </span>
                  </div>
                  <span class="mt-1 flex items-center gap-1" :title="t('run.lastActivity')">
                    <Clock class="h-2.5 w-2.5 shrink-0" :stroke-width="1.5" />
                    {{ runTimestamp(runActivityAt(run)) }}
                  </span>
                </div>
              </button>
              <!-- Actions: a single overflow (⋯) menu revealed on hover/focus, so
                   the row's meta info stays fully visible. The trigger also stays
                   put while its menu is open (actionsMenuRunId). Running sessions
                   get a safe subset — delete/refetch are hidden while live. -->
              <div
                class="absolute right-2 bottom-2 flex items-center opacity-0 pointer-events-none transition-opacity group-hover:opacity-100 group-hover:pointer-events-auto group-focus-within:opacity-100 group-focus-within:pointer-events-auto"
                :class="{ '!opacity-100 !pointer-events-auto': actionsMenuRunId === run.id }"
              >
                <DropdownMenu
                  align="right"
                  @update:open="(o) => (actionsMenuRunId = o ? run.id : null)"
                >
                  <template #trigger>
                    <button
                      type="button"
                      class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground/70 hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
                      :aria-label="t('run.moreActions')"
                      :title="t('run.moreActions')"
                    >
                      <MoreHorizontal class="h-3.5 w-3.5" :stroke-width="1.75" />
                    </button>
                  </template>

                  <!-- Pop this session out into its own OS window (one per run).
                       Hidden inside a session window — can't re-pop what's popped. -->
                  <DropdownItem v-if="!isSessionWindow" @click="popoutSession(run.id)">
                    <AppWindow class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.popOutSession') }}
                  </DropdownItem>
                  <DropdownItem v-if="runGithubUrl(run)" @click="openRunInBrowser(run)">
                    <ExternalLink class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ run.run_type === 'analyze_issue' ? t('run.openIssueOnGithub') : t('run.openPrOnGithub') }}
                  </DropdownItem>
                  <DropdownItem
                    v-if="run.run_type !== 'session' && run.status !== 'running'"
                    :disabled="refetchingRunId === run.id"
                    @click="handleRefetch(run.id)"
                  >
                    <RefreshCw class="h-3.5 w-3.5 shrink-0" :class="{ 'animate-spin': refetchingRunId === run.id }" :stroke-width="1.75" />
                    {{ run.run_type === 'analyze_issue' ? t('run.refetchIssue') : t('run.refetchPr') }}
                  </DropdownItem>
                  <DropdownItem @click="copyLogPath(run)">
                    <ClipboardCopy class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.copyLogPath') }}
                  </DropdownItem>
                  <DropdownItem @click="copySessionId(run)">
                    <Hash class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.copySessionId') }}
                  </DropdownItem>
                  <DropdownItem @click="viewLogFile(run)">
                    <ScrollText class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.viewLogFile') }}
                  </DropdownItem>
                  <DropdownSeparator />
                  <DropdownItem @click="startRename(run)">
                    <Pencil class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.rename') }}
                  </DropdownItem>
                  <DropdownItem :disabled="pinningRunId === run.id" @click="handleTogglePin(run)">
                    <component :is="run.pinned ? PinOff : Pin" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ run.pinned ? t('run.unpinFromTop') : t('run.pinToTop') }}
                  </DropdownItem>
                  <DropdownItem :disabled="protectingRunId === run.id" @click="handleToggleProtect(run)">
                    <component :is="run.protected ? ShieldOff : Shield" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ run.protected ? t('run.unprotect') : t('run.protect') }}
                  </DropdownItem>
                  <DropdownItem
                    v-if="run.run_type === 'session' && run.role !== 'conductor' && run.role !== 'worker' && run.status !== 'running' && run.status !== 'fetched'"
                    :disabled="convertingRunId === run.id"
                    @click="handleConvertToConductor(run)"
                  >
                    <Network class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    {{ t('run.convertToConductor') }}
                  </DropdownItem>
                  <template v-if="run.status !== 'running'">
                    <DropdownSeparator />
                    <DropdownItem
                      variant="destructive"
                      :disabled="deletingRunId === run.id"
                      @click="handleDeleteRun(run.id)"
                    >
                      <Trash2 class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                      {{ t('run.deleteThisRun') }}
                    </DropdownItem>
                  </template>
                </DropdownMenu>
              </div>

              <!-- Inline rename: overlays the row title while editing. -->
              <div
                v-if="renamingRunId === run.id"
                class="absolute inset-0 z-20 flex items-center gap-2 px-3 bg-card"
                @click.stop
              >
                <Pencil class="h-3.5 w-3.5 shrink-0 text-muted-foreground" :stroke-width="1.75" />
                <input
                  :ref="setRenameInputRef"
                  v-model="renameDraft"
                  type="text"
                  class="flex-1 min-w-0 bg-transparent text-[13px] font-medium leading-tight outline-none border-b border-primary/60 pb-0.5"
                  :placeholder="t('run.runNamePlaceholder')"
                  @keyup.enter="commitRename(run.id)"
                  @keyup.esc="cancelRename()"
                  @blur="commitRename(run.id)"
                />
                <Button variant="ghost" size="icon-sm" :aria-label="t('run.saveName')" :title="t('common.save')" @mousedown.prevent @click.stop="commitRename(run.id)">
                  <Check class="h-3.5 w-3.5" :stroke-width="2" />
                </Button>
                <Button variant="ghost" size="icon-sm" :aria-label="t('run.cancelRename')" :title="t('common.cancel')" @mousedown.prevent @click.stop="cancelRename()">
                  <X class="h-3.5 w-3.5" :stroke-width="2" />
                </Button>
              </div>
            </div>
          </div>
        </div>

        <!-- Files tab: VSCode-style lazy file tree for this project. -->
        <FileTree
          v-show="leftTab === 'files'"
          v-if="project?.path"
          :project-path="project.path"
          :active="leftTab === 'files'"
          @open-file="openFile"
        />

      </div>

      <!-- Terminal panel (split-pane: Content | AI Result) -->
      <div class="flex-1 flex flex-col overflow-hidden bg-background">
        <!-- No sessions yet: hide the Content / AI Result panels entirely and
             show a single centered empty state. -->
        <div v-if="noSession" class="flex-1 flex items-center justify-center p-6">
          <div class="text-center max-w-xs">
            <MessageSquare class="h-10 w-10 text-foreground/20 mx-auto mb-4" :stroke-width="1" />
            <p class="text-sm font-medium text-foreground/70 mb-1.5">{{ t('run.noSessionsYet') }}</p>
            <p class="text-xs text-foreground/40 leading-relaxed mb-4">
              {{ t('run.createNewSessionHint') }}
            </p>
            <div class="flex items-center justify-center gap-1.5">
              <Button :disabled="creatingSession" @click="handleNewSession">
                <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
                {{ creatingSession ? t('run.creating') : t('run.newSession') }}
              </Button>
              <Button
                variant="outline"
                :disabled="creatingSession"
                :title="t('conductor.newConductorHint')"
                @click="handleNewConductor"
              >
                <Network class="h-3.5 w-3.5" :stroke-width="2" />
                {{ t('conductor.newConductor') }}
              </Button>
            </div>
          </div>
        </div>

        <!-- Split container -->
        <div
          v-else
          ref="splitContainerEl"
          class="flex-1 flex overflow-hidden min-h-0"
        >
          <!-- Content column (hidden for sessions or when toggled off) -->
          <div
            v-if="contentVisible"
            class="flex flex-col overflow-hidden bg-background min-w-0 min-h-0"
            :style="{ width: contentWidth }"
          >
            <!-- h-8: same row height as the rail tab bar and the AI Result
                 header, so all three border lines meet up. -->
            <div class="flex items-center gap-2 px-3 h-8 bg-card border-b border-border shrink-0">
              <FileText class="h-3.5 w-3.5 text-foreground/40" :stroke-width="1.5" />
              <span class="text-[11px] font-mono text-foreground/60">{{ t('run.content') }}</span>
            </div>
            <div class="flex-1 min-h-0 overflow-auto p-4">
              <template v-if="!currentRunId">
                <div class="flex items-center justify-center h-full">
                  <div class="text-center">
                    <FileText class="h-8 w-8 text-foreground/20 mx-auto mb-3" :stroke-width="1" />
                    <p class="text-xs text-foreground/30 font-mono">{{ t('run.fetchToBegin') }}</p>
                  </div>
                </div>
              </template>
              <template v-else-if="inputContentLoading">
                <div class="flex items-center gap-2 text-xs text-muted-foreground">
                  <Loader2 class="h-3.5 w-3.5 animate-spin text-primary" :stroke-width="2" />
                  <span>{{ t('run.loadingContent') }}</span>
                </div>
              </template>
              <template v-else-if="inputContentError">
                <div class="text-xs text-destructive">
                  <p class="font-medium mb-1">{{ t('run.couldNotLoadContent') }}</p>
                  <p class="text-foreground/60">{{ inputContentError }}</p>
                </div>
              </template>
              <template v-else-if="inputContent">
                <div
                  v-file-links
                  v-mermaid
                  v-copy-code
                  v-html="renderText(inputContent)"
                  class="markdown-output text-sm leading-relaxed text-foreground"
                  @click="onProseClick"
                />
              </template>
              <template v-else>
                <p class="text-xs text-foreground/40">{{ t('run.noContentAvailable') }}</p>
              </template>
            </div>
          </div>

          <!-- Resize handle (only when both panels are visible) -->
          <div
            v-if="showResizeHandle"
            class="group relative shrink-0 w-px bg-border cursor-col-resize select-none"
            :class="{ 'bg-primary/60': isResizing }"
            @mousedown="startResize"
          >
            <!-- Wider hover hit area for easier grabbing. Extends only to the
                 right so it never overlaps the Content column's scrollbar. -->
            <div
              class="absolute inset-y-0 left-0 -right-2.5 z-10 transition-colors group-hover:bg-primary/30"
              :class="{ 'bg-primary/40': isResizing }"
            />
          </div>

          <!-- AI Result column (always shown; full width when Content is hidden) -->
          <div
            ref="resultSplitEl"
            class="relative flex overflow-hidden bg-background min-w-0 min-h-0"
            :style="{ width: resultWidth }"
          >
            <!-- AI result main content (left; always flex-1 so it fills the
                 space the moment the question panel appears/disappears — no
                 intermediate frame where the column is stuck at a fixed width). -->
            <div class="relative flex-1 flex flex-col overflow-hidden min-w-0 min-h-0">
            <div class="bg-card border-b border-border shrink-0">
              <!-- Title row: shows the current run / session title. Fixed h-8
                   (= rail tab bar height) so the header never grows when the
                   mentioned-files / re-dock controls appear. -->
              <div class="flex items-center gap-2 px-3 h-8">
                <MessageSquare class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.5" />
                <span class="text-xs font-medium text-foreground/90 truncate">
                  {{ currentRun ? runLabel(currentRun) : t('run.noRunSelected') }}
                </span>
                <div class="ml-auto flex items-center gap-1.5 shrink-0">
                  <!-- Prompt navigator: step between the user's own messages or
                       pick one from the list. Lives in the toolbar (not floating
                       over the chat) so it never covers the conversation. Only
                       worth showing once there is more than one prompt. -->
                  <div
                    v-if="showTurnNav"
                    class="flex items-center h-6 rounded border border-border text-[11px] font-mono text-foreground/70"
                  >
                    <button
                      type="button"
                      class="flex items-center h-full rounded-l px-1 transition-colors hover:bg-accent/60 hover:text-foreground cursor-pointer"
                      :title="t('run.prevPrompt')"
                      @click="prevTurn"
                    >
                      <ChevronUp class="h-3.5 w-3.5" :stroke-width="2" />
                    </button>
                    <DropdownMenu align="right" @update:open="onTurnMenuToggle">
                      <template #trigger>
                        <button
                          type="button"
                          class="flex items-center gap-1 h-full border-x border-border px-1.5 transition-colors hover:bg-accent/60 hover:text-foreground cursor-pointer"
                          :title="t('run.promptList')"
                        >
                          <MessageSquare class="h-3.5 w-3.5" :stroke-width="1.75" />
                          {{ navTurns.length }}
                        </button>
                      </template>
                      <div class="max-h-96 w-[24rem] max-w-[80vw] overflow-y-auto">
                        <div class="px-2.5 pt-1 pb-1.5 text-[10px] font-medium uppercase tracking-wide text-foreground/40">
                          {{ t('run.promptList') }}
                        </div>
                        <DropdownItem
                          v-for="(turn, ti) in navTurns"
                          :key="ti"
                          :variant="ti === activeTurnSeq ? 'primary' : 'default'"
                          class="!items-start"
                          @click="jumpToSessionTurn(ti, true)"
                        >
                          <span
                            class="mt-px flex h-4 min-w-4 shrink-0 items-center justify-center rounded px-1 text-[10px] font-mono tabular-nums"
                            :class="ti === activeTurnSeq ? 'bg-primary/15 text-primary' : 'bg-muted text-foreground/45'"
                          >{{ ti + 1 }}</span>
                          <span
                            class="min-w-0 flex-1 whitespace-normal break-words leading-snug line-clamp-2"
                            :class="turn.label ? '' : 'italic text-foreground/40'"
                          >{{ turn.label || t('run.promptNoText') }}</span>
                        </DropdownItem>
                      </div>
                    </DropdownMenu>
                    <button
                      type="button"
                      class="flex items-center h-full rounded-r px-1 transition-colors hover:bg-accent/60 hover:text-foreground cursor-pointer"
                      :title="t('run.nextPrompt')"
                      @click="nextTurn"
                    >
                      <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
                    </button>
                  </div>
                  <!-- Issue / PR runs only: opens the fetched Content as a left
                       split column. Sessions have no Content to show. -->
                  <button
                    v-if="canToggleContent"
                    type="button"
                    class="flex items-center gap-1 rounded h-6 px-1.5 text-[11px] font-medium transition-colors cursor-pointer"
                    :class="contentVisible ? 'bg-primary/15 text-primary' : 'text-foreground/50 hover:text-foreground/80 hover:bg-accent/60'"
                    :title="t('run.toggleContentPanel')"
                    :aria-pressed="contentVisible"
                    @click="showContent = !showContent"
                  >
                    <FileText class="h-3.5 w-3.5" :stroke-width="1.75" />
                    {{ t('run.content') }}
                  </button>
                  <MentionedFiles :entries="mentionedFileEntries" @open-file="openFile" />
                  <span v-if="currentStatus === 'running'" class="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
                </div>
              </div>
            </div>

            <!-- Log viewer (selected history run) -->
            <div
              v-if="isViewingHistory"
              ref="historyEl"
              class="flex-1 min-h-0 overflow-auto p-4"
              @scroll="onHistoryScroll"
            >
              <!-- Loading animation while the log is read + parsed from disk. -->
              <div
                v-if="historyLoading"
                class="flex h-full flex-col items-center justify-center gap-3 text-muted-foreground"
              >
                <Loader2 class="h-6 w-6 animate-spin text-primary" :stroke-width="2" />
                <span class="text-xs">{{ t('run.loadingLog') }}</span>
              </div>
              <template v-else-if="historyHasStream">
                <!-- Fetch the next window of older records on demand. Opening a
                     run only reads its tail, so the first paint stays fast no
                     matter how long the conversation got. -->
                <div v-if="historyHasMore" class="mb-3 flex justify-center">
                  <button
                    type="button"
                    class="rounded-full border border-border bg-muted/60 px-3 py-1 text-[11px] text-foreground/70 hover:bg-accent/60 transition-colors cursor-pointer disabled:opacity-50"
                    :disabled="historyLoadingMore"
                    @click="showEarlierHistory"
                  >
                    {{ historyLoadingMore ? t('common.loading') : t('run.showEarlier', { count: HISTORY_PAGE_STEP }) }}
                  </button>
                </div>
                <StreamLog
                  :entries="historyEntries"
                  :running="false"
                  :render-text="renderText"
                  :file-matcher="fileMatcher"
                  @open-file="openFile"
                  @open-url="onOpenUrl"
                />
                <!-- Newer records below the loaded window (only after a jump into
                     the middle of the log). Scrolling down loads them too. -->
                <div v-if="historyHasMoreBelow" class="mt-3 flex justify-center">
                  <button
                    type="button"
                    class="rounded-full border border-border bg-muted/60 px-3 py-1 text-[11px] text-foreground/70 hover:bg-accent/60 transition-colors cursor-pointer disabled:opacity-50"
                    :disabled="historyLoadingMore"
                    @click="showLaterHistory"
                  >
                    {{ historyLoadingMore ? t('common.loading') : t('run.showLater', { count: HISTORY_PAGE_STEP }) }}
                  </button>
                </div>
              </template>
              <div
                v-else
                v-file-links
                v-mermaid
                v-copy-code
                v-html="historyLegacyHtml"
                class="markdown-output text-sm leading-relaxed text-foreground"
                @click="onProseClick"
              />
            </div>

            <!-- Live output -->
            <div
              v-else-if="hasLiveOutput"
              ref="outputEl"
              class="flex-1 min-h-0 overflow-auto p-4"
              @scroll="onOutputScroll"
              @pointerdown="onOutputPointerDown"
            >
              <StreamLog
                v-if="liveHasStream"
                :entries="liveEntries"
                :running="currentStatus === 'running'"
                :render-text="renderText"
                :file-matcher="fileMatcher"
                @open-file="openFile"
                @open-url="onOpenUrl"
              />
              <template v-else>
                <div
                  v-file-links
                  v-mermaid
                  v-copy-code
                  v-html="liveLegacyHtml"
                  class="markdown-output text-sm leading-relaxed text-foreground"
                  @click="onProseClick"
                />
                <div v-if="currentStatus === 'running'" class="text-emerald-500 dark:text-emerald-400 animate-pulse mt-1 select-none font-mono text-xs">▌</div>
              </template>
            </div>

            <!-- Empty state -->
            <div v-else class="flex-1 flex items-center justify-center p-4">
              <div class="text-center max-w-xs">
                <Terminal class="h-8 w-8 text-foreground/20 mx-auto mb-3" :stroke-width="1" />
                <template v-if="currentRunId && currentStatus === 'fetched'">
                  <p class="text-xs font-medium text-amber-500/80 font-mono mb-1">{{ t('run.notRun') }}</p>
                  <p class="text-[11px] text-foreground/40 leading-relaxed">
                    {{ t('run.notRunHint') }}
                  </p>
                </template>
                <p v-else class="text-xs text-foreground/30 font-mono">
                  {{ t('run.fetchIssueOrPrThenRun') }}
                </p>
              </div>
            </div>

            <!-- Jump-to-latest button: shown when the user has scrolled up
                 away from the bottom of the live output. The prompt navigator
                 now lives in the toolbar, so this always sits at the bottom. -->
            <button
              v-if="hasLiveOutput && !isViewingHistory && !stickToBottom"
              class="absolute bottom-4 right-4 z-10 flex items-center gap-1 rounded-full border border-border bg-card px-3 py-1.5 text-[11px] font-mono text-foreground/80 shadow-md transition-colors hover:bg-card hover:text-foreground cursor-pointer"
              :title="t('run.scrollToLatest')"
              @click="stickToBottom = true; scrollOutputToBottom()"
            >
              <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('run.latest') }}
            </button>
            </div>

            <!-- The permission / question prompt is no longer an inline drawer:
                 it pops out into its own window the instant a request arrives
                 (see the openPermissionPopout watch), so the chat is never
                 covered. -->
          </div>
        </div>

        <!-- Composer: prompt input on top, run controls in the footer below -->
        <div
          v-if="composerVisible"
          class="border-t border-border bg-card/40 px-3 py-2 shrink-0"
        >
          <div class="relative">
            <!-- Slash-command palette (anchored above the textarea) -->
            <div
              v-if="slashOpen && slashItems.length"
              class="absolute bottom-full left-0 mb-1 max-h-60 w-72 max-w-[80vw] overflow-y-auto rounded-md border border-border bg-popover shadow-lg z-20"
            >
              <ul class="py-1 text-xs">
                <li
                  v-for="(cmd, i) in slashItems"
                  :key="cmd"
                  class="flex items-center gap-2 px-3 py-1.5 cursor-pointer font-mono"
                  :class="i === slashIndex ? 'bg-primary/15 text-foreground' : 'text-foreground/80 hover:bg-muted'"
                  @mousedown.prevent="selectSlash(cmd)"
                  @mouseenter="slashIndex = i"
                >
                  <span class="text-primary">/</span>
                  <span class="truncate">{{ cmd }}</span>
                </li>
              </ul>
            </div>

            <!-- @-mention autocomplete popup (anchored above the textarea) -->
            <div
              v-if="mentionOpen && mentionItems.length"
              class="absolute bottom-full left-0 mb-1 max-h-60 w-[28rem] max-w-[80vw] overflow-y-auto rounded-md border border-border bg-popover shadow-lg z-20"
            >
              <ul class="py-1 text-xs">
                <li
                  v-for="(item, i) in mentionItems"
                  :key="item.path"
                  class="flex items-center gap-2 px-3 py-1.5 cursor-pointer font-mono"
                  :class="i === mentionIndex ? 'bg-primary/15 text-foreground' : 'text-foreground/80 hover:bg-muted'"
                  @mousedown.prevent="selectMention(item)"
                  @mouseenter="mentionIndex = i"
                >
                  <component :is="item.is_dir ? FolderClosed : FileText" class="h-3.5 w-3.5 shrink-0 opacity-60" :stroke-width="2" />
                  <span class="truncate">{{ item.path }}{{ item.is_dir ? '/' : '' }}</span>
                </li>
              </ul>
            </div>

            <!-- Pasted/attached image thumbnails -->
            <div v-if="pendingImages.length" class="flex flex-wrap gap-2 mb-2">
              <div
                v-for="img in pendingImages"
                :key="img.id"
                class="relative h-16 w-16 rounded-md border border-border overflow-hidden group"
              >
                <img :src="img.url" class="h-full w-full object-cover" alt="attachment" />
                <button
                  class="absolute top-0.5 right-0.5 h-4 w-4 rounded-full bg-background/80 border border-border flex items-center justify-center text-foreground/70 hover:text-foreground hover:bg-background cursor-pointer opacity-0 group-hover:opacity-100 transition-opacity"
                  :title="t('run.removeImage')"
                  @click="removePendingImage(img.id)"
                >
                  <X class="h-2.5 w-2.5" :stroke-width="2.5" />
                </button>
              </div>
            </div>

            <!-- Attached files referenced by path (drag-dropped non-images) -->
            <div v-if="pendingFiles.length" class="flex flex-wrap gap-1.5 mb-2">
              <div
                v-for="file in pendingFiles"
                :key="file.id"
                class="group flex items-center gap-1.5 h-7 pl-2 pr-1 rounded-md border border-border bg-muted/40 text-xs max-w-64"
                :title="file.path"
              >
                <FileText class="h-3.5 w-3.5 shrink-0 opacity-60" :stroke-width="2" />
                <span class="truncate font-mono">{{ file.name }}</span>
                <button
                  class="h-4 w-4 rounded-full flex items-center justify-center text-foreground/50 hover:text-foreground hover:bg-background cursor-pointer shrink-0"
                  :title="t('run.removeFile')"
                  @click="removePendingFile(file.id)"
                >
                  <X class="h-2.5 w-2.5" :stroke-width="2.5" />
                </button>
              </div>
            </div>

            <input
              ref="imageInputEl"
              type="file"
              accept="image/*"
              multiple
              class="hidden"
              @change="onPickImages"
            />

            <!-- Usage row: this run's context window on the left, the account
                 plan-usage chips filling the width that used to sit empty.
                 Both parts self-hide, so the row collapses when there is
                 nothing to report. -->
            <div class="flex items-center gap-2 px-1 pb-1.5 min-w-0">
              <ContextMeter
                :tokens="contextTokens"
                :model="contextModel"
                :rate-limit="contextRateLimit"
                @compact="sendSlashCommand('/compact')"
              />
              <span v-if="contextTokens > 0" class="shrink-0 text-[10px] text-muted-foreground/40">·</span>
              <BudgetBadge />
            </div>

            <!-- Prompt input card: textarea on top, controls toolbar below -->
            <div class="relative rounded-lg border border-border bg-background focus-within:ring-1 focus-within:ring-ring transition-colors">
              <!-- Drag-and-drop hint: shown while an OS file is dragged over the window -->
              <div
                v-if="isDraggingFile"
                class="absolute inset-0 z-10 flex items-center justify-center gap-2 rounded-lg border-2 border-dashed border-primary bg-primary/10 text-xs font-medium text-primary pointer-events-none"
              >
                <Paperclip class="h-4 w-4" :stroke-width="2" />
                {{ t('run.dropFileToAttach') }}
              </div>
              <textarea
                ref="composerEl"
                v-model="followUpInput"
                rows="2"
                :placeholder="chatPlaceholder"
                class="w-full resize-none px-3 pt-2.5 pb-1 bg-transparent text-xs focus:outline-none font-mono min-h-[2.75rem] leading-relaxed"
                :disabled="sendingFollowUp"
                @keydown="onComposerKeydown"
                @input="onComposerInput"
                @click="onComposerInput"
                @compositionend="onComposerInput"
                @paste="onComposerPaste"
                @blur="mentionOpen = false; slashOpen = false"
              />

              <!-- Footer toolbar: attachments/actions plus compact run settings menu -->
              <div class="flex flex-wrap items-center gap-1.5 px-1.5 pb-1.5">
                <button
                  class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                  :disabled="sendingFollowUp"
                  :title="t('run.attachImage')"
                  @click="imageInputEl?.click()"
                >
                  <ImagePlus class="h-4 w-4" :stroke-width="2" />
                </button>
                <button
                  class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                  :disabled="sendingFollowUp"
                  :title="t('run.attachFileByPath')"
                  @click="onPickFiles"
                >
                  <Paperclip class="h-4 w-4" :stroke-width="2" />
                </button>
                <DropdownMenu v-if="savedPrompts.length" align="left" class="shrink-0">
                  <template #trigger>
                    <button
                      class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                      :disabled="sendingFollowUp"
                      :title="t('run.savedPrompts')"
                      :aria-label="t('run.savedPrompts')"
                    >
                      <BookMarked class="h-4 w-4" :stroke-width="2" />
                    </button>
                  </template>
                  <div class="max-h-64 max-w-72 overflow-y-auto">
                    <DropdownItem v-for="p in savedPrompts" :key="p.id" @click="insertSavedPrompt(p)">
                      <span class="min-w-0 flex-1 truncate">{{ promptLabel(p) }}</span>
                    </DropdownItem>
                  </div>
                </DropdownMenu>
                <button
                  class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                  :title="composerExpanded ? t('run.collapseComposer') : t('run.expandComposer')"
                  @click="toggleComposerExpand"
                >
                  <component :is="composerExpanded ? Minimize2 : Maximize2" class="h-4 w-4" :stroke-width="2" />
                </button>
                <div
                  ref="runSettingsRoot"
                  class="relative inline-flex shrink-0"
                  @keydown.esc.stop="closeRunSettingsMenu"
                >
                  <Button
                    variant="ghost"
                    size="sm"
                    class="h-8 gap-1.5 px-2.5 text-foreground/70"
                    :title="runSettingsTitle"
                    :aria-label="runSettingsTitle"
                    aria-haspopup="dialog"
                    :aria-expanded="runSettingsOpen"
                    @click.stop="toggleRunSettingsMenu"
                  >
                    <component :is="engineBadgeIcon" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                    <span class="text-xs font-medium shrink-0">{{ engineBadgeLabel }}</span>
                    <span
                      v-if="composerModelLabel"
                      class="max-w-[9rem] truncate font-mono text-[10px] text-muted-foreground"
                      :title="composerModelLabel"
                    >· {{ composerModelLabel }}</span>
                    <ChevronDown
                      class="h-3 w-3 shrink-0 text-muted-foreground transition-transform duration-200"
                      :class="{ 'rotate-180': runSettingsOpen }"
                      :stroke-width="2"
                    />
                  </Button>
                  <transition
                    enter-active-class="transition duration-150 ease-out"
                    enter-from-class="opacity-0 translate-y-1"
                    leave-active-class="transition duration-100 ease-in"
                    leave-to-class="opacity-0 translate-y-1"
                  >
                    <div
                      v-if="runSettingsOpen"
                      role="dialog"
                      class="absolute bottom-full left-1/2 z-30 mb-2 w-72 max-w-[calc(100vw-2rem)] -translate-x-1/2 rounded-md border border-border bg-popover p-3 shadow-lg shadow-black/20"
                      :aria-label="t('run.runSettingsMenu')"
                    >
                      <div class="space-y-3">
                        <div class="space-y-1.5">
                          <div class="flex items-center gap-1.5 text-[10px] font-medium uppercase text-muted-foreground">
                            <Sparkles class="h-3 w-3 shrink-0" :stroke-width="1.5" />
                            <span>{{ t('run.modelSetting') }}</span>
                          </div>
                          <AppSelect
                            :model-value="modelOverride"
                            @update:model-value="onModelSelect"
                            size="sm"
                            :options="modelOptions"
                            :disabled="currentStatus === 'running'"
                            class="h-8 w-full"
                            :title="t('run.modelTitle')"
                          >
                            <template #leading>
                              <Sparkles class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
                            </template>
                          </AppSelect>
                        </div>
                        <div v-if="showClaudeAccountSelect" class="space-y-1.5">
                          <div class="flex items-center gap-1.5 text-[10px] font-medium uppercase text-muted-foreground">
                            <UserCircle class="h-3 w-3 shrink-0" :stroke-width="1.5" />
                            <span>{{ t('run.claudeAccountSetting') }}</span>
                          </div>
                          <AppSelect
                            :model-value="selectedClaudeAccountId"
                            @update:model-value="handleClaudeAccountChange"
                            size="sm"
                            :options="claudeAccountOptions"
                            :disabled="claudeAccountSelectDisabled"
                            class="h-8 w-full"
                            :title="claudeAccountSelectTitle"
                          >
                            <template #leading>
                              <UserCircle class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
                            </template>
                          </AppSelect>
                        </div>
                        <div class="space-y-1.5">
                          <div class="flex items-center gap-1.5 text-[10px] font-medium uppercase text-muted-foreground">
                            <ShieldQuestion class="h-3 w-3 shrink-0" :stroke-width="1.5" />
                            <span>{{ t('run.permissionSetting') }}</span>
                          </div>
                          <AppSelect
                            v-model="permissionMode"
                            size="sm"
                            :options="PERMISSION_MODE_OPTIONS"
                            :disabled="currentStatus === 'running'"
                            class="h-8 w-full"
                            :title="t('run.permissionModeTitle')"
                          >
                            <template #leading>
                              <span class="text-[10px] font-mono text-muted-foreground shrink-0">{{ t('run.perm') }}</span>
                            </template>
                          </AppSelect>
                        </div>
                      </div>
                    </div>
                  </transition>
                </div>

                <div class="ml-auto flex items-center gap-1.5 shrink-0">
                  <Button
                    v-if="currentStatus === 'running'"
                    variant="destructive"
                    class="h-8 shrink-0"
                    :title="t('run.cancelRunningTurn')"
                    @click="handleCancel"
                  >
                    <Square class="h-3.5 w-3.5" :stroke-width="2" fill="currentColor" />
                    {{ t('run.cancel') }}
                  </Button>
                  <button
                    class="inline-flex items-center justify-center gap-1.5 h-8 px-3.5 text-xs bg-primary text-primary-foreground rounded-md hover:opacity-90 transition-opacity cursor-pointer disabled:opacity-50 shrink-0"
                    :disabled="primaryDisabled"
                    @click="handlePrimaryAction"
                    :title="currentStatus === 'running'
                      ? t('run.sendMessageEnter')
                      : currentStatus === 'fetched'
                        ? t('run.startRunEnter')
                        : chatEligible
                          ? t('run.resumeAndSend')
                          : t('run.rerunWithDefault')"
                  >
                    <component :is="chatSendLabel === t('run.sendLabelRun') ? Play : Send" class="h-3.5 w-3.5" :stroke-width="2" />
                    {{ chatSendLabel }}
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </div>

      <!-- Conductor worker sidebar: for a live conductor, and also for a
           "New conductor" session not yet launched (so its worker limit can be
           set before the goal is sent). The conductor itself is just this
           session; this panel adds its worker tree + control-plane timeline,
           collapsible to a thin rail. -->
      <aside
        v-if="showWorkerPanel"
        class="shrink-0 border-l border-border/60 flex flex-col overflow-hidden bg-card/20 transition-[width]"
        :class="workerPanelOpen ? 'w-80' : 'w-9'"
      >
        <div class="flex items-center gap-1.5 px-2 h-8 border-b border-border/60 shrink-0">
          <button
            type="button"
            class="flex items-center justify-center h-6 w-6 rounded text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer"
            :title="workerPanelOpen ? t('conductor.collapse') : t('conductor.expand')"
            @click="workerPanelOpen = !workerPanelOpen"
          >
            <component :is="workerPanelOpen ? ChevronRight : ChevronLeft" class="h-3.5 w-3.5" :stroke-width="2" />
          </button>
          <template v-if="workerPanelOpen">
            <Network class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.75" />
            <span class="text-xs font-medium text-foreground/90 truncate">{{ t('conductor.workers') }}</span>
            <!-- Live count / editable cap. Click the cap to raise or lower the
                 worker limit — before launch it seeds the launch cap, after it
                 changes the running conductor's limit live. Rendered as a compact
                 pill (running / limit) with the pencil revealed on hover. -->
            <div
              v-if="editingWorkerCap"
              class="ml-auto flex items-center gap-1 shrink-0 rounded-md border border-border bg-background pl-1.5 pr-1 h-6"
            >
              <Cpu class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.75" />
              <span class="text-[11px] font-mono text-muted-foreground">{{ conductorWorkers.length }}/</span>
              <Input
                v-model="workerCapDraft"
                type="number"
                min="1"
                max="100"
                autofocus
                class="h-5 w-10 px-1 text-center text-[11px]"
                @keyup.enter="commitWorkerCap"
                @keyup.esc="cancelEditWorkerCap"
                @blur="commitWorkerCap"
              />
            </div>
            <button
              v-else
              type="button"
              class="group/cap ml-auto flex items-center gap-1 shrink-0 rounded-md border border-transparent px-1.5 h-6 text-[11px] font-mono text-muted-foreground hover:text-foreground hover:border-border hover:bg-accent/60 transition-colors cursor-pointer disabled:opacity-50"
              :title="t('conductor.editMaxWorkers')"
              :aria-label="t('conductor.editMaxWorkers')"
              :disabled="savingWorkerCap"
              @click="beginEditWorkerCap"
            >
              <Cpu class="h-3 w-3 shrink-0" :stroke-width="1.75" />
              <span :title="t('conductor.workerCount', { n: conductorWorkers.length, max: effectiveWorkerCap })">
                <span class="text-foreground/90">{{ conductorWorkers.length }}</span>/{{ effectiveWorkerCap }}
              </span>
              <component
                :is="savingWorkerCap ? Loader2 : Pencil"
                class="h-2.5 w-2.5 shrink-0 opacity-0 transition-opacity group-hover/cap:opacity-100"
                :class="{ 'animate-spin !opacity-100': savingWorkerCap }"
                :stroke-width="1.75"
              />
            </button>
          </template>
          <Network v-else class="h-3.5 w-3.5 text-foreground/40 mx-auto" :stroke-width="1.75" />
        </div>
        <!-- Pre-launch: no workers/timeline yet — guide the user to set the limit
             and send the goal, rather than showing an empty worker tree. -->
        <div
          v-if="workerPanelOpen && pendingConductor"
          class="p-3 text-[11px] leading-relaxed text-muted-foreground"
        >
          {{ t('conductor.preLaunchHint') }}
        </div>
        <ConductorWorkerPanel
          v-else-if="workerPanelOpen"
          :workers="conductorWorkers"
          :events="conductorEvents"
          :active-worker-id="currentRunId"
          :conductor-run-id="conductorStore.selectedId"
          @open-session="openSession"
          @back-to-conductor="openSession(conductorStore.selectedId ?? '')"
        />
      </aside>
    </div>

    <!-- Engine-switch dialog: new session vs. continue (carry context) -->
    <Modal
      :open="!!handoffTarget"
      size="sm"
      :closable="!engineSwitchBusy"
      @close="closeHandoffDialog"
    >
      <template #header>
        <Cpu class="h-4 w-4 text-primary shrink-0" :stroke-width="2" />
        <h3 class="text-sm font-semibold flex-1">
          {{ t('run.switchTo') }} <span class="font-mono">{{ handoffTarget }}</span>
        </h3>
      </template>

      <div class="px-4 py-3 text-xs text-foreground/80 space-y-1.5">
        <p>
          {{ t('run.handoffIntro') }}
          <span class="font-mono">{{ loadedRunEngine || t('run.currentEngine') }}</span>.
          {{ t('run.handoffQuestion') }}
        </p>
        <ul class="list-disc pl-4 text-foreground/60 space-y-0.5">
          <li>{{ t('run.handoffContinueBullet', { engine: handoffTarget }) }}</li>
          <li>{{ t('run.handoffNewBullet') }}</li>
        </ul>
      </div>

      <template #footer>
        <Button variant="outline" :disabled="engineSwitchBusy" @click="startNewEngineSession(handoffTarget!)">
          <RotateCcw v-if="startingEngineSession" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ startingEngineSession ? t('run.creating') : t('run.startNewSession') }}
        </Button>
        <Button :disabled="engineSwitchBusy" @click="doHandoff(handoffTarget!)">
          <RotateCcw v-if="handingOff" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ handingOff ? t('run.loadingContext') : t('run.continueCurrentSession') }}
        </Button>
      </template>
    </Modal>

    <!-- Remote control: create a per-run link + OTP so a phone can drive this run -->
    <RemoteSessionModal
      v-if="remoteModalOpen"
      :open="remoteModalOpen"
      :run-id="currentRunId"
      @close="remoteModalOpen = false; refreshRemoteActive()"
      @open-settings="remoteModalOpen = false; router.push({ name: 'settings', query: { section: 'remote' } })"
    />

    <!-- Floating actions for a text selection in the AI-result output: translate
         it, or keep it as a note filed under this run.
         `mousedown.prevent` keeps the selection alive through the click. -->
    <Teleport to="body">
      <div
        v-if="translateTrigger"
        class="fixed z-[65] flex items-center gap-px overflow-hidden rounded-md border border-border bg-card shadow-lg shadow-black/30"
        :style="{ left: translateTrigger.x + 'px', top: translateTrigger.y + 'px' }"
      >
        <button
          class="inline-flex items-center gap-1 px-2 py-1 text-[11px] font-medium text-primary hover:bg-accent/60 transition-colors cursor-pointer"
          :title="t('run.translateSelection')"
          @mousedown.prevent
          @click="openTranslate"
        >
          <Languages class="h-3 w-3" :stroke-width="1.75" />
          {{ t('run.translate') }}
        </button>
        <span class="h-4 w-px bg-border" />
        <button
          class="inline-flex items-center gap-1 px-2 py-1 text-[11px] font-medium text-foreground/80 hover:bg-accent/60 transition-colors cursor-pointer"
          :title="t('run.saveSelectionAsNoteTitle')"
          @mousedown.prevent
          @click="captureSelectionAsNote"
        >
          <StickyNote class="h-3 w-3" :stroke-width="1.75" />
          {{ t('run.saveSelectionAsNote') }}
        </button>
        <span class="h-4 w-px bg-border" />
        <button
          class="inline-flex items-center gap-1 px-2 py-1 text-[11px] font-medium text-foreground/80 hover:bg-accent/60 transition-colors cursor-pointer"
          :title="t('run.saveSelectionAsTodoTitle')"
          @mousedown.prevent
          @click="captureSelectionAsTodo"
        >
          <ListTodo class="h-3 w-3" :stroke-width="1.75" />
          {{ t('run.saveSelectionAsTodo') }}
        </button>
      </div>
    </Teleport>

    <!-- Translation result popover -->
    <TranslatePopover
      v-if="activeTranslation"
      :text="activeTranslation.text"
      :x="activeTranslation.x"
      :y="activeTranslation.y"
      :initial-lang="defaultTranslateLang"
      @close="closeTranslate"
    />
  </div>
</template>

<!-- Markdown output styling is global (src/assets/main.css): the streaming AI
     result is rendered via v-html inside <StreamLog>, which scoped :deep() rules
     here cannot reach. -->
