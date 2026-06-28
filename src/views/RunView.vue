<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick, type ComponentPublicInstance } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { useProjectsStore, type Repo } from '@/stores/projects'
import { useRunsStore, type RunRecord, type ProjectEntry } from '@/stores/runs'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { invoke } from '@/lib/tauri'
import { openUrl } from '@tauri-apps/plugin-opener'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import {
  Play, Square, GitPullRequest, Bug,
  Clock, Cpu, Terminal, FileText, RotateCcw, RefreshCw,
  Send, MessageSquare, Trash2, Settings, Code2, FolderClosed, FolderOpen, Sparkles, ExternalLink,
  ChevronDown, Maximize2, Minimize2, AppWindow,
  ImagePlus, X,
  ShieldQuestion, MessageCircleQuestion,
  Pin, PinOff, Pencil, Check
} from 'lucide-vue-next'
import AppSelect from '@/components/AppSelect.vue'
import StreamLog from '@/components/StreamLog.vue'
import MentionedFiles from '@/components/MentionedFiles.vue'
import ContextMeter from '@/components/ContextMeter.vue'
import PermissionPrompt from '@/components/PermissionPrompt.vue'
import FileViewer from '@/components/FileViewer.vue'
import { Button, Input, StatusBadge, Badge, Modal } from '@/components/ui'
import { applyMermaidFence, vMermaid } from '@/lib/mermaid'
import { vCopyCode } from '@/lib/copyCode'
import { matchProjectFile, parseLineRef, decorateFileLinks } from '@/lib/fileLinks'
import { openFileWindow } from '@/lib/fileWindow'
import type MarkdownIt from 'markdown-it'
import {
  entriesToPlainText,
  parseStreamLog,
  type StreamEntry,
  type ImageAttachment,
} from '@/lib/streamEvents'

const route = useRoute()
const router = useRouter()
const projectStore = useProjectsStore()
const runsStore = useRunsStore()
const live = useLiveRunsStore()
const tabsStore = useWorkspaceTabsStore()

const projectId = computed(() => route.params.projectId as string)
const project = computed(() => projectStore.projects.find(p => p.id === projectId.value))

const activeRunId = computed(() => route.params.runId as string | undefined)

const repos = ref<Repo[]>([])
const selectedRepoId = ref('')

const fetchType = ref<'issue' | 'pr'>('issue')
const refNumber = ref('')
const fetching = ref(false)
// The fetch form is collapsed by default so the run History gets the space.
const fetchOpen = ref(false)
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
  return value || project.value?.default_engine || 'claude'
}
function syncLoadedRunEngine(engine: string) {
  engineOverride.value = engine
  loadedRunEngine.value = engine
}
// Effective engine for the next start/handoff (selector value, else default).
const effectiveEngine = computed(() => resolveEngineChoice(engineOverride.value))
// Model choices depend on the engine. Empty value = let the engine/setting decide.
const MODEL_OPTIONS: Record<string, Array<{ value: string; label: string }>> = {
  claude: [
    { value: '', label: 'Default (from settings)' },
    // `[1m]` selects the 1M-context variant; the bare alias uses the 200K default.
    { value: 'opus', label: 'Opus (200K)' },
    { value: 'opus[1m]', label: 'Opus (1M)' },
    { value: 'sonnet', label: 'Sonnet (200K)' },
    { value: 'sonnet[1m]', label: 'Sonnet (1M)' },
    { value: 'haiku', label: 'Haiku' },
  ],
  codex: [
    { value: '', label: 'Default (from settings)' },
    { value: 'gpt-5.5', label: 'gpt-5.5' },
    { value: 'gpt-5.4', label: 'gpt-5.4' },
    { value: 'gpt-5.3-codex', label: 'gpt-5.3-codex' },
    { value: 'gpt-5.2-codex', label: 'gpt-5.2-codex' },
    { value: 'gpt-5.1-codex-mini', label: 'gpt-5.1-codex-mini' },
  ],
}
const modelOptions = computed(() => MODEL_OPTIONS[effectiveEngine.value] ?? MODEL_OPTIONS.claude)
// Reset the model when switching to an engine that doesn't offer the current pick.
watch(effectiveEngine, () => {
  if (!modelOptions.value.some(o => o.value === modelOverride.value)) modelOverride.value = ''
})
const permissionMode = ref('')
const PERMISSION_MODE_OPTIONS = [
  { value: '', label: 'Default (from settings)' },
  { value: 'default', label: 'Ask via UI (default)' },
  { value: 'acceptEdits', label: 'Auto-accept edits' },
  { value: 'plan', label: 'Plan only (read-only)' },
  { value: 'auto', label: 'Auto (classifier)' },
  { value: 'bypassPermissions', label: 'Bypass all' },
]
const outputEl = ref<HTMLDivElement | null>(null)
const historyEl = ref<HTMLDivElement | null>(null)
const currentRunId = ref<string | null>(null)
const viewingLogRunId = ref<string | null>(null)
const viewingLog = ref<string>('')

// History (on-disk log) view state — only used when viewing a finished run that
// has no in-memory live session. Kept separate from the live session so a
// background run can keep streaming into its own buffer.
const historyEntries = ref<StreamEntry[]>([])
const historyHasStream = ref(false)
const historyToolIndex = new Map<string, number>()
// Context-window state reconstructed from a persisted log (history view).
const historyContextTokens = ref(0)
const historyModel = ref<string | null>(null)

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
const liveEntries = computed(() => session.value?.entries ?? [])
const liveOutputLines = computed(() => session.value?.outputLines ?? [])
const liveHasStream = computed(() => session.value?.hasStreamEvents ?? false)
const permissionQueue = computed(() => session.value?.permissionQueue ?? [])
const allowedToolsList = computed(() => session.value?.allowedTools ?? [])
// Context-window meter state for the focused run. Prefer the live session; for
// a past run with no live session, use the figures reconstructed from its log.
const contextTokens = computed(() => session.value?.contextTokens ?? historyContextTokens.value)
const contextModel = computed(
  () => session.value?.model ?? historyModel.value ?? currentRun.value?.engine ?? null,
)
const contextRateLimit = computed(() => session.value?.rateLimit ?? null)
// Prefer the live session's status, fall back to the persisted run row.
const currentStatus = computed(() => session.value?.status ?? currentRun.value?.status ?? 'idle')
const currentSessionId = computed(() => session.value?.sessionId ?? currentRun.value?.session_id ?? null)

