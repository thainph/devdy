// Unit tests for lib/runOwnership.ts (cross-device session ownership).
// Run: node --test src/lib/__tests__/runOwnership.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { leaseActive, ownerLabel, runOwnership, shortHead } from '../runOwnership.ts'

const NOW = Date.parse('2026-10-30T12:00:00.000Z')
const owner = (lease_until, device_name = 'Mac mini') => ({
  run_id: 'r1',
  owner_device: 'mini',
  device_name,
  lease_until,
  running: lease_until != null,
  git_head: null,
  git_dirty: null,
})

test('leaseActive compares the lease to now', () => {
  assert.equal(leaseActive('2026-10-30T12:01:00.000Z', NOW), true)
  assert.equal(leaseActive('2026-10-30T11:59:00.000Z', NOW), false)
  assert.equal(leaseActive('2026-10-30T12:00:00.000Z', NOW), false)
  assert.equal(leaseActive('2026-10-30T21:00:30+09:00', NOW), true)
  assert.equal(leaseActive(null, NOW), false)
  assert.equal(leaseActive('garbage', NOW), false)
})

test('runOwnership', () => {
  assert.equal(runOwnership(undefined, NOW), 'local')
  assert.equal(runOwnership(owner('2026-10-30T12:01:00.000Z'), NOW), 'remote-running')
  assert.equal(runOwnership(owner(null), NOW), 'remote-idle')
  // The lease ran out since the backend answered: the owner died.
  assert.equal(runOwnership(owner('2026-10-30T11:59:59.000Z'), NOW), 'remote-idle')
})

test('labels', () => {
  assert.equal(ownerLabel(owner(null), 'another device'), 'Mac mini')
  assert.equal(ownerLabel(owner(null, ' '), 'another device'), 'another device')
  assert.equal(ownerLabel(owner(null, null), 'another device'), 'another device')
  assert.equal(shortHead('0123456789abcdef', '?'), '01234567')
  assert.equal(shortHead(null, '?'), '?')
})
