<script setup lang="ts">
import { computed, nextTick, onMounted, onBeforeUnmount, reactive, ref, watch, watchEffect } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { useProjectsStore } from '@/stores/projects'
import { useFocusStore } from '@/stores/focus'
import { useGithubMentionsStore } from '@/stores/githubMentions'
import { usePrInboxStore } from '@/stores/prInbox'
import { FOCUS_OPEN_WINDOW, type FocusOpenTodo } from '@/lib/focus'
import { invoke } from '@/lib/tauri'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useAppSettingsStore } from '@/stores/appSettings'
import { tabEntryKey, useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useUILayoutStore } from '@/stores/uiLayout'
import { useI18n } from 'vue-i18n'
import { setLocale } from '@/i18n'
import { ListTodo, LockKeyhole, Minimize2, PanelLeftClose, PanelLeftOpen, Search, Slack, StickyNote } from 'lucide-vue-next'
import { activeListPaneHidden, toggleActiveListPane } from '@/composables/useListPane'
import PermissionNotifier from '@/components/PermissionNotifier.vue'
import PermissionWindowManager from '@/components/PermissionWindowManager.vue'
import CalendarReminder from '@/components/CalendarReminder.vue'
import WorkspaceTabs from '@/components/WorkspaceTabs.vue'
import PendingApprovalsPill from '@/components/PendingApprovalsPill.vue'
import CommandPalette, { type PaletteMode } from '@/components/CommandPalette.vue'
import RemoteIndicator from '@/components/RemoteIndicator.vue'
import FileViewerWindow from '@/views/FileViewerWindow.vue'
import PermissionWindow from '@/views/PermissionWindow.vue'
import ThemeDecorations from '@/components/ThemeDecorations.vue'
import { Button, ConfirmModal, PromptModal, ToastHost } from '@/components/ui'
import ImageCompareHost from '@/components/ImageCompareHost.vue'
import CyberFoxHost from '@/components/CyberFoxHost.vue'
import MascotWindow from '@/views/MascotWindow.vue'
import ItemWindow from '@/views/ItemWindow.vue'
import ItemListWindow from '@/views/ItemListWindow.vue'
import SecretNotesWindow from '@/views/SecretNotesWindow.vue'
import SlackThreadsWindow from '@/views/SlackThreadsWindow.vue'
import PrInboxWindow from '@/views/PrInboxWindow.vue'
import GithubMentionsWindow from '@/views/GithubMentionsWindow.vue'
import { PR_INBOX_OPEN_WINDOW, PR_INBOX_WINDOW_OPEN_ROUTE, type PrInboxWindowOpenRoute } from '@/lib/prInboxWindow'
import CalendarWindow from '@/views/CalendarWindow.vue'
import { CALENDAR_WINDOW_OPEN_ROUTE, type CalendarWindowOpenRoute } from '@/lib/calendarWindow'
import { DELIVERY_RADAR_OPEN_ROUTE, type DeliveryRadarOpenRoute } from '@/lib/deliveryRadarWindow'
import {
  SLACK_API_RECEIVED,
  type SlackApiReceived,
  SLACK_NOTIFICATION_CLICKED,
  type SlackNotificationClicked,
  SLACK_WINDOW_OPEN_ROUTE,
  type SlackWindowOpenRoute,
} from '@/lib/slackWindow'
import {
  WINDOW_NEW_SESSION,
  WINDOW_OPEN_RUN,
  createdFromSource,
  linkTargetRun,
  type WindowNewSession,
  type WindowOpenRun,
} from '@/lib/sessionLinks'
import { useChatDraftsStore } from '@/stores/chatDrafts'
import { useToast } from '@/composables/useToast'
import { useAgentAmbient } from '@/composables/useAgentAmbient'
import { applySystemAccent } from '@/lib/systemAccent'
import { openItemCreateWindow } from '@/lib/itemWindow'
import { openSecretNotesWindow } from '@/lib/secretNotesWindow'
import { applyAppMenu, IS_MAC, isMenuActionBound, listenMenuActions, registerMenuAction } from '@/lib/appMenu'
import {
  closeCurrentTab,
  goToTabNumber,
  goToTabOffset,
  interceptRightHalf,
  navigateFocused,
  navigateRightPane,
  popOutCurrent,
  sanitized,
  settleStartupNavigation,
  syncPairWithRoute,
  toggleSplitCurrent,
} from '@/lib/tabActions'
import { openPath, openUrl } from '@tauri-apps/plugin-opener'
import { appDataDir, join } from '@tauri-apps/api/path'
import { NAV_ROUTES, pageRouteFor, startsNavGroup } from '@/lib/navigation'
import { dockAllSurfaceWindows, entryLocation, listenSurfaceRequests, openSurface, rightPaneLocation } from '@/lib/surfaceOpen'
import { monitorCount } from '@/lib/surfacePlacement'
import { cleanParams, shouldDockOnMonitorChange, SURFACE_PATHS, surfaceForPath, surfaceTabId, type SurfaceKind } from '@/lib/surfaces'
import { clampRatio, DEFAULT_SPLIT_RATIO, SPLIT_MIN_LEFT, SPLIT_MIN_RIGHT } from '@/lib/workspaceState'
import SurfaceHost from '@/components/surfaces/SurfaceHost.vue'
import PaneRouterView from '@/components/PaneRouterView.vue'
import { IS_SESSION_WINDOW, SESSION_WINDOW_PROJECT_ID, SESSION_WINDOW_RUN_ID } from '@/lib/windowMode'
import DeliveryRadarView from '@/views/DeliveryRadarView.vue'
import { getVersion } from '@tauri-apps/api/app'
import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow'
import { useRunsStore } from '@/stores/runs'
import { useConductorStore } from '@/stores/conductor'
import { refreshTray, buildSessionItems, TRAY_OPEN_RUN, type TrayOpenRun } from '@/lib/tray'
import { focusSessionWindow } from '@/lib/sessionWindow'
import { useBackgroundProcessesStore } from '@/stores/backgroundProcesses'
import { EVT_TRAY_OPEN } from '@/lib/backgroundProcesses'
import BackgroundProcessesDock from '@/components/processes/BackgroundProcessesDock.vue'
import OrphanProcessesPrompt from '@/components/processes/OrphanProcessesPrompt.vue'
import QuitProcessesDialog from '@/components/processes/QuitProcessesDialog.vue'

// Pop-out windows load the same SPA with a query flag; render a bare,
// chrome-less host (no sidebar / nav / background work) in those cases.
const isFileWindow = new URLSearchParams(window.location.search).get('fileWindow') === '1'
const isPermissionWindow = new URLSearchParams(window.location.search).get('permissionWindow') === '1'
const isItemWindow = new URLSearchParams(window.location.search).get('itemWindow') === '1'
const isItemListWindow = new URLSearchParams(window.location.search).get('itemListWindow') === '1'
const isSecretNotesWindow = new URLSearchParams(window.location.search).get('secretNotesWindow') === '1'
const isRadarWindow = new URLSearchParams(window.location.search).get('radarWindow') === '1'
const isSlackWindow = new URLSearchParams(window.location.search).get('slackWindow') === '1'
const isPrInboxWindow = new URLSearchParams(window.location.search).get('prInboxWindow') === '1'
const isGithubMentionsWindow = new URLSearchParams(window.location.search).get('githubMentionsWindow') === '1'
const isCalendarWindow = new URLSearchParams(window.location.search).get('calendarWindow') === '1'
const isMascotWindow = new URLSearchParams(window.location.search).get('mascotWindow') === '1'
// A session pop-out is different from the bare pop-outs above: it reuses the full
// RunView (routed to one run) with the app chrome hidden, so it still needs the
// stores + router — only the sidebar / nav / docks / app-wide notifiers are cut.
// Captured at module load (see windowMode) so it survives the router rewriting
// the URL out from under us on the first navigation.
const isSessionWindow = IS_SESSION_WINDOW
// All pop-out kinds only need the theme applied; skip the main app's data work.
const isPopoutWindow =
  isFileWindow ||
  isPermissionWindow ||
  isItemWindow ||
  isItemListWindow ||
  isSecretNotesWindow ||
  isRadarWindow ||
  isSlackWindow ||
  isPrInboxWindow ||
  isGithubMentionsWindow ||
  isCalendarWindow ||
  isMascotWindow

