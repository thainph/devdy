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
import type { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { useLiveRunsStore } from '@/stores/liveRuns'
import { useRunsStore } from '@/stores/runs'
import { openPermissionWindow, closePermissionWindow } from '@/lib/permissionWindow'
import { runDisplayName } from '@/lib/tray'

const live = useLiveRunsStore()
const runsStore = useRunsStore()

interface Tracked {
  win: WebviewWindow
  unlisteners: UnlistenFn[]
  // True while WE are closing the window (queue emptied / teardown), so the
  // destroyed handler can tell an app-driven close apart from a user cancel.
  closingProgrammatically: boolean
}
const tracked = new Map<string, Tracked>()

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

function decide(runId: string, decision: 'allow' | 'deny' | 'ask', remember: boolean) {
  const req = headOf(runId)
  if (!req) return
  live.shiftPermission(runId)
  if (remember && decision === 'allow') live.rememberAllowedTool(runId, req.tool_name)
  else if (remember && decision === 'deny') live.rememberDeniedTool(runId, req.tool_name)
  runsStore
    .respondPermission(req.run_id, req.request_id, decision, undefined, { remember })
    .catch((e) => live.appendOutput(runId, `permission response failed: ${String(e)}`))
}

function answerQuestion(runId: string, answers: Record<string, string>) {
  const req = headOf(runId)
  if (!req) return
  live.shiftPermission(runId)
  runsStore
    .respondPermission(req.run_id, req.request_id, 'allow', undefined, { answers })
    .catch((e) => live.appendOutput(runId, `permission response failed: ${String(e)}`))
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
    const current = tracked.get(runId)
    tracked.delete(runId)
    current?.unlisteners.forEach((off) => off())
    if (current?.closingProgrammatically) return
    if (headOf(runId)) decide(runId, 'deny', false)
  })

  // Push the current state now too (the window's ready ping also triggers a sync).
  sync(runId)
}

function closeFor(runId: string) {
  const entry = tracked.get(runId)
  if (!entry) return
  // Flag so the destroyed handler doesn't mistake this for a user cancel; it also
  // clears the map entry + listeners.
  entry.closingProgrammatically = true
  void closePermissionWindow(runId)
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
        } else {
          await openFor(runId, index)
          index++
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
})
</script>

<template>
  <span hidden />
</template>
