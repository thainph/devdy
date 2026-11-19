// Unit tests for lib/projectSort.ts (project picker order).
// Run: node --test src/lib/__tests__/projectSort.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { compareProjectNames, sortByProjectName } from '../projectSort.ts'

const names = (rows) => rows.map((r) => r.name)

test('sorts A–Z ignoring case', () => {
  const rows = [{ name: 'zeta' }, { name: 'Alpha' }, { name: 'beta' }]
  assert.deepEqual(names(sortByProjectName(rows)), ['Alpha', 'beta', 'zeta'])
})

test('compares numbers by value', () => {
  const rows = [{ name: 'yahagi-168' }, { name: 'yahagi-20' }, { name: 'yahagi-167' }]
  assert.deepEqual(names(sortByProjectName(rows)), ['yahagi-20', 'yahagi-167', 'yahagi-168'])
})

test('ignores accents and surrounding spaces', () => {
  assert.ok(compareProjectNames('  apple', 'Banana') < 0)
  assert.ok(compareProjectNames('é', 'f') < 0)
})

test('does not mutate the input and accepts a name getter', () => {
  const rows = [{ label: 'b' }, { label: 'a' }]
  const sorted = sortByProjectName(rows, (r) => r.label)
  assert.deepEqual(sorted.map((r) => r.label), ['a', 'b'])
  assert.deepEqual(rows.map((r) => r.label), ['b', 'a'])
})

test('case-only duplicates keep a stable order', () => {
  const rows = [{ name: 'app' }, { name: 'App' }]
  assert.deepEqual(names(sortByProjectName(rows)), names(sortByProjectName([...rows].reverse())))
})
