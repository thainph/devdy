<script setup lang="ts">
// Global owner of the per-run permission windows (mounted once, in the main
// window). It watches the shared liveRuns store and, while the main window is NOT
// in front, opens one standalone window per run that has a pending request — the
// run being viewed, a popped-out session, or any run streaming in the background —
// mirrors each run's head request to its window, and forwards the user's choice to
// the shared resolve logic (composables/usePermissionDecisions). Closing a window
// while a request is still pending counts as cancelling (denying) it.
//
// While the main window IS in front, requests are answered in-app instead
// (PermissionInlineStack, or a split RunView's dock): open windows are closed
// programmatically — which never counts as a deny — and come back if the main
// window loses focus again. A request the user popped out of the dock keeps its
// window; closing that one returns the request in-app instead of denying it. With
// `permission_float_when_background` = "false", a backgrounded Devdy opens no
// windows and relies on PermissionNotifier's native notification alone.
//
// It renders only the in-app card stack (nothing while no request is pending or
// the main window is in the background).
import { computed, onBeforeUnmount, watch } from 'vue'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useAppSettingsStore } from '@/stores/appSettings'
import {
  openPermissionWindow,
  closePermissionWindow,
  permissionWindowLabel,
} from '@/lib/permissionWindow'
import { desiredWindowRuns } from '@/lib/permissionSurface'
import PermissionInlineStack from '@/components/PermissionInlineStack.vue'
import {
  refreshForeground,
  trackMainForeground,
  usePermissionDecisions,
} from '@/composables/usePermissionDecisions'

const live = useLiveRunsStore()
const perms = usePermissionDecisions()
const { headOf, sessionOf } = perms
useAppSettingsStore().ensureLoaded().catch(() => {})
const stopTracking = trackMainForeground()

interface Tracked {
  win: WebviewWindow
  unlisteners: UnlistenFn[]
  // True while WE are closing the window (queue emptied / teardown / the main
  // window came to the front), so the destroyed handler can tell an app-driven
  // close apart from a user cancel.
  closingProgrammatically: boolean
}
const tracked = new Map<string, Tracked>()
// Runs whose window WE are closing but whose `tauri://destroyed` hasn't fired yet.
// Their label still resolves to the dying window, so reconcile must not reopen
// (it would adopt the dying window, whose destroyed event then reads as a user
// cancel) — it waits, and the destroyed handler re-runs reconcile instead.
const closing = new Set<string>()

// runId → head request id, for every run with a pending permission request.
const pending = computed(() => {
  const map = new Map<string, string>()
  for (const e of perms.entries.value) map.set(e.runId, e.requestId)
  return map
})

function sync(runId: string) {
  void emit(`permission:sync:${runId}`, {
    request: headOf(runId),
    allowedTools: live.get(runId)?.allowedTools ?? [],
    session: sessionOf(runId),
  })
}

// When windows become the surface because the user just left the main window
// (Cmd-Tab to another app, a click into a popped-out session), the windows that
// open for requests already pending must not grab the keyboard from where the
// user is now typing — Enter in a focused prompt means "allow". A request that
// arrives later still gets a focused window, as before.
const SURFACE_SETTLE_MS = 1500
let surfaceChangedAt = 0

async function openFor(runId: string, indexFromRight: number) {
  const focus = Date.now() - surfaceChangedAt > SURFACE_SETTLE_MS
  const win = await openPermissionWindow(runId, indexFromRight, { focus })
  const entry: Tracked = { win, unlisteners: [], closingProgrammatically: false }
  tracked.set(runId, entry)

  entry.unlisteners.push(
    await listen(`permission:window-ready:${runId}`, () => sync(runId)),
    await listen<{ request_id: string; decision: 'allow' | 'deny'; remember: boolean }>(
      `permission:decide:${runId}`,
      (e) => {
        // A stale decision (not the current head request) is ignored by decide.
        if (!e.payload) return
        void perms.decide(runId, e.payload.request_id, e.payload.decision, e.payload.remember)
          .then((ok) => {
            if (!ok) sync(runId)
          })
      },
    ),
    await listen<{ request_id: string; answers: Record<string, string> }>(
      `permission:answer:${runId}`,
      (e) => {
        if (!e.payload) return
        void perms.answerQuestion(runId, e.payload.request_id, e.payload.answers).then((ok) => {
          if (!ok) sync(runId)
        })
      },
    ),
  )

  // User closing the window while a request is still pending = cancel (deny it).
  // An app-driven close flags itself first, so it's skipped here.
  win.once('tauri://destroyed', () => {
    if (entry.closingProgrammatically) {
      closing.delete(runId)
      // A request may have arrived while this window was tearing down (Claude
      // often asks again right after an allow); give it a fresh window.
      void reconcile()
      return
    }
    // Stale handler of a window no longer tracked — nothing to do.
    if (tracked.get(runId) !== entry) return
    tracked.delete(runId)
    entry.unlisteners.forEach((off) => off())
    const head = headOf(runId)
    // A request popped out of a split RunView's dock goes back in-app when the
    // user closes its window while the main window is in front — not a deny.
    if (head && perms.surface.value === 'inline' && perms.poppedOut.value.has(head.request_id)) {
      perms.unpop(head.request_id)
      return
    }
    if (head) void perms.decide(runId, head.request_id, 'deny', false)
  })

  // The window failed to open: drop the dead entry (otherwise reconcile keeps
  // syncing a window that doesn't exist and the request is never shown) and retry.
  win.once('tauri://error', () => {
    if (tracked.get(runId) !== entry) return
    tracked.delete(runId)
    entry.unlisteners.forEach((off) => off())
    setTimeout(() => void reconcile(), 2000)
  })

  // Push the current state now too (the window's ready ping also triggers a sync).
  sync(runId)
}

