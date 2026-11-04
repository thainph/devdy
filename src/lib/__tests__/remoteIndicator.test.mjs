// Unit tests for lib/remoteIndicator.ts (sidebar Remote Control dot).
// Run: node --test src/lib/__tests__/remoteIndicator.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { remoteTone, isControlled, phoneControlled } from '../remoteIndicator.ts'

const base = {
  enabled: true,
  running: true,
  connected: false,
  relay_online: true,
  reconnecting: false,
  session_authenticated: false,
  relay_url: 'wss://relay.example.com/ws',
}

test('no relay hides the dot; a relay shows it even while off', () => {
  assert.equal(remoteTone(null), 'hidden')
  assert.equal(remoteTone({ ...base, enabled: false, relay_url: '' }), 'hidden')
  assert.equal(remoteTone({ ...base, enabled: false, relay_url: '  ' }), 'hidden')
  assert.equal(remoteTone({ ...base, enabled: false, session_authenticated: true }), 'off')
})

test('agent not running is an error', () => {
  assert.equal(remoteTone({ ...base, running: false, relay_online: false }), 'error')
})

test('relay connectivity', () => {
  assert.equal(remoteTone(base), 'online')
  assert.equal(remoteTone({ ...base, relay_online: false }), 'offline')
  assert.equal(remoteTone({ ...base, relay_online: false, reconnecting: true }), 'offline')
})

test('a reported clash wins over the relay state, not over a controller', () => {
  assert.equal(remoteTone({ ...base, deviceConflict: { kind: 'alias', alias: 'mac' } }), 'conflict')
  assert.equal(remoteTone({ ...base, relay_online: false, deviceConflict: { kind: 'device_id' } }), 'conflict')
  assert.equal(remoteTone({ ...base, deviceConflict: null }), 'online')
  assert.equal(
    remoteTone({ ...base, session_authenticated: true, deviceConflict: { kind: 'alias', alias: 'mac' } }),
    'phone',
  )
})

test('an attached controller wins', () => {
  assert.equal(remoteTone({ ...base, session_authenticated: true }), 'phone')
  // Still shown as controlled while the relay socket blips.
  assert.equal(remoteTone({ ...base, relay_online: false, session_authenticated: true }), 'phone')
  assert.ok(isControlled('phone') && !isControlled('online'))
})

test('phone row follows the authenticated session', () => {
  assert.equal(phoneControlled(null), false)
  assert.equal(phoneControlled(base), false)
  assert.equal(phoneControlled({ ...base, session_authenticated: true }), true)
})