const route = useRoute()
const router = useRouter()
const projectsStore = useProjectsStore()
const mentionsStore = useGithubMentionsStore()
const prInboxStore = usePrInboxStore()
// Focus board for the sidebar badge (number of overdue todos).
const focusStore = useFocusStore()
const appSettings = useAppSettingsStore()
const tabsStore = useWorkspaceTabsStore()
const uiLayout = useUILayoutStore()
const live = useLiveRunsStore()
const runsStore = useRunsStore()
const conductorStore = useConductorStore()
const bgProcs = useBackgroundProcessesStore()
// Agent-reactive aurora (`<html data-agent-state>`) — main window only.
if (!isPopoutWindow && !isSessionWindow) useAgentAmbient()
const draftsStore = useChatDraftsStore()
const { toast } = useToast()
const { t, locale } = useI18n()

// Session pop-out: drive the router to the run as early as possible (in setup,
// before first paint) so the window lands on RunView instead of flashing the
// default `/projects` redirect on the way there.
if (isSessionWindow && SESSION_WINDOW_PROJECT_ID && SESSION_WINDOW_RUN_ID) {
  router
    .replace({
      name: 'project-run-detail',
      params: { projectId: SESSION_WINDOW_PROJECT_ID, runId: SESSION_WINDOW_RUN_ID },
    })
    .catch(() => {})
}

// Session pop-out: keep the OS window title on the run's name so the menu-bar
// switcher (lib/tray.ts) can label this window, and refresh the tray whenever the
// title lands or is renamed.
if (isSessionWindow && SESSION_WINDOW_RUN_ID) {
  const runId = SESSION_WINDOW_RUN_ID
  watch(
    () =>
      runsStore.runMeta.get(runId)?.title ??
      runsStore.runs.find((r) => r.id === runId)?.title ??
      null,
    async (title) => {
      const name = (title && title.trim()) || t('tray.untitledSession')
      try {
        await getCurrentWebviewWindow().setTitle(name)
      } catch {
        /* outside the Tauri shell */
      }
      void refreshTray()
    },
    { immediate: true },
  )
}

// Main window owns the menu-bar badge (the running-session count) and the tray's
// running-session section. Refresh the tray whenever either changes — sessions
// starting / finishing / blocking on a permission, or being renamed — even for
// runs that were never popped out into their own window. Debounced: a burst of
// stream events shouldn't rebuild the native menu on every one.
if (!isPopoutWindow && !isSessionWindow) {
  let trayTimer: ReturnType<typeof setTimeout> | null = null
  watch(
    () => `${live.runningIds.length}|${JSON.stringify(buildSessionItems())}`,
    () => {
      if (trayTimer) clearTimeout(trayTimer)
      trayTimer = setTimeout(() => {
        trayTimer = null
        void refreshTray()
      }, 150)
    },
  )

  // A session whose `run:done` was missed would stay "running" forever and keep
  // the badge lit; poll the backend's live runs while any session claims to run.
  let reconcileTimer: ReturnType<typeof setInterval> | null = null
  watch(
    () => live.runningIds.length > 0,
    (anyRunning) => {
      if (anyRunning && !reconcileTimer) {
        reconcileTimer = setInterval(() => void live.reconcileRunning(), 10_000)
      } else if (!anyRunning && reconcileTimer) {
        clearInterval(reconcileTimer)
        reconcileTimer = null
      }
    },
    { immediate: true },
  )
}

// A session row in the menu-bar switcher (or a linked session in the Slack /
// Notes / Todos windows) was clicked: show the run where it lives — its own
// pop-out if it has one, otherwise this window. `projectHint` covers runs this
// window never loaded.
async function openRunFromTray(runId: string, projectHint?: string) {
  if (await focusSessionWindow(runId)) return
  const projectId =
    live.sessions.get(runId)?.projectId ?? runsStore.runMeta.get(runId)?.project_id ?? projectHint
  if (!projectId) return
  try {
    const win = getCurrentWebviewWindow()
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* best-effort focus */
  }
  live.markSeen(runId)
  // Open the project's workspace tab before routing — the run workspace is
  // driven by the tabs store (see PermissionNotifier.navigateToRun).
  tabsStore.open(projectId, runId)
  router
    .push({ name: 'project-run-detail', params: { projectId, runId } })
    .catch(() => {})
}

// A router-less pop-out (Slack threads / PR Inbox / Delivery Radar window) asked to show an app
// screen: bring this window forward and navigate. Only in-app paths.
async function openRouteFromWindow(path: string | undefined) {
  if (!path || !path.startsWith('/')) return
  try {
    const win = getCurrentWebviewWindow()
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* best-effort focus */
  }
  router.push(path).catch(() => {})
}

// "New session from this thread / note / todo" in a pop-out: the session is
// created here, where the drafts store lives, its composer is prefilled with
// the target's reference (not sent), the target is linked to it, then it opens.
// One request per target at a time: a double click must not create two sessions.
const newSessionPending = new Set<string>()
async function newSessionFromWindow({ projectId, text, target }: WindowNewSession) {
  const key = `${target.kind}:${target.id}`
  if (newSessionPending.has(key)) return
  newSessionPending.add(key)
  try {
    let runId: string
    try {
      runId = (await runsStore.createSessionRun(projectId)).id
    } catch (e) {
      toast.error(t('run.createSessionFailed', { error: String(e) }))
      return
    }
    draftsStore.set(runId, { text, images: [], files: [] })
    try {
      await linkTargetRun(target, runId, createdFromSource(target.kind))
    } catch (e) {
      // Best-effort: the session and its prefilled composer still open.
      console.warn('[links] linking the new session failed', e)
    }
    await runsStore.refreshMeta(projectId).catch(() => {})
    await openRunFromTray(runId, projectId)
  } finally {
    newSessionPending.delete(key)
  }
}

const isRunRoute = computed(
  () => route.name === 'project-run' || route.name === 'project-run-detail',
)

// Color themes that ship an animated decorative background scene. When one is
// active AND the user hasn't turned animations off, the app root is made
// transparent so the body-level scene (ThemeDecorations) shows through.
const SCENIC_THEMES = new Set(['midautumn'])
const sceneTheme = computed(() => appSettings.settings?.color_theme ?? 'default')
const sceneOn = computed(
  () =>
    SCENIC_THEMES.has(sceneTheme.value) &&
    appSettings.settings?.animated_background !== 'false',
)
// The macOS accent may change in System Settings while we're in the background.
window.addEventListener('focus', () => applySystemAccent(document.documentElement.dataset.theme ?? ''))
// Exposed on <html> so skin / ambient CSS can drop its motion too.
watchEffect(() => {
  document.documentElement.dataset.animatedBg = appSettings.settings?.animated_background === 'false' ? 'false' : 'true'
})

// Force a clean RunView remount only when the *project* changes (heavy stream
// state survives in the liveRuns store); switching runs within a project is
// handled in-place by RunView's own activeRunId watcher. Non-run routes keep a
// stable per-view key so their existing reuse behaviour is unchanged.
const routeKey = computed(() =>
  isRunRoute.value ? `run-${route.params.projectId}` : String(route.name ?? route.path),
)

// Views kept alive across navigation, so returning to one is instant: no
// teardown, no setup() re-run, no DOM rebuild, and scroll position + form state
// survive. Each still refreshes its data on re-entry via `useActivatedRefresh`.
//
// Matched by COMPONENT NAME, which is why every view listed here declares
// `defineOptions({ name })` rather than relying on the name Vue infers from the
// filename.
//
// RunView is deliberately absent: its heavy stream state already lives in the
// liveRuns store (so it survives navigation anyway) and `routeKey` exists
// precisely to force it to remount when the project changes. Do not add it.
//
// Only views with no timers, no event listeners and no cleanup hooks are listed.
// Anything with a lifecycle side effect needs its `onMounted`/`onUnmounted` work
// moved to `onActivated`/`onDeactivated` first — under KeepAlive those hooks no
// longer fire on navigation, so a timer would otherwise run forever in the
// background.
//
// Surface tabs (PR Inbox, Calendar, Todos…) are not cached here: App keeps one
// SurfaceHost per open tab itself (see `hostedSurfaceTabs`).
const CACHED_VIEWS = [
  'SkillsView',
  'RulesView',
  'McpServersView',
  'ServersView',
]

