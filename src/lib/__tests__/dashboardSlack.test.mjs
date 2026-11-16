// Unit tests for lib/dashboardSlack.ts (Work Dashboard Slack widget).
// Run: node --test src/lib/__tests__/dashboardSlack.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  buildSlackRows, classifySlackThread, isUnread, messageTime, needsAttention, waitLevel,
  SLACK_WAIT_LATE_MS, SLACK_WAIT_WARN_MS,
} from '../dashboardSlack.ts'

const NOW = Date.parse('2026-11-16T10:00:00Z')
const HOUR = 3_600_000

function thread(id, extra = {}) {
  return {
    id, title: `Thread ${id}`, projectId: 'p1', channel: 'general',
    lastReplyTs: '1700000000.000100', lastReplyAt: null, newCount: 0, preview: 'parent', ...extra,
  }
}

function msg(userId, hoursAgo, isCustomer, preview = `from ${userId}`) {
  const at = NOW - hoursAgo * HOUR
  return { userId, displayName: userId, ts: String(at / 1000), postedAt: new Date(at).toISOString(), isCustomer, preview }
}

function activity(threadId, lastHuman, lastCustomer = null, lastSeenTs = '1700000000.000100') {
  return { threadId, lastSeenTs, lastHuman, lastCustomer }
}

test('customer spoke last → waiting on me, with the wait time', () => {
  const c = msg('UC', 5, true, 'Please check')
  const row = classifySlackThread(thread('a'), activity('a', c, c), NOW)
  assert.equal(row.label, 'waitingOnMe')
  assert.equal(row.waitingMs, 5 * HOUR)
  assert.equal(row.waitLevel, 'warn')
  assert.equal(row.preview, 'Please check')
  assert.equal(row.customer.userId, 'UC')
})

test('someone else answered after the customer → waiting on the customer', () => {
  const row = classifySlackThread(thread('b'), activity('b', msg('UME', 1, false), msg('UC', 3, true, 'Q')), NOW)
  assert.equal(row.label, 'waitingOnCustomer')
  assert.equal(row.waitingMs, null)
  assert.equal(row.waitLevel, 'ok')
  // The customer's message is the preview, not mine.
  assert.equal(row.preview, 'Q')
})

test('no customer in the thread → no wait label', () => {
  const row = classifySlackThread(thread('c', { newCount: 2 }), activity('c', msg('UME', 1, false)), NOW)
  assert.equal(row.label, null)
  assert.equal(row.unread, true)
})

test('a thread never opened counts as unread although newCount is 0', () => {
  const t = thread('d')
  assert.equal(isUnread(t, activity('d', msg('U1', 1, false), null, null)), true)
  assert.equal(isUnread(t, activity('d', msg('U1', 1, false))), false)
  assert.equal(isUnread(thread('e', { newCount: 3 }), undefined), true)
  // Nothing at all to read.
  assert.equal(isUnread(thread('f', { lastReplyTs: null }), activity('f', null, null, null)), false)
})

test('wait levels: > 4 h amber, > 1 day red', () => {
  assert.equal(waitLevel(null), 'ok')
  assert.equal(waitLevel(SLACK_WAIT_WARN_MS), 'ok')
  assert.equal(waitLevel(SLACK_WAIT_WARN_MS + 1), 'warn')
  assert.equal(waitLevel(SLACK_WAIT_LATE_MS + 1), 'late')
})

test('messageTime falls back to the Slack ts', () => {
  assert.equal(messageTime({ postedAt: null, ts: '1700000000.500000' }), 1700000000500)
  assert.equal(messageTime({ postedAt: 'bad', ts: 'bad' }), null)
})

test('order: waiting on me (longest first) → unread → rest by latest reply', () => {
  const rows = buildSlackRows(
    [
      thread('old', { lastReplyTs: '1700000001.000000' }),
      thread('new', { lastReplyTs: '1700000009.000000' }),
      thread('unread', { newCount: 1, lastReplyTs: '1700000002.000000' }),
      thread('wait1', { lastReplyTs: '1700000003.000000' }),
      thread('wait30', { lastReplyTs: '1700000004.000000' }),
    ],
    [
      activity('wait1', msg('UC', 1, true), msg('UC', 1, true)),
      activity('wait30', msg('UC', 30, true), msg('UC', 30, true)),
      activity('old', msg('UME', 2, false), msg('UC', 3, true)),
    ],
    NOW,
  )
  assert.deepEqual(rows.map((r) => r.thread.id), ['wait30', 'wait1', 'unread', 'new', 'old'])
  assert.equal(rows[0].waitLevel, 'late')
  assert.deepEqual(rows.map(needsAttention), [true, true, true, false, false])
})
