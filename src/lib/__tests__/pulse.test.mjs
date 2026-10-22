// Unit tests for lib/pulse.ts (Pulse screen labels, filters, row split, source
// refs). Run: node --test src/lib/__tests__/pulse.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  ballAge,
  dueRelative,
  filterProjects,
  githubIssueRef,
  pulseErrorCode,
  redProjectCount,
  shortDate,
  slackSourceRef,
  sourceKey,
  splitRows,
  watchIndex,
} from '../pulse.ts'

const local = (y, m, d, h = 0, min = 0, s = 0) => new Date(y, m - 1, d, h, min, s)
const NOW = local(2026, 10, 22, 9).getTime()

test('dueRelative counts local calendar days', () => {
  assert.deepEqual(dueRelative(local(2026, 10, 22, 23, 59, 59).toISOString(), NOW), { kind: 'today' })
  assert.deepEqual(dueRelative(local(2026, 10, 23, 0, 30).toISOString(), NOW), { kind: 'left', days: 1 })
  assert.deepEqual(dueRelative(local(2026, 10, 30, 23, 59, 59).toISOString(), NOW), { kind: 'left', days: 8 })
  assert.deepEqual(dueRelative(local(2026, 10, 20, 23, 59, 59).toISOString(), NOW), { kind: 'late', days: 2 })
  // Earlier today and already passed: late by 0 days, not "today".
  assert.deepEqual(dueRelative(local(2026, 10, 22, 8).toISOString(), NOW), { kind: 'late', days: 0 })
  assert.equal(dueRelative('soon', NOW), null)
})

test('shortDate is dd/MM local', () => {
  assert.equal(shortDate(local(2026, 10, 30, 23, 59, 59).toISOString()), '30/10')
  assert.equal(shortDate(local(2026, 1, 5).toISOString()), '05/01')
  assert.equal(shortDate('x'), '')
})

test('ballAge: hours under two days, then days', () => {
  const ago = (h) => new Date(NOW - h * 3_600_000).toISOString()
  assert.deepEqual(ballAge(ago(30), NOW), { unit: 'h', value: 30 })
  assert.deepEqual(ballAge(ago(0.5), NOW), { unit: 'h', value: 0 })
  assert.deepEqual(ballAge(ago(47.9), NOW), { unit: 'h', value: 47 })
  assert.deepEqual(ballAge(ago(48), NOW), { unit: 'd', value: 2 })
  assert.deepEqual(ballAge(ago(6 * 24 + 5), NOW), { unit: 'd', value: 6 })
  assert.equal(ballAge(null, NOW), null)
  assert.equal(ballAge('nope', NOW), null)
})

const watch = (id, extra = {}) => ({ id, finished: false, sourceType: 'github_issue', sourceRef: `org/app#${id}`, ...extra })

test('splitRows keeps 5 open rows, counts the rest, and groups finished ones', () => {
  const ws = [1, 2, 3, 4, 5, 6, 7].map((n) => watch(String(n)))
  ws.push(watch('d1', { finished: true }), watch('d2', { finished: true }))
  const r = splitRows(ws)
  assert.deepEqual(r.shown.map((w) => w.id), ['1', '2', '3', '4', '5'])
  assert.equal(r.more, 2)
  assert.deepEqual(r.finished.map((w) => w.id), ['d1', 'd2'])
  assert.deepEqual(splitRows([watch('a')]), { shown: [watch('a')], more: 0, finished: [] })
})

const project = (name, health, watches = []) => ({ projectId: name, projectName: name, health, watches })

test('filterProjects: accent-insensitive search and attention-only', () => {
  const ps = [project('Dự án Đà Nẵng', 'red'), project('Alpha', 'green'), project('Beta', 'yellow'), project('Gamma', 'none')]
  assert.deepEqual(filterProjects(ps, 'da nang', false).map((p) => p.projectName), ['Dự án Đà Nẵng'])
  assert.deepEqual(filterProjects(ps, '', true).map((p) => p.projectName), ['Dự án Đà Nẵng', 'Beta'])
  assert.deepEqual(filterProjects(ps, 'a', true).map((p) => p.projectName), ['Dự án Đà Nẵng', 'Beta'])
  assert.equal(filterProjects(ps, '  ', false).length, 4)
  assert.equal(redProjectCount(ps), 1)
})

test('source refs match the backend normalisation', () => {
  assert.equal(slackSourceRef('C0B83GHDTFG', '1789877283.026359'), 'C0B83GHDTFG:1789877283.026359')
  assert.equal(slackSourceRef(null, '1.2'), null)
  assert.equal(githubIssueRef('https://github.com/Malme-Dev/App/issues/12'), 'malme-dev/app#12')
  assert.equal(githubIssueRef('https://github.com/org/app.web/issues/0012#issuecomment-1'), 'org/app.web#12')
  assert.equal(githubIssueRef('https://github.com/org/app/pull/12'), null)
  assert.equal(githubIssueRef('https://github.com/org/app/issues/0'), null)
  assert.equal(githubIssueRef(null), null)
})

test('watchIndex keys active watches by type and ref', () => {
  const w = watch('1')
  const idx = watchIndex([project('A', 'green', [w])])
  assert.equal(idx.get(sourceKey('github_issue', 'org/app#1')), w)
  assert.equal(idx.size, 1)
})

test('pulseErrorCode splits "<code>: <detail>"', () => {
  assert.deepEqual(pulseErrorCode('watched_in_other_project: this source is already watched in project Alpha'), {
    code: 'watched_in_other_project',
    detail: 'this source is already watched in project Alpha',
  })
  assert.deepEqual(pulseErrorCode(new Error('boom')), { code: null, detail: 'boom' })
})