// Pin every project the user opens as a workspace tab; remember the run being
// viewed so re-selecting the tab returns to it.
watch(
  () => [route.name, route.params.projectId, route.params.runId] as const,
  ([name, projectId, runId]) => {
    // A session pop-out is locked to one run; it must not pin workspace tabs.
    if (isSessionWindow) return
    if ((name === 'project-run' || name === 'project-run-detail') && typeof projectId === 'string') {
      // A run opened under the wrong project (e.g. a conductor reached through a
      // worker it spawned in another project): move to the run's own project
      // instead of pinning it as this project's last run.
      const owner = typeof runId === 'string' ? runsStore.runMeta.get(runId)?.project_id : undefined
      if (owner && owner !== projectId) {
        router.replace({ name: 'project-run-detail', params: { projectId: owner, runId } }).catch(() => {})
        return
      }
      tabsStore.open(projectId, typeof runId === 'string' ? runId : null)
    }
  },
  { immediate: true },
)

// Every screen the router shows is a tab of the strip: a surface (re-pointed at
// what it shows) or a page (Dashboard, Skills, Settings…, remembering its query).
// Projects get theirs from the run-route watcher above. Any navigation lands in
// the left / only pane.
watch(
  () => route.fullPath,
  () => {
    if (isSessionWindow) return
    // A navigation lands in the left / only pane (while split, a click in the
    // left pane already focused it — and a tab click may have focused the right).
    if (!tabsStore.isSplit) tabsStore.focusedPane = 'left'
    const kind = surfaceForPath(route.path)
    if (kind) tabsStore.openSurface(kind, cleanParams(kind, route.query))
    else if (pageRouteFor(route.path)) tabsStore.openPage(route.path, route.fullPath)
    // The split follows: its left half shows what the router shows.
    syncPairWithRoute()
  },
  { immediate: true },
)

// Split view: a navigation to the tab in the right half shows it there instead
// (never the same tab on both sides). Main window only.
const offRightHalfGuard =
  isPopoutWindow || isSessionWindow ? null : router.beforeEach((to) => interceptRightHalf(to))
// The startup navigation leaves the restored split as it was (lib/tabActions).
if (!isPopoutWindow && !isSessionWindow) void settleStartupNavigation()

/** The surface tab the router shows (left / only pane), if any. */
const routedSurfaceId = computed(() => {
  const kind = surfaceForPath(route.path)
  return kind ? surfaceTabId(kind, cleanParams(kind, route.query)) : null
})

// Surface tabs live like windows: each open tab has ONE SurfaceHost from its
// first visit until it is closed or popped out — kept alive (its own KeepAlive,
// so `onActivated` refreshes still run) while another tab shows, and teleported
// into the right pane while it is the split tab. Switching tabs or moving one
// into / out of the split loses nothing; closing one tears it down. Mounted
// lazily: restored tabs cost nothing until opened.
const visitedSurfaces = reactive(new Set<string>())
watch(
  routedSurfaceId,
  (routed) => {
    if (!routed) return
    visitedSurfaces.add(routed)
  },
  { immediate: true },
)
watch(
  () => tabsStore.splitId,
  (split) => {
    if (split && tabsStore.surfaceTab(split)) visitedSurfaces.add(split)
  },
  { immediate: true },
)
const hostedSurfaceTabs = computed(() =>
  tabsStore.entries.filter(
    (e): e is Extract<typeof e, { type: 'surface' }> => e.type === 'surface' && visitedSurfaces.has(e.id),
  ),
)
watch(
  () => tabsStore.entries.length,
  () => {
    // Forget closed tabs, so reopening one starts fresh.
    for (const id of [...visitedSurfaces]) if (!tabsStore.surfaceTab(id)) visitedSurfaces.delete(id)
  },
)

// ── Split view ──────────────────────────────────────────────────────────────
// The right half shows any tab: a surface through its SurfaceHost (teleported
// into #devdy-split-body above), anything else — a session, Dashboard,
// Settings… — through a pane-scoped router (PaneRouterView), so it reads its
// own route and its links navigate the right half.

/** The tab in the right half, while split. */
const rightEntry = computed(() => tabsStore.entry(tabsStore.splitId))
/** …when it is a routed screen rather than a surface. */
const rightRouted = computed(() => (rightEntry.value && rightEntry.value.type !== 'surface' ? rightEntry.value : null))
const rightLocation = computed(() => {
  const e = rightRouted.value
  if (!e) return ''
  const moved = rightPaneLocation.value
  if (moved && moved.key === tabEntryKey(e)) return moved.fullPath
  // Never open a session tab on a run of another project (as routeToLeft / selectEntry).
  return router.resolve(entryLocation(sanitized(e))).fullPath
})
// Where the right half moved to inside its tab only holds for that tab.
watch(
  () => tabsStore.pair?.right,
  (key) => {
    if (rightPaneLocation.value?.key !== key) rightPaneLocation.value = null
  },
)
/** Path the right half shows, for the sidebar highlight. */
const rightPath = computed(() => {
  const e = rightEntry.value
  if (!e) return null
  if (e.type === 'surface') return SURFACE_PATHS[e.surface]
  return router.resolve(rightLocation.value).path
})

// Divider (drag, arrows, double-click = 50/50). The ratio is the left pane's
// share, persisted with the tabs; both panes keep a minimum width.
const splitRef = ref<HTMLElement | null>(null)
const dragRatio = ref<number | null>(null)
const splitRatio = computed(() => dragRatio.value ?? tabsStore.ratio)
function splitWidth(): number {
  return splitRef.value?.getBoundingClientRect().width ?? 0
}
function startDividerDrag(e: PointerEvent) {
  if (e.button !== 0) return
  const el = e.currentTarget as HTMLElement
  const rect = splitRef.value?.getBoundingClientRect()
  if (!rect || rect.width <= 0) return
  e.preventDefault()
  el.setPointerCapture(e.pointerId)
  const move = (ev: PointerEvent) => {
    dragRatio.value = clampRatio((ev.clientX - rect.left) / rect.width, rect.width)
  }
  const up = () => {
    el.removeEventListener('pointermove', move)
    el.removeEventListener('pointerup', up)
    el.removeEventListener('pointercancel', up)
    if (dragRatio.value != null) tabsStore.setRatio(dragRatio.value, rect.width)
    dragRatio.value = null
  }
  el.addEventListener('pointermove', move)
  el.addEventListener('pointerup', up)
  el.addEventListener('pointercancel', up)
}
function onDividerKey(e: KeyboardEvent) {
  const total = splitWidth()
  const step = e.shiftKey ? 0.1 : 0.02
  let next: number | null = null
  if (e.key === 'ArrowLeft') next = splitRatio.value - step
  else if (e.key === 'ArrowRight') next = splitRatio.value + step
  else if (e.key === 'Home') next = 0
  else if (e.key === 'End') next = 1
  else if (e.key === 'Enter') next = DEFAULT_SPLIT_RATIO
  if (next == null) return
  e.preventDefault()
  tabsStore.setRatio(clampRatio(next, total), total)
}
function resetSplitRatio() {
  tabsStore.setRatio(DEFAULT_SPLIT_RATIO, splitWidth())
}

// While split, the sidebar shrinks to its icons (Settings → Windows & tabs).
const compactSidebar = computed(
  () => tabsStore.isSplit && appSettings.settings?.split_compact_sidebar !== 'false',
)

/** Sidebar surfaces open in the focused half of a split (else as usual). */
function openSurfaceFocused(kind: SurfaceKind, params: Record<string, unknown> = {}) {
  void openSurface(kind, params, tabsStore.isSplit && tabsStore.focusedPane === 'right' ? { pane: 'right' } : {})
}

// Global listener that mirrors backend-reported active runs into the liveRuns
// store (see onMounted). Torn down on unmount to avoid a dangling subscription.
let unlistenActivated: UnlistenFn | null = null

// Listener for a session row clicked in the menu-bar switcher (see lib/tray.ts).
let unlistenTrayRun: UnlistenFn | null = null
let unlistenFocusOpen: UnlistenFn | null = null
let unlistenPrInboxOpen: UnlistenFn | null = null

// Listener for the tray's "Background processes" row (lib/tray.ts).
let unlistenTrayProcs: UnlistenFn | null = null

// Listener for a clicked Slack thread OS notification (see lib/slackWindow.ts).
let unlistenCaptureNotification: UnlistenFn | null = null

// Listener for a Slack thread received over the Inbox API (see lib/slackWindow.ts).
let unlistenSlackApiReceived: UnlistenFn | null = null

// Listener for the Slack threads window asking to navigate (e.g. to the MCP settings).
let unlistenSlackRoute: UnlistenFn | null = null

