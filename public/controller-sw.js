/* Devdy Remote — service worker for the phone controller (controller.html).
 *
 * Served from the relay web root (scope "/") and registered only by
 * src/controller/main.ts; the desktop app never registers it.
 *
 * It deliberately caches NOTHING and has no fetch handler: every request goes
 * to the network as if there were no worker, so a redeploy of the relay is
 * picked up on the next load. Its only jobs are Web Push:
 *   - `push`: the Host's payload carries counts only ({pending, done, failed});
 *     the text is composed here in the phone's language, so nothing about a
 *     run (tool, command, project, title) ever leaves the Host.
 *   - `notificationclick`: focus the open controller (or open one) on the Inbox.
 *   - `pushsubscriptionchange`: re-subscribe with the same Host key and tell any
 *     open controller, which hands the new endpoint to the Host (the worker
 *     cannot reach the Host itself: that channel is end-to-end encrypted).
 */

const TAG = 'devdy'
const APP_URL = '/controller.html'

const TEXT = {
  en: {
    title: 'Devdy',
    pending: (n) => (n === 1 ? '1 request waiting for you' : `${n} requests waiting for you`),
    done: (n) => (n === 1 ? '1 run finished' : `${n} runs finished`),
    failed: (n) => (n === 1 ? '1 run failed' : `${n} runs failed`),
    test: 'Test notification — notifications are working.',
    generic: 'Something needs your attention.',
  },
  vi: {
    title: 'Devdy',
    pending: (n) => `${n} việc cần duyệt`,
    done: (n) => `${n} phiên đã xong`,
    failed: (n) => `${n} phiên bị lỗi`,
    test: 'Thông báo thử — thông báo đã hoạt động.',
    generic: 'Có việc cần bạn xem.',
  },
}

function pickLang(language) {
  return typeof language === 'string' && language.toLowerCase().startsWith('vi') ? 'vi' : 'en'
}

function count(v) {
  return typeof v === 'number' && Number.isFinite(v) && v > 0 ? Math.floor(v) : 0
}

/** The notification for a Host payload: `{ title, body, badge }`. Exposed on
 * `self` so the unit test can call it without a browser. */
function composeNotification(data, language) {
  const t = TEXT[pickLang(language)]
  if (data && data.t === 'test') return { title: t.title, body: t.test, badge: 0 }
  const pending = count(data && data.pending)
  const done = count(data && data.done)
  const failed = count(data && data.failed)
  const parts = []
  if (pending) parts.push(t.pending(pending))
  if (failed) parts.push(t.failed(failed))
  if (done) parts.push(t.done(done))
  return { title: t.title, body: parts.length ? parts.join(' · ') : t.generic, badge: pending }
}

self.composeNotification = composeNotification

self.addEventListener('install', () => {
  self.skipWaiting()
})

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim())
})

self.addEventListener('push', (event) => {
  let data = null
  try {
    data = event.data ? event.data.json() : null
  } catch {
    data = null
  }
  // Every push must show a notification (iOS revokes a subscription whose
  // pushes stay silent), so a payload we cannot read still gets a generic one.
  const n = composeNotification(data, self.navigator && self.navigator.language)
  const shown = self.registration.showNotification(n.title, {
    body: n.body,
    tag: TAG,
    renotify: true,
    icon: '/pwa-icon-192.png',
    badge: '/pwa-icon-192.png',
    data: { url: APP_URL },
  })
  const nav = self.navigator
  const badge = !nav
    ? Promise.resolve()
    : n.badge && typeof nav.setAppBadge === 'function'
      ? nav.setAppBadge(n.badge).catch(() => {})
      : !n.badge && typeof nav.clearAppBadge === 'function'
        ? nav.clearAppBadge().catch(() => {})
        : Promise.resolve()
  event.waitUntil(Promise.all([shown, badge]))
})

self.addEventListener('notificationclick', (event) => {
  event.notification.close()
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((list) => {
      const open = list.find((c) => new URL(c.url).pathname === APP_URL)
      if (open) {
        open.postMessage({ type: 'devdy:open-inbox' })
        return open.focus()
      }
      return self.clients.openWindow(APP_URL)
    }),
  )
})

self.addEventListener('pushsubscriptionchange', (event) => {
  const old = event.oldSubscription
  const resubscribe =
    old && old.options && old.options.applicationServerKey
      ? self.registration.pushManager
          .subscribe({ userVisibleOnly: true, applicationServerKey: old.options.applicationServerKey })
          .catch(() => null)
      : Promise.resolve(null)
  event.waitUntil(
    resubscribe
      .then(() => self.clients.matchAll({ type: 'window', includeUncontrolled: true }))
      .then((list) => list.forEach((c) => c.postMessage({ type: 'devdy:subscription-changed' }))),
  )
})
