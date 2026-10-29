/**
 * Boot of the remote-desktop window (`index.html?remote=1`, see
 * lib/remoteDesktopWindow.ts). Without a stored sign-in it shows the password
 * login; with one it connects the IPC shim to the controlled Devdy FIRST (any
 * invoke/listen made before that would hit this machine's backend), then mounts
 * the normal app plus the "Controlling …" banner.
 */
import { createApp, type App } from 'vue'
import { i18n } from './i18n'
import { startRemoteIpc } from './lib/ipc/remote'
import { loadRemoteDesktopSession, saveRemoteDesktopSession } from './lib/remoteDesktopSession'
import RemoteDesktopLogin from './components/remote/RemoteDesktopLogin.vue'
import RemoteDesktopBanner from './components/remote/RemoteDesktopBanner.vue'

export function bootRemoteDesktop(mountApp: () => App): void {
  const session = loadRemoteDesktopSession()
  if (!session) {
    createApp(RemoteDesktopLogin).use(i18n).mount('#app')
    return
  }

  // Calls made from here on are queued until the session authenticates.
  const client = startRemoteIpc(session.link, session.token, (token) => {
    saveRemoteDesktopSession({ ...session, token })
  })
  mountApp()

  const host = document.createElement('div')
  host.id = 'remote-desktop-banner'
  document.body.appendChild(host)
  createApp(RemoteDesktopBanner, { client, device: session.device }).use(i18n).mount(host)
}