// Sidebar/label text for a run: sessions show their title; issue/PR show "#N".
function runLabel(run: RunRecord): string {
  if (run.run_type === 'session') return run.title || 'Session'
  return `${run.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${run.ref_number}`
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

const mdReady = ref(false)
let _md: MarkdownIt | null = null

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

// Panel visibility: user can show Content only, AI Result only, or both.
// Sessions have no issue/PR content, so Content is never shown for them.
const showContent = ref(true)
const showResult = ref(true)
const contentVisible = computed(() => !currentIsSession.value && showContent.value)
const resultVisible = computed(() => currentIsSession.value || showResult.value)
const showResizeHandle = computed(() => contentVisible.value && resultVisible.value)
const contentWidth = computed(() => (showResizeHandle.value ? leftWidthPct.value + '%' : '100%'))
const resultWidth = computed(() => (showResizeHandle.value ? 100 - leftWidthPct.value + '%' : '100%'))

// Toggle a panel, but never let both end up hidden.
function togglePanel(which: 'content' | 'result') {
  if (which === 'content') {
    if (showContent.value && !showResult.value) return
    showContent.value = !showContent.value
  } else {
    if (showResult.value && !showContent.value) return
    showResult.value = !showResult.value
  }
}

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

function escapeHtml(s: string): string {
  return s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
}

function renderText(md: string): string {
  // Read mdReady so this is reactive to renderer load.
  if (!mdReady.value || !_md) return escapeHtml(md ?? '').replace(/\n/g, '<br/>')
  return _md.render(md ?? '')
}

// Legacy (non-stream) markdown for each view column.
const liveLegacyHtml = computed(() => liveHasStream.value ? '' : renderText(
  liveOutputLines.value.map(l => l.text).join('\n')
))
const historyLegacyHtml = computed(() => historyHasStream.value ? '' : renderText(viewingLog.value))

async function loadMarkdown() {
  if (_md) return
  try {
    const MarkdownIt = (await import('markdown-it')).default
    _md = new MarkdownIt({ html: false, linkify: true, typographer: true, breaks: true })
    applyMermaidFence(_md)
    mdReady.value = true
  } catch {
    // leave _md null; renderText falls back to escaped text
  }
}

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
  if (outputEl.value) stickToBottom.value = isNearBottom(outputEl.value)
}

// While the user is actively interacting with the output — holding the mouse
// down to drag-select, or with text already selected inside it — we must NOT
// yank the view to the bottom when new streamed content arrives. Otherwise a
// click/selection near the bottom gets interrupted the moment a new chunk lands.
const pointerDownInOutput = ref(false)
function onOutputPointerDown() {
  pointerDownInOutput.value = true
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

// Keep the live output pinned to the bottom as new entries/lines arrive for
// the focused, running run — but only while the user is parked at the bottom
// and not in the middle of selecting/clicking inside the output.
watch(
  () => [liveEntries.value.length, liveOutputLines.value.length, currentStatus.value] as const,
  () => {
    if (
      currentStatus.value === 'running' &&
      !isViewingHistory.value &&
      stickToBottom.value &&
      !pointerDownInOutput.value &&
      !hasOutputSelection()
    ) {
      scrollOutputToBottom()
    }
  }
)

// When switching to another run's live output, jump to its latest output.
watch(currentRunId, (id) => {
  stickToBottom.value = true
  scrollOutputToBottom()
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

onMounted(async () => {
  loadMarkdown()
  nextTick(autoResizeComposer)
  if (projectStore.projects.length === 0) {
    await projectStore.fetchProjects()
  }
  repos.value = await projectStore.listRepos(projectId.value)
  if (repos.value.length === 1) {
    selectedRepoId.value = repos.value[0].id
  }
  await runsStore.fetchRuns(projectId.value)
  // Preload the project file list so inline-code mentions resolve to links.
  ensureProjectFiles().catch(() => {})
  if (activeRunId.value) {
    await loadRunLog(activeRunId.value)
  }
  window.addEventListener('focus', refreshViewedRunOnFocus)
  window.addEventListener('pointerup', onWindowPointerUp)

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
        refreshViewedRunOnFocus()
      }
    },
  )
})

// When the window regains focus, the run on screen may have been continued
// outside Devdy (claude CLI / VS Code extension write to the same session
// transcript). Re-read its on-disk log so those external turns show up.
// Skip live sessions — those stream their own updates in real time.
let sessionsChangedUnlisten: UnlistenFn | null = null

async function refreshViewedRunOnFocus() {
  const id = currentRunId.value
  if (!id) return
  const ls = live.get(id)
  if (ls && ls.status === 'running') return
  await loadRunLog(id, { preferDisk: true })
}

