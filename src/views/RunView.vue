<script setup lang="ts">
import { ref, shallowRef, computed, onMounted, onUnmounted, watch, nextTick, type ComponentPublicInstance } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRoute, useRouter } from 'vue-router'
import { useProjectsStore, type Repo } from '@/stores/projects'
import { useRunsStore, type RunRecord, type ProjectEntry } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useChatDraftsStore } from '@/stores/chatDrafts'
import { useRunPrefsStore } from '@/stores/runPrefs'
import { useGithubAccountsStore } from '@/stores/githubAccounts'
import { useGitlabAccountsStore } from '@/stores/gitlabAccounts'
import { useServersStore, type ProjectServer } from '@/stores/servers'
import { useAwsAccountsStore } from '@/stores/awsAccounts'
import { useClaudeAccountsStore } from '@/stores/claudeAccounts'
import { useAppSettingsStore } from '@/stores/appSettings'
import { useModelCatalogStore } from '@/stores/modelCatalog'
import { useUILayoutStore } from '@/stores/uiLayout'
import { invoke } from '@/lib/tauri'
import { openUrl } from '@tauri-apps/plugin-opener'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { open as openFileDialog } from '@tauri-apps/plugin-dialog'
import {
  Play, Square, GitPullRequest, Bug,
  Clock, Cpu, Terminal, FileText, RotateCcw, RefreshCw,
  Send, MessageSquare, Trash2, Settings, Code2, FolderClosed, FolderOpen, Sparkles, ExternalLink,
  ChevronDown, Maximize2, Minimize2, AppWindow,
  ImagePlus, X, Paperclip,
  ShieldQuestion, MessageCircleQuestion,
  Pin, PinOff, Pencil, Check, Github, Gitlab, UserCircle,
  ClipboardCopy, ScrollText, HardDrive, Cloud, Radio, Languages, FolderTree, Loader2, ListChecks,
  MoreHorizontal, Users
} from 'lucide-vue-next'
import AppSelect from '@/components/AppSelect.vue'
import StreamLog from '@/components/StreamLog.vue'
import TranslatePopover from '@/components/TranslatePopover.vue'
import MentionedFiles from '@/components/MentionedFiles.vue'
import ContextMeter from '@/components/ContextMeter.vue'
import { mergeContextModel } from '@/lib/contextLimits'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import FileViewer from '@/components/FileViewer.vue'
import FileTree from '@/components/FileTree.vue'
import DuoHistoryList from '@/components/DuoHistoryList.vue'
import DuoWorkspace from '@/components/DuoWorkspace.vue'
import { Button, Input, StatusBadge, Badge, Modal, DropdownMenu, DropdownItem } from '@/components/ui'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { matchProjectFile, parseLineRef, decorateFileLinks } from '@/lib/fileLinks'
import { openFileWindow } from '@/lib/fileWindow'
import { openPermissionWindow, closePermissionWindow } from '@/lib/permissionWindow'
import { useMarkdown } from '@/lib/markdown'
import {
  entriesToPlainText,
  parseStreamLog,
  type StreamEntry,
  type ImageAttachment,
} from '@/lib/streamEvents'
import { MODEL_OPTIONS, PERMISSION_MODE_OPTIONS } from '@/lib/engineOptions'
import RemoteSessionModal from '@/components/remote/RemoteSessionModal.vue'
import { useRemoteControlStore } from '@/stores/remoteControl'

const { t } = useI18n()
const route = useRoute()
const router = useRouter()
const projectStore = useProjectsStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const tabsStore = useWorkspaceTabsStore()
const draftsStore = useChatDraftsStore()
const runPrefsStore = useRunPrefsStore()
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
const leftTab = ref<'session' | 'files' | 'duo'>('session')

