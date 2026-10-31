// Unit tests for lib/remotePush.ts (phone controller Web Push helpers) and the
// notification text composed by public/controller-sw.js.
// Run: node --test src/lib/__tests__/remotePush.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import {
  base64UrlToBytes,
  deviceLabel,
  isIosDevice,
  pushSupport,
  pushUiState,
  sameServerKey,
  shouldAutoResubscribe,
  subscriptionWire,
} from '../remotePush.ts'

const IPHONE_SAFARI =
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1'
const IPAD_DESKTOP_MODE =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15'
const ANDROID_CHROME =
  'Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Mobile Safari/537.36'

const full = { hasServiceWorker: true, hasPushManager: true, hasNotification: true }

test('iOS Safari must be installed to the Home Screen first', () => {
  assert.equal(pushSupport({ ...full, ios: true, standalone: false }), 'needs_install')
  // Not even exposed in a Safari tab — still the install hint, not "unsupported".
  assert.equal(
    pushSupport({ hasServiceWorker: true, hasPushManager: false, hasNotification: false, ios: true, standalone: false }),
    'needs_install',
  )
  assert.equal(pushSupport({ ...full, ios: true, standalone: true }), 'supported')
  // An installed app on an iOS too old for Web Push.
  assert.equal(pushSupport({ ...full, hasPushManager: false, ios: true, standalone: true }), 'unsupported')
  assert.equal(pushSupport({ ...full, ios: false, standalone: false }), 'supported')
})

test('iPadOS in desktop mode is still iOS', () => {
  assert.equal(isIosDevice(IPHONE_SAFARI, 'iPhone', 5), true)
  assert.equal(isIosDevice(IPAD_DESKTOP_MODE, 'MacIntel', 5), true)
  assert.equal(isIosDevice(IPAD_DESKTOP_MODE, 'MacIntel', 0), false, 'a real Mac')
  assert.equal(isIosDevice(ANDROID_CHROME, 'Linux armv8l', 5), false)
})

test('the switch state', () => {
  const base = { support: 'supported', permission: 'granted', browserSubscribed: true, hostSubscribed: true }
  assert.equal(pushUiState(base), 'on')
  assert.equal(pushUiState({ ...base, hostSubscribed: false }), 'off', 'the desktop removed this device')
  assert.equal(pushUiState({ ...base, browserSubscribed: false }), 'off')
  assert.equal(pushUiState({ ...base, permission: 'default' }), 'off')
  assert.equal(pushUiState({ ...base, permission: 'denied' }), 'denied')
  assert.equal(pushUiState({ ...base, hostSubscribed: null }), 'loading')
  assert.equal(pushUiState({ ...base, support: 'needs_install' }), 'needs_install')
  assert.equal(pushUiState({ ...base, support: 'unsupported', permission: 'denied' }), 'unsupported')
})

test('VAPID keys decode from base64url and compare', () => {
  const key = 'BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8'
  const bytes = base64UrlToBytes(key)
  assert.equal(bytes.length, 65)
  assert.equal(bytes[0], 4)
  assert.equal(sameServerKey(bytes.buffer, key), true)
  const other = new Uint8Array(bytes)
  other[64] ^= 1
  assert.equal(sameServerKey(other.buffer, key), false)
  assert.equal(sameServerKey(null, key), false)
})

test('subscription JSON is narrowed to what the Host needs', () => {
  assert.deepEqual(
    subscriptionWire({ endpoint: 'https://web.push.apple.com/x', expirationTime: null, keys: { p256dh: 'BCV', auth: 'BTB' } }),
    { endpoint: 'https://web.push.apple.com/x', p256dh: 'BCV', auth: 'BTB' },
  )
  assert.equal(subscriptionWire({ endpoint: 'https://x', keys: {} }), null)
  assert.equal(subscriptionWire(null), null)
})

test('device labels', () => {
  assert.equal(deviceLabel(IPHONE_SAFARI, true), 'iPhone · Safari')
  assert.equal(deviceLabel(IPAD_DESKTOP_MODE, true), 'iPad · Safari')
  assert.equal(deviceLabel(ANDROID_CHROME, false), 'Android · Chrome')
})

// ---- public/controller-sw.js ----

function loadServiceWorker() {
  const src = readFileSync(new URL('../../../public/controller-sw.js', import.meta.url), 'utf8')
  const listeners = {}
  const self = { addEventListener: (type, fn) => (listeners[type] = fn) }
  vm.runInNewContext(src, { self, URL })
  // Re-create results in this realm so deepEqual compares plain objects.
  return { compose: (...a) => ({ ...self.composeNotification(...a) }), listeners }
}

