// Unit tests for lib/backgroundProcesses.ts (Processes screen formatting and
// grouping). Run: node --test src/lib/__tests__/backgroundProcesses.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  formatAge,
  formatCpu,
  formatRss,
  groupProcessCounts,
  matchesQuery,
  parseProcFilter,
  procSections,
  sortEntries,
  strayCount,
  strayIds,
  totals,
  trayCount,
  truncateCommand,
} from '../backgroundProcesses.ts'

const entry = (id, state, infra, rss_kb, procs = 1) => ({
  id,
  kind: infra ? 'translate' : 'run',
  state,
  label: id,
  run_id: infra ? null : `run-${id}`,
  project_id: null,
  project_name: null,
  created_at: '2026-10-26T00:00:00Z',
  ended_at: null,
  infra,
  processes: Array.from({ length: procs }, (_, i) => ({
    pid: i + 1, ppid: 1, pgid: 1, command: 'x', cpu: 0, rss_kb: 0, started_at: null, is_root: i === 0,
  })),
  cpu: 0,
  rss_kb,
})

test('formatRss scales KiB to KB / MB / GB', () => {
  assert.equal(formatRss(0), '0 KB')
  assert.equal(formatRss(-5), '0 KB')
  assert.equal(formatRss(512), '512 KB')
  assert.equal(formatRss(1024 * 3.5), '3.5 MB')
  assert.equal(formatRss(1024 * 345), '345 MB')
  assert.equal(formatRss(1024 * 1024 * 1.5), '1.50 GB')
  assert.equal(formatRss(1024 * 1024 * 12), '12.0 GB')
})

test('formatCpu keeps one decimal only for small values', () => {
  assert.equal(formatCpu(0), '0%')
  assert.equal(formatCpu(0.42), '0.4%')
  assert.equal(formatCpu(37.6), '38%')
  assert.equal(formatCpu(NaN), '0%')
})

test('formatAge renders compact durations', () => {
  const now = Date.parse('2026-10-26T12:00:00Z')
  assert.equal(formatAge(null, now), '')
  assert.equal(formatAge('garbage', now), '')
  assert.equal(formatAge('2026-10-26T11:59:15Z', now), '45s')
  assert.equal(formatAge('2026-10-26T11:48:00Z', now), '12m')
  assert.equal(formatAge('2026-10-26T09:00:00Z', now), '3h')
  assert.equal(formatAge('2026-10-26T08:55:00Z', now), '3h 5m')
  assert.equal(formatAge('2026-10-24T08:00:00Z', now), '2d 4h')
  assert.equal(formatAge('2026-10-24T12:00:00Z', now), '2d')
  // Clock skew: a start time in the future reads as 0s, not negative.
  assert.equal(formatAge('2026-10-26T12:00:30Z', now), '0s')
})

test('procSections groups leftover + orphan together, drops empty sections', () => {
  const list = [
    entry('a', 'active', false, 10),
    entry('b', 'active', true, 10),
    entry('c', 'leftover', false, 10),
    entry('d', 'leftover', true, 50),
    entry('e', 'orphan', true, 30),
    entry('f', 'active', false, 99),
  ]
  const all = procSections(list, 'all', '', 'rss')
  assert.deepEqual(all.map((s) => s.key), ['leftovers', 'sessions', 'services'])
  // Stray infra stays with the leftovers, heaviest first.
  assert.deepEqual(all[0].items.map((e) => e.id), ['d', 'e', 'c'])
  assert.deepEqual(all[1].items.map((e) => e.id), ['f', 'a'])
  assert.deepEqual(procSections(list, 'services', '', 'rss').map((s) => s.key), ['services'])
  assert.deepEqual(procSections(list, 'sessions', 'zzz', 'rss'), [])
})

test('matchesQuery looks at label, project, pid and command', () => {
  const e = { ...entry('Fix sidebar', 'active', false, 1), project_name: 'Devdy' }
  e.processes[0] = { ...e.processes[0], pid: 48211, command: 'node vite --port 1420' }
  assert.ok(matchesQuery(e, ''))
  assert.ok(matchesQuery(e, 'SIDEBAR'))
  assert.ok(matchesQuery(e, 'devdy'))
  assert.ok(matchesQuery(e, '4821'))
  assert.ok(matchesQuery(e, 'vite'))
  assert.ok(!matchesQuery(e, 'codex'))
})

test('sortEntries orders by cpu / procs / age, label breaks ties', () => {
  const a = { ...entry('a', 'active', false, 5, 1), cpu: 3, created_at: '2026-10-26T10:00:00Z' }
  const b = { ...entry('b', 'active', false, 5, 3), cpu: 9, created_at: '2026-10-26T11:00:00Z' }
  const c = { ...entry('c', 'active', false, 5, 1), cpu: 3, created_at: '2026-10-25T10:00:00Z' }
  assert.deepEqual(sortEntries([a, b, c], 'cpu').map((e) => e.id), ['b', 'a', 'c'])
  assert.deepEqual(sortEntries([a, b, c], 'procs').map((e) => e.id), ['b', 'a', 'c'])
  assert.deepEqual(sortEntries([a, b, c], 'age').map((e) => e.id), ['c', 'a', 'b'])
  assert.deepEqual(sortEntries([a, b, c], 'rss').map((e) => e.id), ['a', 'b', 'c'])
})

test('groupProcessCounts / parseProcFilter', () => {
  const list = [entry('a', 'active', false, 1, 2), entry('b', 'orphan', false, 1, 3), entry('c', 'active', true, 1)]
  assert.deepEqual(groupProcessCounts(list), { all: 6, leftovers: 3, sessions: 2, services: 1 })
  assert.equal(parseProcFilter('leftovers'), 'leftovers')
  assert.equal(parseProcFilter('nope'), 'all')
  assert.equal(parseProcFilter(undefined), 'all')
})

test('strayIds / totals / counts', () => {
  const list = [entry('a', 'active', false, 10, 2), entry('b', 'leftover', false, 20, 3), entry('c', 'orphan', true, 5)]
  assert.deepEqual(strayIds(list), ['b', 'c'])
  assert.deepEqual(totals(list), { processes: 6, rss_kb: 35 })
  const counts = { active: 2, leftover: 1, orphan: 3, total_processes: 9 }
  assert.equal(strayCount(counts), 4)
  assert.equal(trayCount(counts), 6)
})

test('truncateCommand clips with an ellipsis', () => {
  assert.equal(truncateCommand('  node server.js  '), 'node server.js')
  assert.equal(truncateCommand('abcdefghij', 5), 'abcd…')
})
