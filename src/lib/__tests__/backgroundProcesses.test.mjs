// Unit tests for lib/backgroundProcesses.ts (Processes screen formatting and
// grouping). Run: node --test src/lib/__tests__/backgroundProcesses.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  formatAge,
  formatCpu,
  formatRss,
  groupEntries,
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

test('groupEntries splits by state, separating infra from sessions', () => {
  const g = groupEntries([
    entry('a', 'active', false, 10),
    entry('b', 'active', true, 10),
    entry('c', 'leftover', false, 10),
    entry('d', 'leftover', true, 50),
    entry('e', 'orphan', true, 10),
    entry('f', 'active', false, 99),
  ])
  assert.deepEqual(g.sessions.map((e) => e.id), ['f', 'a'])
  assert.deepEqual(g.services.map((e) => e.id), ['b'])
  // Stray infra stays in the stray sections, heaviest first.
  assert.deepEqual(g.leftover.map((e) => e.id), ['d', 'c'])
  assert.deepEqual(g.orphan.map((e) => e.id), ['e'])
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
