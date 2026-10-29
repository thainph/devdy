/**
 * Boot of a remote-desktop window (`?remote=1`).
 *
 * The main one (`remote-desktop`, see lib/remoteDesktopWindow.ts): without a
 * stored sign-in it shows the password login; with one it connects the IPC shim
 * to the controlled Devdy FIRST (any invoke/listen made before that would hit
 * this machine's backend), then mounts the normal app plus the "Controlling …"
 * banner, and serves that one connection to its pop-outs (lib/ipc/mux.ts).
 *
 * A pop-out opened from a remote window (`remote-…` label, lib/ipc/windows.ts)
 * has no connection of its own: its shim talks to the controlled Devdy through
 * the main remote-desktop window.
 */
import { createApp, type App } from 'vue'
import { getAllWebviewWindows } from '@tauri-apps/api/webviewWindow'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { i18n } from './i18n'
import { installRemoteIpc, startRemoteIpc } from './lib/ipc/remote'
import { startMuxClient, startMuxHost } from './lib/ipc/muxTransport'
import { broadcastResync, installResyncReload } from './lib/ipc/resync'
import { REMOTE_OWNER_LABEL, isRemoteLabel } from './lib/ipc/windows'
import { loadRemoteDesktopSession, saveRemoteDesktopSession } from './lib/remoteDesktopSession'
import RemoteDesktopLogin from './components/remote/RemoteDesktopLogin.vue'
import RemoteDesktopBanner from './components/remote/RemoteDesktopBanner.vue'

/** Signed out: pop-outs of the previous sign-in have nothing to talk to. */
async function closeRemotePopouts(): Promise<void> {
  const wins = await getAllWebviewWindows().catch(() => [])
  for (const w of wins) {
    if (isRemoteLabel(w.label) && w.label !== REMOTE_OWNER_LABEL) await w.close().catch(() => {})
  }
}

export function bootRemoteDesktop(mountApp: () => App): void {
  const label = getCurrentWindow().label
  if (label !== REMOTE_OWNER_LABEL) {
    // Calls are queued until the main remote-desktop window answers.
    installRemoteIpc(startMuxClient(label))
    void installResyncReload().catch(() => {})
    mountApp()
    return
  }

  const session = loadRemoteDesktopSession()
  if (!session) {
    void closeRemotePopouts()
    createApp(RemoteDesktopLogin).use(i18n).mount('#app')
    return
  }

  // Calls made from here on are queued until the session authenticates.
  const client = startRemoteIpc(
    session.link,
    session.token,
    (token) => saveRemoteDesktopSession({ ...session, token }),
    broadcastResync,
  )
  void installResyncReload().catch(() => {})
  void startMuxHost(client).catch((e) => console.error('[remoteDesktop] pop-out link failed', e))
  mountApp()

  const host = document.createElement('div')
  host.id = 'remote-desktop-banner'
  document.body.appendChild(host)
  createApp(RemoteDesktopBanner, { client, device: session.device }).use(i18n).mount(host)
}
