// Unit tests for lib/remoteIndicator.ts (sidebar Remote Control dot).
// Run: node --test src/lib/__tests__/remoteIndicator.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { remoteTone, isControlled, phoneControlled, desktopLinkTone } from '../remoteIndicator.ts'

const base = {
  enabled: true,
  running: true,
  connected: true,
  reconnecting: false,
  session_authenticated: false,
  desktop_controlled: false,
}

test('disabled or unknown status hides the dot', () => {
  assert.equal(remoteTone(null), 'off')
  assert.equal(remoteTone({ ...base, enabled: false, desktop_controlled: true }), 'off')
})

test('agent not running is an error', () => {
  assert.equal(remoteTone({ ...base, running: false, connected: false }), 'error')
})

test('relay connectivity', () => {
  assert.equal(remoteTone(base), 'online')
  assert.equal(remoteTone({ ...base, connected: false }), 'offline')
  assert.equal(remoteTone({ ...base, connected: false, reconnecting: true }), 'offline')
})

test('an attached controller wins, remote desktop first', () => {
  assert.equal(remoteTone({ ...base, session_authenticated: true }), 'phone')
  assert.equal(remoteTone({ ...base, desktop_controlled: true, session_authenticated: true }), 'desktop')
  // Still shown as controlled while the relay socket blips.
  assert.equal(remoteTone({ ...base, connected: false, desktop_controlled: true }), 'desktop')
  assert.ok(isControlled('phone') && isControlled('desktop') && !isControlled('online'))
})

test('the phone row only shows without a remote desktop attached', () => {
  assert.equal(phoneControlled(null), false)
  assert.equal(phoneControlled({ ...base, session_authenticated: true }), true)
  // The bound session is authenticated for a remote desktop too.
  assert.equal(phoneControlled({ ...base, session_authenticated: true, desktop_controlled: true }), false)
})

test('controlling-side link tone', () => {
  const link = { status: 'connected', auth: 'authenticated', superseded: false }
  assert.equal(desktopLinkTone(link), 'connected')
  assert.equal(desktopLinkTone({ ...link, auth: 'pending' }), 'connecting')
  assert.equal(desktopLinkTone({ ...link, status: 'reconnecting', auth: null }), 'connecting')
  assert.equal(desktopLinkTone({ ...link, status: 'error' }), 'failed')
  assert.equal(desktopLinkTone({ ...link, auth: 'failed' }), 'failed')
  // A takeover ends in `error` too, but is not a sign-in problem.
  assert.equal(desktopLinkTone({ ...link, status: 'error', superseded: true }), 'superseded')
})
