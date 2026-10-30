// Unit tests for lib/syncCore.ts (Settings → Sync helpers).
// Run: node --test src/lib/__tests__/syncCore.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DISABLED_STATUS,
  ageParts,
  countRows,
  deviceLabel,
  isMissingCommand,
  sortDevices,
  syncStateKey,
  syncTone,
  unmappedProjects,
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

test('isMissingCommand recognises Tauri unknown-command errors only', () => {
  assert.ok(isMissingCommand('Command sync_resync_all not found'))
  assert.ok(isMissingCommand(new Error('command sync_bootstrap_preview not found')))
  assert.ok(!isMissingCommand('relay unreachable'))
  assert.ok(!isMissingCommand('not found'))
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

test('countRows flattens nested numeric counts and drops zeros', () => {
  assert.deepEqual(countRows({ counts: { notes: 3, todos: 0 } }), [['notes', 3]])
  assert.deepEqual(countRows({ notes: 2, label: 'x', list: [1] }), [['notes', 2]])
  assert.deepEqual(countRows({ remote: { notes: 1 }, matched: { projects: 2 } }), [
    ['remote.notes', 1],
    ['matched.projects', 2],
  ])
  assert.deepEqual(countRows(null), [])
  assert.deepEqual(countRows(5), [])
})

test('unmappedProjects keeps projects without a local folder', () => {
  const list = [
    { remote_project_id: 'a', name: 'A', local_path: '/x' },
    { remote_project_id: 'b', name: 'B', local_path: null },
    { remote_project_id: 'c', name: 'C', local_path: '  ' },
  ]
  assert.deepEqual(unmappedProjects(list).map((p) => p.remote_project_id), ['b', 'c'])
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
