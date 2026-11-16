// Unit tests for lib/remoteInbox.ts (Remote Controller Inbox: pending queues,
// cross-run aggregation, recent sessions).
// Run: node --test src/lib/__tests__/remoteInbox.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  aggregatePending,
  dequeuePending,
  dropRun,
  enqueuePending,
  agoParts,
  finishedAt,
  finishedKind,
  createSyncGate,
  headOf,
  isUnread,
  parseHostTime,
  pendingCount,
  RECENT_MAX,
  RECENT_WINDOW_MS,
  recentRuns,
  retainPending,
} from '../remoteInbox.ts'

const req = (run_id, request_id, tool_name = 'Bash') => ({ run_id, request_id, tool_name })

test('enqueuePending keeps arrival order and replaces a re-sent id in place', () => {
  let q = {}
  q = enqueuePending(q, req('r1', 'a'))
  q = enqueuePending(q, req('r1', 'b'))
  q = enqueuePending(q, req('r2', 'c'))
  q = enqueuePending(q, req('r1', 'a', 'Edit'))
  assert.deepEqual(q.r1.map((p) => p.request_id), ['a', 'b'])
  assert.equal(q.r1[0].tool_name, 'Edit')
  assert.equal(pendingCount(q), 3)
})

test('dequeuePending removes one prompt and drops an emptied run', () => {
  let q = enqueuePending(enqueuePending({}, req('r1', 'a')), req('r1', 'b'))
  q = dequeuePending(q, 'r1', 'a')
  assert.deepEqual(q.r1.map((p) => p.request_id), ['b'])
  q = dequeuePending(q, 'r1', 'b')
  assert.equal('r1' in q, false)
  // Unknown ids are a no-op (same object back).
  assert.equal(dequeuePending(q, 'r9', 'x'), q)
})

test('dropRun removes a whole run', () => {
  const q = enqueuePending(enqueuePending({}, req('r1', 'a')), req('r2', 'b'))
  assert.deepEqual(Object.keys(dropRun(q, 'r1')), ['r2'])
  assert.equal(dropRun(q, 'r9'), q)
})

test('headOf picks the oldest prompt of each run', () => {
  const q = enqueuePending(enqueuePending({}, req('r1', 'a')), req('r1', 'b'))
  assert.equal(headOf(q).r1.request_id, 'a')
})

test('retainPending drops prompts the Host no longer reports', () => {
  let q = enqueuePending(enqueuePending({}, req('r1', 'a')), req('r2', 'b'))
  q = retainPending(q, new Set(['b']))
  assert.deepEqual(Object.keys(q), ['r2'])
})

test('aggregatePending flattens every run, oldest seen first', () => {
  let q = {}
  q = enqueuePending(q, req('r1', 'a'))
  q = enqueuePending(q, req('r2', 'b'))
  q = enqueuePending(q, req('r1', 'c'))
  const order = aggregatePending(q, { a: 30, b: 10, c: 20 }).map((p) => p.request_id)
  assert.deepEqual(order, ['b', 'c', 'a'])
  // Unknown seen times sort last, keeping their relative order.
  assert.deepEqual(aggregatePending(q, { c: 1 }).map((p) => p.request_id), ['c', 'a', 'b'])
})

test('parseHostTime handles RFC 3339 and SQLite UTC strings', () => {
  assert.equal(parseHostTime('2026-10-31T10:00:00Z'), Date.UTC(2026, 9, 31, 10))
  assert.equal(parseHostTime('2026-10-31T19:00:00+09:00'), Date.UTC(2026, 9, 31, 10))
  assert.equal(parseHostTime('2026-10-31 10:00:00'), Date.UTC(2026, 9, 31, 10))
  assert.equal(parseHostTime(''), null)
  assert.equal(parseHostTime(null), null)
  assert.equal(parseHostTime('nope'), null)
})

test('finishedKind splits success / failure and ignores the rest', () => {
  assert.equal(finishedKind('done'), 'success')
  assert.equal(finishedKind('completed'), 'success')
  assert.equal(finishedKind('failed'), 'failure')
  assert.equal(finishedKind('error'), 'failure')
  assert.equal(finishedKind('running'), null)
  assert.equal(finishedKind('cancelled'), null)
})