// Open one of a Duo's two sessions in the Session tab (from DuoWorkspace).
async function openDuoSession(runId: string) {
  leftTab.value = 'session'
  // Make sure the project's run list is loaded so the row highlights, then open.
  if (!runsStore.runs.some((r) => r.id === runId)) {
    await runsStore.fetchRuns(projectId.value).catch(() => {})
  }
  await loadRunLog(runId)
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
// Model choices depend on the engine. Empty value = let the engine/setting decide.
// Option tables live in @/lib/engineOptions (shared with the remote controller);
// for Claude we augment them with models discovered from the account.
const modelOptions = computed(() => {
  const base = MODEL_OPTIONS[effectiveEngine.value] ?? MODEL_OPTIONS.claude
  if (effectiveEngine.value === 'claude') return modelCatalog.mergedClaudeOptions(base)
  if (effectiveEngine.value === 'codex') return modelCatalog.codexOptions(base)
  return base
})
// Reset the model when switching to an engine that doesn't offer the current pick.
watch(effectiveEngine, () => {
  if (!modelOptions.value.some(o => o.value === modelOverride.value)) modelOverride.value = ''
})
const permissionMode = ref('')

// ── Per-project run preferences ───────────────────────────────────────────
// Permission mode, engine and model selectors used to reset to the global
// default on every new run / app reload. Restore the project's remembered
// choices here and mirror later edits back so they stick. Engine is persisted
// explicitly in `onEngineChange` (the selector value also mirrors the loaded
// run's engine, which must not overwrite the saved default).
{
  const p = runPrefsStore.get(projectId.value)
  if (p.permissionMode) permissionMode.value = p.permissionMode
  if (p.engine) engineOverride.value = p.engine
  if (p.model) modelOverride.value = p.model
}
watch(permissionMode, (v) => runPrefsStore.set(projectId.value, { permissionMode: v }))
watch(modelOverride, (v) => runPrefsStore.set(projectId.value, { model: v }))

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
// single-session status (only one run is ever bound at a time). Deriving with
// computeds — instead of a ref refreshed imperatively — means switching sessions
// re-evaluates against THIS run's id immediately, so the button/chip never stay
// lit on a session that isn't the bound one (the reported confusion).
const remoteStatus = computed(() => remoteControlStore.status)
/** A link exists for THIS run (bound), regardless of whether a phone attached. */
const remoteBoundHere = computed(
  () => !!currentRunId.value && remoteStatus.value?.bound_run_id === currentRunId.value,
)
/** A phone has completed auth and is actively driving THIS run. */
const remoteActive = computed(
  () => remoteBoundHere.value && !!remoteStatus.value?.session_authenticated,
)
/** The remote is bound to some OTHER run — clicking here would supersede it. */
const remoteBoundElsewhere = computed(
  () =>
    !!remoteStatus.value?.bound_run_id &&
    remoteStatus.value?.bound_run_id !== currentRunId.value,
)
/**
 * Remote-state marker for the History list, so the user can tell AT A GLANCE
 * which session a phone is driving without opening it. Only the single bound run
 * carries a marker (one run is ever bound at a time):
 *   success → a phone is authenticated and actively driving it;
 *   warning → a link exists but no phone has connected yet.
 */
const remoteRowMarker = computed<
  { runId: string; tone: 'success' | 'warning'; title: string } | null
>(() => {
  const st = remoteStatus.value
  if (!st?.bound_run_id) return null
  return st.session_authenticated
    ? { runId: st.bound_run_id, tone: 'success', title: t('run.remoteRowDriving') }
    : {
        runId: st.bound_run_id,
        tone: 'warning',
        title: t('run.remoteRowWaiting'),
      }
})
/** State-aware tooltip for the Remote button. */
const remoteButtonTitle = computed(() => {
  if (!currentRunId.value) return t('run.remoteBtnSelectRun')
  if (remoteActive.value) return t('run.remoteBtnActive')
  if (remoteBoundHere.value) return t('run.remoteBtnBoundHere')
  if (remoteBoundElsewhere.value) return t('run.remoteBtnBoundElsewhere')
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

// ── History windowing ───────────────────────────────────────────────────────
// Long logs freeze the UI on open because EVERY entry mounts + renders markdown
// synchronously. Opening a run jumps to the bottom, so we render only the last
// `historyWindow` entries and let the user reveal older ones on demand. This
// caps the first-render cost regardless of how long the conversation is.
const HISTORY_WINDOW_INITIAL = 80
const HISTORY_WINDOW_STEP = 120
const historyWindow = ref(HISTORY_WINDOW_INITIAL)
// The tail slice actually handed to StreamLog. `displayedEntries`, plain-text
// export and the "files mentioned" list keep using the FULL array.
const windowedHistoryEntries = computed(() => {
  const all = historyEntries.value
  return all.length > historyWindow.value ? all.slice(all.length - historyWindow.value) : all
})
const hiddenHistoryCount = computed(() =>
  Math.max(0, historyEntries.value.length - windowedHistoryEntries.value.length),
)

// Reveal an older chunk, preserving the viewport: older entries prepend, so we
// bump scrollTop by the height they added (measured across the next frame) so
// the content the user was reading doesn't jump.
async function showEarlierHistory() {
  const el = historyEl.value
  const beforeHeight = el?.scrollHeight ?? 0
  const beforeTop = el?.scrollTop ?? 0
  historyWindow.value += HISTORY_WINDOW_STEP
  await nextTick()
  requestAnimationFrame(() => {
    const e = historyEl.value
    if (!e) return
    e.scrollTop = beforeTop + (e.scrollHeight - beforeHeight)
  })
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
const permissionQueue = computed(() => session.value?.permissionQueue ?? [])
const allowedToolsList = computed(() => session.value?.allowedTools ?? [])
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
const contextRateLimit = computed(() => session.value?.rateLimit ?? null)
// Prefer the live session's status, fall back to the persisted run row.
const currentStatus = computed(() => session.value?.status ?? currentRun.value?.status ?? 'idle')
const currentSessionId = computed(() => session.value?.sessionId ?? currentRun.value?.session_id ?? null)

// Sidebar/label text for a run: sessions show their title; issue/PR show "#N".
function runLabel(run: RunRecord): string {
  if (run.run_type === 'session') return run.title || t('run.labelSession')
  return `${run.run_type === 'analyze_issue' ? t('run.labelIssue') : t('run.labelPr')} #${run.ref_number}`
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
// Entries currently shown in the AI Result column — drives the "files mentioned"
// quick-access list so it matches whatever the user is looking at.
const displayedEntries = computed(() => isViewingHistory.value ? historyEntries.value : liveEntries.value)
const hasLiveOutput = computed(
  () => liveOutputLines.value.length > 0 || liveEntries.value.length > 0 || currentStatus.value === 'running'
)
// Split-pane state: percentage of the left (Content) column.
const splitContainerEl = ref<HTMLElement | null>(null)
const leftWidthPct = ref(50)
const isResizing = ref(false)

// Inner split inside the AI Result column: AI result (left) vs question /
// permission prompt (right). Percentage is the question panel's width.
const resultSplitEl = ref<HTMLElement | null>(null)
// Width of the permission drawer as a % of the AI-result column.
const questionWidthPct = ref(42)
const isResizingQuestion = ref(false)

// When true, the permission prompt is detached into a standalone pop-out window
// (see permissionWindow.ts). The inline panel is hidden so the chat reclaims
// full width and stops reflowing; this window mirrors its state to the pop-out.
const poppedOut = ref(false)

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

function startQuestionResize(e: MouseEvent) {
  e.preventDefault()
  isResizingQuestion.value = true
  document.body.style.cursor = 'col-resize'
  document.body.style.userSelect = 'none'
  window.addEventListener('mousemove', onQuestionResize)
  window.addEventListener('mouseup', stopQuestionResize)
}

function onQuestionResize(e: MouseEvent) {
  if (!resultSplitEl.value) return
  const rect = resultSplitEl.value.getBoundingClientRect()
  const pct = ((rect.right - e.clientX) / rect.width) * 100
  questionWidthPct.value = Math.max(20, Math.min(80, pct))
}

function stopQuestionResize() {
  isResizingQuestion.value = false
  document.body.style.cursor = ''
  document.body.style.userSelect = ''
  window.removeEventListener('mousemove', onQuestionResize)
  window.removeEventListener('mouseup', stopQuestionResize)
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
  // Pointer interaction pauses auto-follow. Do not silently re-enable it while
  // the pointer is still down (for example while drag-selecting text).
  if (outputEl.value && !pointerDownInOutput.value) {
    stickToBottom.value = isNearBottom(outputEl.value)
  }
  // The floating translate button is anchored to a viewport rect that scrolling
  // invalidates — hide it (the popover, once open, dismisses on its own).
  clearTranslateTrigger()
  captureScrollAnchor()
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

// The question panel opening/closing is the main trigger for the reflow. This
// watcher runs before Vue patches the DOM (default 'pre' flush), so it snapshots
// the reading position *before* the width changes; the ResizeObserver then
// restores it after layout settles — covering the case where the user hasn't
// scrolled yet and no anchor was captured otherwise.
watch(() => permissionQueue.value.length > 0, () => captureScrollAnchor())
// Detaching/re-docking the prompt also changes the chat column's width; anchor
// the reading position across that toggle too so it doesn't jump.
watch(poppedOut, () => captureScrollAnchor())

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
  // Hydrate the persisted model caches so the composer's picker shows the last
  // refreshed lists (no discovery here — refresh is manual in Settings).
  modelCatalog.ensureLoaded().catch(() => {})
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
    // sorted pinned-first then created_at desc, so the first one is the latest.
    // With no runs at all, the "chưa có session nào" empty state is shown.
    const latest = runsStore.runs[0]
    if (latest) await loadRunLog(latest.id)
  }
  window.addEventListener('focus', onAppFocus)
  window.addEventListener('pointerup', onWindowPointerUp)
  window.addEventListener('mouseup', onSelectionMouseUp)
  setupPopoutBridge()

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
}

onUnmounted(() => {
  // Run event listeners live in the liveRuns store, so they intentionally
  // survive this component unmounting — that's what lets runs keep streaming
  // while the user is on another screen.
  if (isResizing.value) stopResize()
  if (isResizingQuestion.value) stopQuestionResize()
  outputRO?.disconnect()
  outputRO = null
  window.removeEventListener('focus', onAppFocus)
  document.removeEventListener('mousedown', onRunSettingsPointerDown)
  window.removeEventListener('pointerup', onWindowPointerUp)
  window.removeEventListener('mouseup', onSelectionMouseUp)
  sessionsChangedUnlisten?.()
  sessionsChangedUnlisten = null
  dragDropUnlisten?.()
  dragDropUnlisten = null
  remoteUnlisteners.forEach((fn) => fn())
  remoteUnlisteners.length = 0
  metaUnlisten?.()
  metaUnlisten = null
  popoutUnlisten.forEach((fn) => fn())
  popoutUnlisten = []
  // Tear down the pop-out with its owner view so it can't outlive the run.
  closePermissionWindow()
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
  historyWindow.value = HISTORY_WINDOW_INITIAL
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
    live.get(id)?.outputLines.push({ text: `Error: ${String(e)}`, isStderr: true })
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

// Called when the engine selector changes. Offer to carry context only when a
// real conversation exists and the user picked a *different* engine than the
// one that produced it.
function onEngineChange(next: string) {
  if (next === engineOverride.value) return
  const nextEngine = resolveEngineChoice(next)
  const currentEngine = loadedRunEngine.value || currentRun.value?.engine || effectiveEngine.value
  // Works for both Claude (streamEntries) and Codex (raw log) runs.
  const hasConversation =
    !!currentRunId.value &&
    currentStatus.value !== 'fetched' &&
    sourceText.value.trim().length > 0
  if (hasConversation && nextEngine !== currentEngine) {
    closeRunSettingsMenu()
    handoffTarget.value = nextEngine
    return
  }
  engineOverride.value = next
  // Remember the engine as the project default for future runs.
  runPrefsStore.set(projectId.value, { engine: next })
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
    const seed = t('run.handoffSeed', { engine: sourceEngine, path: context_path })
    await launchFreshRun(run.id, targetEngine, seed, [], override)
  } catch (e) {
    toast.error(t('run.handoffFailed', { error: String(e) }))
  } finally {
    handingOff.value = false
    handoffTarget.value = null
  }
}

async function startNewEngineSession(targetEngine: string) {
  if (engineSwitchBusy.value) return
  startingEngineSession.value = true
  try {
    handoffTarget.value = null
    await createSessionWithEngine(targetEngine)
  } finally {
    startingEngineSession.value = false
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
  if (run.claude_account_id) return run.claude_account_id
  // Existing sessions with no run-level account are legacy/global ~/.claude.
  // Not-yet-started runs still inherit project/default at start time.
  return run.session_id ? '' : effectiveProjectClaudeAccountId.value
})
const claudeAccountOptions = computed(() => {
  const opts = claudeStore.accounts.map((acc) => ({
    value: acc.id,
    label: `${acc.label}${acc.is_default ? ` (${t('settings.claude.default')})` : ''}${acc.email ? ` · ${acc.email}` : ''}`,
  }))
  const currentId = currentRun.value?.claude_account_id
  if (currentId && !opts.some((opt) => opt.value === currentId)) {
    opts.unshift({ value: currentId, label: t('run.missingClaudeAccount') })
  }
  if (currentRun.value?.session_id) {
    opts.unshift({ value: '', label: t('run.globalClaudeAccount') })
  }
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
// RunView remounts on project switch (RouterView key is `run-<projectId>`), so
// the composer would otherwise start empty. Restore any unsent draft for this
// project — text, pasted images and attached files — and mirror later edits
// back to the store so they survive project switches and full app reloads.
{
  const draft = draftsStore.get(projectId.value)
  followUpInput.value = draft.text
  for (const img of draft.images) pushPendingImage(img.media_type, img.data)
  for (const f of draft.files) addPendingFile(f.path)
}
watch(
  [followUpInput, pendingImages, pendingFiles],
  () => {
    draftsStore.set(projectId.value, {
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

// ── File viewer (click a file the AI created/mentioned to preview it) ─────
// The viewer body lives in the reusable <FileViewer> component; RunView only
// tracks which file/line is shown and the modal's open / full-screen state.
const fileViewerOpen = ref(false)
const fileViewerPath = ref('')
// Target line to highlight + scroll to (from a `file:NN` / `#LNN` link), or null.
const fileViewerLine = ref<number | null>(null)
const fileViewerFullscreen = ref(false)

// Set of known project file paths, for resolving inline-code mentions.
const projectFileSet = computed(() => new Set(projectFiles.value.map(e => !e.is_dir && e.path).filter(Boolean) as string[]))

// Resolve a raw inline-code token (e.g. `src/foo.ts` or just `foo.ts`) to a
// project file path, or null when it doesn't name a real file.
function fileMatcher(raw: string): string | null {
  return matchProjectFile(raw, projectFileSet.value)
}

// Open a file in the in-app modal viewer. The <FileViewer> component does the
// actual reading / rendering; we just point it at the file + line.
function openFileViewer(path: string, line?: number | null) {
  if (!project.value?.path) return
  fileViewerPath.value = path
  fileViewerLine.value = line ?? null
  fileViewerFullscreen.value = false
  fileViewerOpen.value = true
}

function closeFileViewer() {
  fileViewerOpen.value = false
}

// Pop the current file out into a standalone OS window (for side-by-side
// viewing on another monitor), then close the in-app modal.
function popOutFileViewer() {
  const p = project.value?.path
  if (p && fileViewerPath.value) openFileWindow(p, fileViewerPath.value, fileViewerLine.value)
  closeFileViewer()
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
  if (path) { e.preventDefault(); openFileViewer(path); return }
  // Markdown link: prevent the webview from navigating (relative hrefs blank
  // the SPA). Open project files in the viewer, external URLs in the browser.
  const anchor = target.closest('a') as HTMLAnchorElement | null
  if (anchor) {
    e.preventDefault()
    const href = anchor.getAttribute('href') ?? ''
    if (!href) return
    if (/^(https?:|mailto:)/i.test(href)) { onOpenUrl(href); return }
    const fp = fileMatcher(href.replace(/#.*$/, ''))
    if (fp) openFileViewer(fp, parseLineRef(href))
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

// Insert a file mention (`@path`) into the composer from the file-tree context
// menu. Inserts at the caret when the composer is focused, otherwise appends.
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
      // First run on this fetched record — use the typed text as the prompt.
      await launchFreshRun(id, engineOverride.value || undefined, prompt, images, override)
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
    live.get(id)?.outputLines.push({ text: t('run.sendFailed', { error: String(e) }), isStderr: true })
  } finally {
    sendingFollowUp.value = false
  }
}

async function handlePermissionDecision(decision: 'allow' | 'deny' | 'ask', remember: boolean) {
  const id = currentRunId.value
  if (!id) return
  const req = permissionQueue.value[0]
  if (!req) return
  live.shiftPermission(id)
  if (remember && decision === 'allow') {
    live.rememberAllowedTool(id, req.tool_name)
  } else if (remember && decision === 'deny') {
    live.rememberDeniedTool(id, req.tool_name)
  }
  try {
    await runsStore.respondPermission(req.run_id, req.request_id, decision)
  } catch (e) {
    live.get(id)?.outputLines.push({ text: t('run.permissionResponseFailed', { error: String(e) }), isStderr: true })
  }
  // Return focus to the composer so the user can keep chatting right away.
  nextTick(() => composerEl.value?.focus())
}

// --- Detachable permission window ------------------------------------------
// The pop-out window is a remote view; this component stays the single source
// of truth. We mirror the current head request to it, and run the real resolve
// logic here when it forwards the user's decision back.
function syncPermissionToPopout() {
  if (!poppedOut.value) return
  const head = permissionQueue.value[0] ?? null
  // Nothing left to answer (resolved or run finished) → close the pop-out and
  // re-dock, so the window doesn't linger after the user responds.
  if (!head) {
    redockPermission()
    return
  }
  emit('permission:sync', { request: head, allowedTools: allowedToolsList.value })
}

async function openPermissionPopout() {
  poppedOut.value = true
  const win = await openPermissionWindow()
  // Re-dock whenever the window goes away, regardless of who closed it (OS close
  // button, "Thu về", or programmatic close). The lifecycle event is reliable,
  // unlike an app-level event emitted mid-teardown.
  win.once('tauri://destroyed', () => {
    poppedOut.value = false
  })
  // Initial state is also pushed on the window's 'ready' ping, but send now too
  // in case it was already open (focus path emits no ready event).
  syncPermissionToPopout()
}

// Re-dock: return the prompt to the inline panel and close the pop-out. Flip the
// flag immediately (don't wait for the destroyed event) so the panel comes back
// even if the window is slow to tear down.
function redockPermission() {
  poppedOut.value = false
  closePermissionWindow()
}

// Keep the pop-out in sync whenever the head request changes (queue advances,
// new request arrives, or the run finishes and clears it).
watch(
  () => [permissionQueue.value[0]?.request_id ?? null, poppedOut.value] as const,
  () => syncPermissionToPopout(),
)

let popoutUnlisten: UnlistenFn[] = []
async function setupPopoutBridge() {
  popoutUnlisten.push(
    await listen('permission:window-ready', () => syncPermissionToPopout()),
    await listen<{ request_id: string; decision: 'allow' | 'deny'; remember: boolean }>(
      'permission:decide',
      (e) => {
        // Ignore stale decisions that don't match the current head request.
        if (e.payload?.request_id !== permissionQueue.value[0]?.request_id) return
        handlePermissionDecision(e.payload.decision, e.payload.remember)
      },
    ),
    await listen<{ request_id: string; answers: Record<string, string> }>(
      'permission:answer',
      (e) => {
        if (e.payload?.request_id !== permissionQueue.value[0]?.request_id) return
        handlePermissionAnswer(e.payload.answers)
      },
    ),
  )
}

async function handlePermissionAnswer(answers: Record<string, string>) {
  const id = currentRunId.value
  if (!id) return
  const req = permissionQueue.value[0]
  if (!req) return
  live.shiftPermission(id)
  try {
    await runsStore.respondPermission(req.run_id, req.request_id, 'allow', undefined, { answers })
  } catch (e) {
    live.get(id)?.outputLines.push({ text: t('run.permissionResponseFailed', { error: String(e) }), isStderr: true })
  }
  // Return focus to the composer so the user can keep chatting right away.
  nextTick(() => composerEl.value?.focus())
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

function startRename(run: RunRecord, e?: MouseEvent) {
  e?.stopPropagation()
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

async function handleTogglePin(run: RunRecord, e?: MouseEvent) {
  e?.stopPropagation()
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

function clearActiveRunState() {
  if (currentRunId.value) live.discard(currentRunId.value)
  currentRunId.value = null
  // A fresh run starts from the project's remembered engine default, not blank.
  engineOverride.value = runPrefsStore.get(projectId.value).engine
  loadedRunEngine.value = ''
  clearHistoryView()
  inputContent.value = ''
  inputContentRunId.value = null
  inputContentError.value = null
}

async function handleDeleteRun(runId: string, e?: MouseEvent) {
  e?.stopPropagation()
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
    historyWindow.value = HISTORY_WINDOW_INITIAL
    viewingLog.value = ''
    historyLoading.value = true
  }
  viewingLogRunId.value = runId
  try {
    const content = await runsStore.getRunLog(runId)
    // Byte-identical to what's already displayed — leave all reactive state
    // untouched so nothing re-renders (the common case on every refocus).
    if (isFocusRefresh && content === viewingLog.value) return
    const parsed = parseStreamLog(content)
    if (parsed && parsed.entries.length > 0) {
      historyToolIndex.clear()
      historyEntries.value = parsed.entries
      for (const [k, v] of parsed.toolIndex) historyToolIndex.set(k, v)
      historyHasStream.value = true
      historyContextTokens.value = parsed.contextTokens
      historyModel.value = parsed.model
      viewingLog.value = content
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
      // No parseable log yet — likely the run hasn't actually produced output
      // (file missing, empty, or DB output_path still points to the input
      // markdown). Treat as "no AI result" instead of rendering raw markdown.
      viewingLog.value = ''
      viewingLogRunId.value = null
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
    <div class="@container flex items-center justify-between gap-3 px-6 h-13 border-b border-border/60 shrink-0">
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
      <div class="flex items-center gap-2 shrink-0">
        <Button
          variant="outline"
          :disabled="!currentRunId"
          :title="remoteButtonTitle"
          class="relative"
          @click="remoteModalOpen = true"
        >
          <Radio
            class="h-4 w-4"
            :class="remoteActive ? 'text-emerald-500' : remoteBoundElsewhere ? 'text-muted-foreground' : ''"
            :stroke-width="2"
          />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.remote') }}</span>
          <!-- Live pulse ONLY when a phone is actively driving THIS run. -->
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
          variant="outline"
          :disabled="!project"
          :title="t('projects.openInVscode')"
          @click="handleOpenInVscode"
        >
          <Code2 class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">VS Code</span>
        </Button>
        <Button
          variant="outline"
          :disabled="!project"
          :title="t('projects.openFolder')"
          @click="handleOpenInFolder"
        >
          <FolderOpen class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.folder') }}</span>
        </Button>
        <Button
          variant="outline"
          :disabled="!project"
          :title="t('projects.openInTerminal')"
          @click="handleOpenInTerminal"
        >
          <Terminal class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.terminal') }}</span>
        </Button>
        <Button
          variant="outline"
          :disabled="!project"
          :title="t('run.issuesByMilestone')"
          @click="router.push({ name: 'gantt', query: { project: projectId } })"
        >
          <ListChecks class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.issues') }}</span>
        </Button>
        <Button
          variant="outline"
          :title="t('run.projectSettingsTitle')"
          @click="router.push(`/projects/${projectId}/settings`)"
        >
          <Settings class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.settings') }}</span>
        </Button>
        <Button
          variant="outline"
          :title="uiLayout.focusMode ? t('run.exitFocusMode') : t('run.enterFocusMode')"
          @click="uiLayout.toggleFocus()"
        >
          <component :is="uiLayout.focusMode ? Minimize2 : Maximize2" class="h-4 w-4" :stroke-width="2" />
          <span v-if="!uiLayout.focusMode" class="hidden @[820px]:inline">{{ t('run.focus') }}</span>
        </Button>
      </div>
    </div>

    <div class="flex flex-1 overflow-hidden">
      <!-- Left panel: controls + history (hidden in focus mode) -->
      <div v-if="!uiLayout.focusMode" class="w-72 shrink-0 border-r border-border/60 flex flex-col overflow-hidden bg-card/20">

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
          <button
            class="flex-1 flex items-center justify-center gap-1.5 py-2 transition-colors cursor-pointer"
            :class="leftTab === 'duo' ? 'text-foreground border-b-2 border-primary' : 'text-muted-foreground hover:text-foreground'"
            @click="leftTab = 'duo'"
          >
            <Users class="h-3.5 w-3.5" :stroke-width="2" />
            {{ t('run.duo') }}
          </button>
        </div>

        <!-- Compact top toolbar: New session, then a collapsible Fetch -->
        <div v-show="leftTab === 'session'" class="p-3 border-b border-border/60 space-y-2">
          <Button class="w-full" :disabled="creatingSession" @click="handleNewSession">
            <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
            {{ creatingSession ? t('run.creating') : t('run.newSession') }}
          </Button>

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
        <div v-show="leftTab === 'session'" ref="sessionListEl" class="flex-1 overflow-auto">
          <div class="flex items-center justify-between px-4 py-2.5 border-b border-border/40">
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
          <div v-if="runsStore.loading" class="px-4 py-3 text-xs text-muted-foreground">{{ t('common.loading') }}</div>
          <div v-else-if="runsStore.runs.length === 0" class="px-4 py-6 text-center text-xs text-muted-foreground">
            {{ t('run.noSessionsYet') }}
          </div>
          <div
            v-else
            v-for="run in runsStore.runs"
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
                  :is="run.run_type === 'session' ? MessageSquare : run.run_type === 'analyze_issue' ? Bug : GitPullRequest"
                  class="h-4 w-4 text-muted-foreground shrink-0"
                  :stroke-width="1.75"
                />
                <Pin
                  v-if="run.pinned"
                  class="h-3 w-3 shrink-0 text-primary"
                  :stroke-width="2"
                  :aria-label="t('run.pinned')"
                />
                <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight">{{ runLabel(run) }}</span>
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
                <StatusBadge :status="run.status" :run-type="run.run_type" size="xs" class="shrink-0" />
              </div>
              <!-- Meta: date + engine. Always visible so hovering never hides
                   the session info; actions sit to the right of this band. -->
              <div class="flex items-center gap-2 mt-2 pl-6 pr-3 text-[10px] text-muted-foreground/70">
                <span class="flex items-center gap-1 shrink-0">
                  <Clock class="h-2.5 w-2.5" :stroke-width="1.5" />
                  {{ new Date(run.created_at).toLocaleDateString() }}
                </span>
                <span class="shrink-0 px-1.5 py-0.5 rounded bg-muted/60 font-mono text-[9px] uppercase tracking-wide text-muted-foreground">
                  {{ run.engine }}
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
                <DropdownItem @click="viewLogFile(run)">
                  <ScrollText class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                  {{ t('run.viewLogFile') }}
                </DropdownItem>
                <div class="my-1 h-px bg-border" aria-hidden="true" />
                <DropdownItem @click="startRename(run, $event)">
                  <Pencil class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                  {{ t('run.rename') }}
                </DropdownItem>
                <DropdownItem :disabled="pinningRunId === run.id" @click="handleTogglePin(run, $event)">
                  <component :is="run.pinned ? PinOff : Pin" class="h-3.5 w-3.5 shrink-0" :stroke-width="1.75" />
                  {{ run.pinned ? t('run.unpinFromTop') : t('run.pinToTop') }}
                </DropdownItem>
                <template v-if="run.status !== 'running'">
                  <div class="my-1 h-px bg-border" aria-hidden="true" />
                  <DropdownItem :disabled="deletingRunId === run.id" @click="handleDeleteRun(run.id, $event)">
                    <Trash2 class="h-3.5 w-3.5 shrink-0 text-destructive" :stroke-width="1.75" />
                    <span class="text-destructive">{{ t('run.deleteThisRun') }}</span>
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

        <!-- Files tab: VSCode-style lazy file tree for this project. -->
        <FileTree
          v-show="leftTab === 'files'"
          v-if="project?.path"
          :project-path="project.path"
          :active="leftTab === 'files'"
          :active-path="fileViewerOpen ? fileViewerPath : null"
          @open-file="openFileViewer"
          @mention-file="mentionFileInComposer"
        />

        <!-- Duo tab: saved Duo-session switcher, scoped to this project. -->
        <div v-show="leftTab === 'duo'" class="flex-1 overflow-hidden">
          <DuoHistoryList :project-id="projectId" />
        </div>
      </div>

      <!-- Duo workspace takes over the main area when its tab is active. Mounted
           only on demand (v-if) so opening a project doesn't auto-restore a duo. -->
      <DuoWorkspace v-if="leftTab === 'duo'" :project-id="projectId" class="flex-1 min-w-0"
        @open-session="openDuoSession" />

      <!-- Terminal panel (split-pane: Content | AI Result) -->
      <div v-show="leftTab !== 'duo'" class="flex-1 flex flex-col overflow-hidden bg-background">
        <!-- No sessions yet: hide the Content / AI Result panels entirely and
             show a single centered empty state. -->
        <div v-if="noSession" class="flex-1 flex items-center justify-center p-6">
          <div class="text-center max-w-xs">
            <MessageSquare class="h-10 w-10 text-foreground/20 mx-auto mb-4" :stroke-width="1" />
            <p class="text-sm font-medium text-foreground/70 mb-1.5">{{ t('run.noSessionsYet') }}</p>
            <p class="text-xs text-foreground/40 leading-relaxed mb-4">
              {{ t('run.createNewSessionHint') }}
            </p>
            <Button :disabled="creatingSession" @click="handleNewSession">
              <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
              {{ creatingSession ? t('run.creating') : t('run.newSession') }}
            </Button>
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
                  <MentionedFiles :entries="displayedEntries" @open-file="openFileViewer" />
                  <!-- When popped out the drawer is hidden, so surface the
                       re-dock control here (the pop-out control lives in the
                       drawer itself, see below). -->
                  <button
                    v-if="poppedOut"
                    type="button"
                    class="flex items-center gap-1 rounded h-6 px-1.5 text-[11px] text-indigo-500 hover:bg-accent/60 transition-colors cursor-pointer"
                    :title="t('run.questionInOwnWindow')"
                    @click="redockPermission"
                  >
                    <AppWindow class="h-3.5 w-3.5" :stroke-width="1.75" />
                    {{ t('run.dockBack') }}
                  </button>
                  <span v-if="currentStatus === 'running'" class="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
                </div>
              </div>
            </div>

            <!-- Log viewer (selected history run) -->
            <div
              v-if="isViewingHistory"
              ref="historyEl"
              class="flex-1 min-h-0 overflow-auto p-4"
              @scroll="clearTranslateTrigger"
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
                <!-- Reveal older entries on demand — long logs render only their
                     tail on open so the first paint stays fast. -->
                <div v-if="hiddenHistoryCount > 0" class="mb-3 flex justify-center">
                  <button
                    type="button"
                    class="rounded-full border border-border bg-muted/60 px-3 py-1 text-[11px] text-foreground/70 hover:bg-accent/60 transition-colors cursor-pointer"
                    @click="showEarlierHistory"
                  >
                    {{ t('run.showEarlier', { count: Math.min(hiddenHistoryCount, HISTORY_WINDOW_STEP), hidden: hiddenHistoryCount }) }}
                  </button>
                </div>
                <StreamLog
                  :entries="windowedHistoryEntries"
                  :running="false"
                  :render-text="renderText"
                  :file-matcher="fileMatcher"
                  @open-file="openFileViewer"
                  @open-url="onOpenUrl"
                />
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
                @open-file="openFileViewer"
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
                 away from the bottom of the live output. -->
            <button
              v-if="hasLiveOutput && !isViewingHistory && !stickToBottom"
              class="absolute bottom-4 right-4 z-10 flex items-center gap-1 rounded-full border border-border bg-card/90 px-3 py-1.5 text-[11px] font-mono text-foreground/80 shadow-md backdrop-blur transition-colors hover:bg-card hover:text-foreground cursor-pointer"
              :title="t('run.scrollToLatest')"
              @click="stickToBottom = true; scrollOutputToBottom()"
            >
              <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
              {{ t('run.latest') }}
            </button>
            </div>

            <!-- Question / permission prompt: an overlay drawer sliding in from
                 the right. It floats ABOVE the AI result (absolute) instead of
                 sharing the flex row, so the chat keeps its full width and never
                 reflows / loses the reading position when a prompt appears. -->
            <div
              v-if="permissionQueue.length > 0 && !poppedOut"
              class="absolute inset-y-0 right-0 z-20 flex bg-card border-l border-border shadow-[-8px_0_24px_-12px_rgba(0,0,0,0.45)]"
              :style="{ width: questionWidthPct + '%' }"
            >
              <!-- Left-edge resize handle to widen / narrow the drawer. -->
              <div
                class="group relative shrink-0 w-px bg-border cursor-col-resize select-none"
                :class="{ 'bg-primary/60': isResizingQuestion }"
                @mousedown="startQuestionResize"
              >
                <div
                  class="absolute inset-y-0 -left-1.5 -right-1.5 z-10 transition-colors group-hover:bg-primary/30"
                  :class="{ 'bg-primary/40': isResizingQuestion }"
                />
              </div>

              <div class="relative flex-1 min-w-0 min-h-0 overflow-auto">
                <!-- Detach into a standalone window (drag to another monitor). -->
                <button
                  type="button"
                  class="absolute top-2 right-2 z-10 flex items-center justify-center rounded p-1 text-foreground/50 hover:bg-accent/60 hover:text-foreground transition-colors cursor-pointer"
                  :title="t('run.detachQuestion')"
                  @click="openPermissionPopout"
                >
                  <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
                </button>
                <PermissionPrompt
                  :key="permissionQueue[0].request_id"
                  :request="permissionQueue[0]"
                  :allowed-tools="allowedToolsList"
                  :render-text="renderText"
                  @decide="handlePermissionDecision"
                  @answer="handlePermissionAnswer"
                />
              </div>
            </div>
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

            <!-- Context-window meter (shows once the run reports token usage) -->
            <ContextMeter
              :tokens="contextTokens"
              :model="contextModel"
              :rate-limit="contextRateLimit"
              @compact="sendSlashCommand('/compact')"
            />

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
                    <span class="text-xs font-medium">{{ engineBadgeLabel }}</span>
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
                            <Cpu class="h-3 w-3 shrink-0" :stroke-width="1.5" />
                            <span>{{ t('run.engineSetting') }}</span>
                          </div>
                          <AppSelect
                            :model-value="engineOverride"
                            @update:model-value="onEngineChange"
                            size="sm"
                            :options="[
                              { value: '', label: t('run.defaultEngine') },
                              { value: 'claude', label: 'claude' },
                              { value: 'codex', label: 'codex' },
                            ]"
                            class="h-8 w-full"
                          >
                            <template #leading>
                              <Cpu class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
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
                        <div class="space-y-1.5">
                          <div class="flex items-center gap-1.5 text-[10px] font-medium uppercase text-muted-foreground">
                            <Sparkles class="h-3 w-3 shrink-0" :stroke-width="1.5" />
                            <span>{{ t('run.modelSetting') }}</span>
                          </div>
                          <AppSelect
                            v-model="modelOverride"
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
    </div>

    <!-- File viewer: preview a file the AI created/edited/mentioned -->
    <Modal
      :open="fileViewerOpen"
      :size="fileViewerFullscreen ? 'full' : 'xl'"
      scroll-body
      hide-header
      @close="closeFileViewer"
    >
      <FileViewer
        v-if="fileViewerOpen"
        :project-path="project?.path || ''"
        :path="fileViewerPath"
        :line="fileViewerLine"
        can-compare
        @open-file="openFileViewer"
        @open-url="onOpenUrl"
      >
        <template #actions>
          <!-- Pop the file out into a standalone OS window for side-by-side viewing -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            :title="t('run.openInNewWindow')"
            @click="popOutFileViewer"
          >
            <AppWindow class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <!-- Full-screen toggle -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            :title="fileViewerFullscreen ? t('run.exitFullScreen') : t('run.fullScreen')"
            @click="fileViewerFullscreen = !fileViewerFullscreen"
          >
            <component :is="fileViewerFullscreen ? Minimize2 : Maximize2" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <!-- Close -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            :title="t('run.closeEsc')"
            @click="closeFileViewer"
          >
            <X class="h-4 w-4" :stroke-width="1.75" />
          </button>
        </template>
      </FileViewer>
    </Modal>

    <!-- Engine-switch dialog: new session vs. continue (carry context) -->
    <Modal
      :open="!!handoffTarget"
      size="sm"
      :closable="!engineSwitchBusy"
      @close="handoffTarget = null"
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
    />

    <!-- Floating "Dịch" trigger for a text selection in the AI-result output.
         `mousedown.prevent` keeps the selection alive through the click. -->
    <Teleport to="body">
      <button
        v-if="translateTrigger"
        class="fixed z-[65] inline-flex items-center gap-1 rounded-md border border-border bg-card px-2 py-1 text-[11px] font-medium text-primary shadow-lg shadow-black/30 hover:bg-accent/60 transition-colors cursor-pointer"
        :style="{ left: translateTrigger.x + 'px', top: translateTrigger.y + 'px' }"
        :title="t('run.translateSelection')"
        @mousedown.prevent
        @click="openTranslate"
      >
        <Languages class="h-3 w-3" :stroke-width="1.75" />
        {{ t('run.translate') }}
      </button>
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