onUnmounted(() => {
  // Run event listeners live in the liveRuns store, so they intentionally
  // survive this component unmounting — that's what lets runs keep streaming
  // while the user is on another screen.
  if (isResizing.value) stopResize()
  window.removeEventListener('focus', refreshViewedRunOnFocus)
  window.removeEventListener('pointerup', onWindowPointerUp)
  sessionsChangedUnlisten?.()
  sessionsChangedUnlisten = null
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
}

async function handleFetch(linkedIssueOverride?: number) {
  const n = parseInt(refNumber.value.trim())
  if (isNaN(n) || n <= 0) { fetchError.value = 'Enter a valid number'; return }
  if (!selectedRepoId.value) { fetchError.value = 'Select a repository first'; return }

  fetchError.value = null
  fetching.value = true
  try {
    let run: RunRecord
    if (fetchType.value === 'issue') {
      run = await runsStore.fetchIssue(projectId.value, selectedRepoId.value, n)
    } else {
      run = await runsStore.fetchPr(projectId.value, selectedRepoId.value, n, linkedIssueOverride)
    }
    await runsStore.fetchRuns(projectId.value)
    currentRunId.value = run.id
    clearHistoryView()
    needsLinkedIssue.value = false
    linkedIssueInput.value = ''
    await loadInputContent(run.id)
  } catch (e) {
    const msg = String(e)
    if (fetchType.value === 'pr' && msg.includes('NO_LINKED_ISSUE')) {
      needsLinkedIssue.value = true
      fetchError.value = "PR body doesn't reference an issue (e.g. \"Fixes #123\"). Enter the linked issue number below."
    } else {
      fetchError.value = msg
    }
  } finally {
    fetching.value = false
  }
}

async function handleSubmitLinkedIssue() {
  const n = parseInt(linkedIssueInput.value.trim())
  if (isNaN(n) || n <= 0) { fetchError.value = 'Enter a valid issue number'; return }
  await handleFetch(n)
}

function setLocalRunStatus(runId: string, status: RunRecord['status'], engine?: string) {
  const run = runsStore.runs.find(r => r.id === runId)
  if (run) {
    run.status = status
    if (engine) run.engine = engine
  }
}

// A start/resume/follow-up was refused for exceeding the global budget.
const BUDGET_CONFIRM_MSG = 'Đã vượt ngân sách (plan/token) cho chu kỳ hiện tại.\nVẫn tiếp tục?'
function isBudgetError(e: unknown): boolean {
  return String(e).includes('BUDGET_EXCEEDED')
}
// When a budget refusal happens, confirm an override. Returns true to proceed.
function budgetOverrideConfirmed(e: unknown): boolean {
  return isBudgetError(e) && confirm(BUDGET_CONFIRM_MSG)
}

// Run a budget-gated backend call. On BUDGET_EXCEEDED, ask the user; if they
// confirm, retry with the override flag. If they decline, run `onDecline` and
// swallow the error. Non-budget errors propagate to the caller.
async function withBudgetOverride(
  fn: (override: boolean) => Promise<void>,
  onDecline?: () => void,
): Promise<void> {
  try {
    await fn(false)
  } catch (e) {
    if (!isBudgetError(e)) throw e
    if (confirm(BUDGET_CONFIRM_MSG)) {
      await fn(true)
      return
    }
    onDecline?.()
  }
}

async function handleStartRun(overrideBudget = false) {
  if (!currentRunId.value) return
  const id = currentRunId.value
  const engine = effectiveEngine.value

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
      overrideBudget,
    )
  } catch (e) {
    if (!overrideBudget && budgetOverrideConfirmed(e)) {
      await handleStartRun(true)
      return
    }
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
    nextTick(() => composerEl.value?.focus())
  } catch (e) {
    alert(`Không tạo được session: ${String(e)}`)
  } finally {
    creatingSession.value = false
  }
}

async function handleNewSession() {
  await createSessionWithEngine(engineOverride.value || undefined)
}

