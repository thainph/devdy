// Unit tests for lib/runOwnership.ts (cross-device session ownership).
// Run: node --test src/lib/__tests__/runOwnership.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { leaseLive, ownerLabel, runOwnership, shortHead, stampReceived, syncPercent } from '../runOwnership.ts'

const NOW = Date.parse('2026-10-30T12:00:00.000Z')
const owner = (remaining, device_name = 'Mac mini') => ({
  run_id: 'r1',
  owner_device: 'mini',
  device_name,
  // A wall-clock value from the other machine: never compared with this clock.
  lease_until: remaining == null ? null : '2000-01-01T00:00:00.000Z',
  running: remaining != null,
  lease_remaining_ms: remaining,
  git_head: null,
  git_dirty: null,
})

test('leaseLive counts the locally measured remainder down from receipt', () => {
  const [o] = stampReceived([owner(60_000)], NOW)
  assert.equal(o.received_at_ms, NOW)
  assert.equal(leaseLive(o, NOW), true)
  assert.equal(leaseLive(o, NOW + 59_999), true)
  assert.equal(leaseLive(o, NOW + 60_000), false)
  assert.equal(leaseLive(owner(null), NOW), false)
  // Not stamped yet: trust the backend's answer.
  assert.equal(leaseLive(owner(60_000), NOW + 999_999), true)
})

test('runOwnership', () => {
  assert.equal(runOwnership(undefined, NOW), 'local')
  const [live] = stampReceived([owner(30_000)], NOW)
  assert.equal(runOwnership(live, NOW), 'remote-running')
  assert.equal(runOwnership(owner(null), NOW), 'remote-idle')
  // No renewal arrived since the backend answered: the owner died.
  assert.equal(runOwnership(live, NOW + 31_000), 'remote-idle')
})

test('syncPercent', () => {
  assert.equal(syncPercent({ have: 40, want: 100 }), 40)
  assert.equal(syncPercent({ have: 2, want: 3 }), 66)
  assert.equal(syncPercent({ have: 5, want: 0 }), 100)
  assert.equal(syncPercent({ have: 200, want: 100 }), 100)
})

test('labels', () => {
  assert.equal(ownerLabel(owner(null), 'another device'), 'Mac mini')
  assert.equal(ownerLabel(owner(null, ' '), 'another device'), 'another device')
  assert.equal(ownerLabel(owner(null, null), 'another device'), 'another device')
  assert.equal(shortHead('0123456789abcdef', '?'), '01234567')
  assert.equal(shortHead(null, '?'), '?')
})
