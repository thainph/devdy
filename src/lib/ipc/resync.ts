/**
 * Remote desktop resync: the controlled Devdy dropped events for us on a
 * congested link (`ipc_resync`), so stores and queries fed by those events may
 * be stale. The app has no generic "refetch everything" path — each store
 * loads once and then follows events — so a remote window recovers the simple,
 * robust way: it reloads, which re-runs every load against the controlled
 * Devdy. Reloads are throttled per window (a link that keeps shedding must not
 * reload-loop); a resync inside the window is deferred, never dropped.
 *
 * The main remote-desktop window turns `ipc_resync` into {@link RESYNC_EVENT}
 * on this machine's bus, so its pop-outs (which share its connection) reload
 * too.
 */
import { emit, listen, type UnlistenFn } from '@tauri-real/event'
import { RESYNC_EVENT, resyncDelay } from './resyncCore'

const STORAGE_KEY = 'devdy.remoteDesktop.lastResyncReload'

/** Ask every remote window of this machine to resync. */
export function broadcastResync(): void {
  void emit(RESYNC_EVENT).catch(() => {})
}

/** Reload this window (throttled) whenever a resync is broadcast. */
export function installResyncReload(): Promise<UnlistenFn> {
  let scheduled = false
  return listen(RESYNC_EVENT, () => {
    if (scheduled) return
    scheduled = true
    const stored = Number(sessionStorage.getItem(STORAGE_KEY))
    const delay = resyncDelay(Date.now(), stored > 0 ? stored : null)
    setTimeout(() => {
      sessionStorage.setItem(STORAGE_KEY, String(Date.now()))
      location.reload()
    }, delay)
  })
}
