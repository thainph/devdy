// Unit tests for lib/remoteIndicator.ts (sidebar Remote Control dot).
// Run: node --test src/lib/__tests__/remoteIndicator.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { remoteTone, isControlled } from '../remoteIndicator.ts'

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
