// Unit tests for lib/todoDue.ts (a todo deadline against today).
// Run: node --test src/lib/__tests__/todoDue.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { compareByDue, daysUntil, dueStatus, localYmd } from '../todoDue.ts'

test('daysUntil counts calendar days, across months and years', () => {
  assert.equal(daysUntil('2026-10-29', '2026-10-29'), 0)
  assert.equal(daysUntil('2026-11-01', '2026-10-29'), 3)
  assert.equal(daysUntil('2026-10-28', '2026-10-29'), -1)
  assert.equal(daysUntil('2027-01-01', '2026-12-31'), 1)
})

test('daysUntil is null without a usable date', () => {
  assert.equal(daysUntil(null, '2026-10-29'), null)
  assert.equal(daysUntil('', '2026-10-29'), null)
  assert.equal(daysUntil('soon', '2026-10-29'), null)
})

test('dueStatus buckets by urgency', () => {
  const today = '2026-10-29'
  assert.equal(dueStatus('2026-10-20', today), 'overdue')
  assert.equal(dueStatus('2026-10-29', today), 'today')
  assert.equal(dueStatus('2026-10-30', today), 'soon')
  assert.equal(dueStatus('2026-11-01', today), 'soon')
  assert.equal(dueStatus('2026-11-02', today), 'later')
  assert.equal(dueStatus(null, today), null)
})

test('localYmd formats the local date', () => {
  assert.equal(localYmd(new Date(2026, 0, 5, 23, 59)), '2026-01-05')
})

test('compareByDue: open first, earliest deadline first, undated last, oldest breaks ties', () => {
  const rows = [
    { id: 'none', dueDate: null, done: false, at: '2026-01-01T00:00:00Z' },
    { id: 'done', dueDate: '2026-01-01', done: true, at: '2026-01-01T00:00:00Z' },
    { id: 'late', dueDate: '2026-10-01', done: false, at: '2026-09-01T00:00:00Z' },
    { id: 'nov-new', dueDate: '2026-11-20', done: false, at: '2026-11-02T00:00:00Z' },
    { id: 'nov-old', dueDate: '2026-11-20', done: false, at: '2026-11-01T00:00:00Z' },
    { id: 'bad', dueDate: 'soon', done: false, at: '2025-01-01T00:00:00Z' },
    { id: 'today', dueDate: '2026-11-11', done: false, at: '2026-11-10T00:00:00Z' },
  ]
  assert.deepEqual(
    [...rows].sort(compareByDue).map((r) => r.id),
    ['late', 'today', 'nov-old', 'nov-new', 'bad', 'none', 'done'],
  )
})
