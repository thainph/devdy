// Unit tests for lib/slackListView.ts (the Slack threads sidebar's filter menu).
// Run: node --test src/lib/__tests__/slackListView.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  SLACK_NO_PROJECT,
  clearSlackFilters,
  defaultSlackListView,
  isSlackFiltering,
  matchesSlackView,
  parseSlackSort,
  slackActivityCounts,
  slackFilterSummary,
  slackViewDiffersFromDefault,
  sortSlackThreads,
} from '../slackListView.ts'

const NOW = Date.parse('2026-11-21T12:00:00Z')
const HOUR = 60 * 60 * 1000
const iso = (ms) => new Date(ms).toISOString()

const th = (id, projectId, over = {}) => ({
  id,
  projectId,
  title: id,
  newCount: 0,
  lastReplyAt: null,
  updatedAt: iso(NOW - 48 * HOUR),
  ...over,
})

const THREADS = [
  th('a', 'p1', { newCount: 2, lastReplyAt: iso(NOW - HOUR) }),
  th('b', 'p1'),
  th('c', 'p2', { newCount: 1 }),
  th('d', null, { lastReplyAt: iso(NOW - 2 * HOUR) }),
]

test('defaults: no filter, latest activity first', () => {
  assert.deepEqual(defaultSlackListView(), { activity: '', project: '', sort: 'recent' })
  assert.equal(slackViewDiffersFromDefault(defaultSlackListView()), false)
})

test('parseSlackSort keeps only known sorts', () => {
  assert.equal(parseSlackSort('channel'), 'channel')
  assert.equal(parseSlackSort('title'), 'title')
  assert.equal(parseSlackSort('bogus'), 'recent')
  assert.equal(parseSlackSort(null), 'recent')
})

test('filtering vs. differs from defaults', () => {
  const v = defaultSlackListView()
  assert.equal(isSlackFiltering({ ...v, activity: 'unread' }), true)
  assert.equal(isSlackFiltering({ ...v, project: 'p1' }), true)
  assert.equal(isSlackFiltering({ ...v, sort: 'title' }), false)
  assert.equal(slackViewDiffersFromDefault({ ...v, sort: 'title' }), true)
  assert.deepEqual(clearSlackFilters({ activity: 'unread', project: 'p1', sort: 'title' }), {
    activity: '',
    project: '',
    sort: 'title',
  })
})

test('matchesSlackView: project, no project, activity, kept open thread', () => {
  const v = defaultSlackListView()
  const ids = (view, keep = null) => THREADS.filter((x) => matchesSlackView(x, view, NOW, keep)).map((x) => x.id)
  assert.deepEqual(ids(v), ['a', 'b', 'c', 'd'])
  assert.deepEqual(ids({ ...v, project: 'p1' }), ['a', 'b'])
  assert.deepEqual(ids({ ...v, project: SLACK_NO_PROJECT }), ['d'])
  assert.deepEqual(ids({ ...v, activity: 'unread' }), ['a', 'c'])
  assert.deepEqual(ids({ ...v, activity: 'recent' }), ['a', 'd'])
  // The open thread stays under the activity filter, not under the project one.
  assert.deepEqual(ids({ ...v, activity: 'unread' }, 'b'), ['a', 'b', 'c'])
  assert.deepEqual(ids({ ...v, project: 'p2' }, 'b'), ['c'])
})

test('slackActivityCounts: project in scope, before the activity filter', () => {
  const v = defaultSlackListView()
  assert.deepEqual(slackActivityCounts(THREADS, v, NOW), { '': 4, unread: 2, recent: 2 })
  assert.deepEqual(slackActivityCounts(THREADS, { ...v, project: 'p1', activity: 'unread' }, NOW), {
    '': 2,
    unread: 1,
    recent: 1,
  })
})

test('sortSlackThreads: recent, title (untitled last), channel keeps order', () => {
  const list = [th('x', null, { title: 'b' }), th('y', null, { title: '', lastReplyAt: iso(NOW) }), th('z', null, { title: 'a' })]
  assert.deepEqual(sortSlackThreads(list, 'recent').map((x) => x.id)[0], 'y')
  assert.deepEqual(sortSlackThreads(list, 'title').map((x) => x.id), ['z', 'x', 'y'])
  assert.deepEqual(sortSlackThreads(list, 'channel').map((x) => x.id), ['x', 'y', 'z'])
})

test('slackFilterSummary: activity then project', () => {
  const labels = { activity: (a) => `A:${a}`, project: (p) => `P:${p}` }
  assert.deepEqual(slackFilterSummary(defaultSlackListView(), labels), [])
  assert.deepEqual(slackFilterSummary({ activity: 'unread', project: 'p1', sort: 'title' }, labels), ['A:unread', 'P:p1'])
})
