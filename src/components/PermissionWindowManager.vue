<script setup lang="ts">
// Global owner of the per-run permission windows (mounted once, in the main
// window). It is the SINGLE writer for permission decisions: it watches the shared
// liveRuns store, opens one standalone window per run that has a pending request —
// the run being viewed, a popped-out session, or any run streaming in the
// background — mirrors each run's head request to its window, and runs the real
// resolve logic when a window forwards the user's choice. Closing a window while a
// request is still pending counts as cancelling (denying) it.
//
// Headless (like PermissionNotifier): it renders nothing.
import { computed, onBeforeUnmount, watch } from 'vue'
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event'
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { useLiveRunsStore } from '@/stores/liveRuns'
import {
  openPermissionWindow,
  closePermissionWindow,
  permissionWindowLabel,
} from '@/lib/permissionWindow'
import { runDisplayName } from '@/lib/tray'

const live = useLiveRunsStore()

interface Tracked {
  win: WebviewWindow
  unlisteners: UnlistenFn[]
  // True while WE are closing the window (queue emptied / teardown), so the
  // destroyed handler can tell an app-driven close apart from a user cancel.
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
  live.sessions.forEach((s, runId) => {
    const head = s.permissionQueue[0]
    if (head) map.set(runId, head.request_id)
  })
  return map
})

function headOf(runId: string) {
  return live.get(runId)?.permissionQueue[0] ?? null
}

// Which session / project a run belongs to, so its window can say who is asking.
function sessionOf(runId: string) {
  const s = live.get(runId)
  return runDisplayName(runId, s?.projectId ?? '')
}

function sync(runId: string) {
  void emit(`permission:sync:${runId}`, {
    request: headOf(runId),
    allowedTools: live.get(runId)?.allowedTools ?? [],
    session: sessionOf(runId),
  })
}

// The request leaves the queue (and the window moves on) only once the answer
// reached the backend; on failure it stays as the head so the user can retry.
function decide(runId: string, decision: 'allow' | 'deny' | 'ask', remember: boolean) {
  const req = headOf(runId)
  if (!req) return
  if (remember && decision === 'allow') live.rememberAllowedTool(runId, req.tool_name)
  else if (remember && decision === 'deny') live.rememberDeniedTool(runId, req.tool_name)
  void live.respondPermission(runId, req, decision, { remember }).then((ok) => {
    if (!ok) sync(runId)
  })
}

function answerQuestion(runId: string, answers: Record<string, string>) {
  const req = headOf(runId)
  if (!req) return
  void live.respondPermission(runId, req, 'allow', { answers }).then((ok) => {
    if (!ok) sync(runId)
  })
}

async function openFor(runId: string, indexFromRight: number) {
  const win = await openPermissionWindow(runId, indexFromRight)
  const entry: Tracked = { win, unlisteners: [], closingProgrammatically: false }
  tracked.set(runId, entry)

  entry.unlisteners.push(
    await listen(`permission:window-ready:${runId}`, () => sync(runId)),
    await listen<{ request_id: string; decision: 'allow' | 'deny'; remember: boolean }>(
      `permission:decide:${runId}`,
      (e) => {
        // Ignore stale decisions that don't match the current head request.
        if (e.payload?.request_id !== headOf(runId)?.request_id) return
        decide(runId, e.payload.decision, e.payload.remember)
      },
    ),
    await listen<{ request_id: string; answers: Record<string, string> }>(
      `permission:answer:${runId}`,
      (e) => {
        if (e.payload?.request_id !== headOf(runId)?.request_id) return
        answerQuestion(runId, e.payload.answers)
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
    if (headOf(runId)) decide(runId, 'deny', false)
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

// Reconcile the open windows against the runs that currently have a pending
// request: open the new, sync the existing, close the resolved. Opening is async,
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
      const map = pending.value
      for (const runId of [...tracked.keys()]) {
        if (!map.has(runId)) closeFor(runId)
      }
      let index = tracked.size
      for (const runId of map.keys()) {
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
watch(pending, () => void reconcile(), { deep: false })

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
  <span hidden />
</template>
