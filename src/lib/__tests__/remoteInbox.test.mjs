// Unit tests for lib/remoteInbox.ts (Remote Controller Inbox: pending queues,
// cross-run aggregation, "since last visit").
// Run: node --test src/lib/__tests__/remoteInbox.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  aggregatePending,
  dequeuePending,
  dropRun,
  enqueuePending,
  FIRST_VISIT_LOOKBACK_MS,
  agoParts,
  finishedKind,
  createSyncGate,
  finishedSince,
  headOf,
  leaveVisit,
  markRead,
  parseHostTime,
  parseVisitState,
  pendingCount,
  READ_MAX,
  retainPending,
  startVisit,
  VISIT_GAP_MS,
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

test('finishedSince lists runs finished after the anchor, newest first', () => {
  const since = Date.UTC(2026, 9, 31, 9)
  const runs = [
    { id: 'old', status: 'done', updated_at: '2026-10-31T08:00:00Z' },
    { id: 'a', status: 'done', updated_at: '2026-10-31T09:30:00Z' },
    { id: 'b', status: 'failed', updated_at: '2026-10-31 10:00:00' },
    { id: 'live', status: 'running', updated_at: '2026-10-31T10:30:00Z' },
    { id: 'nots', status: 'done', updated_at: null },
    { id: 'created', status: 'error', updated_at: null, created_at: '2026-10-31T09:10:00Z' },
  ]
  assert.deepEqual(finishedSince(runs, since).map((r) => r.id), ['b', 'a', 'created'])
})

test('finishedSince counts a session whose later turn ended after the anchor', () => {
  const since = Date.UTC(2026, 9, 31, 9)
  // Follow-up sent before the visit (updated_at), turn finished during it.
  const runs = [
    { id: 'turn', status: 'done', updated_at: '2026-10-31T08:50:00Z', finished_at: '2026-10-31T09:20:00Z' },
    { id: 'stale', status: 'done', updated_at: '2026-10-31T09:40:00Z', finished_at: '2026-10-31T08:00:00Z' },
  ]
  assert.deepEqual(finishedSince(runs, since).map((r) => r.id), ['turn'])
})

test('parseVisitState tolerates junk', () => {
  const empty = { anchor: null, lastLeftAt: null, read: [] }
  assert.deepEqual(parseVisitState(null), empty)
  assert.deepEqual(parseVisitState('{bad'), empty)
  assert.deepEqual(parseVisitState('[]'), { ...empty })
  assert.deepEqual(parseVisitState('{"anchor":3,"lastLeftAt":5,"read":["x",1]}'), {
    anchor: 3,
    lastLeftAt: 5,
    read: ['x'],
  })
})

test('startVisit: first visit looks back a day', () => {
  const now = 100 * FIRST_VISIT_LOOKBACK_MS
  const v = startVisit(parseVisitState(null), now)
  assert.equal(v.anchor, now - FIRST_VISIT_LOOKBACK_MS)
})

test('startVisit: a long absence anchors at when the user left', () => {
  const left = 1_000_000
  const v = startVisit({ anchor: 10, lastLeftAt: left, read: [] }, left + VISIT_GAP_MS)
  assert.equal(v.anchor, left)
})

test('startVisit: a quick switch or reload keeps the current anchor', () => {
  const left = 1_000_000
  const v = startVisit({ anchor: 10, lastLeftAt: left, read: [] }, left + 1000)
  assert.equal(v.anchor, 10)
  // No anchor yet but a recent leave: fall back to the leave time.
  assert.equal(startVisit({ anchor: null, lastLeftAt: left, read: [] }, left + 1000).anchor, left)
})

test('leaveVisit stamps the leave time and keeps the rest', () => {
  assert.deepEqual(leaveVisit({ anchor: 1, lastLeftAt: null, read: ['a'] }, 9), {
    anchor: 1,
    lastLeftAt: 9,
    read: ['a'],
  })
})

test('markRead dedupes and caps', () => {
  let s = markRead({ anchor: null, lastLeftAt: null, read: [] }, 'r1')
  assert.equal(markRead(s, 'r1'), s)
  for (let i = 0; i < READ_MAX + 5; i++) s = markRead(s, `x${i}`)
  assert.equal(s.read.length, READ_MAX)
  assert.equal(s.read.at(-1), `x${READ_MAX + 4}`)
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