function closeFor(runId: string) {
  const entry = tracked.get(runId)
  if (!entry) return
  // Flag so the destroyed handler doesn't mistake this for a user cancel. Untrack
  // right away: a request arriving before the window is gone must get a new
  // window, not a sync to the dying one.
  entry.closingProgrammatically = true
  tracked.delete(runId)
  entry.unlisteners.forEach((off) => off())
  closing.add(runId)
  void closePermissionWindow(runId).finally(() => {
    // Safety net: if the window vanished without a destroyed event reaching us,
    // don't leave the run blocked in `closing` forever.
    setTimeout(async () => {
      if (!closing.has(runId)) return
      const still = await WebviewWindow.getByLabel(permissionWindowLabel(runId)).catch(() => null)
      if (still) return
      closing.delete(runId)
      void reconcile()
    }, 1500)
  })
}

// Reconcile the open windows against the runs that should have one (every pending
// run while windows are the surface; none while the main window is in front):
// open the new, sync the existing, close the rest. Opening is async,
// so loop until stable — a `pending` change that lands mid-open (e.g. a batch of
// conductor workers all asking at once) sets `dirty` and gets picked up here
// rather than dropped.
let reconciling = false
let dirty = false
async function reconcile() {
  if (reconciling) {
    dirty = true
    return
  }
  reconciling = true
  try {
    do {
      dirty = false
      const want = desiredWindowRuns(
        perms.surface.value,
        pending.value.keys(),
        tracked.keys(),
        perms.poppedOutRuns.value,
      )
      for (const runId of [...tracked.keys()]) {
        if (!want.has(runId)) closeFor(runId)
      }
      let index = tracked.size
      for (const runId of want) {
        if (tracked.has(runId)) {
          sync(runId)
        } else if (!closing.has(runId)) {
          // One run failing to open must not stop the others from being shown.
          try {
            await openFor(runId, index)
            index++
          } catch (e) {
            console.error('[PermissionWindowManager] failed to open window', e)
            setTimeout(() => void reconcile(), 2000)
          }
        }
      }
    } while (dirty)
  } finally {
    reconciling = false
  }
}
// A request arriving while the main window is not in front: re-check which window
// is (the user may have moved from a popped-out session to another app since the
// last focus event) before choosing between a window and the notification alone.
watch(pending, () => {
  if (perms.foreground.value === 'main') void reconcile()
  else void refreshForeground().finally(() => void reconcile())
})
// A docked request popped out into its own window (or back in).
watch(perms.poppedOutRuns, () => void reconcile())
// Main window focused / blurred, or the setting changed.
watch(perms.surface, () => {
  surfaceChangedAt = Date.now()
  void reconcile()
})

// Re-sync open windows when a run's name resolves or changes (rename, or runMeta
// arriving after the window opened) so the header never stays on a stale name.
watch(
  () => [...pending.value.keys()].map((runId) => {
    const { name, project } = sessionOf(runId)
    return `${runId}\u0000${name}\u0000${project}`
  }).join('\u0001'),
  () => {
    for (const runId of tracked.keys()) sync(runId)
  },
)

onBeforeUnmount(() => {
  stopTracking()
  for (const [runId, entry] of tracked) {
    entry.closingProgrammatically = true
    entry.unlisteners.forEach((off) => off())
    void closePermissionWindow(runId)
  }
  tracked.clear()
  closing.clear()
})
</script>

<template>
  <PermissionInlineStack />
</template>
