// Unit tests for lib/syncCore.ts (Settings → Sync helpers).
// Run: node --test src/lib/__tests__/syncCore.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DISABLED_STATUS,
  ageParts,
  deviceLabel,
  missingProjects,
  sortDevices,
  syncStateKey,
  syncTone,
  tableRows,
  totalRows,
} from '../syncCore.ts'

const st = (over) => ({ ...DISABLED_STATUS, state: 'online', ...over })

test('syncTone maps state, pause and backlog to a dot colour', () => {
  assert.equal(syncTone(null), 'off')
  assert.equal(syncTone(DISABLED_STATUS), 'off')
  assert.equal(syncTone(st()), 'ok')
  assert.equal(syncTone(st({ pending: 3 })), 'busy')
  assert.equal(syncTone(st({ state: 'connecting' })), 'busy')
  assert.equal(syncTone(st({ state: 'offline' })), 'warn')
  assert.equal(syncTone(st({ paused: true })), 'warn')
  assert.equal(syncTone(st({ state: 'error', paused: true })), 'error')
})

test('syncStateKey prefers "paused" over the connection state', () => {
  assert.equal(syncStateKey(undefined), 'disabled')
  assert.equal(syncStateKey(st({ state: 'offline', paused: true })), 'paused')
  assert.equal(syncStateKey(st({ state: 'connecting' })), 'connecting')
})

test('ageParts buckets elapsed time', () => {
  const now = 10_000_000_000
  assert.deepEqual(ageParts(null, now), { unit: 'never', n: 0 })
  assert.deepEqual(ageParts(now - 5_000, now), { unit: 'now', n: 0 })
  assert.deepEqual(ageParts(now + 5_000, now), { unit: 'now', n: 0 })
  assert.deepEqual(ageParts(now - 3 * 60_000, now), { unit: 'minutes', n: 3 })
  assert.deepEqual(ageParts(now - 2 * 3_600_000, now), { unit: 'hours', n: 2 })
  assert.deepEqual(ageParts(now - 3 * 86_400_000, now), { unit: 'days', n: 3 })
})

const counts = (over = {}) => ({ matched: 0, rekeyed: 0, new_remote: 0, new_local: 0, conflicts: 0, deleted_remote: 0, ...over })

test('totalRows lists non-zero preview counts in display order', () => {
  assert.deepEqual(totalRows(counts({ matched: 2, new_remote: 5, conflicts: 1 })), [
    ['new_remote', 5],
    ['matched', 2],
    ['conflicts', 1],
  ])
  assert.deepEqual(totalRows(counts()), [])
})

test('tableRows drops tables with nothing to report', () => {
  const p = { tables: [{ tbl: 'notes', ...counts({ new_local: 3 }) }, { tbl: 'todos', ...counts() }] }
  assert.deepEqual(tableRows(p), [{ tbl: 'notes', counts: [['new_local', 3]] }])
})

test('missingProjects keeps synced projects not on this machine', () => {
  const m = (remote_id, on_this_machine) => ({ remote_id, name: remote_id, synced_path: '~/x', local_id: null, matched_by: null, local_path: '/x', on_this_machine })
  assert.deepEqual(missingProjects({ projects: [m('a', true), m('b', false)] }).map((p) => p.remote_id), ['b'])
})

test('a pending first-join merge overrides the disabled state', () => {
  assert.equal(syncTone(DISABLED_STATUS, true), 'warn')
  assert.equal(syncStateKey(DISABLED_STATUS, true), 'mergePending')
  assert.equal(syncStateKey(DISABLED_STATUS, false), 'disabled')
})

test('sortDevices puts this device first, then the leader, then by name', () => {
  const d = (id, name, extra = {}) => ({ id, name, last_seen: null, is_self: false, is_leader: false, ...extra })
  const sorted = sortDevices([d('3', 'zeta'), d('2', 'alpha'), d('1', 'mini', { is_leader: true }), d('0', 'air', { is_self: true })])
  assert.deepEqual(sorted.map((x) => x.id), ['0', '1', '2', '3'])
})

test('deviceLabel falls back to the id prefix', () => {
  assert.equal(deviceLabel({ id: 'abcdef1234', name: ' Mac mini ' }), 'Mac mini')
  assert.equal(deviceLabel({ id: 'abcdef1234', name: '' }), 'abcdef12')
})
