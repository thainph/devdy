// Unit tests for lib/listGroups.ts (list-detail sidebar search + grouping).
// Run: node --test src/lib/__tests__/listGroups.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  GROUP_NONE,
  byNewest,
  byText,
  groupRows,
  haystack,
  matchesSearch,
  parseCollapsed,
  splitTags,
} from '../listGroups.ts'

test('haystack lower-cases and skips empty parts', () => {
  assert.equal(haystack('Prod', null, undefined, '', 22), 'prod\n22')
})

test('matchesSearch needs every term', () => {
  const hay = haystack('prod-admin', 'root@203.0.113.20')
  assert.equal(matchesSearch(hay, ''), true)
  assert.equal(matchesSearch(hay, '  PROD  root '), true)
  assert.equal(matchesSearch(hay, 'prod sora'), false)
})

test('groupRows sorts A–Z, catch-all last, keeps row order', () => {
  const rows = [
    { id: 'a', g: 'stg' },
    { id: 'b', g: null },
    { id: 'c', g: 'prod' },
    { id: 'd', g: 'stg' },
  ]
  const groups = groupRows(rows, (r) => r.g, (k) => k.toUpperCase(), 'None')
  assert.deepEqual(
    groups.map((g) => [g.key, g.label, g.rows.map((r) => r.id)]),
    [
      ['prod', 'PROD', ['c']],
      ['stg', 'STG', ['a', 'd']],
      [GROUP_NONE, 'None', ['b']],
    ],
  )
})

test('splitTags trims and de-duplicates', () => {
  assert.deepEqual(splitTags(' prod, vn ,,prod '), ['prod', 'vn'])
  assert.deepEqual(splitTags(null), [])
})

test('parseCollapsed tolerates bad data', () => {
  assert.deepEqual([...parseCollapsed('["a",1,"b"]')], ['a', 'b'])
  assert.deepEqual([...parseCollapsed('{')], [])
  assert.deepEqual([...parseCollapsed(null)], [])
})

test('byText sorts A–Z with empty text last', () => {
  const rows = [{ n: 'beta' }, { n: '' }, { n: 'alpha' }]
  assert.deepEqual(rows.sort(byText((r) => r.n)).map((r) => r.n), ['alpha', 'beta', ''])
})

test('byNewest sorts the latest time first, missing last', () => {
  const rows = [{ at: '2026-01-01' }, { at: null }, { at: '2026-03-01' }]
  assert.deepEqual(rows.sort(byNewest((r) => r.at)).map((r) => r.at), ['2026-03-01', '2026-01-01', null])
})