// Listener for the PR Inbox window asking to navigate (e.g. "Add to devdy").
let unlistenPrInboxRoute: UnlistenFn | null = null
let unlistenCalendarRoute: UnlistenFn | null = null
let unlistenRadarRoute: UnlistenFn | null = null

// Listeners for the Slack / Notes / Todos windows opening a linked session or
// starting a new one (lib/sessionLinks).
let unlistenWindowRun: UnlistenFn | null = null
let unlistenWindowNewSession: UnlistenFn | null = null
let unlistenSurfaces: UnlistenFn | null = null

// Screen count, polled: when the second screen goes away, its surface windows
// (PR Inbox, Calendar…) come back as tabs if the setting asks for it. macOS has
// no "monitors changed" event the webview can hear.
let monitorTimer: ReturnType<typeof setInterval> | null = null
function startMonitorWatch() {
  let last = 0
  const check = async () => {
    const now = await monitorCount(true)
    const before = last
    last = now
    if (!before || !shouldDockOnMonitorChange(before, now, appSettings.settings?.window_dock_on_unplug !== 'false')) return
    const n = await dockAllSurfaceWindows()
    if (n > 0) toast.info(t('surfaces.toast.docked', { n }, n))
  }
  void check()
  monitorTimer = setInterval(() => void check(), 5000)
}

// True when the event target is a text-entry surface where Backspace/navigation
// keys are legitimately used to edit text.
function isEditableTarget(el: EventTarget | null): boolean {
  const node = el as HTMLElement | null
  if (!node || typeof node.closest !== 'function') return false
  if (node.isContentEditable) return true
  return !!node.closest('input, textarea, select, [contenteditable=""], [contenteditable="true"]')
}

// Swallow navigation keys (mainly Backspace) when focus is NOT in a text field.
// Vietnamese IMEs (Telex/VNI) compose diacritics by injecting Backspace at the
// OS level; if the composer hasn't been focused yet, those Backspaces reach the
// webview and trigger history "back" navigation — the app appears to jump to a
// different screen while the user is just typing. Blocking them here is the
// backstop; RunView still focuses the composer on session open.
// Esc leaves Focus mode — unless something closer owns the key: a text field,
// an open dialog / menu, or a peeked list (it stops the event itself).
function onFocusEscape(e: KeyboardEvent) {
  if (e.key !== 'Escape' || !uiLayout.focusMode || e.defaultPrevented) return
  if (isEditableTarget(e.target)) return
  if (document.querySelector('[role="dialog"], [role="menu"]')) return
  uiLayout.focusMode = false
}

function onGlobalNavKeyGuard(e: KeyboardEvent) {
  if (e.key === 'Backspace' && !isEditableTarget(e.target)) {
    e.preventDefault()
  }
}

// The sidebar is gone either because the user hid it (any route, persisted) or
// because focus mode is collapsing a run down to its conversation.
const sidebarVisible = computed(
  () => !uiLayout.sidebarHidden && !uiLayout.focusMode,
)

// Command palette (⌘K everything / ⌘P quick open). The same shortcut again
// closes it; the other one switches mode.
const paletteMode = ref<PaletteMode | null>(null)
function togglePalette(mode: PaletteMode) {
  paletteMode.value = paletteMode.value === mode ? null : mode
}

// ── Native menu bar ─────────────────────────────────────────────────────────
// Every menu action that isn't screen-specific is bound here, at the app root,
// so it works from any route. RunView binds `file.newSession` itself — starting
// a session needs a project, which only that screen has.
function bindGlobalMenuActions() {
  const unbinds = [
    // A NEW todo / note is ALWAYS captured in the standalone capture window, from
    // every entry point — menu, shortcut, mascot, run screen (existing ones are
    // read and edited in the Notes / Todos windows).
    registerMenuAction('file.newTodo', () =>
      openItemCreateWindow('todo', routeCaptureContext()),
    ),
    registerMenuAction('file.newNote', () =>
      openItemCreateWindow('note', routeCaptureContext()),
    ),
    registerMenuAction('window.todoList', () =>
      openSurface('todos', { projectId: routeCaptureContext().projectId }),
    ),
    registerMenuAction('window.noteList', () =>
      openSurface('notes', { projectId: routeCaptureContext().projectId }),
    ),
    registerMenuAction('window.secretNotes', () =>
      openSecretNotesWindow({ projectId: routeCaptureContext().projectId }),
    ),
    registerMenuAction('window.slackThreads', () =>
      openSurface('slack', { projectId: routeCaptureContext().projectId }),
    ),
    registerMenuAction('view.toggleSidebar', () => uiLayout.toggleSidebar()),
    registerMenuAction('view.toggleFocus', () => uiLayout.toggleFocus()),
    // ⌘B: the list column of the screen in view (composables/useListPane).
    registerMenuAction('view.toggleList', () => void toggleActiveListPane()),
    // Windows/Linux only — macOS uses the OS full-screen item (see appMenu.ts).
    registerMenuAction('view.toggleFullscreen', async () => {
      const win = getCurrentWebviewWindow()
      await win.setFullscreen(!(await win.isFullscreen())).catch(() => {})
    }),
    registerMenuAction('help.docs', () => void openUrl(DOCS_URL).catch(() => {})),
    registerMenuAction('help.reportIssue', () => void openUrl(NEW_ISSUE_URL).catch(() => {})),
    registerMenuAction('help.openLogs', async () => {
      try {
        // Same folder lib.rs points the tracing file appender at.
        await openPath(await join(await appDataDir(), 'logs'))
      } catch (e) {
        toast.error(String(e))
      }
    }),
    registerMenuAction('help.reload', () => window.location.reload()),
    // The shortcut list on the About screen (lib/shortcuts).
    registerMenuAction('help.shortcuts', () => router.push({ path: '/about', hash: '#shortcuts' }).catch(() => {})),
    // Palette + tab strip; Rust drops these while a pop-out has the focus.
    registerMenuAction('palette.open', () => togglePalette('all')),
    registerMenuAction('palette.quickOpen', () => togglePalette('quick')),
    registerMenuAction('tabs.prev', () => goToTabOffset(-1)),
    registerMenuAction('tabs.next', () => goToTabOffset(1)),
    registerMenuAction('tabs.close', closeCurrentTab),
    registerMenuAction('tabs.toggleSplit', toggleSplitCurrent),
    registerMenuAction('tabs.popOut', popOutCurrent),
    ...[1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => registerMenuAction(`tabs.goto.${n}`, () => goToTabNumber(n))),
    ...NAV_ROUTES.map((r) =>
      registerMenuAction(`go.${r.path}`, () => (r.surface ? openSurface(r.surface) : router.push(r.path))),
    ),
  ]
  return () => unbinds.forEach((off) => off())
}

const DOCS_URL = 'https://github.com/thainph/devdy#readme'
const NEW_ISSUE_URL = 'https://github.com/thainph/devdy/issues/new'

let unbindMenuActions: (() => void) | null = null
let unlistenMenu: UnlistenFn | null = null

// Labels carry both the language AND the current layout state ("Hide" vs "Show
// Sidebar"), and New Session is greyed out unless a project screen is mounted, so
// the menu is rebuilt whenever any of those moves. Watching the i18n
// locale rather than the settings row avoids rebuilding with stale labels: the
// setting is what *drives* the switch, `locale` is what has actually applied.
// The View menu's list item also names the state of the screen in view, read
// from the DOM once it has settled (after a navigation or a ⌘B).
function menuState() {
  return { sidebarHidden: uiLayout.sidebarHidden, focusMode: uiLayout.focusMode, listHidden: activeListPaneHidden() }
}
watch(
  () =>
    [
      locale.value,
      uiLayout.sidebarHidden,
      uiLayout.focusMode,
      isMenuActionBound('file.newSession'),
      route.fullPath,
      uiLayout.hiddenPanes.join(),
      tabsStore.focusedPane,
    ] as const,
  async () => {
    if (isPopoutWindow || isSessionWindow) return
    await nextTick()
    applyAppMenu(menuState())
  },
)

// Rebuild the tray on language change so its static labels (Main Window / Quit)
// follow the app language, matching the app menu above.
watch(locale, () => {
  if (isSessionWindow) return
  void refreshTray()
})

// File the capture under whatever the user is looking at, so capturing from a
// run needs no project picking at all.
function routeCaptureContext() {
  // The focused right half of a split, when it is a session.
  const right = tabsStore.focusedPane === 'right' && rightEntry.value ? sanitized(rightEntry.value) : null
  if (right?.type === 'project') return { projectId: right.projectId, runId: right.lastRunId }
  return {
    projectId: typeof route.params.projectId === 'string' ? route.params.projectId : null,
    runId: typeof route.params.runId === 'string' ? route.params.runId : null,
  }
}

