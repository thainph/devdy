// Unit tests for lib/slackThreadUtils.ts (message grouping, sidebar tree,
// initials, link check). Run: node --test src/lib/__tests__/
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  checkThreadLink,
  createSeenGuard,
  groupMessages,
  groupThreads,
  initials,
} from '../slackThreadUtils.ts'

const T0 = Date.parse('2026-10-08T09:00:00Z')

function msg(seq, userId, minutes, extra = {}) {
  return {
    ts: String(seq),
    seq,
    userId,
    botId: null,
    authorName: userId ?? 'bot',
    avatarPath: null,
    subtype: null,
    textRaw: 'x',
    textVi: null,
    translateStatus: 'pending',
    translateError: null,
    editedTs: null,
    postedAt: new Date(T0 + minutes * 60_000).toISOString(),
    reactions: [],
    fileIds: [],
    ...extra,
  }
}

test('parent never groups; first reply never groups with parent', () => {
  const rows = groupMessages([msg(0, 'U1', 0), msg(1, 'U1', 1), msg(2, 'U1', 2)])
  assert.deepEqual(
    rows.map((r) => [r.isParent, r.continued]),
    [
      [true, false],
      [false, false],
      [false, true],
    ],
  )
})

test('same sender within 5 minutes groups; gap > 5 min or other sender breaks', () => {
  const rows = groupMessages([
    msg(0, 'U1', 0),
    msg(1, 'U2', 1),
    msg(2, 'U2', 6), // exactly 5 min later → grouped
    msg(3, 'U2', 12), // 6 min later → new group
    msg(4, 'U1', 13), // other sender
    msg(5, null, 13, { botId: 'B1' }),
    msg(6, null, 14, { botId: 'B1' }), // same bot → grouped
  ])
  assert.deepEqual(
    rows.map((r) => r.continued),
    [false, false, true, false, false, false, true],
  )
})

test('rows are ordered by seq', () => {
  const rows = groupMessages([msg(2, 'U1', 2), msg(0, 'U1', 0), msg(1, 'U2', 1)])
  assert.deepEqual(rows.map((r) => r.message.seq), [0, 1, 2])
})

function thread(id, position, workspace, channel, extra = {}) {
  return { id, position, workspace, workspaceDomain: null, channel, channelId: null, ...extra }
}

test('sidebar groups workspace → channel ordered by min(position)', () => {
  const groups = groupThreads([
    thread('a', 5, 'malme', 'dev'),
    thread('b', 1, 'malme', 'ops'),
    thread('c', 3, 'malme', 'dev'),
    thread('d', 2, null, null, { workspaceDomain: 'x.slack.com', channelId: 'C9' }),
    thread('e', 9, null, null),
  ])
  assert.deepEqual(
    groups.map((g) => [g.label, g.channels.map((c) => [c.label, c.threads.map((t) => t.id)])]),
    [
      ['malme', [['#ops', ['b']], ['#dev', ['c', 'a']]]],
      ['x.slack.com', [['#C9', ['d']]]],
      [null, [[null, ['e']]]],
    ],
  )
})

test('initials', () => {
  assert.equal(initials('Nguyen Thai'), 'NT')
  assert.equal(initials('ken'), 'KE')
  assert.equal(initials('thai.nph'), 'TN')
  assert.equal(initials('  '), '?')
})

test('thread link check mirrors spec §4', () => {
  assert.equal(checkThreadLink('https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359'), 'ok')
  assert.equal(
    checkThreadLink(
      'https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG',
    ),
    'ok',
  )
  assert.equal(checkThreadLink('<https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359>'), 'ok')
  assert.equal(
    checkThreadLink('https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C999'),
    'invalid_link',
  )
  assert.equal(checkThreadLink('https://malme.slack.com/archives/D01ABCDEFG/p1789877283026359'), 'dm_unsupported')
  assert.equal(checkThreadLink('https://evil.com/archives/C0B83GHDTFG/p1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('https://malme.slack.com/archives/C0B83GHDTFG/1789877283026359'), 'invalid_link')
  assert.equal(checkThreadLink('https://malme.slack.com/archives/C0B83GHDTFG/p178987728302635'), 'invalid_link')
  assert.equal(checkThreadLink('abc'), 'invalid_link')
})

test('seen guard: once per (thread, newest reply) — breaks the mark-seen → changed → reload loop', () => {
  const guard = createSeenGuard()
  assert.equal(guard.take('t1', '1.000001'), true)
  // The reload triggered by our own mark-seen must not mark again.
  for (let i = 0; i < 5; i++) assert.equal(guard.take('t1', '1.000001'), false)
  // A new reply arrives → marked once more.
  assert.equal(guard.take('t1', '1.000002'), true)
  assert.equal(guard.take('t1', '1.000002'), false)
  // Other thread is independent; no messages → nothing to mark.
  assert.equal(guard.take('t2', '1.000001'), true)
  assert.equal(guard.take('t3', null), false)
  // A failed request releases the pair for a retry.
  guard.release('t2', '1.000001')
  assert.equal(guard.take('t2', '1.000001'), true)
})
