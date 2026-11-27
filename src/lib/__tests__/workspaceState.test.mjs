// Unit tests for lib/workspaceState.ts (tab strip data: split view, persistence).
// Run: node --test src/lib/__tests__/workspaceState.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  clampRatio,
  DEFAULT_SPLIT_RATIO,
  entryTargetFor,
  entryTargetKey,
  parseWorkspaceState,
  pickSession,
  sanitizeRatio,
  serializeWorkspaceState,
  splitSide,
  tabEntryKey,
  withOwnRun,
} from '../workspaceState.ts'
import { surfaceForPath } from '../surfaces.ts'

const proj = (projectId) => ({ type: 'project', projectId, lastRunId: null })
const page = (path) => ({ type: 'page', id: `page:${path}`, path, fullPath: path })
const todos = { type: 'surface', id: 'surface:todos', surface: 'todos', params: {} }

test('splitSide: sessions on the left, every other tab on the right', () => {
  assert.equal(splitSide(proj('a')), 'left')
  assert.equal(splitSide(page('/dashboard')), 'right')
  assert.equal(splitSide(todos), 'right')
})

test('pickSession: the preferred session while open, else the first one', () => {
  const entries = [page('/dashboard'), proj('a'), todos, proj('b')]
  assert.equal(pickSession(entries, 'project:b'), 'project:b')
  assert.equal(pickSession(entries, 'project:gone'), 'project:a')
  assert.equal(pickSession(entries, null), 'project:a')
  // Never a non-session tab.
  assert.equal(pickSession(entries, 'surface:todos'), 'project:a')
  assert.equal(pickSession([page('/dashboard'), todos], null), null)
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
  assert.equal(parseWorkspaceState(serializeWorkspaceState({ entries: [], split: false, left: null, right: null, ratio: wide })).ratio, wide)
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
  assert.equal(s.split, false)
  assert.equal(s.left, null)
  assert.equal(s.right, null)
  assert.equal(s.ratio, DEFAULT_SPLIT_RATIO)
})

test('parseWorkspaceState v3 round-trips and re-validates the halves', () => {
  const entries = [proj('p1'), page('/dashboard'), todos]
  const raw = serializeWorkspaceState({ entries, split: true, left: 'project:p1', right: 'surface:todos', ratio: 0.6 })
  const s = parseWorkspaceState(raw)
  assert.equal(s.ratio, 0.6)
  assert.equal(s.split, true)
  assert.equal(s.left, 'project:p1')
  assert.equal(s.right, 'surface:todos')
  // Strip order is the user's.
  assert.deepEqual(s.entries.map(tabEntryKey), ['project:p1', 'page:/dashboard', 'surface:todos'])

  // A session can't be on the right, nor another tab on the left.
  const swapped = JSON.stringify({ version: 3, entries, split: true, left: 'surface:todos', right: 'project:p1' })
  assert.equal(parseWorkspaceState(swapped).left, null)
  assert.equal(parseWorkspaceState(swapped).right, null)
  // …but split stays on while some session can take the left half.
  assert.equal(parseWorkspaceState(swapped).split, true)
  // No session at all: never split.
  const noSession = JSON.stringify({ version: 3, entries: [todos], split: true, right: 'surface:todos' })
  assert.equal(parseWorkspaceState(noSession).split, false)
  // Gone tabs and bad values.
  const gone = JSON.stringify({ version: 3, entries: [proj('a')], split: 'yes', left: 'project:x', right: 'page:/x', ratio: 7 })
  const g = parseWorkspaceState(gone)
  assert.deepEqual([g.split, g.left, g.right, g.ratio], [false, null, null, DEFAULT_SPLIT_RATIO])
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

test('withOwnRun drops a remembered run filed under another project', () => {
  const owners = new Map([['r1', 'p1'], ['r2', 'p2']])
  const ownerOf = (id) => owners.get(id)
  const own = { type: 'project', projectId: 'p1', lastRunId: 'r1' }
  assert.equal(withOwnRun(own, ownerOf), own)
  assert.deepEqual(withOwnRun({ type: 'project', projectId: 'p1', lastRunId: 'r2' }, ownerOf), {
    type: 'project',
    projectId: 'p1',
    lastRunId: null,
  })
  // Unknown owner (not loaded yet): kept.
  const unknown = { type: 'project', projectId: 'p1', lastRunId: 'r9' }
  assert.equal(withOwnRun(unknown, ownerOf), unknown)
  assert.equal(withOwnRun(todos, ownerOf), todos)
})

test('parseWorkspaceState migrates a v2 pair with a session half into the split', () => {
  const entries = [todos, proj('p1'), page('/dashboard')]
  // Either order: the session goes left, the other tab right.
  const shown = parseWorkspaceState(
    JSON.stringify({ version: 2, entries, pair: { left: 'surface:todos', right: 'project:p1' }, pairShown: true, ratio: 0.5 }),
  )
  assert.deepEqual([shown.split, shown.left, shown.right], [true, 'project:p1', 'surface:todos'])
  // A pair waiting on the strip: split off, halves remembered.
  const waiting = parseWorkspaceState(
    JSON.stringify({ version: 2, entries, pair: { left: 'project:p1', right: 'surface:todos' }, pairShown: false }),
  )
  assert.deepEqual([waiting.split, waiting.left, waiting.right], [false, 'project:p1', 'surface:todos'])
  // No session half: the pair is dropped.
  const noSession = parseWorkspaceState(
    JSON.stringify({ version: 2, entries, pair: { left: 'page:/dashboard', right: 'surface:todos' }, pairShown: true }),
  )
  assert.deepEqual([noSession.split, noSession.left, noSession.right], [false, null, null])
})
