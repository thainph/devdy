// Unit tests for lib/runHistory.ts (Dashboard History: names, times, paging).
// Run: node --test src/lib/__tests__/runHistory.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { appendHistory, historyKind, historyTitle, historyWhen, isOlder, mergeHistory } from '../runHistory.ts'

const run = (runId, lastActivityAt, extra = {}) => ({
  runId,
  projectId: 'p',
  projectName: 'proj',
  runType: 'session',
  refNumber: null,
  title: `Run ${runId}`,
  refTitle: null,
  status: 'done',
  engine: 'claude',
  role: null,
  workerCount: 0,
  pinned: false,
  unread: false,
  lastActivityAt,
  ...extra,
})

test('historyTitle: title, else the issue / PR, else null', () => {
  assert.equal(historyTitle({ title: '  Fix CI ', refTitle: null, runType: 'session', refNumber: null }), 'Fix CI')
  assert.equal(historyTitle({ title: null, refTitle: 'Broken login', runType: 'review_pr', refNumber: 12 }), '#12 Broken login')
  assert.equal(historyTitle({ title: '', refTitle: null, runType: 'analyze_issue', refNumber: 7 }), 'Issue #7')
  assert.equal(historyTitle({ title: null, refTitle: null, runType: 'session', refNumber: null }), null)
  assert.equal(historyKind({ runType: 'review_pr' }), 'PR')
  assert.equal(historyKind({ runType: 'session' }), null)
})

test('historyWhen: time today, date + time otherwise', () => {
  const now = new Date(2026, 10, 19, 16, 30).getTime()
  assert.equal(historyWhen(new Date(2026, 10, 19, 9, 5).toISOString(), now), '09:05')
  assert.equal(historyWhen(new Date(2026, 10, 18, 23, 59).toISOString(), now), '18/11 23:59')
  assert.equal(historyWhen('nope', now), '')
})

test('isOlder follows the backend order (activity desc, id desc)', () => {
  assert.equal(isOlder(run('a', '2026-11-10'), run('b', '2026-11-11')), true)
  assert.equal(isOlder(run('a', '2026-11-11'), run('b', '2026-11-11')), true)
  assert.equal(isOlder(run('c', '2026-11-11'), run('b', '2026-11-11')), false)
})

test('mergeHistory: the fresh head wins, older loaded rows stay after it', () => {
  const loaded = [run('d', '2026-11-14'), run('c', '2026-11-13'), run('b', '2026-11-12'), run('a', '2026-11-11')]
  // "b" was resumed: it moves to the top and its old row goes; "c" and "a" lie past the head.
  const head = [run('b', '2026-11-15', { status: 'running' }), run('d', '2026-11-14')]
  const merged = mergeHistory(loaded, head)
  assert.deepEqual(merged.map((r) => r.runId), ['b', 'd', 'c', 'a'])
  // A run newer than the head's tail that the head no longer has (deleted) is dropped.
  assert.deepEqual(mergeHistory([run('x', '2026-11-16'), ...loaded], head).map((r) => r.runId), ['b', 'd', 'c', 'a'])
  assert.equal(merged[0].status, 'running')
  assert.deepEqual(mergeHistory(loaded, head, true).map((r) => r.runId), ['b', 'd'])
  assert.deepEqual(mergeHistory(loaded, []).map((r) => r.runId), ['d', 'c', 'b', 'a'])
})

test('appendHistory skips runs already listed', () => {
  const loaded = [run('b', '2026-11-12')]
  assert.deepEqual(appendHistory(loaded, [run('b', '2026-11-12'), run('a', '2026-11-11')]).map((r) => r.runId), ['b', 'a'])
})
