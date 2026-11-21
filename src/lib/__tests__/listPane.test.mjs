// Unit tests for lib/listPane.ts (per-screen hidden list panes).
// Run: node --test src/lib/__tests__/listPane.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  LIST_PANE_NARROW_BELOW,
  isListPaneKey,
  listPaneToggleAction,
  narrowFromWidth,
  parseHiddenPanes,
} from '../listPane.ts'

test('only known screens have a list pane (Settings is not one)', () => {
  assert.equal(isListPaneKey('skills'), true)
  assert.equal(isListPaneKey('run'), true)
  assert.equal(isListPaneKey('settings'), false)
  assert.equal(isListPaneKey(3), false)
})

test('parseHiddenPanes keeps known, unique keys and survives junk', () => {
  assert.deepEqual(parseHiddenPanes('["skills","todos","skills","settings"]'), ['skills', 'todos'])
  assert.deepEqual(parseHiddenPanes('{"a":1}'), [])
  assert.deepEqual(parseHiddenPanes('nope'), [])
  assert.deepEqual(parseHiddenPanes(null), [])
})

test('narrowFromWidth: below the threshold, and a hidden (0-wide) screen keeps its answer', () => {
  assert.equal(LIST_PANE_NARROW_BELOW, 640)
  assert.equal(narrowFromWidth(639, false), true)
  assert.equal(narrowFromWidth(640, true), false)
  assert.equal(narrowFromWidth(1200, true), false)
  assert.equal(narrowFromWidth(0, true), true)
  assert.equal(narrowFromWidth(0, false), false)
  assert.equal(narrowFromWidth(Number.NaN, true), true)
  assert.equal(narrowFromWidth(500, false, 480), false)
})

test('listPaneToggleAction: a narrow screen peeks, else show / hide', () => {
  assert.equal(listPaneToggleAction({ hidden: true, narrow: true }), 'peek')
  assert.equal(listPaneToggleAction({ hidden: true, narrow: false }), 'show')
  assert.equal(listPaneToggleAction({ hidden: false, narrow: false }), 'hide')
})
