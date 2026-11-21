// Unit tests for lib/workspaceState.ts (tab strip data: split pair, order, persistence).
// Run: node --test src/lib/__tests__/workspaceState.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  canPair,
  clampRatio,
  DEFAULT_SPLIT_RATIO,
  entryTargetFor,
  entryTargetKey,
  moveUnit,
  parseWorkspaceState,
  placePair,
  replacePairSide,
  sanitizeRatio,
  serializeWorkspaceState,
  stripUnits,
  tabEntryKey,
} from '../workspaceState.ts'
import { surfaceForPath } from '../surfaces.ts'

const proj = (projectId) => ({ type: 'project', projectId, lastRunId: null })
const page = (path) => ({ type: 'page', id: `page:${path}`, path, fullPath: path })
const todos = { type: 'surface', id: 'surface:todos', surface: 'todos', params: {} }

test('canPair: never a tab with itself, never two sessions', () => {
  assert.equal(canPair(proj('a'), page('/dashboard')), true)
  assert.equal(canPair(page('/skills'), page('/dashboard')), true)
  assert.equal(canPair(todos, todos), false)
  assert.equal(canPair(proj('a'), proj('b')), false)
})

test('placePair moves the right tab next to the left one', () => {
  assert.deepEqual(placePair(['a', 'b', 'c', 'd'], { left: 'b', right: 'd' }), ['a', 'b', 'd', 'c'])
  assert.deepEqual(placePair(['d', 'a', 'b'], { left: 'b', right: 'd' }), ['a', 'b', 'd'])
  // Unknown keys: untouched.
  assert.deepEqual(placePair(['a', 'b'], { left: 'b', right: 'x' }), ['a', 'b'])
})

test('stripUnits groups the adjacent pair into one unit', () => {
  assert.deepEqual(stripUnits(['a', 'b', 'c'], null), [[0], [1], [2]])
  assert.deepEqual(stripUnits(['a', 'b', 'c'], { left: 'b', right: 'c' }), [[0], [1, 2]])
  // Not adjacent (shouldn't happen): plain tabs.
  assert.deepEqual(stripUnits(['b', 'a', 'c'], { left: 'b', right: 'c' }), [[0], [1], [2]])
})

test('moveUnit drags the pair as one unit', () => {
  const pair = { left: 'b', right: 'c' }
  assert.deepEqual(moveUnit(['a', 'b', 'c', 'd'], pair, 1, 0), ['b', 'c', 'a', 'd'])
  assert.deepEqual(moveUnit(['a', 'b', 'c', 'd'], pair, 1, 2), ['a', 'd', 'b', 'c'])
  assert.deepEqual(moveUnit(['a', 'b', 'c', 'd'], pair, 0, 2), ['b', 'c', 'd', 'a'])
  assert.deepEqual(moveUnit(['a', 'b', 'c', 'd'], pair, 0, 9), ['a', 'b', 'c', 'd'])
})

test('replacePairSide swaps one half and keeps the old one right after the pair', () => {
  const pair = { left: 'b', right: 'c' }
  assert.deepEqual(replacePairSide(['a', 'b', 'c', 'd', 'e'], pair, 'left', 'e'), {
    keys: ['a', 'e', 'c', 'b', 'd'],
    pair: { left: 'e', right: 'c' },
  })
  assert.deepEqual(replacePairSide(['e', 'a', 'b', 'c', 'd'], pair, 'right', 'e'), {
    keys: ['a', 'b', 'e', 'c', 'd'],
    pair: { left: 'b', right: 'e' },
  })
  // The other half can't also be this half.
  assert.equal(replacePairSide(['a', 'b', 'c'], pair, 'left', 'c'), null)
  // Same tab: unchanged.
  assert.deepEqual(replacePairSide(['a', 'b', 'c'], pair, 'right', 'c').pair, pair)
})

test('clampRatio keeps both minimum widths', () => {
  assert.equal(clampRatio(0.5, 1000), 0.5)
  assert.equal(clampRatio(0.1, 1000), 0.36)
  assert.ok(Math.abs(clampRatio(0.9, 1000) - 0.68) < 1e-9)
  // Too narrow for both minimums: proportional.
  assert.equal(clampRatio(0.5, 500), 360 / 680)
  assert.equal(clampRatio(Number.NaN, 1000), DEFAULT_SPLIT_RATIO)
})

