// Unit tests for lib/deviceSync.ts.
// Run: node --test src/lib/__tests__/deviceSync.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  SYNC_CATEGORY_KEYS,
  defaultCategories,
  progressPercent,
  reportRows,
  reportTotals,
  selectedCategories,
  syncErrorKind,
} from '../deviceSync.ts'

test('defaultCategories ticks every category', () => {
  const c = defaultCategories()
  assert.deepEqual(Object.keys(c), [...SYNC_CATEGORY_KEYS])
  assert.ok(Object.values(c).every(v => v === true))
  assert.deepEqual(selectedCategories(c), [...SYNC_CATEGORY_KEYS])
})

test('selectedCategories keeps category order', () => {
  const c = defaultCategories()
  for (const k of SYNC_CATEGORY_KEYS) c[k] = false
  c.accounts = true
  c.todos = true
  assert.deepEqual(selectedCategories(c), ['todos', 'accounts'])
})

test('syncErrorKind maps the known codes, anything else is other', () => {
  assert.equal(syncErrorKind('wrong_password'), 'wrongPassword')
  assert.equal(syncErrorKind(' device_offline '), 'deviceOffline')
  assert.equal(syncErrorKind('relay closed'), 'other')
  assert.equal(syncErrorKind(new Error('wrong_password')), 'other')
  assert.equal(syncErrorKind(undefined), 'other')
})

test('progressPercent follows the phase', () => {
  assert.equal(progressPercent(null), 0)
  assert.equal(progressPercent({ phase: 'connecting', received: 0, total: 0 }), 0)
  assert.equal(progressPercent({ phase: 'downloading', received: 5, total: 0 }), 0)
  assert.equal(progressPercent({ phase: 'downloading', received: 1, total: 4 }), 25)
  assert.equal(progressPercent({ phase: 'downloading', received: 9, total: 4 }), 100)
  assert.equal(progressPercent({ phase: 'applying', received: 4, total: 4 }), 100)
  assert.equal(progressPercent({ phase: 'done', received: 0, total: 0 }), 100)
})

test('reportRows orders categories first, extras sorted, zero-filled', () => {
  const rows = reportRows({
    counts: {
      zeta: { created: 1 },
      accounts: { created: 1, updated: 0, unchanged: 2, deleted: 0, skipped: 3 },
      todos: { created: 2, updated: 1, unchanged: 0, deleted: 4, skipped: 0 },
      alpha: { updated: 5 },
    },
    warnings: [],
  })
  assert.deepEqual(rows.map(r => r.key), ['todos', 'accounts', 'alpha', 'zeta'])
  assert.deepEqual(rows[2].counts, { created: 0, updated: 5, unchanged: 0, deleted: 0, skipped: 0 })
})

test('reportTotals sums every row', () => {
  const total = reportTotals({
    counts: {
      todos: { created: 2, updated: 1, unchanged: 0, deleted: 4, skipped: 0 },
      notes: { created: 1, updated: 1, unchanged: 1, deleted: 0, skipped: 1 },
    },
    warnings: [],
  })
  assert.deepEqual(total, { created: 3, updated: 2, unchanged: 1, deleted: 4, skipped: 1 })
  assert.deepEqual(reportTotals({ counts: {}, warnings: [] }), { created: 0, updated: 0, unchanged: 0, deleted: 0, skipped: 0 })
})
