// Unit tests for lib/surfaces.ts (surface tabs: params, ids, routes).
// Run: node --test src/lib/__tests__/surfaces.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  cleanParams,
  shouldDockOnMonitorChange,
  surfaceForPath,
  surfaceLocation,
  surfaceTabId,
} from '../surfaces.ts'

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