const HOUR = 60 * 60 * 1000
const NOW = Date.parse('2026-11-16T12:00:00Z')
const iso = (ms) => new Date(ms).toISOString()
const run = (id, over = {}) => ({
  id,
  status: 'done',
  created_at: iso(NOW - 100 * HOUR),
  updated_at: iso(NOW - HOUR),
  finished_at: iso(NOW - HOUR),
  seen_at: iso(NOW - HOUR),
  ...over,
})

test('finishedAt only for a finished run', () => {
  assert.equal(finishedAt(run('a')), NOW - HOUR)
  assert.equal(finishedAt(run('a', { status: 'running', finished_at: null })), null)
  assert.equal(finishedAt(run('a', { status: 'cancelled' })), null)
})

test('isUnread: finished after the Host last saw it', () => {
  assert.equal(isUnread(run('a'), true), false, 'seen at its finish')
  assert.equal(isUnread(run('a', { seen_at: iso(NOW - 2 * HOUR) }), true), true, 'a later turn finished')
  assert.equal(isUnread(run('a', { seen_at: null }), true), true, 'never seen')
  assert.equal(isUnread(run('a', { status: 'failed', seen_at: null }), true), true)
  assert.equal(isUnread(run('a', { status: 'running', finished_at: null, seen_at: null }), true), false)
  // An older Host has no read state: no dots rather than every run lit.
  assert.equal(isUnread(run('a', { seen_at: undefined }), false), false)
})

test('recentRuns: last user activity first, within the window', () => {
  const runs = [
    run('old', { updated_at: iso(NOW - RECENT_WINDOW_MS - HOUR) }),
    run('mid', { updated_at: iso(NOW - 5 * HOUR) }),
    run('new', { updated_at: iso(NOW - HOUR), status: 'running', finished_at: null }),
    // An agent turn finished just now, but the user touched it 2h ago: by activity.
    run('agent', { updated_at: iso(NOW - 2 * HOUR), finished_at: iso(NOW - 60_000), seen_at: iso(NOW - 60_000) }),
  ]
  assert.deepEqual(recentRuns(runs, NOW, true).map((r) => r.id), ['new', 'agent', 'mid'])
})

test('recentRuns keeps an unread result whatever its age, and caps the rest', () => {
  const stale = run('stale', {
    updated_at: iso(NOW - RECENT_WINDOW_MS - 10 * HOUR),
    finished_at: iso(NOW - 30 * 60_000),
    seen_at: null,
  })
  const many = Array.from({ length: RECENT_MAX + 5 }, (_, i) => run(`r${i}`, { updated_at: iso(NOW - (i + 1) * 60_000) }))
  const got = recentRuns([...many, stale], NOW, true)
  assert.equal(got.length, RECENT_MAX)
  assert.ok(got.some((r) => r.id === 'stale'), 'unread is never dropped')
  assert.equal(got[got.length - 1].id, 'stale', 'still ordered by activity')
  // Without read state it ages out like any other.
  assert.ok(!recentRuns([...many, stale], NOW, false).some((r) => r.id === 'stale'))
  // The input is left as it was.
  assert.equal(many[0].id, 'r0')
})

test('agoParts picks the largest whole unit', () => {
  const now = 10_000_000_000
  assert.deepEqual(agoParts(now - 5_000, now), { value: -5, unit: 'second' })
  assert.deepEqual(agoParts(now - 2 * 60_000, now), { value: -2, unit: 'minute' })
  assert.deepEqual(agoParts(now - 3 * 3_600_000, now), { value: -3, unit: 'hour' })
  assert.deepEqual(agoParts(now - 50 * 3_600_000, now), { value: -2, unit: 'day' })
  // A clock skew into the future reads as "now", never as a future time.
  assert.deepEqual(agoParts(now + 5_000, now), { value: -0, unit: 'second' })
})

test('createSyncGate: one sync at a time, stale acks ignored', () => {
  const gate = createSyncGate()
  const a = gate.begin()
  assert.equal(gate.begin(), null, 'a second sync waits for the first')
  assert.equal(gate.end(a), true)
  assert.equal(gate.end(a), false, 'an ack applies once')
  const b = gate.begin()
  gate.reset() // reconnect abandons b
  const c = gate.begin()
  assert.equal(gate.end(b), false, "an abandoned sync's ack cannot prune")
  assert.equal(gate.end(c), true)
})