test('clampRatio clamps a drag past an edge instead of snapping to 50/50', () => {
  // Near / past the left edge (drag, Home = 0): the left minimum.
  assert.equal(clampRatio(0.02, 1000), 0.36)
  assert.equal(clampRatio(0, 1000), 0.36)
  assert.equal(clampRatio(-0.4, 1000), 0.36)
  // Near / past the right edge (drag, End = 1): the right minimum.
  assert.ok(Math.abs(clampRatio(0.98, 1000) - 0.68) < 1e-9)
  assert.ok(Math.abs(clampRatio(1, 1000) - 0.68) < 1e-9)
  assert.ok(Math.abs(clampRatio(3, 1000) - 0.68) < 1e-9)
  // A wide window allows more than 95% — and that survives a reload.
  const wide = clampRatio(1, 10000)
  assert.ok(Math.abs(wide - 0.968) < 1e-9)
  assert.equal(parseWorkspaceState(serializeWorkspaceState({ entries: [], pair: null, ratio: wide })).ratio, wide)
  // Unknown width: clamped into (0, 1) but never reset for a finite edge value.
  assert.equal(clampRatio(0.3, 0), 0.3)
  assert.equal(clampRatio(Number.POSITIVE_INFINITY, 1000), DEFAULT_SPLIT_RATIO)
})

test('sanitizeRatio accepts any persisted share strictly between 0 and 1', () => {
  assert.equal(sanitizeRatio(0.97), 0.97)
  assert.equal(sanitizeRatio(0.03), 0.03)
  assert.equal(sanitizeRatio(0), DEFAULT_SPLIT_RATIO)
  assert.equal(sanitizeRatio(1), DEFAULT_SPLIT_RATIO)
  assert.equal(sanitizeRatio('0.4'), DEFAULT_SPLIT_RATIO)
  assert.equal(sanitizeRatio(Number.NaN), DEFAULT_SPLIT_RATIO)
})

test('parseWorkspaceState migrates the v1 bare array (no split saved)', () => {
  const v1 = JSON.stringify([
    { projectId: 'p1', lastRunId: 'r1' },
    { type: 'page', path: '/skills', fullPath: '/skills?id=x' },
    { type: 'surface', surface: 'todos', params: { id: 't1', junk: 1 } },
    { type: 'surface', surface: 'nope' },
    { type: 'project', projectId: 'p1' },
    null,
  ])
  const s = parseWorkspaceState(v1)
  assert.deepEqual(s.entries.map(tabEntryKey), ['project:p1', 'page:/skills', 'surface:todos'])
  assert.equal(s.entries[0].lastRunId, 'r1')
  assert.equal(s.entries[1].fullPath, '/skills?id=x')
  assert.deepEqual(s.entries[2].params, { id: 't1' })
  assert.equal(s.pair, null)
  assert.equal(s.ratio, DEFAULT_SPLIT_RATIO)
})

test('parseWorkspaceState v2 round-trips and re-validates the pair', () => {
  const entries = [proj('p1'), page('/dashboard'), todos]
  const raw = serializeWorkspaceState({ entries, pair: { left: 'project:p1', right: 'surface:todos' }, ratio: 0.6 })
  const s = parseWorkspaceState(raw)
  assert.equal(s.ratio, 0.6)
  assert.deepEqual(s.pair, { left: 'project:p1', right: 'surface:todos' })
  // The pair is made adjacent on load.
  assert.deepEqual(s.entries.map(tabEntryKey), ['project:p1', 'surface:todos', 'page:/dashboard'])

  const bad = JSON.stringify({ version: 2, entries: [proj('a'), proj('b')], pair: { left: 'project:a', right: 'project:b' } })
  assert.equal(parseWorkspaceState(bad).pair, null)
  const gone = JSON.stringify({ version: 2, entries: [proj('a')], pair: { left: 'project:a', right: 'page:/x' }, ratio: 7 })
  assert.equal(parseWorkspaceState(gone).pair, null)
  assert.equal(parseWorkspaceState(gone).ratio, DEFAULT_SPLIT_RATIO)
  assert.deepEqual(parseWorkspaceState('{oops').entries, [])
  assert.deepEqual(parseWorkspaceState(null).entries, [])
})

test('entryTargetFor maps routes to tabs', () => {
  const isPage = (p) => p === '/dashboard' || p === '/skills'
  const r = (name, path, params = {}, query = {}, fullPath = path) => ({ name, path, params, query, fullPath })
  const run = entryTargetFor(r('project-run-detail', '/projects/p/run/x', { projectId: 'p', runId: 'x' }), surfaceForPath, isPage)
  assert.deepEqual(run, { type: 'project', projectId: 'p', runId: 'x' })
  assert.equal(entryTargetKey(run), 'project:p')
  const s = entryTargetFor(r(undefined, '/todos', {}, { id: '1', _tab: '1' }), surfaceForPath, isPage)
  assert.deepEqual(s, { type: 'surface', surface: 'todos', params: { id: '1' } })
  assert.equal(entryTargetKey(s), 'surface:todos')
  const p = entryTargetFor(r('skills', '/skills', {}, { id: 'a' }, '/skills?id=a'), surfaceForPath, isPage)
  assert.deepEqual(p, { type: 'page', path: '/skills', fullPath: '/skills?id=a' })
  assert.equal(entryTargetKey(p), 'page:/skills')
  // A project's settings is not a tab.
  assert.equal(entryTargetFor(r('project-settings', '/projects/p/settings', { projectId: 'p' }), surfaceForPath, isPage), null)
})
