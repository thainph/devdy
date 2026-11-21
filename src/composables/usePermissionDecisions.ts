// Shared state + the single resolve logic for pending permission / question
// requests in the main window. Two surfaces present them:
//
//   - the in-app card stack (PermissionInlineStack) while the main window is in
//     front, and
//   - the standalone per-run permission windows (PermissionWindowManager) while it
//     isn't (see lib/permissionSurface for the rules).
//
// In split view a RunView docks its own run's request in place of the composer
// (setDockedRun); the stack then skips that run, and a docked request the user
// pops out (popOut) gets its standalone window even while the main window is in
// front.
//
// Both answer through `decide` / `answerQuestion` here, which only ever resolve the
// run's CURRENT head request (a stale or repeated answer is ignored, and
// liveRuns.respondPermission drops one already in flight), so a request can't be
// answered twice whichever surface the click came from.
//
// The state is module-level: there is one main window, and the manager, the card
// stack and the title-bar pill all read the same refs.
import { computed, readonly, ref } from 'vue'
import { useRouter } from 'vue-router'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { getAllWebviewWindows } from '@tauri-apps/api/webviewWindow'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { useWorkspaceTabsStore } from '@/stores/workspaceTabs'
import { useAppSettingsStore } from '@/stores/appSettings'
import type { PermissionRequest } from '@/components/PermissionPrompt.vue'
import { runDisplayName } from '@/lib/tray'
import { focusSessionWindow } from '@/lib/sessionWindow'
import { openPermissionWindow, permissionWindowLabel } from '@/lib/permissionWindow'
import {
  floatWhenBackgroundEnabled,
  permissionSurface,
  promote,
  resolveForeground,
  stackEntries as filterStackEntries,
  type AppForeground,
} from '@/lib/permissionSurface'

export interface PendingPermission {
  runId: string
  requestId: string
  projectId: string
  request: PermissionRequest
}

// --- Main-window foreground -------------------------------------------------

const foreground = ref<AppForeground>(
  typeof document !== 'undefined' && document.hasFocus() ? 'main' : 'background',
)

/** Read-only view of which window is in front (tracked by the manager). */
export const appForeground = readonly(foreground)

/**
 * True while the main window counts as in front only because the user moved
 * into a permission window they popped out of the dock (lib/permissionSurface
 * `resolveForeground`). Re-checked on the next request and when that window
 * loses focus.
 */
const heldByPopout = ref(false)
export const foregroundHeldByPopout = readonly(heldByPopout)

/** Window labels of the requests popped out of the dock (still pending). */
function poppedOutLabels(): Set<string> {
  const out = new Set<string>()
  if (!poppedOut.value.size) return out
  useLiveRunsStore().sessions.forEach((s, runId) => {
    const head = s.permissionQueue[0]
    if (head && poppedOut.value.has(head.request_id)) out.add(permissionWindowLabel(runId))
  })
  return out
}

/** Re-read which window is in front: main, another Devdy window, or none. */
export async function refreshForeground(): Promise<AppForeground> {
  try {
    const main = getCurrentWindow()
    const mainFocused = (await main.isFocused()) && !(await main.isMinimized())
    let focusedOthers: string[] = []
    if (!mainFocused) {
      const others = (await getAllWebviewWindows()).filter((w) => w.label !== main.label)
      const focused = await Promise.all(others.map((w) => w.isFocused().catch(() => false)))
      focusedOthers = others.filter((_, i) => focused[i]).map((w) => w.label)
    }
    const next = resolveForeground(foreground.value, mainFocused, focusedOthers, poppedOutLabels())
    foreground.value = next.foreground
    heldByPopout.value = next.held
  } catch {
    foreground.value = document.hasFocus() ? 'main' : 'background'
    heldByPopout.value = false
  }
  return foreground.value
}

// A blur is confirmed after a short delay: focus hopping straight back (Cmd-Tab
// and back, a native dialog) must not flash the permission windows open.
const BLUR_SETTLE_MS = 300

/**
 * Follow the main window's focus. Call once, from the main window (the manager
 * does); returns the stop function.
 */