onBeforeUnmount(() => {
  unlistenActivated?.()
  unlistenActivated = null
  unlistenTrayRun?.()
  unlistenTrayRun = null
  unlistenFocusOpen?.()
  unlistenFocusOpen = null
  unlistenPrInboxOpen?.()
  unlistenPrInboxOpen = null
  unlistenTrayProcs?.()
  unlistenTrayProcs = null
  unlistenCaptureNotification?.()
  unlistenCaptureNotification = null
  unlistenSlackApiReceived?.()
  unlistenSlackApiReceived = null
  unlistenSlackRoute?.()
  unlistenSlackRoute = null
  unlistenPrInboxRoute?.()
  unlistenPrInboxRoute = null
  unlistenCalendarRoute?.()
  unlistenCalendarRoute = null
  unlistenRadarRoute?.()
  unlistenRadarRoute = null
  unlistenWindowRun?.()
  unlistenWindowRun = null
  unlistenWindowNewSession?.()
  unlistenWindowNewSession = null
  unlistenSurfaces?.()
  unlistenSurfaces = null
  if (monitorTimer) clearInterval(monitorTimer)
  monitorTimer = null
  unbindMenuActions?.()
  unbindMenuActions = null
  unlistenMenu?.()
  unlistenMenu = null
  window.removeEventListener('keydown', onGlobalNavKeyGuard, true)
  window.removeEventListener('keydown', onFocusEscape)
  offRightHalfGuard?.()
})

// App version shown in the sidebar footer; read from Tauri so it always matches
// the packaged build (tauri.conf.json) instead of a hardcoded string.
const appVersion = ref('')
onMounted(async () => {
  window.addEventListener('keydown', onGlobalNavKeyGuard, true)
  if (!isPopoutWindow && !isSessionWindow) window.addEventListener('keydown', onFocusEscape)
  try {
    appVersion.value = await getVersion()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
})

/** Sidebar row: on screen in either half. */
function navActive(path: string, prefix = false): boolean {
  const on = (p: string | null) => !!p && (prefix ? p.startsWith(path) : p === path)
  return on(route.path) || on(rightPath.value)
}

/** Sidebar row look: highlighted while either half shows `path`. */
function navClass(path: string, prefix = false): string {
  return navActive(path, prefix)
    ? 'is-active bg-accent text-foreground font-medium'
    : 'text-muted-foreground hover:text-foreground hover:bg-accent/50'
}

// Same source of truth as the native "Go" menu (see lib/navigation.ts).
const navItems = computed(() =>
  NAV_ROUTES.map((r) => ({
    path: r.path,
    label: t(r.labelKey),
    icon: r.icon,
    group: r.group,
    surface: r.surface,
  })),
)

const isDark = ref(false)

function applyTheme(theme: string) {
  if (theme === 'dark') {
    document.documentElement.classList.add('dark')
    isDark.value = true
  } else if (theme === 'light') {
    document.documentElement.classList.remove('dark')
    isDark.value = false
  } else {
    const dark = window.matchMedia('(prefers-color-scheme: dark)').matches
    document.documentElement.classList.toggle('dark', dark)
    isDark.value = dark
    window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', (e) => {
      document.documentElement.classList.toggle('dark', e.matches)
      isDark.value = e.matches
    })
  }
}

// Named color palette. Sets the `data-theme` attribute on <html> (see base.css);
// the default (indigo) theme uses no attribute so it falls back to :root/.dark.
function applyColorTheme(theme: string) {
  const t = theme && theme !== 'default' ? theme : ''
  if (t) document.documentElement.setAttribute('data-theme', t)
  else document.documentElement.removeAttribute('data-theme')
  applySystemAccent(t)
}

