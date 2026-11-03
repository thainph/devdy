/**
 * Pure Web Push helpers for the phone controller (`docs/remote-mobile-push-spec.md`):
 * whether this browser can receive pushes, what the notification switch
 * shows, and the small conversions the subscribe flow needs. No DOM access —
 * the caller passes what it read from `navigator` / `window`.
 */

/** Can this browser get pushes here? `needs_install`: iOS / iPadOS Safari,
 * which only delivers Web Push to a web app added to the Home Screen. */
export type PushSupport = 'supported' | 'needs_install' | 'unsupported'

export interface PushEnv {
  hasServiceWorker: boolean
  hasPushManager: boolean
  hasNotification: boolean
  ios: boolean
  /** Running as an installed web app (display-mode standalone). */
  standalone: boolean
}

export function pushSupport(env: PushEnv): PushSupport {
  if (env.ios && !env.standalone) return 'needs_install'
  if (!env.hasServiceWorker || !env.hasPushManager || !env.hasNotification) return 'unsupported'
  return 'supported'
}

/** iPhone / iPod / iPad — including iPadOS, which reports itself as a Mac. */
export function isIosDevice(userAgent: string, platform: string, maxTouchPoints: number): boolean {
  if (/iPhone|iPad|iPod/i.test(userAgent)) return true
  return platform === 'MacIntel' && maxTouchPoints > 1
}

/** What the notification switch shows. */
export type PushUiState = 'unsupported' | 'needs_install' | 'denied' | 'loading' | 'on' | 'off'

export function pushUiState(input: {
  support: PushSupport
  /** `Notification.permission`. */
  permission: 'default' | 'granted' | 'denied'
  /** This browser holds a push subscription made with the Host's key. */
  browserSubscribed: boolean
  /** The Host knows that subscription; null until it answered. */
  hostSubscribed: boolean | null
}): PushUiState {
  if (input.support !== 'supported') return input.support
  if (input.permission === 'denied') return 'denied'
  if (input.hostSubscribed === null) return 'loading'
  return input.permission === 'granted' && input.browserSubscribed && input.hostSubscribed ? 'on' : 'off'
}

/** Re-subscribe on the Host without asking the user: notifications were
 * granted and this browser still holds a subscription for the Host's key, but
 * the Host lost it (kicked session, key replaced, rotated endpoint). The Host
 * refuses this for a device removed on the desktop. */
export function shouldAutoResubscribe(input: {
  permission: 'default' | 'granted' | 'denied'
  /** A local subscription made with the Host's current key. */
  hasLocalSubscription: boolean
  hostSubscribed: boolean | null
  /** Already tried for this endpoint on this page load. */
  alreadyTried: boolean
}): boolean {
  return (
    input.permission === 'granted' &&
    input.hasLocalSubscription &&
    input.hostSubscribed === false &&
    !input.alreadyTried
  )
}

/** base64url (VAPID public key) → bytes for `applicationServerKey`. */
export function base64UrlToBytes(s: string): Uint8Array {
  const b64 = s.replace(/-/g, '+').replace(/_/g, '/')
  const padded = b64 + '='.repeat((4 - (b64.length % 4)) % 4)
  const bin = atob(padded)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** Whether an existing subscription was made with `vapidKey` (a different
 * Host, or a key the Host replaced, means it must be redone). */
export function sameServerKey(subscriptionKey: ArrayBuffer | null | undefined, vapidKey: string): boolean {
  if (!subscriptionKey) return false
  const a = new Uint8Array(subscriptionKey)
  const b = base64UrlToBytes(vapidKey)
  return a.length === b.length && a.every((v, i) => v === b[i])
}

/** The parts of `PushSubscription.toJSON()` the Host needs, or null. */
export function subscriptionWire(json: unknown): { endpoint: string; p256dh: string; auth: string } | null {
  if (!json || typeof json !== 'object') return null
  const o = json as { endpoint?: unknown; keys?: { p256dh?: unknown; auth?: unknown } }
  if (typeof o.endpoint !== 'string' || !o.endpoint) return null
  const p256dh = o.keys?.p256dh
  const auth = o.keys?.auth
  if (typeof p256dh !== 'string' || typeof auth !== 'string' || !p256dh || !auth) return null
  return { endpoint: o.endpoint, p256dh, auth }
}

/** A short device name for the desktop's list, e.g. "iPhone · Safari". */
export function deviceLabel(userAgent: string, ios: boolean): string {
  const device = /iPad/i.test(userAgent) || (ios && !/iPhone|iPod/i.test(userAgent))
    ? 'iPad'
    : /iPhone|iPod/i.test(userAgent)
      ? 'iPhone'
      : /Android/i.test(userAgent)
        ? 'Android'
        : /Windows/i.test(userAgent)
          ? 'Windows'
          : /Mac OS X|Macintosh/i.test(userAgent)
            ? 'Mac'
            : /Linux/i.test(userAgent)
              ? 'Linux'
              : 'Browser'
  const browser = /EdgA?\//.test(userAgent)
    ? 'Edge'
    : /Firefox|FxiOS/.test(userAgent)
      ? 'Firefox'
      : /SamsungBrowser/.test(userAgent)
        ? 'Samsung Internet'
        : /Chrome|CriOS/.test(userAgent)
          ? 'Chrome'
          : /Safari/.test(userAgent)
            ? 'Safari'
            : ''
  return browser ? `${device} · ${browser}` : device
}

/** A tapped Focus reminder: open the Todo tab, and `todoId` in the editor
 * (null = just the tab, e.g. the morning summary). */
export interface FocusOpen {
  todoId: string | null
}

const FOCUS_OPEN = 'open'
const FOCUS_TODO = 'todo'

function cleanTodoId(v: unknown): string | null {
  return typeof v === 'string' && v.trim() && v.length <= 128 ? v.trim() : null
}

/** The controller URL the service worker opens for a reminder when no window
 * is open (`focusUrl` in public/controller-sw.js builds the same). */
export function focusOpenUrl(todoId: string | null, base = '/controller.html'): string {
  return todoId ? `${base}?${FOCUS_OPEN}=todo&${FOCUS_TODO}=${encodeURIComponent(todoId)}` : `${base}?${FOCUS_OPEN}=todo`
}

/** A reminder tap from the page URL's query (`location.search`), or null. */
export function focusOpenFromSearch(search: string): FocusOpen | null {
  const q = new URLSearchParams(search)
  if (q.get(FOCUS_OPEN) !== 'todo') return null
  return { todoId: cleanTodoId(q.get(FOCUS_TODO)) }
}

/** A reminder tap from a service worker message (`devdy:open-todo`), or null. */
export function focusOpenFromMessage(msg: unknown): FocusOpen | null {
  if (!msg || typeof msg !== 'object' || (msg as { type?: unknown }).type !== 'devdy:open-todo') return null
  return { todoId: cleanTodoId((msg as { todo_id?: unknown }).todo_id) }
}

/** `href` without the reminder-tap query, so a reload does not reopen it. */
export function withoutFocusOpen(href: string): string {
  const url = new URL(href)
  url.searchParams.delete(FOCUS_OPEN)
  url.searchParams.delete(FOCUS_TODO)
  return url.toString()
}
