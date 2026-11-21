// Unit tests for lib/listPane.ts (per-screen hidden list panes).
// Run: node --test src/lib/__tests__/listPane.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isListPaneKey, parseHiddenPanes } from '../listPane.ts'

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