onMounted(async () => {
  if (isPopoutWindow) {
    // Pop-out window: only theme matters; skip the main app's data fetches.
    try {
      await appSettings.refresh()
      applyTheme(appSettings.settings?.theme ?? 'system')
      applyColorTheme(appSettings.settings?.color_theme ?? 'default')
      setLocale(appSettings.settings?.language ?? 'en')
    } catch {
      applyTheme('system')
    }
    return
  }
  if (isSessionWindow) {
    // Chrome-less run window: apply the theme, then let RunView (already routed to
    // the run in setup) fetch its own projects / runs and open its stream. Skip
    // the main window's app-wide services — native menu, the
    // run:activated mirror and PermissionNotifier — so a popped-out session can't
    // double-fire OS notifications the main window already owns.
    try {
      await appSettings.refresh()
      applyTheme(appSettings.settings?.theme ?? 'system')
      applyColorTheme(appSettings.settings?.color_theme ?? 'default')
      setLocale(appSettings.settings?.language ?? 'en')
    } catch {
      applyTheme('system')
    }
    return
  }
  void focusStore.start()
  // Sidebar badge: mentions waiting for an answer (the backend polls).
  void mentionsStore.watchCache()
  // Sidebar badge: PRs waiting for your review (the backend polls).
  void prInboxStore.watchCache()
  // Tracked background processes: counts for the sidebar row + tray, and the
  // first snapshot the startup orphan prompt waits on.
  void bgProcs.start()

  // Native menu bar: bind the actions first, then install the menu, so an
  // impatient click on a freshly drawn item can't land on nothing.
  unbindMenuActions = bindGlobalMenuActions()
  try {
    unlistenMenu = await listenMenuActions()
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }
  applyAppMenu(menuState())

  // Populate the menu-bar window switcher (just the main window at first; session
  // pop-outs re-send as they open — see lib/tray.ts / lib/sessionWindow.ts).
  void refreshTray()

  // Attach per-run listeners for any run the backend reports as active — even
  // ones this window never opened (started/resumed from a remote Controller, or
  // still live after a restart). This is what lets a remotely-driven run's
  // permission / question prompts surface on the desktop (dashboard +
  // native notification), not just on the phone. `startListening` is idempotent,
  // so re-announcing a locally-started run is a harmless no-op.
  try {
    unlistenActivated = await listen<{ run_id: string; project_id: string; history_end?: number | null }>(
      'run:activated',
      async (e) => {
        const { run_id, project_id, history_end } = e.payload ?? {}
        if (!run_id || !project_id) return
        // `activate` (not just `startListening`): on a backend-driven resume the
        // listeners survived the previous turn, so only this resets the stale
        // terminal status that would otherwise let the session be evicted.
        live.activate(run_id, project_id, history_end).catch(() => {})
        // A freshly-spawned run (notably a conductor's worker) lands in
        // live.sessions the instant it activates, but the dock groups workers
        // under their conductor via runMeta[run].conductor_run_id — which only
        // list_runs fills. Without this, a new worker flashes as a standalone
        // row until something opens a RunView for its project and refetches.
        // refreshMeta learns the worker's conductor so the dock nests it at
        // once; when the run is in the on-screen project it also refreshes the
        // foreground runs list (quietly, no loading flash) so RunView's History
        // gives the conductor its expand chevron immediately too.
        if (!runsStore.runMeta.has(run_id)) {
          await runsStore.refreshMeta(project_id).catch(() => {})
        }
        // The conductor may live in another project: learn its project too, or
        // the dock would file (and open) the conductor under the worker's project.
        await runsStore.ensureConductorMeta(run_id).catch(() => {})
        // Worker of the conductor currently open in a RunView? Refresh that
        // conductor's right-side worker panel too. The panel is otherwise driven
        // ONLY by the per-select `conductor:changed` listener — a single point of
        // failure that, if it misses or is torn down, leaves a freshly-spawned
        // worker invisible until the user reopens the session. Piggybacking on
        // this always-on `run:activated` signal (the same one the dock trusts)
        // makes the new worker appear in the panel the instant it activates.
        const meta = runsStore.runMeta.get(run_id)
        if (meta?.conductor_run_id && meta.conductor_run_id === conductorStore.selectedId) {
          conductorStore.refreshDetail().catch(() => {})
        }
      },
    )
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  try {
    unlistenTrayRun = await listen<TrayOpenRun>(TRAY_OPEN_RUN, (e) => {
      if (e.payload?.runId) void openRunFromTray(e.payload.runId)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // A todo reminder click (or the tray's Todos row): open the Todos window on
  // that todo. The id is also parked in Rust for a window that reads it on
  // mount — taken here so it is not reused later.
  try {
    unlistenFocusOpen = await listen<FocusOpenTodo>(FOCUS_OPEN_WINDOW, (e) => {
      void invoke('focus_take_open_todo').catch(() => {})
      void openSurface('todos', e.payload?.todo_id ? { id: e.payload.todo_id } : {})
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // A "new PR to review" notification click while the PR Inbox window is
  // closed (pr_inbox::open_window): open it.
  try {
    unlistenPrInboxOpen = await listen(PR_INBOX_OPEN_WINDOW, () => {
      void openSurface('prInbox')
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // The tray's "Background processes" row: show the Processes screen here.
  try {
    unlistenTrayProcs = await listen(EVT_TRAY_OPEN, () => {
      void openRouteFromWindow('/processes')
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // A clicked "Slack thread translated / failed" notification opens the Slack
  // threads window on that thread. Registered here, past the pop-out / session
  // early returns, so only the main window handles it.
  try {
    unlistenCaptureNotification = await listen<SlackNotificationClicked>(
      SLACK_NOTIFICATION_CLICKED,
      (e) => {
        const { id, kind } = e.payload ?? {}
        if (id && kind === 'slack') void openSurface('slack', { id })
      },
    )
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // A Slack thread sent from the browser extension opens the Slack threads
  // window on it right away (it fills in as the background job progresses).
  try {
    unlistenSlackApiReceived = await listen<SlackApiReceived>(SLACK_API_RECEIVED, (e) => {
      if (e.payload?.id) void openSurface('slack', { id: e.payload.id })
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // The Slack threads window has no router of this app: it asks the main window
  // to focus itself and navigate (e.g. to the MCP settings when no token is set).
  try {
    unlistenSlackRoute = await listen<SlackWindowOpenRoute>(SLACK_WINDOW_OPEN_ROUTE, (e) => {
      void openRouteFromWindow(e.payload?.path)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Same for the PR Inbox window (e.g. "Add to devdy" → the Projects screen).
  try {
    unlistenPrInboxRoute = await listen<PrInboxWindowOpenRoute>(PR_INBOX_WINDOW_OPEN_ROUTE, (e) => {
      void openRouteFromWindow(e.payload?.path)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Same for the Calendar window (e.g. "Reconnect" → the Settings screen).
  try {
    unlistenCalendarRoute = await listen<CalendarWindowOpenRoute>(CALENDAR_WINDOW_OPEN_ROUTE, (e) => {
      void openRouteFromWindow(e.payload?.path)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Same for the Delivery Radar window (e.g. "Open settings" → a project's settings).
  try {
    unlistenRadarRoute = await listen<DeliveryRadarOpenRoute>(DELIVERY_RADAR_OPEN_ROUTE, (e) => {
      void openRouteFromWindow(e.payload?.path)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Linked sessions / "New session from this …" of the Slack, Notes and Todos
  // windows (no router of this app there).
  try {
    unlistenWindowRun = await listen<WindowOpenRun>(WINDOW_OPEN_RUN, (e) => {
      const { projectId, runId } = e.payload ?? {}
      if (projectId && runId) void openRunFromTray(runId, projectId)
    })
    unlistenWindowNewSession = await listen<WindowNewSession>(WINDOW_NEW_SESSION, (e) => {
      const p = e.payload
      if (p?.projectId && p.target?.id) void newSessionFromWindow(p)
    })
  } catch {
    // Ignore (e.g. running outside the Tauri shell during dev in a browser).
  }

  // Pop-outs asking for a surface tab, and surface windows docking back.
  unlistenSurfaces = await listenSurfaceRequests()
  startMonitorWatch()

  // Load projects up front so app-wide UI (e.g. permission notifications) can
  // resolve project names without waiting for the Projects view to open.
  projectsStore.fetchProjects()
  projectsStore.fetchConflicts()
  projectsStore.fetchRuleConflicts()
  try {
    await appSettings.refresh()
    applyTheme(appSettings.settings?.theme ?? 'system')
    applyColorTheme(appSettings.settings?.color_theme ?? 'default')
    setLocale(appSettings.settings?.language ?? 'en')
  } catch {
    applyTheme('system')
  }
})
</script>

<template>
  <!-- Pop-out file viewer window: bare layout, no app chrome. -->
  <FileViewerWindow v-if="isFileWindow" />

  <!-- Pop-out permission prompt window: bare layout, mirrors the main window. -->
  <PermissionWindow v-else-if="isPermissionWindow" />

  <!-- THE todo / note window: create or edit, the only place either is written. -->
  <ItemWindow v-else-if="isItemWindow" />

  <!-- THE todo / note LIST window: stands beside the main window, replacing the drawer. -->
  <ItemListWindow v-else-if="isItemListWindow" />

  <!-- THE Secret notes window: the password-sealed vault (local only). -->
  <SecretNotesWindow v-else-if="isSecretNotesWindow" />

  <!-- THE Slack threads window: Slack-like reader of the fetched + translated threads. -->
  <SlackThreadsWindow v-else-if="isSlackWindow" />

  <!-- THE PR Inbox window: the PR Inbox screen on its own OS window. -->
  <PrInboxWindow v-else-if="isPrInboxWindow" />

  <!-- THE GitHub Mentions window: the mentions list on its own OS window. -->
  <GithubMentionsWindow v-else-if="isGithubMentionsWindow" />

  <!-- THE Calendar window: the Calendar screen on its own OS window. -->
  <CalendarWindow v-else-if="isCalendarWindow" />


  <!-- Delivery Radar pop-out: the radar plus the dialog / toast hosts its row
       menu ("+ Todo") needs, like PrInboxWindow. -->
  <div
    v-else-if="isRadarWindow"
    class="app-shell flex h-screen w-screen flex-col overflow-hidden bg-background text-foreground"
  >
    <DeliveryRadarView class="min-h-0 flex-1" />
    <ConfirmModal />
    <ToastHost />
  </div>

  <!-- Desktop-pet window: transparent, frameless host that floats only the fox. -->
  <MascotWindow v-else-if="isMascotWindow" />

  <!-- Session pop-out: one run in its own OS window. Reuses the full RunView via
       the router, but with no sidebar / nav / docks / app-wide notifiers — just
       the conversation, its toolbar and composer. -->
  <div
    v-else-if="isSessionWindow"
    class="app-shell flex flex-col h-screen bg-background text-foreground overflow-hidden"
  >
    <ThemeDecorations :active="sceneOn" :theme="sceneTheme" />
    <main class="flex-1 min-w-0 flex flex-col overflow-hidden">
      <div class="flex-1 min-w-0 overflow-auto">
        <!-- Same key as the main window: hopping from a conductor to a worker in
             another project remounts RunView on that project. -->
        <RouterView v-slot="{ Component }">
          <component :is="Component" :key="routeKey" />
        </RouterView>
      </div>
    </main>
    <!-- Overlay hosts the run screen relies on; NOT PermissionNotifier /
         CalendarReminder — those stay owned by the main window. -->
    <ConfirmModal />
    <PromptModal />
    <ToastHost />
    <ImageCompareHost />
  </div>

  <div v-else class="app-shell flex flex-col h-screen bg-background text-foreground overflow-hidden">
    <!-- Animated decorative overlay for scenic themes (e.g. Full Moon 🌕).
         Sits ABOVE the UI as a non-interactive, screen-blended light layer so it
         stays visible over the app's opaque panels without blocking clicks. -->
    <ThemeDecorations :active="sceneOn" :theme="sceneTheme" />

    <!-- Unified title bar: brand + open-project tabs + global actions on ONE 44px
         row spanning the whole window, above both columns.

         This is the app's only chrome row. The sidebar has no brand header of
         its own any more, so nothing competes with a view's header for the top
         line, and the run screen gains back ~52px of height for the conversation.
         Hidden in focus mode, where the point is no chrome at all. -->
    <header
      v-if="!uiLayout.focusMode"
      class="app-titlebar flex items-center gap-2 h-11 px-3 shrink-0 bg-sidebar border-b border-border/50"
    >
      <img src="/logo.png" alt="Devdy" class="h-[18px] w-[18px] rounded shrink-0" />
      <p class="text-[13px] font-semibold leading-none tracking-tight shrink-0">Devdy</p>
      <div v-if="tabsStore.entries.length > 0" class="h-4 w-px bg-border shrink-0 mx-1" aria-hidden="true" />

      <WorkspaceTabs />

      <div class="ml-auto flex shrink-0 items-center gap-2">
        <!-- Runs waiting for an approval; click = that run's card / session. -->
        <PendingApprovalsPill />

        <Button
          variant="ghost"
          size="xs"
          class="shrink-0"
          :title="`${t('palette.open')} (${IS_MAC ? '⌘K' : 'Ctrl+K'})`"
          :aria-label="t('palette.open')"
          @click="togglePalette('all')"
        >
          <Search class="h-3.5 w-3.5" :stroke-width="1.75" />
          <span class="hidden lg:inline text-muted-foreground/80">{{ IS_MAC ? '⌘K' : 'Ctrl+K' }}</span>
        </Button>

        <!-- Sits here rather than inside the sidebar: a "hide" button that
             disappears along with the thing it hides can't bring it back. -->
        <Button
          variant="ghost"
          size="icon-sm"
          class="shrink-0"
          :title="uiLayout.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar')"
          :aria-label="uiLayout.sidebarHidden ? t('menu.showSidebar') : t('menu.hideSidebar')"
          @click="uiLayout.toggleSidebar()"
        >
          <component :is="uiLayout.sidebarHidden ? PanelLeftOpen : PanelLeftClose" class="h-4 w-4" :stroke-width="1.75" />
        </Button>
      </div>
    </header>

    <div class="flex flex-1 min-h-0">
    <!-- Sidebar. Hidden either by the title-bar toggle (all routes, persisted) or
         by focus mode — the latter only while in the run workspace, so other
         routes like project settings keep their navigation. Once hidden it is
         gone entirely; View → Show Sidebar brings it back.

         It collapses by animating its own width to zero while the inner column
         keeps a fixed 220px, so the nav slides out of view instead of the labels
         reflowing into a squashed mess on the way. -->
    <Transition name="sidebar">
      <aside
        v-if="sidebarVisible"
        class="app-sidebar shrink-0 overflow-hidden bg-sidebar border-r border-border/50"
        :class="{ 'is-compact': compactSidebar }"
      >
        <!-- Split view shrinks it to its icons (labels move to tooltips). -->
        <div class="app-sidebar-inner h-full flex flex-col">
          <!-- No brand header: the logo moved to the title bar, so the nav starts
               at the very top and a view's header owns the first line alone. -->
          <nav class="flex-1 px-2 py-2.5 space-y-0.5">
            <template v-for="(item, index) in navItems" :key="item.path">
              <!-- Todos, Notes and Slack threads are surfaces (lib/surfaces): a
                   tab of this window or their own window, per the "Windows &
                   tabs" setting. They close out the daily-work group, right
                   before the config screens. -->
              <template v-if="startsNavGroup(index) && item.group === 'config'">
                <button
                  type="button"
                  class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none"
                  :class="navClass('/todos')"
                  :title="t('item.navTodos')"
                  @click="openSurfaceFocused('todos', { projectId: routeCaptureContext().projectId })"
                >
                  <ListTodo class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                  <span class="nav-label flex-1 truncate text-left">{{ t('item.navTodos') }}</span>
                  <span
                    v-if="focusStore.overdueCount > 0"
                    class="nav-badge flex h-4 min-w-[16px] items-center justify-center rounded-full bg-destructive px-1 text-[10px] font-medium text-destructive-foreground leading-none"
                    :title="t('item.lateness.overdue')"
                  >
                    {{ focusStore.overdueCount }}
                  </span>
                </button>

                <button
                  type="button"
                  class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none"
                  :class="navClass('/notes')"
                  :title="t('item.navNotes')"
                  @click="openSurfaceFocused('notes', { projectId: routeCaptureContext().projectId })"
                >
                  <StickyNote class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                  <span class="nav-label flex-1 truncate text-left">{{ t('item.navNotes') }}</span>
                </button>

                <!-- Secret notes: their own window. -->
                <button
                  type="button"
                  class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none text-muted-foreground hover:text-foreground hover:bg-accent/50"
                  :title="t('secretNotes.nav')"
                  @click="openSecretNotesWindow({ projectId: routeCaptureContext().projectId })"
                >
                  <LockKeyhole class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                  <span class="nav-label flex-1 truncate text-left">{{ t('secretNotes.nav') }}</span>
                </button>

                <button
                  type="button"
                  class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none"
                  :class="navClass('/slack')"
                  :title="t('slackThread.nav')"
                  @click="openSurfaceFocused('slack', { projectId: routeCaptureContext().projectId })"
                >
                  <Slack class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                  <span class="nav-label flex-1 truncate text-left">{{ t('slackThread.nav') }}</span>
                </button>
              </template>

              <!-- Divider between nav groups (daily work / agent config / app). -->
              <div v-if="startsNavGroup(index)" class="mx-3 !my-2 border-t border-border/50" />

              <!-- Surfaces (PR Inbox, Calendar…) open as a tab or their own
                   window (lib/surfaceOpen), like the Slack button above. -->
              <button
                v-if="item.surface"
                type="button"
                class="relative flex w-full items-center gap-2.5 rounded-md px-3 py-2 text-sm transition-colors cursor-pointer select-none"
                :class="navClass(item.path)"
                :title="item.label"
                @click="openSurfaceFocused(item.surface)"
              >
                <span
                  v-if="navActive(item.path)"
                  class="app-nav-indicator absolute left-0 inset-y-[6px] w-[2px] rounded-r-full bg-primary"
                />
                <component :is="item.icon" class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                <span class="nav-label flex-1 truncate text-left">{{ item.label }}</span>
                <span
                  v-if="item.path === '/github-mentions' && mentionsStore.pendingCount > 0"
                  class="nav-badge flex h-4 min-w-[16px] items-center justify-center rounded-full bg-primary px-1 text-[10px] font-medium text-primary-foreground leading-none"
                  :title="t('githubMentions.pendingTitle')"
                >
                  {{ mentionsStore.pendingCount }}
                </span>
                <span
                  v-else-if="item.path === '/pr-inbox' && prInboxStore.pendingCount > 0"
                  class="nav-badge flex h-4 min-w-[16px] items-center justify-center rounded-full bg-primary px-1 text-[10px] font-medium text-primary-foreground leading-none"
                  :title="t('prInbox.pendingTitle')"
                >
                  {{ prInboxStore.pendingCount }}
                </span>
              </button>
              <!-- Pages navigate the focused half of a split (lib/tabActions). -->
              <a
                v-else
                :href="item.path"
                class="relative flex items-center gap-2.5 px-3 py-2 rounded-md text-sm transition-colors cursor-pointer select-none"
                :class="navClass(item.path, true)"
                :title="item.label"
                @click.prevent="navigateFocused(item.path)"
              >
                <span
                  v-if="navActive(item.path, true)"
                  class="app-nav-indicator absolute left-0 inset-y-[6px] w-[2px] rounded-r-full bg-primary"
                />
                <component :is="item.icon" class="h-[15px] w-[15px] shrink-0" :stroke-width="1.75" />
                <span class="nav-label flex-1 truncate">{{ item.label }}</span>
                <span
                  v-if="item.path === '/projects' && (projectsStore.conflicts.length + projectsStore.ruleConflicts.length) > 0"
                  class="nav-badge flex h-4 min-w-[16px] items-center justify-center rounded-full bg-warning px-1 text-[10px] font-medium text-warning-foreground leading-none"
                >
                  {{ projectsStore.conflicts.length + projectsStore.ruleConflicts.length }}
                </span>
              </a>
            </template>
          </nav>

          <!-- Warning row while processes outlive their session -->
          <BackgroundProcessesDock />

          <!-- Version footer -->
          <div class="flex items-center justify-between gap-2 px-4 py-3 border-t border-border/50">
            <p v-if="appVersion && !compactSidebar" class="text-[10px] text-muted-foreground/50 font-mono">v{{ appVersion }}</p>
            <!-- This machine's Remote Control dot (+ device sync); hidden until a relay is set up. -->
            <RemoteIndicator />
          </div>
        </div>
      </aside>
    </Transition>

    <!-- Main content -->
    <main class="flex-1 min-w-0 flex flex-col overflow-hidden">
      <div class="flex-1 min-w-0 overflow-hidden">
        <!-- `:key` stays on the inner <component>, NOT on <RouterView>: KeepAlive
             caches entries by their vnode key, and a key on the RouterView would
             tear the whole cache down on every navigation — the opposite of the
             point. `routeKey` keeps its existing contract either way (see above):
             run routes get `run-<projectId>` so RunView still remounts per
             project, other routes get a stable per-view key. -->
        <!-- Split: the routed screen on the left, any other tab on the right
             (the paired tab of WorkspaceTabs). A click in a half focuses it,
             so the next tab click lands there. Both halves start level at the
             top: the pair tab carries their names and close buttons. -->
        <div ref="splitRef" class="flex h-full min-h-0">
          <div
            data-pane="left"
            class="h-full min-w-0 flex-1 overflow-auto"
            :class="tabsStore.isSplit && tabsStore.focusedPane === 'left' ? 'shadow-[inset_0_2px_0_hsl(var(--primary))]' : ''"
            :style="tabsStore.isSplit ? { flex: `${splitRatio} 1 0`, minWidth: `min(${SPLIT_MIN_LEFT}px, 50%)` } : undefined"
            @pointerdown.capture="tabsStore.focusedPane = 'left'"
          >
            <RouterView v-slot="{ Component }">
              <KeepAlive :include="CACHED_VIEWS" :max="8">
                <component :is="Component" :key="routeKey" />
              </KeepAlive>
            </RouterView>
            <template v-for="tab in hostedSurfaceTabs" :key="tab.id">
              <Teleport defer to="#devdy-split-body" :disabled="tab.id !== tabsStore.splitId">
                <KeepAlive>
                  <SurfaceHost
                    v-if="tab.id === routedSurfaceId || tab.id === tabsStore.splitId"
                    :surface="tab.surface"
                    :params="tab.params"
                    :in-split="tab.id === tabsStore.splitId"
                    class="h-full"
                  />
                </KeepAlive>
              </Teleport>
            </template>
          </div>
          <!-- Drag to resize, ←/→ (⇧ for big steps), double-click or Enter = 50/50. -->
          <div
            v-if="tabsStore.isSplit"
            class="split-divider"
            :class="{ 'is-dragging': dragRatio != null }"
            role="separator"
            aria-orientation="vertical"
            tabindex="0"
            :aria-valuenow="Math.round(splitRatio * 100)"
            aria-valuemin="0"
            aria-valuemax="100"
            :aria-label="t('surfaces.tabs.resizeSplit')"
            :title="t('surfaces.tabs.resizeSplit')"
            @pointerdown="startDividerDrag"
            @dblclick="resetSplitRatio"
            @keydown="onDividerKey"
          />
          <!-- Always in the DOM (hidden when not split): the teleport target of
               the split tab's host. -->
          <section
            v-show="tabsStore.isSplit"
            data-pane="right"
            class="flex h-full min-w-0 flex-1 flex-col"
            :class="tabsStore.focusedPane === 'right' ? 'shadow-[inset_0_2px_0_hsl(var(--primary))]' : ''"
            :style="tabsStore.isSplit ? { flex: `${1 - splitRatio} 1 0`, minWidth: `min(${SPLIT_MIN_RIGHT}px, 50%)` } : undefined"
            @pointerdown.capture="tabsStore.focusedPane = 'right'"
          >
            <div v-show="!rightRouted" id="devdy-split-body" class="min-h-0 flex-1 overflow-auto" />
            <div v-if="rightRouted" class="min-h-0 flex-1 overflow-auto">
              <PaneRouterView :key="tabEntryKey(rightRouted)" :location="rightLocation" @navigate="navigateRightPane" />
            </div>
          </section>
        </div>
      </div>
    </main>
    </div>

    <!-- Focus mode (⌘.): the chrome is gone, so a faint pill at the top is the
         way back (also ⌘. and Esc). -->
    <div
      v-if="uiLayout.focusMode"
      class="focus-pill fixed left-1/2 top-2 z-40 flex -translate-x-1/2 items-center gap-2 rounded-full border border-border/70 bg-card py-1 pl-3 pr-1 text-[11px] text-muted-foreground shadow-lg"
    >
      <span>{{ t('listPane.focus') }}</span>
      <Button variant="outline" size="xs" @click="uiLayout.focusMode = false">
        <Minimize2 class="h-3 w-3" :stroke-width="2" />
        {{ t('listPane.exitFocus') }}
        <kbd class="ml-0.5 font-mono text-[10px] opacity-70">{{ IS_MAC ? '⌘.' : 'Ctrl+.' }}</kbd>
      </Button>
    </div>

    <!-- Headless: fires native OS notifications for runs awaiting input while the
         app is backgrounded; the in-app signal is the History attention icon. -->
    <PermissionNotifier />

    <!-- Headless: opens one standalone permission window per run that is asking
         (viewed, popped-out, or background) and owns the resolve logic. -->
    <PermissionWindowManager />

    <!-- Headless: fires native reminders for upcoming calendar events app-wide;
         clicking opens the event's detail drawer on the Calendar screen. -->
    <CalendarReminder />

    <!-- Startup prompt for processes left running by the previous app session -->
    <OrphanProcessesPrompt />

    <!-- Answers the backend's quit prompt when background processes are running -->
    <QuitProcessesDialog />

    <!-- ⌘K / ⌘P command palette (title-bar search button, View / Go menu) -->
    <CommandPalette v-model:mode="paletteMode" />

    <!-- App-wide confirm dialog host (see useConfirm) -->
    <ConfirmModal />

    <!-- App-wide text-input dialog host (see usePrompt) -->
    <PromptModal />

    <!-- App-wide toast host (see useToast) -->
    <ToastHost />

    <!-- Sidebar hidden: the Remote Control dot moves to a corner while a
         Controller drives this one, so that warning is never invisible. -->
    <RemoteIndicator v-if="!sidebarVisible" floating />

    <!-- App-wide side-by-side image comparison (banner + picker + compare view) -->
    <ImageCompareHost />

    <!-- DY mascot: app-wide operator. Host picks in-app floating vs desktop pet. -->
    <CyberFoxHost />
  </div>
</template>

<style scoped>
/* Sidebar collapse/expand. Width is set here rather than as a Tailwind class so
   the transition classes below can override it without a specificity fight. */
.app-sidebar {
  width: 220px;
}

.app-sidebar-inner {
  width: 220px;
  transition: width 180ms ease;
}

/* Split view: icons only, labels in the rows' tooltips, counts as corner pips. */
.app-sidebar.is-compact,
.app-sidebar.is-compact .app-sidebar-inner {
  width: 52px;
}
.app-sidebar {
  transition: width 180ms ease;
}
.app-sidebar.is-compact nav :is(a, button) {
  justify-content: center;
  padding-left: 0;
  padding-right: 0;
}
.app-sidebar.is-compact .nav-label {
  display: none;
}
.app-sidebar.is-compact .nav-badge {
  position: absolute;
  top: 1px;
  right: 3px;
  height: 14px;
  min-width: 14px;
  font-size: 9px;
}

.sidebar-enter-active,
.sidebar-leave-active {
  transition: width 180ms ease, opacity 180ms ease;
}

/* Split divider: a wide grab area around a 1px line, with a grip in the middle. */
.split-divider {
  position: relative;
  flex-shrink: 0;
  width: 7px;
  margin: 0 -3px;
  z-index: 5;
  cursor: col-resize;
  touch-action: none;
  outline: none;
}
.split-divider::before {
  content: '';
  position: absolute;
  inset: 0 3px;
  background: hsl(var(--border) / 0.6);
  transition: background-color 120ms ease;
}
.split-divider::after {
  content: '';
  position: absolute;
  top: 50%;
  left: 1px;
  width: 5px;
  height: 28px;
  margin-top: -14px;
  border-radius: 9999px;
  background: hsl(var(--muted-foreground) / 0.35);
  opacity: 0;
  transition: opacity 120ms ease;
}
.split-divider:hover::before,
.split-divider:focus-visible::before,
.split-divider.is-dragging::before {
  inset: 0 2px;
  background: hsl(var(--primary));
}
.split-divider:hover::after,
.split-divider:focus-visible::after,
.split-divider.is-dragging::after {
  opacity: 1;
}

.sidebar-enter-from,
.sidebar-leave-to {
  width: 0;
  /* The border would otherwise linger as a 1px line at the very end. */
  border-right-width: 0;
  opacity: 0;
}

/* Faint until reached for, so it doesn't sit on the content. */
.focus-pill {
  opacity: 0.4;
  transition: opacity 150ms ease;
}
.focus-pill:hover,
.focus-pill:focus-within {
  opacity: 1;
}

@media (prefers-reduced-motion: reduce) {
  .focus-pill {
    transition: none;
  }
  .sidebar-enter-active,
  .sidebar-leave-active,
  .app-sidebar,
  .app-sidebar-inner {
    transition: none;
  }
}
</style>
