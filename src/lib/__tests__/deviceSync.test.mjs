// Unit tests for lib/deviceSync.ts.
// Run: node --test src/lib/__tests__/deviceSync.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  MIRROR_DELETE_KEYS,
  SYNC_CATEGORY_KEYS,
  defaultCategories,
  mirrorDeletions,
  progressPercent,
  reportRows,
  reportTotals,
  selectedCategories,
  syncErrorKind,
} from '../deviceSync.ts'

test('defaultCategories ticks every category with mirror off', () => {
  const c = defaultCategories()
  assert.deepEqual(Object.keys(c), [...SYNC_CATEGORY_KEYS, 'mirror'])
  assert.equal(c.mirror, false)
  assert.ok(SYNC_CATEGORY_KEYS.every(k => c[k] === true))
  assert.deepEqual(selectedCategories(c), [...SYNC_CATEGORY_KEYS])
  assert.deepEqual(mirrorDeletions(c), [])
})

test('mirrorDeletions lists only ticked deletable categories', () => {
  const c = defaultCategories()
  c.mirror = true
  assert.deepEqual(mirrorDeletions(c), [...MIRROR_DELETE_KEYS])
  for (const k of ['secretNotes', 'slackThreads', 'projects', 'settings']) {
    assert.ok(!MIRROR_DELETE_KEYS.includes(k), k)
  }
  c.notes = false
  c.accounts = false
  assert.deepEqual(mirrorDeletions(c), ['todos', 'skillsRules', 'mcpServers', 'vpsServers'])
  c.mirror = false
  assert.deepEqual(mirrorDeletions(c), [])
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
