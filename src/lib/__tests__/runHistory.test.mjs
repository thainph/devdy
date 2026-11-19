// Unit tests for lib/runHistory.ts (Dashboard History: names, times, paging).
// Run: node --test src/lib/__tests__/runHistory.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  appendHistory,
  historyAgo,
  historyExact,
  historyKind,
  historyRepoRef,
  historyTitle,
  historyType,
  historyWebUrl,
  isOlder,
  mergeHistory,
} from '../runHistory.ts'

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
  protected: false,
  unread: false,
  accountName: null,
  repoProvider: null,
  repoPath: null,
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

test('historyAgo: now, minutes / hours today, yesterday, days, then a date', () => {
  const now = new Date(2026, 10, 19, 16, 30).getTime()
  const at = (...a) => new Date(2026, ...a).toISOString()
  assert.deepEqual(historyAgo(at(10, 19, 16, 29, 30), now), { key: 'now', n: 0, date: '19/11' })
  assert.equal(historyAgo(at(10, 19, 16, 40), now).key, 'now', 'clock skew')
  assert.deepEqual(historyAgo(at(10, 19, 16, 25), now), { key: 'minutes', n: 5, date: '19/11' })
  assert.deepEqual(historyAgo(at(10, 19, 9, 0), now), { key: 'hours', n: 7, date: '19/11' })
  assert.deepEqual(historyAgo(at(10, 18, 23, 59), now), { key: 'yesterday', n: 1, date: '18/11' })
  assert.deepEqual(historyAgo(at(10, 12, 10, 0), now), { key: 'days', n: 7, date: '12/11' })
  assert.deepEqual(historyAgo(at(10, 11, 10, 0), now), { key: 'date', n: 0, date: '11/11' })
  assert.equal(historyAgo('nope', now), null)
  assert.equal(historyExact(at(10, 18, 9, 5)), '18/11/2026 09:05')
})

test('historyType / historyRepoRef / historyWebUrl', () => {
  assert.equal(historyType({ runType: 'review_pr', role: null, workerCount: 0 }), 'pr')
  assert.equal(historyType({ runType: 'analyze_issue', role: null, workerCount: 0 }), 'issue')
  assert.equal(historyType({ runType: 'session', role: 'conductor', workerCount: 0 }), 'conductor')
  assert.equal(historyType({ runType: 'session', role: null, workerCount: 2 }), 'conductor')
  assert.equal(historyType({ runType: 'session', role: null, workerCount: 0 }), 'session')

  const pr = { runType: 'review_pr', repoPath: 'malme/api', repoProvider: 'github', refNumber: 412 }
  assert.equal(historyRepoRef(pr), 'malme/api#412')
  assert.equal(historyWebUrl(pr), 'https://github.com/malme/api/pull/412')
  assert.equal(historyWebUrl({ ...pr, runType: 'analyze_issue' }), 'https://github.com/malme/api/issues/412')
  assert.equal(historyWebUrl({ ...pr, repoProvider: 'gitlab', repoPath: 'g/sub/api' }), 'https://gitlab.com/g/sub/api/-/merge_requests/412')
  assert.equal(historyWebUrl({ ...pr, repoProvider: 'gitlab', repoPath: 'g/api', runType: 'analyze_issue' }), 'https://gitlab.com/g/api/-/issues/412')
  assert.equal(historyRepoRef({ ...pr, runType: 'session' }), null)
  assert.equal(historyWebUrl({ ...pr, repoPath: null }), null)
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