export function trackMainForeground(): () => void {
  let timer: ReturnType<typeof setTimeout> | null = null
  let off: (() => void) | null = null
  let stopped = false
  const clear = () => {
    if (timer) clearTimeout(timer)
    timer = null
  }
  getCurrentWindow()
    .onFocusChanged(({ payload: focused }) => {
      clear()
      if (focused) {
        foreground.value = 'main'
        heldByPopout.value = false
      } else timer = setTimeout(() => void refreshForeground(), BLUR_SETTLE_MS)
    })
    .then((unlisten) => {
      if (stopped) unlisten()
      else off = unlisten
    })
    .catch(() => {})
  void refreshForeground()
  return () => {
    stopped = true
    clear()
    off?.()
  }
}

// --- In-app stack state -----------------------------------------------------

// Request ids the user collapsed in-app (still pending — collapsing never denies).
const collapsed = ref<Set<string>>(new Set())
// Run ids the user brought to the front, most recent first.
const promoted = ref<string[]>([])
// Number of mounted in-app stacks (0 when the integrator didn't mount one).
const stackHosts = ref(0)
// A request (from the title-bar pill) to scroll to / focus a run's card.
const focusRequest = ref<{ runId: string; nonce: number } | null>(null)
let focusNonce = 0

// Split view: a RunView answers its own run's request in place of the composer.
// Each mounted RunView holds one slot (token → the run it docks, if any).
const dockSlots = ref<Map<symbol, string>>(new Map())
const dockedRuns = computed(() => new Set(dockSlots.value.values()))
// Request ids the user popped out of the dock into their own window.
const poppedOut = ref<Set<string>>(new Set())

/**
 * Claim (or release, with null) the run whose request a RunView docks. The card
 * stack skips that run while it is docked. Returns nothing; call again on change
 * and with null on unmount.
 */
export function setDockedRun(token: symbol, runId: string | null) {
  const cur = dockSlots.value.get(token) ?? null
  if (cur === runId) return
  const next = new Map(dockSlots.value)
  if (runId) next.set(token, runId)
  else next.delete(token)
  dockSlots.value = next
}

export function registerStackHost(): () => void {
  stackHosts.value++
  return () => {
    stackHosts.value--
  }
}

