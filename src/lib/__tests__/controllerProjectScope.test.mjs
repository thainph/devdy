// Unit tests for lib/controllerProjectScope.ts (phone controller project picker).
// Run: node --test src/lib/__tests__/controllerProjectScope.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  RECENT_LIMIT,
  concreteProject,
  foldText,
  pickerRows,
  rememberProject,
  validScope,
} from '../controllerProjectScope.ts'

const P = [
  { id: 'a', name: 'devdy', path: '/Users/x/Workspace/devdy' },
  { id: 'b', name: 'Yahagi 168', path: '/Users/x/Workspace/yahagi-168' },
  { id: 'c', name: 'Dự án Lunchbox', path: '/Users/x/lunchbox' },
]
const ids = (rows) => rows.map((p) => p.id)

test('remembering a project moves it to the front, once, within the cap', () => {
  assert.deepEqual(rememberProject([], 'a'), ['a'])
  assert.deepEqual(rememberProject(['a', 'b'], 'b'), ['b', 'a'])
  const many = ['1', '2', '3', '4', '5']
  assert.equal(rememberProject(many, 'x').length, RECENT_LIMIT)
  assert.deepEqual(rememberProject(many, 'x').slice(0, 2), ['x', '1'])
})

test('folding ignores case and Vietnamese diacritics', () => {
  assert.equal(foldText('Dự Án Đẹp'), 'du an dep')
})

test('without a query, recent projects come first in recency order', () => {
  const rows = pickerRows(P, ['c', 'gone', 'a'], '')
  assert.deepEqual(ids(rows.recent), ['c', 'a'], 'unknown ids are skipped')
  assert.deepEqual(ids(rows.others), ['b'])
})

test('a query searches name and path in one flat list', () => {
  assert.deepEqual(ids(pickerRows(P, ['a'], 'du an').others), ['c'])
  assert.deepEqual(ids(pickerRows(P, ['a'], 'yahagi-168').others), ['b'])
  assert.deepEqual(pickerRows(P, ['a'], 'DEV').recent, [])
  assert.deepEqual(ids(pickerRows(P, [], 'workspace').others), ['a', 'b'])
  assert.deepEqual(ids(pickerRows(P, [], 'nothing').others), [])
})

test('a screen that needs one project falls back scope → recent → first', () => {
  assert.equal(concreteProject(P, 'b', ['c']), 'b')
  assert.equal(concreteProject(P, null, ['gone', 'c']), 'c')
  assert.equal(concreteProject(P, 'gone', []), 'a')
  assert.equal(concreteProject([], null, ['a']), null)
})

test('a stale scope means all projects, but only once the list is known', () => {
  assert.equal(validScope(P, 'b'), 'b')
  assert.equal(validScope(P, 'gone'), null)
  assert.equal(validScope([], 'gone'), 'gone')
  assert.equal(validScope(P, null), null)
})
