// Unit tests for lib/serverRows.ts (Servers screen filtering + tag editing).
// Run: node --test src/lib/__tests__/serverRows.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { GROUP_NONE } from '../listGroups.ts'
import {
  addTags,
  allServerTags,
  filterServers,
  groupServers,
  joinTags,
  serverStatus,
  sshCommand,
  summarizeTests,
} from '../serverRows.ts'

const server = (id, over = {}) => ({
  id,
  label: id,
  host: `${id}.example.com`,
  port: 22,
  username: 'root',
  tags: null,
  status: null,
  ...over,
})

const servers = [
  server('prod-admin', { host: '203.0.113.20', tags: 'prod, vn', status: 'online' }),
  server('sora', { username: 'ops', tags: 'stg', status: 'offline' }),
  server('scratch'),
]

const all = { search: '', tag: '', status: '' }

test('serverStatus maps null to unknown', () => {
  assert.equal(serverStatus({ status: null }), 'unknown')
  assert.equal(serverStatus({ status: 'unknown' }), 'unknown')
  assert.equal(serverStatus({ status: 'online' }), 'online')
})

test('filterServers searches label, host, username and tags', () => {
  const ids = (f) => filterServers(servers, { ...all, ...f }).map((s) => s.id)
  assert.deepEqual(ids({}), ['prod-admin', 'sora', 'scratch'])
  assert.deepEqual(ids({ search: '203.0.113' }), ['prod-admin'])
  assert.deepEqual(ids({ search: 'OPS' }), ['sora'])
  assert.deepEqual(ids({ search: 'vn root' }), ['prod-admin'])
})

test('filterServers applies tag and status filters', () => {
  const ids = (f) => filterServers(servers, { ...all, ...f }).map((s) => s.id)
  assert.deepEqual(ids({ tag: 'vn' }), ['prod-admin'])
  assert.deepEqual(ids({ status: 'offline' }), ['sora'])
  assert.deepEqual(ids({ status: 'unknown' }), ['scratch'])
  assert.deepEqual(ids({ tag: 'stg', status: 'online' }), [])
})

test('allServerTags is de-duplicated and sorted', () => {
  assert.deepEqual(allServerTags([...servers, server('x', { tags: 'vn,dev' })]), ['dev', 'prod', 'stg', 'vn'])
})

test('groupServers groups by first tag, untagged last', () => {
  const groups = groupServers(servers, 'Untagged')
  assert.deepEqual(
    groups.map((g) => [g.key, g.label, g.rows.map((r) => r.id)]),
    [
      ['prod', 'prod', ['prod-admin']],
      ['stg', 'stg', ['sora']],
      [GROUP_NONE, 'Untagged', ['scratch']],
    ],
  )
})

test('addTags splits on commas and skips duplicates', () => {
  assert.deepEqual(addTags(['prod'], ' vn, prod ,, sg'), ['prod', 'vn', 'sg'])
  assert.deepEqual(addTags([], '  '), [])
})

test('joinTags stores a comma-separated string or null', () => {
  assert.equal(joinTags(['prod', ' vn ', 'prod']), 'prod, vn')
  assert.equal(joinTags([]), null)
})

test('sshCommand always includes the port', () => {
  assert.equal(sshCommand({ host: '1.2.3.4', port: 2222, username: 'ops' }), 'ssh -p 2222 ops@1.2.3.4')
})

test('summarizeTests counts rejections as offline', () => {
  assert.deepEqual(
    summarizeTests([
      { status: 'fulfilled', value: { ok: true } },
      { status: 'fulfilled', value: { ok: false } },
      { status: 'rejected', reason: new Error('x') },
    ]),
    { online: 1, offline: 2 },
  )
})
