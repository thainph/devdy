// Unit tests for lib/controllerRunBrowser.ts (phone run browser project rows).
// Run: node --test src/lib/__tests__/controllerRunBrowser.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { browserProjectRows } from '../controllerRunBrowser.ts'

const P = [
  { id: 'b', name: 'beta' },
  { id: 'a', name: 'alpha' },
  { id: 'c', name: 'gamma' },
]
const ids = (rows) => rows.map((p) => p.id)

test('keeps the host project order, whatever order the runs come in', () => {
  const runs = [
    { id: 'r1', project_id: 'c' },
    { id: 'r2', project_id: 'a' },
    { id: 'r3', project_id: 'c' },
  ]
  assert.deepEqual(ids(browserProjectRows(P, runs, new Set(), '?')), ['b', 'a', 'c'])
})

test('lists projects with no session yet, like the desktop sidebar', () => {
  const rows = browserProjectRows(P, [], new Set(), '?')
  assert.deepEqual(ids(rows), ['b', 'a', 'c'])
  assert.deepEqual(rows.map((r) => r.total), [0, 0, 0])
})

test('a pending run badges its project without moving it', () => {
  const runs = [
    { id: 'r1', project_id: 'b' },
    { id: 'r2', project_id: 'c' },
    { id: 'r3', project_id: 'c' },
  ]
  const rows = browserProjectRows(P, runs, new Set(['r3']), '?')
  assert.deepEqual(ids(rows), ['b', 'a', 'c'])
  assert.deepEqual(
    rows.map((r) => [r.total, r.pending]),
    [[1, 0], [0, 0], [2, 1]],
  )
})

test('runs of an unlisted project still get a row, after the known ones', () => {
  const runs = [
    { id: 'r1', project_id: 'x', project_name: ' old host ' },
    { id: 'r2', project_id: 'a' },
    { id: 'r3', project_id: 'y', project_name: '' },
  ]
  const rows = browserProjectRows(P, runs, new Set(), 'Ungrouped')
  assert.deepEqual(ids(rows), ['b', 'a', 'c', 'x', 'y'])
  assert.equal(rows[3].name, 'old host')
  assert.equal(rows[4].name, 'Ungrouped')
})
