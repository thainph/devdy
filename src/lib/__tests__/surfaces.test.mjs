// Unit tests for lib/surfaces.ts (tab vs window placement of surfaces).
// Run: node --test src/lib/__tests__/surfaces.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  cleanParams,
  parseOverrides,
  parseWindowMode,
  placementFor,
  shouldDockOnMonitorChange,
  surfaceForPath,
  surfaceLocation,
  surfaceTabId,
} from '../surfaces.ts'

test('auto: one monitor opens every surface as a tab', () => {
  for (const k of ['prInbox', 'calendar', 'slack', 'todos', 'file']) {
    assert.equal(placementFor(k, 'auto', {}, 1), 'tab')
  }
})

test('auto: a second monitor moves only the dashboard-like surfaces to windows', () => {
  assert.equal(placementFor('calendar', 'auto', {}, 2), 'window')
  assert.equal(placementFor('prInbox', 'auto', {}, 3), 'window')
  assert.equal(placementFor('slack', 'auto', {}, 2), 'window')
  assert.equal(placementFor('todos', 'auto', {}, 2), 'tab')
  assert.equal(placementFor('file', 'auto', {}, 2), 'tab')
})

test('fixed modes ignore the monitor count', () => {
  assert.equal(placementFor('calendar', 'tab', {}, 2), 'tab')
  assert.equal(placementFor('todos', 'window', {}, 1), 'window')
})

test('a per-surface override wins over the mode', () => {
  assert.equal(placementFor('calendar', 'auto', { calendar: 'tab' }, 2), 'tab')
  assert.equal(placementFor('todos', 'tab', { todos: 'window' }, 1), 'window')
})

test('parseWindowMode falls back to auto', () => {
  assert.equal(parseWindowMode('tab'), 'tab')
  assert.equal(parseWindowMode('window'), 'window')
  assert.equal(parseWindowMode(''), 'auto')
  assert.equal(parseWindowMode(undefined), 'auto')
  assert.equal(parseWindowMode('nope'), 'auto')
})

test('parseOverrides drops malformed input and unknown keys/values', () => {
  assert.deepEqual(parseOverrides('{"calendar":"tab","todos":"window"}'), { calendar: 'tab', todos: 'window' })
  assert.deepEqual(parseOverrides('{"calendar":"inherit","bogus":"tab"}'), {})
  assert.deepEqual(parseOverrides('not json'), {})
  assert.deepEqual(parseOverrides('[1,2]'), {})
  assert.deepEqual(parseOverrides(''), {})
})

test('cleanParams keeps only known non-empty keys', () => {
  assert.deepEqual(cleanParams('todos', { id: 'a', projectId: '', junk: 'x' }), { id: 'a' })
  assert.deepEqual(cleanParams('file', { projectPath: '/p', path: 'a.ts', line: 12 }), {
    projectPath: '/p',
    path: 'a.ts',
    line: '12',
  })
  assert.deepEqual(cleanParams('calendar', { id: 'a' }), {})
  assert.deepEqual(cleanParams('slack', null), {})
})

test('one tab per surface, one per file for the file viewer', () => {
  assert.equal(surfaceTabId('todos', { id: 'a' }), surfaceTabId('todos', { id: 'b' }))
  assert.notEqual(
    surfaceTabId('file', { projectPath: '/p', path: 'a.ts' }),
    surfaceTabId('file', { projectPath: '/p', path: 'b.ts' }),
  )
  assert.equal(
    surfaceTabId('file', { projectPath: '/p', path: 'a.ts', line: '3' }),
    surfaceTabId('file', { projectPath: '/p', path: 'a.ts' }),
  )
})

test('surface paths round-trip', () => {
  assert.equal(surfaceForPath('/calendar'), 'calendar')
  assert.equal(surfaceForPath('/github-mentions'), 'mentions')
  assert.equal(surfaceForPath('/projects'), null)
  assert.deepEqual(surfaceLocation('radar', { project: 'p1', id: 'x' }), { path: '/radar', query: { project: 'p1' } })
})

test('only dropping to one monitor docks, and only when enabled', () => {
  assert.equal(shouldDockOnMonitorChange(2, 1, true), true)
  assert.equal(shouldDockOnMonitorChange(3, 1, true), true)
  assert.equal(shouldDockOnMonitorChange(3, 2, true), false)
  assert.equal(shouldDockOnMonitorChange(1, 2, true), false)
  assert.equal(shouldDockOnMonitorChange(2, 1, false), false)
})

test('the tab-intent flag rides in the query but never in the tab identity', () => {
  const loc = surfaceLocation('todos', { id: 'a' }, true)
  assert.equal(loc.query._tab, '1')
  assert.deepEqual(cleanParams('todos', loc.query), { id: 'a' })
  assert.equal(surfaceTabId('todos', cleanParams('todos', loc.query)), surfaceTabId('todos', { id: 'a' }))
})
