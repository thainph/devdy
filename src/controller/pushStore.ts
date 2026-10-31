/**
 * Web Push on the phone controller ({@link FEATURE_PUSH}): the service worker
 * registration, this browser's push subscription, and keeping the Host's copy
 * in step with it. Pure decisions live in `@/lib/remotePush`.
 *
 * The Host sends pushes itself (VAPID + aes128gcm); this side only subscribes
 * with the Host's public key and hands it the endpoint + keys.
 */
import { reactive } from 'vue'
import {
  deviceLabel,
  isIosDevice,
  pushSupport,
  pushUiState,
  base64UrlToBytes,
  sameServerKey,
  shouldAutoResubscribe,
  subscriptionWire,
  type PushSupport,
  type PushUiState,
} from '@/lib/remotePush'
import type { SendCommand } from './dataStore'
import {
  pushConfigCmd,
  pushSubscribeCmd,
  pushTestCmd,
  pushUnsubscribeCmd,
  pushVisibilityCmd,
  type PushEventsWire,
  type StreamPayload,
} from './protocol.ts'

const SW_URL = '/controller-sw.js'

/** While on screen, re-assert visibility this often: the Host treats "on
 * screen" as stale after 60s of silence (iOS may freeze the page before it
 * can say it left). */
const VISIBLE_HEARTBEAT_MS = 30_000

/** Register the (non-caching) service worker at the site root. Never inside
 * the desktop app's webview. */
export function registerControllerServiceWorker(): void {
  if (typeof navigator === 'undefined' || !('serviceWorker' in navigator)) return
  if ('__TAURI_INTERNALS__' in window) return
  navigator.serviceWorker.register(SW_URL, { scope: '/', updateViaCache: 'none' }).catch(() => {
    // No worker: the app still works, it just cannot receive pushes.
  })
}

function readEnv(): { support: PushSupport; ios: boolean } {
  const ios = isIosDevice(navigator.userAgent, navigator.platform, navigator.maxTouchPoints ?? 0)
  const standalone =
    window.matchMedia?.('(display-mode: standalone)').matches === true ||
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  return {
    ios,
    support: pushSupport({
      hasServiceWorker: 'serviceWorker' in navigator,
      hasPushManager: 'PushManager' in window,
      hasNotification: 'Notification' in window,
      ios,
      standalone,
    }),
  }
}

function permissionNow(): NotificationPermission {
  return typeof Notification === 'undefined' ? 'default' : Notification.permission
}

async function registration(): Promise<ServiceWorkerRegistration> {
  const existing = await navigator.serviceWorker.getRegistration('/')
  if (existing) return existing
  await navigator.serviceWorker.register(SW_URL, { scope: '/', updateViaCache: 'none' })
  return navigator.serviceWorker.ready
}

export type PushError = 'not_ready' | 'permission' | 'subscribe' | 'host' | 'test'

export interface PushState {
  support: PushSupport
  permission: NotificationPermission
  /** The Host's VAPID public key; null until `push_config` arrives. */
  vapidKey: string | null
  /** This browser's subscription endpoint (made with the Host's key). */
  endpoint: string | null
  /** Whether the Host has that endpoint; null until it answered. */
  hostSubscribed: boolean | null
  events: PushEventsWire | null
  busy: boolean
  error: PushError | null
  /** Outcome of the last test send. */
  test: 'sent' | 'failed' | null
}

