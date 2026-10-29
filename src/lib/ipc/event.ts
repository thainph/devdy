/**
 * `@tauri-apps/api/event` as the app sees it (Vite alias). Identical to the real
 * module, except in a remote-desktop window (see ./remote.ts):
 * - app events are subscribed on the controlled Devdy, AND on the local bus the
 *   remote windows of this machine use among themselves;
 * - `emit` / `emitTo` (window-to-window messages: "open this run", "re-point
 *   the list"…) go on that local bus, so they reach the other remote windows
 *   but never this machine's own windows (./windows.ts).
 * Tauri's own events (`tauri://…`) and window-targeted listens stay local.
 */
import {
  emit as realEmit,
  emitTo as realEmitTo,
  listen as realListen,
  once as realOnce,
  type EventCallback,
  type EventName,
  type EventTarget,
  type Options,
  type UnlistenFn,
} from '@tauri-real/event'
import { remoteIpc } from './remote'
import { busEventName, toRemoteLabel } from './windows'
import { traceBusEvent } from './diagnostics'

export * from '@tauri-real/event'

function isRemoteEvent(event: EventName, options?: Options): boolean {
  return !String(event).startsWith('tauri://') && options?.target === undefined
}

/** Subscribe on the controlled Devdy and on the local remote-window bus. */
async function listenBoth<T>(event: string, handler: EventCallback<T>): Promise<UnlistenFn> {
  const remote = remoteIpc()!
  const [r, b] = await Promise.allSettled([
    remote.listen<T>(event, handler),
    realListen<T>(busEventName(event), (e) => {
      traceBusEvent('recv', event, 'bus')
      handler({ ...e, event })
    }),
  ])
  // One side failed: release the other, or it would leak (nobody gets its unlisten).
  if (r.status === 'rejected' || b.status === 'rejected') {
    if (r.status === 'fulfilled') r.value()
    if (b.status === 'fulfilled') b.value()
    throw r.status === 'rejected' ? r.reason : (b as PromiseRejectedResult).reason
  }
  const offRemote = r.value
  const offBus = b.value
  return () => {
    offRemote()
    offBus()
  }
}

export async function listen<T>(event: EventName, handler: EventCallback<T>, options?: Options): Promise<UnlistenFn> {
  if (remoteIpc() && isRemoteEvent(event, options)) {
    traceBusEvent('listen', String(event))
    return listenBoth<T>(String(event), handler)
  }
  return realListen<T>(event, handler, options)
}

export async function once<T>(event: EventName, handler: EventCallback<T>, options?: Options): Promise<UnlistenFn> {
  if (remoteIpc() && isRemoteEvent(event, options)) {
    let fired = false
    let off: UnlistenFn | null = null
    off = await listenBoth<T>(String(event), (e) => {
      if (fired) return
      fired = true
      off?.()
      handler(e)
    })
    // Fired while still subscribing.
    if (fired) off()
    return off
  }
  return realOnce<T>(event, handler, options)
}

export async function emit<T>(event: string, payload?: T): Promise<void> {
  if (remoteIpc() && !event.startsWith('tauri://')) {
    traceBusEvent('emit', event)
    return realEmit(busEventName(event), payload)
  }
  return realEmit(event, payload)
}

export async function emitTo<T>(target: EventTarget | string, event: string, payload?: T): Promise<void> {
  if (!remoteIpc() || event.startsWith('tauri://')) return realEmitTo(target, event, payload)
  // "main" / "session-…" of a remote window are its remote siblings.
  const mapped =
    typeof target === 'string'
      ? toRemoteLabel(target)
      : 'label' in target
        ? { ...target, label: toRemoteLabel(target.label) }
        : target
  return realEmitTo(mapped, busEventName(event), payload)
}
