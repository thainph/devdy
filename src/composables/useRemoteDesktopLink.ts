/**
 * The controlling side's link to the controlled Devdy, for the sidebar dot
 * (components/remote/RemoteDesktopIndicator.vue) and the problem pill
 * (components/remote/RemoteDesktopAlert.vue).
 *
 * Only the main remote-desktop window owns the connection; everywhere else
 * (local windows, remote pop-outs) `enabled` is false and nothing shows.
 *
 * A rejected token surfaces as `auth === 'failed'` (the host signed us out);
 * "Sign in again" is also offered after STUCK_MS without a connection, as a
 * fallback. When another device takes the session over, the client stops
 * instead of fighting it and only a deliberate "Reconnect" takes it back.
 */
import { computed, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { isRemoteMode, remoteIpc } from '@/lib/ipc/remote'
import { REMOTE_OWNER_LABEL } from '@/lib/ipc/windows'
import { clearRemoteDesktopSession, loadRemoteDesktopSession } from '@/lib/remoteDesktopSession'
import { openFileTransferWindow } from '@/lib/fileTransferWindow'
import { DESKTOP_LINK_DOT_CLASS, desktopLinkTone } from '@/lib/remoteIndicator'

const STUCK_MS = 60_000

export function useRemoteDesktopLink() {
  const { t } = useI18n()
  const client = isRemoteMode() && getCurrentWindow().label === REMOTE_OWNER_LABEL ? remoteIpc() : null
  const enabled = client !== null
  const device = loadRemoteDesktopSession()?.device ?? ''

  const status = computed(() => client?.state.status.value ?? 'idle')
  const auth = computed(() => client?.state.auth.value ?? null)
  const superseded = computed(() => client?.state.superseded.value ?? false)
  const tone = computed(() =>
    desktopLinkTone({ status: status.value, auth: auth.value, superseded: superseded.value }),
  )
  const connected = computed(() => tone.value === 'connected')

  // Continuously not connected for STUCK_MS ⇒ probably a dead token.
  const stuck = ref(false)
  let stuckTimer: ReturnType<typeof setTimeout> | null = null
  watch(
    connected,
    (ok) => {
      if (stuckTimer) clearTimeout(stuckTimer)
      stuckTimer = null
      stuck.value = false
      if (!ok && enabled) stuckTimer = setTimeout(() => (stuck.value = true), STUCK_MS)
    },
    { immediate: true },
  )
  onUnmounted(() => {
    if (stuckTimer) clearTimeout(stuckTimer)
  })

  const label = computed(() => device || t('remote.desktop.banner.unnamed'))

  const stateText = computed(() => {
    if (superseded.value) return t('remote.desktop.banner.superseded')
    if (auth.value === 'failed') return t('remote.desktop.banner.authFailed')
    if (connected.value) return t('remote.desktop.banner.connected')
    switch (status.value) {
      case 'error':
        return t('remote.desktop.banner.error')
      case 'disconnected':
        return t('remote.desktop.banner.disconnected')
      case 'reconnecting':
        return t('remote.desktop.banner.reconnecting')
      default:
        return t('remote.desktop.banner.connecting')
    }
  })

  const dotClass = computed(() => DESKTOP_LINK_DOT_CLASS[tone.value])

  const canReconnect = computed(() => status.value === 'error' || status.value === 'disconnected')
  // A takeover is not a sign-in problem: the token still works.
  const needsSignIn = computed(() => tone.value === 'failed' || (stuck.value && !superseded.value))

  function reconnect(): void {
    // A fresh boot re-runs startRemoteIpc with the stored token (after a
    // takeover, this takes the session back from the other device).
    location.reload()
  }

  function openFiles(): void {
    void openFileTransferWindow().catch((e) => console.error('[remoteDesktop] file transfer window failed', e))
  }

  function signOut(): void {
    client?.close()
    clearRemoteDesktopSession()
    location.reload()
  }

  return {
    enabled,
    tone,
    connected,
    label,
    stateText,
    dotClass,
    canReconnect,
    needsSignIn,
    reconnect,
    openFiles,
    signOut,
  }
}
