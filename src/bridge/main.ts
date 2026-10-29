/**
 * Remote desktop bridge (hidden window on the controlled Devdy).
 *
 * Another Devdy drives this app's UI over the relay; its `invoke` / `listen`
 * calls arrive here and are replayed with the REAL Tauri APIs, so they behave
 * exactly as if the local user had made them. Results and events go back via
 * `remote_bridge_reply` / `remote_bridge_event` (see `src-tauri/src/remote/ipc.rs`).
 */
// Always the real APIs: this page serves remote calls, it never makes them.
import { invoke } from '@tauri-real/core'
import { listen, type UnlistenFn } from '@tauri-real/event'

type Request =
  | { type: 'invoke'; id: string; cmd: string; args: Record<string, unknown> | null }
  | { type: 'listen'; event: string }
  | { type: 'unlisten'; event: string }
  | { type: 'reset' }

const subscriptions = new Map<string, Promise<UnlistenFn>>()

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
    await invoke('remote_bridge_reply', { id, ok: true, value: value ?? null, error: null })
  } catch (e) {
    await invoke('remote_bridge_reply', { id, ok: false, value: null, error: toWireError(e) }).catch(() => {})
  }
}

function subscribe(event: string): void {
  if (subscriptions.has(event)) return
  subscriptions.set(
    event,
    listen(event, (e) => {
      void invoke('remote_bridge_event', { event, payload: e.payload ?? null }).catch(() => {})
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
