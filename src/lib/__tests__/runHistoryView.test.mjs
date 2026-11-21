// Unit tests for lib/runHistoryView.ts (the session screen's History filter menu).
// Run: node --test src/lib/__tests__/runHistoryView.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  defaultRunHistoryView,
  isRunHistoryFiltering,
  matchesRunHistoryStatus,
  matchesRunSearch,
  runHistoryFilterSummary,
  runHistoryStatusCounts,
  runHistoryViewChanged,
} from '../runHistoryView.ts'

const flags = (over = {}) => ({ waiting: false, unread: false, running: false, ...over })

test('defaults: every session, compact rows', () => {
  assert.deepEqual(defaultRunHistoryView(), { status: '', density: 'compact' })
})

test('status filter', () => {
  assert.equal(matchesRunHistoryStatus('', flags()), true)
  assert.equal(matchesRunHistoryStatus('waiting', flags()), false)
  assert.equal(matchesRunHistoryStatus('waiting', flags({ waiting: true })), true)
  assert.equal(matchesRunHistoryStatus('unread', flags({ running: true })), false)
  assert.equal(matchesRunHistoryStatus('running', flags({ running: true, waiting: true })), true)
})

test('status counts: all, and each flag independently', () => {
  const counts = runHistoryStatusCounts([
    flags({ waiting: true, running: true }),
    flags({ unread: true }),
    flags(),
  ])
  assert.deepEqual(counts, { '': 3, waiting: 1, unread: 1, running: 1 })
  assert.deepEqual(runHistoryStatusCounts([]), { '': 0, waiting: 0, unread: 0, running: 0 })
})

test('filtering vs changed: density alone is not a filter but moves off the defaults', () => {
  const d = defaultRunHistoryView()
  assert.equal(isRunHistoryFiltering(d), false)
  assert.equal(runHistoryViewChanged(d), false)
  assert.equal(isRunHistoryFiltering({ ...d, density: 'dense' }), false)
  assert.equal(runHistoryViewChanged({ ...d, density: 'dense' }), true)
  assert.equal(isRunHistoryFiltering({ ...d, status: 'waiting' }), true)
  assert.equal(runHistoryViewChanged({ ...d, status: 'waiting' }), true)
})

test('summary names the active status only', () => {
  const label = (s) => `L:${s}`
  assert.deepEqual(runHistoryFilterSummary(defaultRunHistoryView(), label), [])
  assert.deepEqual(runHistoryFilterSummary({ status: 'unread', density: 'dense' }, label), ['L:unread'])
})

test('search: trimmed, case-insensitive substring over the shown text', () => {
  const hay = ['Fix Login', null, 'owner/repo #42', undefined, 'claude']
  assert.equal(matchesRunSearch('', hay), true)
  assert.equal(matchesRunSearch('   ', hay), true)
  assert.equal(matchesRunSearch('  login ', hay), true)
  assert.equal(matchesRunSearch('#42', hay), true)
  assert.equal(matchesRunSearch('fix login', hay), true)
  assert.equal(matchesRunSearch('login fix', hay), false)
  assert.equal(matchesRunSearch('codex', hay), false)
})
