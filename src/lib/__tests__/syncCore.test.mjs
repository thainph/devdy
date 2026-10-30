// Unit tests for lib/syncCore.ts (Settings → Sync helpers).
// Run: node --test src/lib/__tests__/syncCore.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DISABLED_STATUS,
  ageParts,
  canRemoveDevices,
  countdown,
  groupCreator,
  syncErrorCode,
  deviceLabel,
  missingProjects,
  sortDevices,
  syncStateKey,
  syncTone,
  tableRows,
  totalRows,
} from '../syncCore.ts'

const st = (over) => ({ ...DISABLED_STATUS, state: 'online', ...over })

test('syncTone gives each state its own dot, most urgent first', () => {
  assert.equal(syncTone(null), 'off')
  assert.equal(syncTone(DISABLED_STATUS), 'off')
  assert.equal(syncTone(st()), 'ok')
  // On demand: queued changes wait for another device's Sync, they are not "syncing".
  assert.equal(syncTone(st({ pending: 3 })), 'ok')
  assert.equal(syncTone(st(), false, true), 'syncing', 'a sync running here')
  assert.equal(syncTone(st({ state: 'connecting' })), 'syncing')
  assert.equal(syncTone(st({ parked: 2 })), 'parked')
  assert.equal(syncTone(st({ parked: 2 }), false, true), 'parked')
  assert.equal(syncTone(st({ state: 'offline', parked: 2 })), 'offline')
  assert.equal(syncTone(st({ state: 'offline', paused: true })), 'paused')
  assert.equal(syncTone(st({ state: 'error', paused: true })), 'error')
  assert.equal(syncTone(st({ state: 'error' }), true), 'merge')
})

test('syncStateKey prefers "paused" over the connection state', () => {
  assert.equal(syncStateKey(undefined), 'disabled')
  assert.equal(syncStateKey(st({ state: 'offline', paused: true })), 'paused')
  assert.equal(syncStateKey(st({ state: 'connecting' })), 'connecting')
  assert.equal(syncStateKey(st({ parked: 1 })), 'parked')
  assert.equal(syncStateKey(st({ state: 'error', parked: 1 })), 'error')
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
  assert.equal(syncTone(DISABLED_STATUS, true), 'merge')
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

const dev = (id, over) => ({ id, name: id, last_seen: null, is_self: false, is_leader: false, is_creator: false, ...over })

test('only the creator removes devices; unknown creator leaves it to the relay', () => {
  const me = dev('me', { is_self: true })
  assert.equal(canRemoveDevices([me, dev('mini')]), true)
  assert.equal(groupCreator([me, dev('mini')]), null)
  assert.equal(canRemoveDevices([{ ...me, is_creator: true }, dev('mini')]), true)
  assert.equal(canRemoveDevices([me, dev('mini', { is_creator: true })]), false)
  assert.equal(groupCreator([me, dev('mini', { is_creator: true })]).id, 'mini')
})

test('countdown renders m:ss and never goes negative', () => {
  assert.equal(countdown(300_000, 0), '5:00')
  assert.equal(countdown(61_500, 0), '1:02')
  assert.equal(countdown(5_000, 9_000), '0:00')
  assert.equal(countdown(null, 0), '0:00')
})

test('syncErrorCode picks the backend code prefix', () => {
  assert.equal(syncErrorCode('wrong_password'), 'wrong_password')
  assert.equal(syncErrorCode('busy'), 'busy', 'a peer serving too many requests')
  assert.equal(syncErrorCode('folder fetch failed: busy'), 'busy')
  assert.equal(syncErrorCode('not_accepting: open Settings'), 'not_accepting')
  assert.equal(syncErrorCode('declined: the request was declined on that machine'), 'declined')
  assert.equal(syncErrorCode('relay_not_configured: relay URL is not set'), 'relay_not_configured')
  assert.equal(syncErrorCode('sync_bootstrap_runs_live: 2 session(s) still running'), 'sync_bootstrap_runs_live')
  assert.equal(syncErrorCode('sync_bootstrap_not_pending: this device has no merge waiting'), 'sync_bootstrap_not_pending')
  assert.equal(syncErrorCode('network: connect timeout'), null)
  assert.equal(syncErrorCode(undefined), null)
  assert.equal(syncErrorCode(''), null)
})

test('syncErrorCode finds relay codes wrapped by the backend', () => {
  assert.equal(syncErrorCode('relay rejected this device: device_revoked'), 'device_revoked')
  assert.equal(syncErrorCode('relay rejected this device: ticket_invalid'), 'ticket_invalid')
  assert.equal(syncErrorCode('relay rejected this device: ticket_required'), 'ticket_required')
  assert.equal(syncErrorCode('relay rejected this device: group_exists'), 'group_exists')
  assert.equal(syncErrorCode('relay error: quota_exceeded'), 'quota_exceeded')
  assert.equal(syncErrorCode('relay error: forbidden'), 'forbidden')
  // Whole codes only.
  assert.equal(syncErrorCode('relay error: not_forbidden_x'), null)
  assert.equal(syncErrorCode('relay rejected this device: auth_failed'), 'auth_failed')
  assert.equal(syncErrorCode('relay rejected this device: relay_behind'), 'relay_behind')
  assert.equal(syncErrorCode('relay rejected this device: ticket_invalid_x'), null)
})

test('syncErrorCode maps known phrases without a code', () => {
  assert.equal(syncErrorCode('only the device that created the group can remove other devices'), 'forbidden')
  assert.equal(syncErrorCode('relay not configured'), 'relay_not_configured')
  assert.equal(syncErrorCode(new Error('Relay not configured')), 'relay_not_configured')
})