test('the service worker words counts in the phone language', () => {
  const { compose } = loadServiceWorker()
  assert.deepEqual(compose({ v: 1, t: 'summary', pending: 2, done: 1, failed: 0 }, 'vi-VN'), {
    title: 'Devdy',
    body: '2 việc cần duyệt · 1 phiên đã xong',
    badge: 2,
  })
  assert.deepEqual(compose({ v: 1, t: 'summary', pending: 1, done: 0, failed: 2 }, 'en-US'), {
    title: 'Devdy',
    body: '1 request waiting for you · 2 runs failed',
    badge: 1,
  })
  assert.equal(compose({ v: 1, t: 'test' }, 'vi').body, 'Thông báo thử — thông báo đã hoạt động.')
  // Unreadable payloads still show something (iOS drops silent pushers).
  assert.equal(compose(null, 'en').body, 'Something needs your attention.')
  assert.equal(compose({ pending: -3, done: 'x' }, 'en').body, 'Something needs your attention.')
})

test('the service worker caches nothing', () => {
  const { listeners } = loadServiceWorker()
  assert.deepEqual(Object.keys(listeners).sort(), [
    'activate',
    'install',
    'notificationclick',
    'push',
    'pushsubscriptionchange',
  ])
})

test('auto re-subscribe only when the Host lost a granted, current subscription', () => {
  const base = { permission: 'granted', hasLocalSubscription: true, hostSubscribed: false, alreadyTried: false }
  assert.equal(shouldAutoResubscribe(base), true)
  assert.equal(shouldAutoResubscribe({ ...base, hostSubscribed: true }), false)
  assert.equal(shouldAutoResubscribe({ ...base, hostSubscribed: null }), false, 'Host has not answered')
  assert.equal(shouldAutoResubscribe({ ...base, permission: 'default' }), false)
  assert.equal(shouldAutoResubscribe({ ...base, hasLocalSubscription: false }), false)
  assert.equal(shouldAutoResubscribe({ ...base, alreadyTried: true }), false, 'once per endpoint per load')
})

function fakeWorkerEnv() {
  const src = readFileSync(new URL('../../../public/controller-sw.js', import.meta.url), 'utf8')
  const listeners = {}
  const calls = []
  const windows = []
  const self = {
    addEventListener: (type, fn) => (listeners[type] = fn),
    navigator: {
      language: 'en',
      setAppBadge: async (n) => calls.push(['setAppBadge', n]),
      clearAppBadge: async () => calls.push(['clearAppBadge']),
    },
    registration: {
      showNotification: async (title, opts) => calls.push(['show', title, opts.tag]),
      pushManager: { subscribe: async (o) => (calls.push(['subscribe', o.applicationServerKey]), {}) },
    },
    clients: {
      matchAll: async () => windows,
      openWindow: async (url) => calls.push(['openWindow', url]),
    },
  }
  vm.runInNewContext(src, { self, URL })
  const fire = async (type, event) => {
    let done
    listeners[type]({ ...event, waitUntil: (p) => (done = p) })
    await done
  }
  return { fire, calls, windows }
}

test('the badge follows the pending count and clears at zero', async () => {
  const w = fakeWorkerEnv()
  const push = (data) => ({ data: { json: () => data } })
  await w.fire('push', push({ v: 1, t: 'summary', pending: 2, done: 0, failed: 0 }))
  await w.fire('push', push({ v: 1, t: 'summary', pending: 0, done: 1, failed: 0 }))
  assert.deepEqual(
    w.calls.filter((c) => c[0] !== 'show'),
    [['setAppBadge', 2], ['clearAppBadge']],
  )
  assert.ok(w.calls.filter((c) => c[0] === 'show').every((c) => c[2] === 'devdy'), 'one tag')
})

test('a click focuses only the controller window, else opens it', async () => {
  const w = fakeWorkerEnv()
  const msgs = []
  const win = (url) => ({ url, postMessage: (m) => msgs.push([url, m.type]), focus: async () => msgs.push([url, 'focus']) })
  w.windows.push(win('https://relay.example/other'))
  await w.fire('notificationclick', { notification: { close() {} } })
  assert.deepEqual(w.calls.at(-1), ['openWindow', '/controller.html'])
  assert.equal(msgs.length, 0)
  w.windows.push(win('https://relay.example/controller.html#x'))
  await w.fire('notificationclick', { notification: { close() {} } })
  assert.deepEqual(msgs, [
    ['https://relay.example/controller.html#x', 'devdy:open-inbox'],
    ['https://relay.example/controller.html#x', 'focus'],
  ])
})

test('a rotated subscription is renewed with the same key and reported to open windows', async () => {
  const w = fakeWorkerEnv()
  const msgs = []
  w.windows.push({ url: 'https://relay.example/controller.html', postMessage: (m) => msgs.push(m.type) })
  await w.fire('pushsubscriptionchange', { oldSubscription: { options: { applicationServerKey: 'KEY' } } })
  assert.deepEqual(w.calls, [['subscribe', 'KEY']])
  assert.deepEqual(msgs, ['devdy:subscription-changed'])
})