// Start a brand-new turn on `runId` with `text` as the prompt. Shared by the
// "fetched" first-run path and by engine handoffs.
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
  try {
    await runsStore.startRun(runId, selectedEngine, permissionMode.value || undefined, text, modelOverride.value || undefined, images, overrideBudget)
  } catch (e) {
    if (!overrideBudget && budgetOverrideConfirmed(e)) {
      await launchFreshRun(runId, engine, text, images, true)
      return
    }
    throw e
  }
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
    handoffTarget.value = nextEngine
    return
  }
  engineOverride.value = next
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
    const { run, context_path } = await runsStore.createHandoffRun(sourceId, targetEngine, transcript)
    await runsStore.fetchRuns(projectId.value) // surface the new run in the sidebar
    syncLoadedRunEngine(targetEngine)
    const seed =
      `Tiếp tục công việc đang dở từ phiên trước (engine: ${sourceEngine}).\n\n` +
      `Toàn bộ ngữ cảnh/hội thoại trước đó được lưu ở file:\n${context_path}\n\n` +
      `Hãy đọc file đó trước để nắm tình hình, sau đó tiếp tục từ đúng chỗ đang dở. ` +
      `Không lặp lại những việc đã hoàn thành.`
    await launchFreshRun(run.id, targetEngine, seed)
  } catch (e) {
    alert(`Không thể chuyển engine: ${String(e)}`)
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

// ── Pasted / attached images in the composer ──────────────────────────────
interface PendingImage extends ImageAttachment {
  id: string
  url: string // data URL for the thumbnail preview
}
const pendingImages = ref<PendingImage[]>([])
const imageInputEl = ref<HTMLInputElement | null>(null)
let imageSeq = 0

const MAX_IMAGE_BYTES = 10 * 1024 * 1024 // ~10MB per image, matches engine limits

// Read a File/Blob into a base64 attachment and add it to the pending strip.
function addImageFile(file: File | null) {
  if (!file || !file.type.startsWith('image/')) return
  if (file.size > MAX_IMAGE_BYTES) {
    alert(`Ảnh quá lớn (tối đa ${MAX_IMAGE_BYTES / 1024 / 1024}MB).`)
    return
  }
  const reader = new FileReader()
  reader.onload = () => {
    const url = String(reader.result || '')
    const comma = url.indexOf(',')
    if (comma < 0) return
    pendingImages.value.push({
      id: `img-${imageSeq++}`,
      media_type: file.type,
      data: url.slice(comma + 1),
      url,
    })
  }
  reader.readAsDataURL(file)
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

const engineLabel = computed(() => loadedRunEngine.value || currentRun.value?.engine || effectiveEngine.value || 'the agent')

const chatPlaceholder = computed(() => {
  if (currentStatus.value === 'running') {
    return `Send a follow-up to ${engineLabel.value} — Enter to send, Shift+Enter for newline`
  }
  if (currentStatus.value === 'fetched') {
    return currentIsSession.value
      ? 'Mô tả việc bạn muốn làm — Enter để bắt đầu (mention file bằng @)'
      : 'Type a custom prompt and Enter to run (leave empty + click Run to use default)'
  }
  if (currentSessionId.value) {
    return `Continue session with ${engineLabel.value} — Enter sends and resumes the run`
  }
  return 'Chat unavailable for this run'
})

const chatSendLabel = computed(() => {
  if (sendingFollowUp.value) return 'Sending…'
  if (currentStatus.value === 'running') return 'Send'
  if (currentStatus.value === 'fetched') return 'Run'
  if (!chatEligible.value) return 'Run'
  return 'Resume'
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
  return !followUpInput.value.trim() && !pendingImages.value.length
})

// One entry point for the footer's primary button: cancel while running is a
// separate button, so here we either send a typed message/resume, or kick off
// the default-prompt run when nothing was typed.
function handlePrimaryAction() {
  if (currentStatus.value === 'running') { handleSendFollowUp(); return }
  const hasInput = followUpInput.value.trim().length > 0 || pendingImages.value.length > 0
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
  const maxPx = composerExpanded.value
    ? Math.round(window.innerHeight * 0.6)
    : 180
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

async function ensureProjectFiles() {
  const path = project.value?.path
  if (!path) return
  if (projectFilesPath.value === path && projectFiles.value.length) return
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
  mentionStart.value = i
  mentionQuery.value = text.slice(i + 1, caret)
  mentionIndex.value = 0
  mentionOpen.value = true
  ensureProjectFiles()
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
  if ((!text && !hasImages) || !currentRunId.value || sendingFollowUp.value) return
  const id = currentRunId.value
  const wasRunning = currentStatus.value === 'running'
  sendingFollowUp.value = true
  try {
    if (currentStatus.value === 'fetched') {
      // First run on this fetched record — use the typed text as the prompt.
      const images = takePendingImages()
      await launchFreshRun(id, engineOverride.value || undefined, text, images)
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

    const images = takePendingImages()
    // Show the user message right away in the timeline (running / resume paths).
    live.pushUser(id, projectId.value, text, images)
    stickToBottom.value = true // re-pin: the user just sent a message
    scrollOutputToBottom()

    const prevStatus = currentStatus.value
    if (!wasRunning) {
      live.setStatus(id, 'running')
      setLocalRunStatus(id, 'running')
      await live.startListening(id, projectId.value)
      await withBudgetOverride(
        (o) => runsStore.resumeRun(id, permissionMode.value || undefined, modelOverride.value || undefined, o),
        () => {
          // User declined the budget override — revert the optimistic running state.
          live.setStatus(id, prevStatus)
          setLocalRunStatus(id, prevStatus as RunRecord['status'])
        },
      )
    }

    await withBudgetOverride((o) => runsStore.sendUserMessage(id, text, images, o))
    followUpInput.value = ''
  } catch (e) {
    live.get(id)?.outputLines.push({ text: `Send failed: ${String(e)}`, isStderr: true })
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
  }
  try {
    await runsStore.respondPermission(req.run_id, req.request_id, decision)
  } catch (e) {
    live.get(id)?.outputLines.push({ text: `Permission response failed: ${String(e)}`, isStderr: true })
  }
  // Return focus to the composer so the user can keep chatting right away.
  nextTick(() => composerEl.value?.focus())
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
    live.get(id)?.outputLines.push({ text: `Permission response failed: ${String(e)}`, isStderr: true })
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
    alert(String(e))
  }
}

const deletingRunId = ref<string | null>(null)
const refetchingRunId = ref<string | null>(null)
const clearingAll = ref(false)
const pinningRunId = ref<string | null>(null)

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
  } catch (err) {
    alert(String(err))
  } finally {
    cancelRename()
  }
}

async function handleTogglePin(run: RunRecord, e?: MouseEvent) {
  e?.stopPropagation()
  pinningRunId.value = run.id
  try {
    await runsStore.setRunPinned(run.id, !run.pinned)
  } catch (err) {
    alert(String(err))
  } finally {
    pinningRunId.value = null
  }
}

function clearActiveRunState() {
  if (currentRunId.value) live.discard(currentRunId.value)
  currentRunId.value = null
  engineOverride.value = ''
  loadedRunEngine.value = ''
  clearHistoryView()
  inputContent.value = ''
  inputContentRunId.value = null
  inputContentError.value = null
}

async function handleDeleteRun(runId: string, e?: MouseEvent) {
  e?.stopPropagation()
  const run = runsStore.runs.find(r => r.id === runId)
  const label = run ? runLabel(run) : 'this run'
  if (!confirm(`Delete fetched ${label}? Cached markdown and logs will be removed.`)) return
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
  } catch (err) {
    alert(String(err))
  } finally {
    deletingRunId.value = null
  }
}

async function handleClearAllRuns() {
  if (runsStore.runs.length === 0) return
  if (!confirm(`Delete all fetched runs for this project? Running runs will be skipped.`)) return
  clearingAll.value = true
  try {
    await runsStore.deleteAllRuns(projectId.value)
    clearActiveRunState()
    if (activeRunId.value) {
      router.replace(`/projects/${projectId.value}`)
    }
  } catch (err) {
    alert(String(err))
  } finally {
    clearingAll.value = false
  }
}

