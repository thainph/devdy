// Unit tests for lib/focusTray.ts (tray badge) and the reminder-kind helper of
// lib/focus.ts.
// Run: node --test src/lib/__tests__/focusTray.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { trayBadge } from '../focusTray.ts'
import { NOTIFY_KINDS, notifyKindsOf } from '../focus.ts'

test('trayBadge joins running sessions and overdue todos', () => {
  assert.equal(trayBadge(0, 0), '')
  assert.equal(trayBadge(2, 0), '2')
  assert.equal(trayBadge(0, 3), '⏰3')
  assert.equal(trayBadge(2, 3), '2 ⏰3')
})

test('notifyKindsOf defaults every kind on and keeps explicit offs', () => {
  const all = notifyKindsOf({})
  assert.deepEqual(Object.keys(all), [...NOTIFY_KINDS])
  assert.ok(Object.values(all).every(Boolean))
  const some = notifyKindsOf({ morning: false, overdue: true })
  assert.equal(some.morning, false)
  assert.equal(some.overdue, true)
  assert.equal(notifyKindsOf(null).remind, true)
})
