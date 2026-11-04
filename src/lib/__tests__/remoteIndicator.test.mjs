// Unit tests for lib/remoteIndicator.ts (sidebar Remote Control dot).
// Run: node --test src/lib/__tests__/remoteIndicator.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { remoteTone, isControlled, phoneControlled } from '../remoteIndicator.ts'

const base = {
  enabled: true,
  running: true,
  connected: true,
  reconnecting: false,
  session_authenticated: false,
}

test('disabled or unknown status hides the dot', () => {
  assert.equal(remoteTone(null), 'off')
  assert.equal(remoteTone({ ...base, enabled: false, session_authenticated: true }), 'off')
})

test('agent not running is an error', () => {
  assert.equal(remoteTone({ ...base, running: false, connected: false }), 'error')
})

test('relay connectivity', () => {
  assert.equal(remoteTone(base), 'online')
  assert.equal(remoteTone({ ...base, connected: false }), 'offline')
  assert.equal(remoteTone({ ...base, connected: false, reconnecting: true }), 'offline')
})

test('an attached controller wins', () => {
  assert.equal(remoteTone({ ...base, session_authenticated: true }), 'phone')
  // Still shown as controlled while the relay socket blips.
  assert.equal(remoteTone({ ...base, connected: false, session_authenticated: true }), 'phone')
  assert.ok(isControlled('phone') && !isControlled('online'))
})

test('phone row follows the authenticated session', () => {
  assert.equal(phoneControlled(null), false)
  assert.equal(phoneControlled(base), false)
  assert.equal(phoneControlled({ ...base, session_authenticated: true }), true)
})