export function createPushStore(send: SendCommand) {
  const env = typeof window === 'undefined' ? { support: 'unsupported' as PushSupport, ios: false } : readEnv()
  const state = reactive<PushState>({
    support: env.support,
    permission: typeof window === 'undefined' ? 'default' : permissionNow(),
    vapidKey: null,
    endpoint: null,
    hostSubscribed: null,
    events: null,
    busy: false,
    error: null,
    test: null,
  })

  function ui(): PushUiState {
    return pushUiState({
      support: state.support,
      permission: state.permission,
      browserSubscribed: !!state.endpoint,
      hostSubscribed: state.hostSubscribed,
    })
  }

  /** This browser's subscription, if it was made with the Host's key. */
  async function currentSubscription(): Promise<PushSubscription | null> {
    if (state.support !== 'supported') return null
    try {
      const reg = await navigator.serviceWorker.getRegistration('/')
      const sub = (await reg?.pushManager.getSubscription()) ?? null
      if (sub && state.vapidKey && !sameServerKey(sub.options.applicationServerKey, state.vapidKey)) return null
      return sub
    } catch {
      return null
    }
  }

  /** Endpoints already re-subscribed automatically on this page load. */
  const autoTried = new Set<string>()
  let heartbeat: ReturnType<typeof setInterval> | null = null

  /** Ask the Host for its key and whether it knows this browser. */
  async function refresh(): Promise<void> {
    if (!heartbeat) {
      heartbeat = setInterval(() => {
        if (document.visibilityState === 'visible') send(pushVisibilityCmd(true))
      }, VISIBLE_HEARTBEAT_MS)
    }
    state.permission = permissionNow()
    const sub = await currentSubscription()
    state.endpoint = sub?.endpoint ?? null
    send(pushConfigCmd(state.endpoint))
  }

  function apply(p: StreamPayload): void {
    if (p.kind !== 'push_config') return
    state.vapidKey = p.vapid_public_key
    state.hostSubscribed = p.subscribed
    state.events = p.events
    void reconcile()
  }

  /** Now that the Host's key is known: a subscription this browser made for
   * another key (another Host, or one the Host replaced) does not count. When
   * that changes which endpoint we hold, ask again about the right one. */
  async function reconcile(): Promise<void> {
    const sub = await currentSubscription()
    const endpoint = sub?.endpoint ?? null
    if (endpoint !== state.endpoint) {
      state.endpoint = endpoint
      send(pushConfigCmd(endpoint))
      return
    }
    state.permission = permissionNow()
    if (
      !sub ||
      state.busy ||
      !shouldAutoResubscribe({
        permission: state.permission,
        hasLocalSubscription: true,
        hostSubscribed: state.hostSubscribed,
        alreadyTried: autoTried.has(sub.endpoint),
      })
    ) {
      return
    }
    autoTried.add(sub.endpoint)
    const wire = subscriptionWire(sub.toJSON())
    // Answered with a fresh push_config when accepted; a refusal (removed on
    // the desktop) just leaves the switch off.
    if (wire) send(pushSubscribeCmd({ ...wire, label: deviceLabel(navigator.userAgent, env.ios) }, true))
  }

  function ack(resolve: (ok: boolean) => void) {
    return (accepted: boolean) => resolve(accepted)
  }

  /** Turn notifications on. Call straight from the tap: iOS only shows the
   * permission prompt for a user gesture, so it is the first thing done. */
  async function enable(): Promise<void> {
    if (state.busy || state.support !== 'supported') return
    state.error = null
    state.test = null
    const permission = typeof Notification === 'undefined' ? 'denied' : await Notification.requestPermission()
    state.permission = permission
    if (permission !== 'granted') {
      state.error = permission === 'denied' ? null : 'permission'
      return
    }
    if (!state.vapidKey) {
      state.error = 'not_ready'
      return
    }
    state.busy = true
    try {
      const reg = await registration()
      let sub = await reg.pushManager.getSubscription()
      if (sub && !sameServerKey(sub.options.applicationServerKey, state.vapidKey)) {
        // Made for another Host key: tell the Host to forget it first, or its
        // row lingers and every send to it fails verification.
        send(pushUnsubscribeCmd(sub.endpoint))
        await sub.unsubscribe().catch(() => false)
        sub = null
      }
      if (!sub) {
        sub = await reg.pushManager.subscribe({
          userVisibleOnly: true,
          applicationServerKey: base64UrlToBytes(state.vapidKey),
        })
      }
      const wire = subscriptionWire(sub.toJSON())
      if (!wire) throw new Error('subscription without keys')
      state.endpoint = wire.endpoint
      const ok = await new Promise<boolean>((resolve) => {
        if (!send(pushSubscribeCmd({ ...wire, label: deviceLabel(navigator.userAgent, env.ios) }), ack(resolve))) {
          resolve(false)
        }
      })
      if (!ok) state.error = 'host'
    } catch {
      state.error = 'subscribe'
    } finally {
      state.busy = false
    }
  }

  /** Turn notifications off: forget it on the Host, then in the browser. */
  async function disable(): Promise<void> {
    if (state.busy) return
    state.error = null
    state.test = null
    state.busy = true
    try {
      const sub = await currentSubscription()
      const endpoint = sub?.endpoint ?? state.endpoint
      if (endpoint) {
        const ok = await new Promise<boolean>((resolve) => {
          if (!send(pushUnsubscribeCmd(endpoint), ack(resolve))) resolve(false)
        })
        if (!ok) {
          state.error = 'host'
          return
        }
      }
      await sub?.unsubscribe().catch(() => false)
      state.endpoint = null
      state.hostSubscribed = false
    } finally {
      state.busy = false
    }
  }

  /** Ask the Host to send this device a test notification. */
  function sendTest(): void {
    if (!state.endpoint || state.busy) return
    state.test = null
    state.busy = true
    const done = (accepted: boolean) => {
      state.busy = false
      state.test = accepted ? 'sent' : 'failed'
    }
    if (!send(pushTestCmd(state.endpoint), done)) done(false)
  }

  /** Tell the Host whether the app is on screen (it stays quiet while it is). */
  function setVisible(visible: boolean): void {
    send(pushVisibilityCmd(visible))
    if (visible) {
      const nav = navigator as Navigator & { clearAppBadge?: () => Promise<void> }
      nav.clearAppBadge?.().catch(() => {})
    }
  }

  return { state, ui, refresh, apply, enable, disable, sendTest, setVisible }
}

export type PushStore = ReturnType<typeof createPushStore>