export function usePermissionDecisions() {
  const live = useLiveRunsStore()
  const runsStore = useRunsStore()
  const tabsStore = useWorkspaceTabsStore()
  const appSettings = useAppSettingsStore()
  const router = useRouter()

  /** Every run with a pending request, its head request, in arrival order. */
  const entries = computed<PendingPermission[]>(() => {
    const out: PendingPermission[] = []
    live.sessions.forEach((s, runId) => {
      const head = s.permissionQueue[0]
      if (head) out.push({ runId, requestId: head.request_id, projectId: s.projectId, request: head })
    })
    return out
  })

  const floatWhenBackground = computed(() =>
    floatWhenBackgroundEnabled(appSettings.settings?.permission_float_when_background),
  )
  const surface = computed(() => permissionSurface(foreground.value, floatWhenBackground.value))

  /** What the in-app card stack shows: not docked in a RunView, not popped out. */
  const stackEntries = computed(() =>
    filterStackEntries(entries.value, dockedRuns.value, poppedOut.value),
  )
  /** Runs whose head request the user popped out into its own window. */
  const poppedOutRuns = computed(() =>
    entries.value.filter((e) => poppedOut.value.has(e.requestId)).map((e) => e.runId),
  )

  function headOf(runId: string): PermissionRequest | null {
    return live.get(runId)?.permissionQueue[0] ?? null
  }

  function projectOf(runId: string): string {
    return live.get(runId)?.projectId ?? runsStore.runMeta.get(runId)?.project_id ?? ''
  }

  /** Which session / project a run belongs to, so a prompt can say who is asking. */
  function sessionOf(runId: string) {
    return runDisplayName(runId, projectOf(runId))
  }

  // The request leaves the queue only once the answer reached the backend; on
  // failure it stays as the head so the user can retry (resolves false).
  async function decide(
    runId: string,
    requestId: string,
    decision: 'allow' | 'deny' | 'ask',
    remember: boolean,
  ): Promise<boolean> {
    const req = headOf(runId)
    if (!req || req.request_id !== requestId) return false
    if (remember && decision === 'allow') live.rememberAllowedTool(runId, req.tool_name, req.request_id)
    else if (remember && decision === 'deny') live.rememberDeniedTool(runId, req.tool_name, req.request_id)
    return live.respondPermission(runId, req, decision, { remember })
  }

  async function answerQuestion(
    runId: string,
    requestId: string,
    answers: Record<string, string>,
  ): Promise<boolean> {
    const req = headOf(runId)
    if (!req || req.request_id !== requestId) return false
    return live.respondPermission(runId, req, 'allow', { answers })
  }

  function collapse(requestId: string) {
    const next = new Set(collapsed.value)
    next.add(requestId)
    collapsed.value = next
  }

  /** Bring a run's card to the front of the stack (un-collapsing it). */
  function bringToFront(runId: string) {
    const head = headOf(runId)
    if (head && collapsed.value.has(head.request_id)) {
      const next = new Set(collapsed.value)
      next.delete(head.request_id)
      collapsed.value = next
    }
    promoted.value = promote(promoted.value, runId)
  }

  /** Move a run's head request out of the dock into its own permission window. */
  function popOut(runId: string) {
    const head = headOf(runId)
    if (!head || poppedOut.value.has(head.request_id)) return
    poppedOut.value = new Set(poppedOut.value).add(head.request_id)
  }

  /** Bring a popped-out request back in-app (its window was closed by the user). */
  function unpop(requestId: string) {
    if (!poppedOut.value.has(requestId)) return
    const next = new Set(poppedOut.value)
    next.delete(requestId)
    poppedOut.value = next
  }

  // Forget collapse / promote / pop-out marks of requests and runs that are gone.
  function prune() {
    const ids = new Set(entries.value.map((e) => e.requestId))
    const runs = new Set(entries.value.map((e) => e.runId))
    if ([...poppedOut.value].some((id) => !ids.has(id))) {
      poppedOut.value = new Set([...poppedOut.value].filter((id) => ids.has(id)))
    }
    if ([...collapsed.value].some((id) => !ids.has(id))) {
      collapsed.value = new Set([...collapsed.value].filter((id) => ids.has(id)))
    }
    if (promoted.value.some((id) => !runs.has(id))) {
      promoted.value = promoted.value.filter((id) => runs.has(id))
    }
  }

  /** Show the run: its pop-out window if it has one, otherwise in the main window. */
  async function openSession(runId: string) {
    if (await focusSessionWindow(runId)) return
    const projectId = projectOf(runId)
    if (!projectId) return
    live.markSeen(runId)
    // Open the project's workspace tab before routing (see App.vue openRunFromTray).
    tabsStore.open(projectId, runId)
    router.push({ name: 'project-run-detail', params: { projectId, runId } }).catch(() => {})
  }

  /**
   * Focus a run's in-app card (or its prompt docked in a RunView, which watches
   * `focusRequest` too), its own window when the user popped it out, or open its
   * session when no stack is showing.
   */
  function focusRun(runId: string) {
    const head = headOf(runId)
    if (head && poppedOut.value.has(head.request_id)) {
      void openPermissionWindow(runId).catch(() => {})
      return
    }
    if (stackHosts.value > 0 && surface.value === 'inline') {
      bringToFront(runId)
      focusRequest.value = { runId, nonce: ++focusNonce }
    } else {
      void openSession(runId)
    }
  }

  return {
    entries,
    stackEntries,
    poppedOut,
    poppedOutRuns,
    dockedRuns,
    surface,
    foreground,
    collapsed,
    promoted,
    focusRequest,
    headOf,
    sessionOf,
    decide,
    answerQuestion,
    collapse,
    bringToFront,
    popOut,
    unpop,
    prune,
    openSession,
    focusRun,
  }
}
