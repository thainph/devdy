// Unit tests for lib/todoDue.ts (a todo deadline against today).
// Run: node --test src/lib/__tests__/todoDue.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { daysUntil, dueStatus, localYmd } from '../todoDue.ts'

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
