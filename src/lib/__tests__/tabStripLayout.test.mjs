// Unit tests for lib/tabStripLayout.ts (tab strip overflow).
// Run: node --test src/lib/__tests__/tabStripLayout.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { tabStripLayout } from '../tabStripLayout.ts'

const o = { min: 140, max: 200, gap: 4, more: 52 }

test('everything fits: shared width, capped at max', () => {
  const l = tabStripLayout(3, 1000, 0, o)
  assert.deepEqual(l.shown, [0, 1, 2])
  assert.deepEqual(l.hidden, [])
  assert.equal(l.width, 200)
  assert.equal(tabStripLayout(4, 600, 0, o).width, (600 - 12) / 4)
})

test('never below the minimum width: the rest overflows', () => {
  const l = tabStripLayout(10, 800, 0, o)
  // (800 - 52 - 4 + 4) / 144 = 5 tabs
  assert.deepEqual(l.shown, [0, 1, 2, 3, 4])
  assert.deepEqual(l.hidden, [5, 6, 7, 8, 9])
  assert.ok(l.width >= 140)
})

test('a hidden current tab takes the last visible slot', () => {
  const l = tabStripLayout(10, 800, 8, o)
  assert.deepEqual(l.shown, [0, 1, 2, 3, 8])
  assert.deepEqual(l.hidden, [4, 5, 6, 7, 9])
})

test('at least one tab shows, and unmeasured shows all', () => {
  assert.deepEqual(tabStripLayout(5, 100, 3, o).shown, [3])
  assert.deepEqual(tabStripLayout(5, 0, 3, o).shown, [0, 1, 2, 3, 4])
})
