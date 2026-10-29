/**
 * Remote desktop bridge (hidden window on the controlled Devdy).
 *
 * Another Devdy drives this app's UI over the relay; its `invoke` / `listen`
 * calls arrive here and are replayed with the REAL Tauri APIs, so they behave
 * exactly as if the local user had made them. Results and events go back, in
 * the order they happened, through one serialized `remote_bridge_batch` queue
 * (see lib/ipc/bridgeOutbox.ts and `src-tauri/src/remote/ipc.rs`).
 */
// Always the real APIs: this page serves remote calls, it never makes them.
import { invoke } from '@tauri-real/core'
import { listen, type UnlistenFn } from '@tauri-real/event'
import { BridgeOutbox } from '@/lib/ipc/bridgeOutbox'
import { isBridgeDeniedEvent } from '@/lib/ipc/bridgeEvents'

type Request =
  | { type: 'invoke'; id: string; cmd: string; args: Record<string, unknown> | null }
  | { type: 'listen'; event: string }
  | { type: 'unlisten'; event: string }
  | { type: 'reset' }

const BRIDGE_LABEL = 'remote-bridge'

const subscriptions = new Map<string, Promise<UnlistenFn>>()

const outbox = new BridgeOutbox({
  sendBatch: (items) => invoke('remote_bridge_batch', { items }),
  onError: (e) => console.error('[remote-bridge] batch failed', e),
})

/** Errors cross the wire as JSON; keep strings as-is, describe the rest. */
function toWireError(e: unknown): unknown {
  if (e instanceof Error) return e.message
  try {
    JSON.stringify(e)
    return e
  } catch {
    return String(e)
  }
}

async function runInvoke(id: string, cmd: string, args: Record<string, unknown> | null): Promise<void> {
  try {
    const value = await invoke(cmd, args ?? undefined)
    outbox.push({ kind: 'result', id, ok: true, value: value ?? null, error: null })
  } catch (e) {
    outbox.push({ kind: 'result', id, ok: false, value: null, error: toWireError(e) })
  }
}

function subscribe(event: string): void {
  if (subscriptions.has(event) || isBridgeDeniedEvent(event)) return
  subscriptions.set(
    event,
    // Targeted at this webview: app-wide emits (`app.emit`, JS `emit`) still
    // match any listener, but `emit_to("main", …)` — menu actions, tray
    // clicks, window-to-window messages meant for THIS machine's windows —
    // only reaches listeners targeted at that label, so it stays here.
    listen(event, (e) => outbox.push({ kind: 'event', event, payload: e.payload ?? null }), {
      target: { kind: 'AnyLabel', label: BRIDGE_LABEL },
    }),
  )
}

function unsubscribe(event: string): void {
  const sub = subscriptions.get(event)
  if (!sub) return
  subscriptions.delete(event)
  void sub.then((off) => off())
}

function handle(req: Request): void {
  switch (req.type) {
    case 'invoke':
      void runInvoke(req.id, req.cmd, req.args)
      break
    case 'listen':
      subscribe(req.event)
      break
    case 'unlisten':
      unsubscribe(req.event)
      break
    case 'reset':
      for (const event of [...subscriptions.keys()]) unsubscribe(event)
      break
  }
}

async function start(): Promise<void> {
  // Our own event comes in through the REAL listen (this page is never remote).
  await listen<Request>('remote-bridge://request', (e) => handle(e.payload))
  const hello = await invoke<{ requests: Request[]; events: string[] }>('remote_bridge_ready')
  for (const event of hello.events) subscribe(event)
  for (const req of hello.requests) handle(req)
}

void start()
