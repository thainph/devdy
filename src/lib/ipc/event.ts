/**
 * `@tauri-apps/api/event` as the app sees it (Vite alias). Identical to the real
 * module, except app events are subscribed on the controlled Devdy when this
 * window is a remote-desktop window (see ./remote.ts). Tauri's own events
 * (`tauri://…`) and window-targeted listens stay local.
 */
import {
  listen as realListen,
  once as realOnce,
  type EventCallback,
  type EventName,
  type Options,
  type UnlistenFn,
} from '@tauri-real/event'
import { remoteIpc } from './remote'

export * from '@tauri-real/event'

function isRemoteEvent(event: EventName, options?: Options): boolean {
  return !String(event).startsWith('tauri://') && options?.target === undefined
}

export async function listen<T>(event: EventName, handler: EventCallback<T>, options?: Options): Promise<UnlistenFn> {
  const remote = remoteIpc()
  if (remote && isRemoteEvent(event, options)) return remote.listen<T>(String(event), handler)
  return realListen<T>(event, handler, options)
}

export async function once<T>(event: EventName, handler: EventCallback<T>, options?: Options): Promise<UnlistenFn> {
  const remote = remoteIpc()
  if (remote && isRemoteEvent(event, options)) {
    const off = await remote.listen<T>(String(event), (e) => {
      off()
      handler(e)
    })
    return off
  }
  return realOnce<T>(event, handler, options)
}