async function loadRunLog(runId: string, opts: { preferDisk?: boolean } = {}) {
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
  // `preferDisk` (a focus refresh) overrides this for finished sessions so we
  // re-read the log, which may now include turns added outside Devdy.
  const liveSession = live.get(runId)
  if (liveSession && !(opts.preferDisk && liveSession.status !== 'running')) {
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

  // Otherwise read the persisted log from disk (history view).
  historyEntries.value = []
  historyHasStream.value = false
  historyToolIndex.clear()
  historyContextTokens.value = 0
  historyModel.value = null
  viewingLogRunId.value = runId
  viewingLog.value = 'Loading…'
  try {
    const content = await runsStore.getRunLog(runId)
    const parsed = parseStreamLog(content)
    if (parsed && parsed.entries.length > 0) {
      historyEntries.value = parsed.entries
      for (const [k, v] of parsed.toolIndex) historyToolIndex.set(k, v)
      historyHasStream.value = true
      historyContextTokens.value = parsed.contextTokens
      historyModel.value = parsed.model
      viewingLog.value = content
      // Jump to the end so the latest AI result is visible right away.
      nextTick(() => {
        if (historyEl.value) historyEl.value.scrollTop = historyEl.value.scrollHeight
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
  }
}

function parseIssueOrPrNumber(input: string): number | null {
  const urlMatch = input.match(/\/(?:issues|pull)\/(\d+)/)
  if (urlMatch) return parseInt(urlMatch[1])
  const n = parseInt(input.trim())
  return isNaN(n) ? null : n
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
    alert(String(e))
  }
}

async function handleOpenInFolder() {
  if (!project.value) return
  try {
    await projectStore.openInFolder(project.value.path)
  } catch (e) {
    alert(String(e))
  }
}

async function handleOpenInTerminal() {
  if (!project.value) return
  try {
    const settings = await invoke<{ terminal_app: string }>('get_settings')
    await projectStore.openInTerminal(project.value.path, settings.terminal_app)
  } catch (e) {
    alert(String(e))
  }
}

function handleRefInput(val: string) {
  const parsed = parseIssueOrPrNumber(val)
  refNumber.value = parsed !== null ? String(parsed) : val
  needsLinkedIssue.value = false
  linkedIssueInput.value = ''
  fetchError.value = null
}
</script>

<template>
  <div class="flex flex-col h-full">
    <!-- Header -->
    <div class="flex items-center justify-between gap-3 px-6 h-13 border-b border-border/60 shrink-0">
      <div class="flex items-center gap-2 min-w-0">
        <h1 class="text-sm font-semibold truncate">{{ project?.name ?? 'Project' }}</h1>
        <Badge v-if="project" tone="neutral" class="font-mono shrink-0">
          {{ project.default_engine }}
        </Badge>
        <span
          v-if="project"
          class="text-[11px] text-muted-foreground font-mono truncate hidden md:inline"
          :title="project.path"
        >{{ project.path }}</span>
      </div>
      <div class="flex items-center gap-2 shrink-0">
        <Button
          variant="outline"
          :disabled="!project"
          :title="currentRunId ? 'Open project in VS Code with the loaded issue/PR file' : 'Open project folder in VS Code'"
          @click="handleOpenInVscode"
        >
          <Code2 class="h-3.5 w-3.5" :stroke-width="1.75" />
          VS Code
        </Button>
        <Button
          variant="outline"
          :disabled="!project"
          title="Open project folder"
          @click="handleOpenInFolder"
        >
          <FolderOpen class="h-3.5 w-3.5" :stroke-width="1.75" />
          Folder
        </Button>
        <Button
          variant="outline"
          :disabled="!project"
          title="Open project folder in terminal"
          @click="handleOpenInTerminal"
        >
          <Terminal class="h-3.5 w-3.5" :stroke-width="1.75" />
          Terminal
        </Button>
        <Button
          variant="outline"
          title="Project settings (repos, PAT, skills)"
          @click="router.push(`/projects/${projectId}/settings`)"
        >
          <Settings class="h-3.5 w-3.5" :stroke-width="1.75" />
          Settings
        </Button>
      </div>
    </div>

    <div class="flex flex-1 overflow-hidden">
      <!-- Left panel: controls + history -->
      <div class="w-72 shrink-0 border-r border-border/60 flex flex-col overflow-hidden bg-card/20">

        <!-- Compact top toolbar: New session, then a collapsible Fetch -->
        <div class="p-3 border-b border-border/60 space-y-2">
          <Button class="w-full" :disabled="creatingSession" @click="handleNewSession">
            <MessageSquare class="h-3.5 w-3.5" :stroke-width="2" />
            {{ creatingSession ? 'Đang tạo…' : 'New session' }}
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
              Fetch Issue / PR
            </span>
            <ChevronDown class="h-3.5 w-3.5 transition-transform" :class="{ 'rotate-180': fetchOpen }" :stroke-width="1.75" />
          </Button>

          <!-- Fetch form (collapsible) -->
          <div v-if="fetchOpen" class="space-y-3 pt-1">

          <!-- Repo selector (only shown when project has multiple repos) -->
          <div v-if="repos.length > 1">
            <AppSelect
              v-model="selectedRepoId"
              :options="repos.map(r => ({ value: r.id, label: r.name }))"
              placeholder="Select repo"
              size="sm"
            />
          </div>
          <div v-else-if="repos.length === 0" class="text-[10px] text-amber-500/80 leading-relaxed">
            No repositories configured. Add one in project settings.
          </div>

          <!-- Type toggle -->
          <div class="flex gap-1 p-0.5 bg-muted rounded-md">
            <button
              class="flex-1 flex items-center justify-center gap-1.5 py-1 text-xs rounded transition-colors cursor-pointer"
              :class="fetchType === 'issue'
                ? 'bg-background text-foreground shadow-sm'
                : 'text-muted-foreground hover:text-foreground'"
              @click="fetchType = 'issue'"
            >
              <Bug class="h-3 w-3" :stroke-width="1.75" />
              Issue
            </button>
            <button
              class="flex-1 flex items-center justify-center gap-1.5 py-1 text-xs rounded transition-colors cursor-pointer"
              :class="fetchType === 'pr'
                ? 'bg-background text-foreground shadow-sm'
                : 'text-muted-foreground hover:text-foreground'"
              @click="fetchType = 'pr'"
            >
              <GitPullRequest class="h-3 w-3" :stroke-width="1.75" />
              PR
            </button>
          </div>

          <!-- Number input -->
          <div>
            <Input
              :model-value="refNumber"
              @update:model-value="handleRefInput"
              :placeholder="fetchType === 'issue' ? '#123 or GitHub URL' : 'PR #123 or URL'"
              @keyup.enter="handleFetch()"
            />
            <p v-if="fetchError" class="mt-1 text-[10px] text-destructive">{{ fetchError }}</p>
          </div>

          <Button
            v-if="!needsLinkedIssue"
            variant="outline"
            class="w-full"
            :disabled="fetching || !refNumber"
            @click="handleFetch()"
          >
            {{ fetching ? 'Fetching…' : `Fetch ${fetchType === 'issue' ? 'Issue' : 'PR'}` }}
          </Button>

          <!-- Linked issue prompt (shown when PR body has no Fixes/Closes/Resolves #N) -->
          <div v-if="needsLinkedIssue" class="space-y-2">
            <Input
              v-model="linkedIssueInput"
              placeholder="Linked issue # (e.g. 42)"
              @keyup.enter="handleSubmitLinkedIssue"
            />
            <Button
              variant="outline"
              class="w-full"
              :disabled="fetching || !linkedIssueInput"
              @click="handleSubmitLinkedIssue"
            >
              {{ fetching ? 'Fetching…' : 'Fetch with linked issue' }}
            </Button>
          </div>
          </div>
        </div>

        <!-- Run history -->
        <div class="flex-1 overflow-auto">
          <div class="flex items-center justify-between px-4 py-2.5 border-b border-border/40">
            <p class="text-[10px] font-semibold text-muted-foreground uppercase tracking-wider">History</p>
            <button
              v-if="runsStore.runs.length > 0"
              class="flex items-center gap-1 text-[10px] text-muted-foreground/70 hover:text-destructive transition-colors cursor-pointer disabled:opacity-50 disabled:cursor-not-allowed"
              :disabled="clearingAll"
              :title="clearingAll ? 'Clearing…' : 'Delete all fetched runs (running runs are skipped)'"
              @click="handleClearAllRuns"
            >
              <Trash2 class="h-3 w-3" :stroke-width="1.75" />
              {{ clearingAll ? 'Clearing…' : 'Clear all' }}
            </button>
          </div>
          <div v-if="runsStore.loading" class="px-4 py-3 text-xs text-muted-foreground">Loading…</div>
          <div v-else-if="runsStore.runs.length === 0" class="px-4 py-6 text-center text-xs text-muted-foreground">
            No runs yet
          </div>
          <div
            v-else
            v-for="run in runsStore.runs"
            :key="run.id"
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
                  aria-label="Pinned"
                />
                <span class="flex-1 min-w-0 truncate text-[13px] font-medium leading-tight">{{ runLabel(run) }}</span>
                <!-- Animated attention marker: this run is waiting for a
                     permission / question answer (replaces the floating toast). -->
                <span
                  v-if="pendingRequest(run.id)"
                  class="relative flex h-4 w-4 shrink-0 items-center justify-center text-primary"
                  :title="
                    pendingRequest(run.id)?.tool_name === 'AskUserQuestion'
                      ? 'Waiting for your answer'
                      : 'Waiting for your permission'
                  "
                >
                  <span class="absolute inset-0 animate-ping rounded-full bg-primary/30" />
                  <component
                    :is="pendingRequest(run.id)?.tool_name === 'AskUserQuestion' ? MessageCircleQuestion : ShieldQuestion"
                    class="relative h-4 w-4"
                    :stroke-width="2"
                  />
                </span>
                <StatusBadge :status="run.status" size="xs" class="shrink-0" />
              </div>
              <!-- Meta: date + engine. Fades out on hover so the actions can
                   take over the same band instead of overlapping it. -->
              <div class="flex items-center gap-2 mt-2 pl-6 pr-3 text-[10px] text-muted-foreground/70 transition-opacity group-hover:opacity-0 group-focus-within:opacity-0">
                <span class="flex items-center gap-1 shrink-0">
                  <Clock class="h-2.5 w-2.5" :stroke-width="1.5" />
                  {{ new Date(run.created_at).toLocaleDateString() }}
                </span>
                <span class="shrink-0 px-1.5 py-0.5 rounded bg-muted/60 font-mono text-[9px] uppercase tracking-wide text-muted-foreground">
                  {{ run.engine }}
                </span>
              </div>
            </button>
            <!-- Actions: bigger hit area, spaced, revealed on hover/focus -->
            <div
              v-if="run.status !== 'running'"
              class="absolute right-2 bottom-2 flex items-center gap-1 opacity-0 group-hover:opacity-100 group-focus-within:opacity-100 transition-opacity"
            >
              <Button
                v-if="runGithubUrl(run)"
                variant="ghost"
                size="icon-sm"
                :aria-label="'Open this ' + (run.run_type === 'analyze_issue' ? 'issue' : 'PR') + ' on GitHub'"
                :title="'Open this ' + (run.run_type === 'analyze_issue' ? 'issue' : 'PR') + ' on GitHub'"
                @click.stop="openRunInBrowser(run)"
              >
                <ExternalLink class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                v-if="run.run_type !== 'session'"
                variant="ghost"
                size="icon-sm"
                :disabled="refetchingRunId === run.id"
                :aria-label="'Re-fetch this ' + (run.run_type === 'analyze_issue' ? 'issue' : 'PR') + ' content from GitHub (keeps AI result)'"
                :title="'Re-fetch this ' + (run.run_type === 'analyze_issue' ? 'issue' : 'PR') + ' content from GitHub (keeps AI result)'"
                @click.stop="handleRefetch(run.id)"
              >
                <RefreshCw class="h-3.5 w-3.5" :class="{ 'animate-spin': refetchingRunId === run.id }" :stroke-width="1.75" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label="Rename"
                title="Rename"
                @click.stop="startRename(run, $event)"
              >
                <Pencil class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                variant="ghost"
                size="icon-sm"
                :disabled="pinningRunId === run.id"
                :aria-label="run.pinned ? 'Unpin from top' : 'Pin to top'"
                :title="run.pinned ? 'Unpin from top' : 'Pin to top'"
                @click.stop="handleTogglePin(run, $event)"
              >
                <component :is="run.pinned ? PinOff : Pin" class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
              <Button
                variant="destructive-ghost"
                size="icon-sm"
                :disabled="deletingRunId === run.id"
                aria-label="Delete this run"
                title="Delete this run"
                @click.stop="handleDeleteRun(run.id, $event)"
              >
                <Trash2 class="h-3.5 w-3.5" :stroke-width="1.75" />
              </Button>
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
                placeholder="Run name"
                @keyup.enter="commitRename(run.id)"
                @keyup.esc="cancelRename()"
                @blur="commitRename(run.id)"
              />
              <Button variant="ghost" size="icon-sm" aria-label="Save name" title="Save" @mousedown.prevent @click.stop="commitRename(run.id)">
                <Check class="h-3.5 w-3.5" :stroke-width="2" />
              </Button>
              <Button variant="ghost" size="icon-sm" aria-label="Cancel rename" title="Cancel" @mousedown.prevent @click.stop="cancelRename()">
                <X class="h-3.5 w-3.5" :stroke-width="2" />
              </Button>
            </div>
          </div>
        </div>
      </div>

      <!-- Terminal panel (split-pane: Content | AI Result) -->
      <div class="flex-1 flex flex-col overflow-hidden bg-background">
        <!-- View toggles: show Content, AI Result, or both. Hidden for
             standalone sessions (they have no issue/PR Content). -->
        <div
          v-if="!currentIsSession"
          class="flex items-center gap-1 px-3 py-1.5 border-b border-border bg-card/40 shrink-0"
        >
          <span class="text-[11px] font-mono text-foreground/40 mr-1">View</span>
          <button
            class="flex items-center gap-1.5 rounded px-2 py-1 text-[11px] font-mono transition-colors cursor-pointer"
            :class="contentVisible ? 'bg-primary/15 text-primary' : 'text-foreground/40 hover:text-foreground/70 hover:bg-card'"
            title="Toggle Content panel"
            @click="togglePanel('content')"
          >
            <FileText class="h-3 w-3" :stroke-width="1.75" />
            Content
          </button>
          <button
            class="flex items-center gap-1.5 rounded px-2 py-1 text-[11px] font-mono transition-colors cursor-pointer"
            :class="resultVisible ? 'bg-primary/15 text-primary' : 'text-foreground/40 hover:text-foreground/70 hover:bg-card'"
            title="Toggle AI Result panel"
            @click="togglePanel('result')"
          >
            <MessageSquare class="h-3 w-3" :stroke-width="1.75" />
            AI Result
          </button>
        </div>
        <!-- Split container -->
        <div
          ref="splitContainerEl"
          class="flex-1 flex overflow-hidden min-h-0"
        >
          <!-- Content column (hidden for sessions or when toggled off) -->
          <div
            v-if="contentVisible"
            class="flex flex-col overflow-hidden bg-background min-w-0 min-h-0"
            :style="{ width: contentWidth }"
          >
            <div class="flex items-center gap-2 px-3 py-2 bg-card border-b border-border shrink-0">
              <FileText class="h-3.5 w-3.5 text-foreground/40" :stroke-width="1.5" />
              <span class="text-[11px] font-mono text-foreground/60">Content</span>
            </div>
            <div class="flex-1 min-h-0 overflow-auto p-4">
              <template v-if="!currentRunId">
                <div class="flex items-center justify-center h-full">
                  <div class="text-center">
                    <FileText class="h-8 w-8 text-foreground/20 mx-auto mb-3" :stroke-width="1" />
                    <p class="text-xs text-foreground/30 font-mono">fetch an issue or PR to begin</p>
                  </div>
                </div>
              </template>
              <template v-else-if="inputContentLoading">
                <p class="text-xs text-muted-foreground">Loading content…</p>
              </template>
              <template v-else-if="inputContentError">
                <div class="text-xs text-destructive">
                  <p class="font-medium mb-1">Could not load content</p>
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
                <p class="text-xs text-foreground/40">No content available for this run.</p>
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

          <!-- AI Result column (full width for sessions or when Content is hidden) -->
          <div
            v-if="resultVisible"
            class="relative flex flex-col overflow-hidden bg-background min-w-0 min-h-0"
            :style="{ width: resultWidth }"
          >
            <div class="bg-card border-b border-border shrink-0">
              <!-- Title row: shows the current run / session title -->
              <div class="flex items-center gap-2 px-3 py-2">
                <MessageSquare class="h-3.5 w-3.5 text-foreground/40 shrink-0" :stroke-width="1.5" />
                <span class="text-xs font-medium text-foreground/90 truncate">
                  {{ currentRun ? runLabel(currentRun) : 'No run selected' }}
                </span>
                <div class="ml-auto flex items-center gap-2 shrink-0">
                  <MentionedFiles :entries="displayedEntries" @open-file="openFileViewer" />
                  <span v-if="currentStatus === 'running'" class="h-1.5 w-1.5 rounded-full bg-emerald-500 animate-pulse" />
                </div>
              </div>
            </div>

            <!-- Log viewer (selected history run) -->
            <div
              v-if="isViewingHistory"
              ref="historyEl"
              class="flex-1 min-h-0 overflow-auto p-4"
            >
              <StreamLog
                v-if="historyHasStream"
                :entries="historyEntries"
                :running="false"
                :render-text="renderText"
                :file-matcher="fileMatcher"
                @open-file="openFileViewer"
                @open-url="onOpenUrl"
              />
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
                  <p class="text-xs font-medium text-amber-500/80 font-mono mb-1">chưa run</p>
                  <p class="text-[11px] text-foreground/40 leading-relaxed">
                    click Run để dùng prompt mặc định, hoặc gõ prompt ở khung chat phía dưới
                  </p>
                </template>
                <p v-else class="text-xs text-foreground/30 font-mono">
                  fetch an issue or PR, then run
                </p>
              </div>
            </div>

            <!-- Jump-to-latest button: shown when the user has scrolled up
                 away from the bottom of the live output. -->
            <button
              v-if="hasLiveOutput && !isViewingHistory && !stickToBottom"
              class="absolute bottom-4 right-4 z-10 flex items-center gap-1 rounded-full border border-border bg-card/90 px-3 py-1.5 text-[11px] font-mono text-foreground/80 shadow-md backdrop-blur transition-colors hover:bg-card hover:text-foreground cursor-pointer"
              title="Scroll to latest"
              @click="stickToBottom = true; scrollOutputToBottom()"
            >
              <ChevronDown class="h-3.5 w-3.5" :stroke-width="2" />
              Latest
            </button>
          </div>
        </div>

        <!-- Permission / question prompt: docked inline above the composer so it
             stays within this run's view and never blocks the rest of the app. -->
        <PermissionPrompt
          v-if="permissionQueue.length > 0"
          :key="permissionQueue[0].request_id"
          :request="permissionQueue[0]"
          :allowed-tools="allowedToolsList"
          @decide="handlePermissionDecision"
          @answer="handlePermissionAnswer"
        />

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
                  title="Remove image"
                  @click="removePendingImage(img.id)"
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
            <div class="rounded-lg border border-border bg-background focus-within:ring-1 focus-within:ring-ring transition-colors">
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

              <!-- Footer toolbar: engine / permission / model controls -->
              <div class="flex items-center gap-1.5 px-1.5 pb-1.5">
                <button
                  class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                  :disabled="sendingFollowUp"
                  title="Attach image (or paste from clipboard)"
                  @click="imageInputEl?.click()"
                >
                  <ImagePlus class="h-4 w-4" :stroke-width="2" />
                </button>
                <button
                  class="inline-flex items-center justify-center h-8 w-8 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer disabled:opacity-50 shrink-0"
                  :title="composerExpanded ? 'Thu gọn ô soạn thảo' : 'Mở rộng ô soạn thảo (dễ đọc/sửa văn bản dài)'"
                  @click="toggleComposerExpand"
                >
                  <component :is="composerExpanded ? Minimize2 : Maximize2" class="h-4 w-4" :stroke-width="2" />
                </button>
                <AppSelect
                  :model-value="engineOverride"
                  @update:model-value="onEngineChange"
                  size="sm"
                  variant="ghost"
                  :options="[
                    { value: '', label: 'Default engine' },
                    { value: 'claude', label: 'claude' },
                    { value: 'codex', label: 'codex' },
                  ]"
                  class="w-32 h-8 shrink-0"
                >
                  <template #leading>
                    <Cpu class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
                  </template>
                </AppSelect>
                <AppSelect
                  v-model="permissionMode"
                  size="sm"
                  variant="ghost"
                  :options="PERMISSION_MODE_OPTIONS"
                  :disabled="currentStatus === 'running'"
                  class="min-w-0 max-w-44 h-8 shrink"
                  title="Permission mode for this run"
                >
                  <template #leading>
                    <span class="text-[10px] font-mono text-muted-foreground shrink-0">perm</span>
                  </template>
                </AppSelect>
                <AppSelect
                  v-model="modelOverride"
                  size="sm"
                  variant="ghost"
                  :options="modelOptions"
                  :disabled="currentStatus === 'running'"
                  class="w-40 h-8 shrink-0"
                  title="Model for this run (empty = engine/settings default)"
                >
                  <template #leading>
                    <Sparkles class="h-3 w-3 text-muted-foreground shrink-0" :stroke-width="1.5" />
                  </template>
                </AppSelect>

                <div class="ml-auto flex items-center gap-1.5 shrink-0">
                  <Button
                    v-if="currentStatus === 'running'"
                    variant="destructive"
                    class="h-8 shrink-0"
                    title="Cancel the running turn"
                    @click="handleCancel"
                  >
                    <Square class="h-3.5 w-3.5" :stroke-width="2" fill="currentColor" />
                    Cancel
                  </Button>
                  <button
                    class="inline-flex items-center justify-center gap-1.5 h-8 px-3.5 text-xs bg-primary text-primary-foreground rounded-md hover:opacity-90 transition-opacity cursor-pointer disabled:opacity-50 shrink-0"
                    :disabled="primaryDisabled"
                    @click="handlePrimaryAction"
                    :title="currentStatus === 'running'
                      ? 'Send message (Enter)'
                      : currentStatus === 'fetched'
                        ? 'Start run (Enter — empty uses the default prompt)'
                        : chatEligible
                          ? 'Resume session and send (Enter)'
                          : 'Re-run with the default prompt'"
                  >
                    <component :is="chatSendLabel === 'Run' ? Play : Send" class="h-3.5 w-3.5" :stroke-width="2" />
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
        @open-file="openFileViewer"
        @open-url="onOpenUrl"
      >
        <template #actions>
          <!-- Pop the file out into a standalone OS window for side-by-side viewing -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            title="Mở ra cửa sổ mới"
            @click="popOutFileViewer"
          >
            <AppWindow class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <!-- Full-screen toggle -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-foreground/60 hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            :title="fileViewerFullscreen ? 'Exit full screen' : 'Full screen'"
            @click="fileViewerFullscreen = !fileViewerFullscreen"
          >
            <component :is="fileViewerFullscreen ? Minimize2 : Maximize2" class="h-3.5 w-3.5" :stroke-width="1.75" />
          </button>
          <!-- Close -->
          <button
            class="flex items-center justify-center h-6 w-6 rounded-md text-muted-foreground hover:text-foreground hover:bg-accent transition-colors cursor-pointer shrink-0"
            title="Close (Esc)"
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
          Chuyển sang <span class="font-mono">{{ handoffTarget }}</span>
        </h3>
      </template>

      <div class="px-4 py-3 text-xs text-foreground/80 space-y-1.5">
        <p>
          Bạn đang có một phiên làm việc với
          <span class="font-mono">{{ loadedRunEngine || 'engine hiện tại' }}</span>.
          Bạn muốn làm gì?
        </p>
        <ul class="list-disc pl-4 text-foreground/60 space-y-0.5">
          <li><b>Tiếp tục phiên hiện tại</b>: tạo phiên mới trên <span class="font-mono">{{ handoffTarget }}</span> và nạp lại toàn bộ ngữ cảnh đang có.</li>
          <li><b>Bắt đầu phiên mới</b>: tạo session mới trên engine đã chọn, không mang ngữ cảnh cũ.</li>
        </ul>
      </div>

      <template #footer>
        <Button variant="outline" :disabled="engineSwitchBusy" @click="startNewEngineSession(handoffTarget!)">
          <RotateCcw v-if="startingEngineSession" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ startingEngineSession ? 'Đang tạo…' : 'Bắt đầu phiên mới' }}
        </Button>
        <Button :disabled="engineSwitchBusy" @click="doHandoff(handoffTarget!)">
          <RotateCcw v-if="handingOff" class="h-3.5 w-3.5 animate-spin" :stroke-width="2" />
          {{ handingOff ? 'Đang nạp ngữ cảnh…' : 'Tiếp tục phiên hiện tại' }}
        </Button>
      </template>
    </Modal>
  </div>
</template>

<!-- Markdown output styling is global (src/assets/main.css): the streaming AI
     result is rendered via v-html inside <StreamLog>, which scoped :deep() rules
     here cannot reach. -->
