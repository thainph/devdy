// Unit tests for lib/dashboardLayout.ts (Work Dashboard v2 cards / project view).
// Run: node --test src/lib/__tests__/dashboardLayout.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  compareCardPrs, DEFAULT_CATEGORY_ORDER, fitGroups, healthReasons, moveTo, moveToFront, normalizeOrder, prCardGroups, prLevel, prSize,
  projectCard, projectRows, projectSignals, sessionActivity, slackCard, todoCardGroups, todoLevel, unknownByReason,
} from '../dashboardLayout.ts'
import { currentMilestoneProgress } from '../projectHealth.ts'

// Monday 2026-11-16 10:00 local time.
const NOW = new Date(2026, 10, 16, 10, 0).getTime()

test('saved card order is made whole and safe', () => {
  assert.deepEqual(normalizeOrder(null), DEFAULT_CATEGORY_ORDER)
  assert.deepEqual(normalizeOrder('junk'), DEFAULT_CATEGORY_ORDER)
  assert.deepEqual(normalizeOrder(['prs', 'nope', 'prs', 'slack']), ['prs', 'slack', 'todos', 'projects'])
})

test('move to front and drop onto another card', () => {
  const o = ['slack', 'todos', 'prs', 'projects']
  assert.deepEqual(moveToFront(o, 'prs'), ['prs', 'slack', 'todos', 'projects'])
  assert.deepEqual(moveToFront(o, 'slack'), o)
  assert.deepEqual(moveTo(o, 'projects', 'todos'), ['slack', 'projects', 'todos', 'prs'])
  assert.deepEqual(moveTo(o, 'slack', 'prs'), ['todos', 'prs', 'slack', 'projects'])
  assert.deepEqual(moveTo(o, 'slack', 'slack'), o)
  assert.notEqual(moveTo(o, 'slack', 'slack'), o, 'returns a copy')
})

test('moveTo by index lands the card on the drop slot (useDragSort onDrop(from, to))', () => {
  const o = ['slack', 'todos', 'prs', 'projects']
  for (let from = 0; from < o.length; from++) {
    for (let to = 0; to < o.length; to++) {
      const next = moveTo(o, o[from], o[to])
      assert.equal(next.indexOf(o[from]), to, `${from}→${to}`)
      assert.equal(next.length, o.length)
    }
  }
})

test('fitGroups fills groups in order and folds the rest', () => {
  const fitted = fitGroups([
    { key: 'a', items: [1, 2, 3] },
    { key: 'empty', items: [] },
    { key: 'b', items: [4, 5, 6] },
    { key: 'c', items: [7] },
  ], 5)
  assert.deepEqual(fitted.map((g) => [g.key, g.shown, g.hidden, g.total]), [
    ['a', [1, 2, 3], 0, 3],
    ['b', [4, 5], 1, 3],
    ['c', [], 1, 1],
  ])
})

function slackRow(label, extra = {}) {
  return {
    thread: { id: extra.id ?? label, title: 't', projectId: extra.projectId ?? null, channel: null, lastReplyTs: null, lastReplyAt: null, newCount: 0, preview: '' },
    label, unread: extra.unread ?? false, customer: extra.customer ?? null, waitingMs: extra.waitingMs ?? null,
    waitLevel: extra.waitLevel ?? 'ok', preview: '',
  }
}

test('slack card: waiting on me, later, folded unassigned threads, level', () => {
  const card = slackCard([
    slackRow('waitingOnMe', { waitLevel: 'warn' }),
    slackRow('waitingOnCustomer'),
    slackRow(null, { unread: true, id: 'u1' }),
    slackRow(null, { unread: true, id: 'u2' }),
    slackRow(null, { id: 'quiet' }),
  ])
  assert.equal(card.waiting.length, 1)
  assert.equal(card.later.length, 1)
  assert.equal(card.unassignedNew, 2)
  assert.equal(card.level, 'warn')
  assert.equal(slackCard([slackRow('waitingOnMe', { waitLevel: 'late' })]).level, 'bad')
  assert.equal(slackCard([]).level, 'none')
})

function todo(group, extra = {}) {
  return {
    item: { todo: { id: extra.id ?? group, project_id: extra.projectId ?? null }, score: 0 },
    group, daysLeft: extra.daysLeft ?? null, status: 'todo',
  }
}

test('todo card groups and level', () => {
  const todos = [todo('overdue'), todo('today'), todo('soon'), todo('active')]
  assert.deepEqual(todoCardGroups(todos).map((g) => [g.key, g.items.length]), [['overdue', 1], ['today', 1], ['upcoming', 2]])
  assert.equal(todoLevel(todos), 'bad')
  assert.equal(todoLevel([todo('today')]), 'warn')
  assert.equal(todoLevel([todo('soon')]), 'ok')
  assert.equal(todoLevel([]), 'none')
})

function ranked(number, extra = {}) {
  return {
    pr: { number, draft: extra.draft ?? false, additions: extra.additions ?? null, deletions: extra.deletions ?? null, project_id: extra.projectId ?? null },
    ai: extra.ai ?? 'none', waitLevel: extra.waitLevel ?? 'ok', waitDays: extra.waitDays ?? null,
    ci: extra.ci ?? null, small: extra.small ?? false, soleReviewer: extra.sole ?? false,
  }
}

test('dashboard PR order: quick reviews first, failing CI and drafts last, wait only breaks ties', () => {
  const list = [
    ranked(1, { waitDays: 9, waitLevel: 'late' }), // long wait, nothing quick about it
    ranked(2, { small: true, ci: 'success', sole: true, waitDays: 0.1 }),
    ranked(3, { small: true, ci: 'failure', sole: true, waitDays: 5 }),
    ranked(4, { small: true, ci: 'success', waitDays: 2 }),
    ranked(5, { small: true, ci: 'success', waitDays: 4 }),
    ranked(6, { small: true, ci: 'success', sole: true, draft: true }),
  ]
  assert.deepEqual([...list].sort(compareCardPrs).map((r) => r.pr.number), [2, 5, 4, 1, 3, 6])
  const groups = prCardGroups({ rows: list, bots: [] })
  assert.deepEqual(groups[1].items.map((r) => r.pr.number), [2, 5, 4, 1, 3, 6])
  assert.deepEqual(list.map((r) => r.pr.number), [1, 2, 3, 4, 5, 6], 'input left untouched')
})

test('PR groups: re-review → to review (drafts kept) → bots; level; size words', () => {
  const r = {
    rows: [ranked(1, { ai: 'updated' }), ranked(2), ranked(3, { ai: 'updated', draft: true })],
    bots: [ranked(9)],
  }
  assert.deepEqual(prCardGroups(r).map((g) => [g.key, g.items.map((x) => x.pr.number)]), [
    ['reReview', [1]], ['toReview', [2, 3]], ['bots', [9]],
  ])
  assert.equal(prLevel(r), 'warn')
  assert.equal(prLevel({ rows: [ranked(1, { waitLevel: 'late' })], bots: [] }), 'bad')
  assert.equal(prLevel({ rows: [ranked(1, { waitLevel: 'late', draft: true })], bots: [] }), 'ok')
  assert.equal(prLevel({ rows: [], bots: [] }), 'none')
  assert.equal(prSize({ additions: 50, deletions: 20 }), 'small')
  assert.equal(prSize({ additions: 300, deletions: 0 }), 'medium')
  assert.equal(prSize({ additions: 400, deletions: 200 }), 'large')
  assert.equal(prSize({ additions: null, deletions: null }), null)
})

function health(level, extra = {}) {
  return {
    projectId: extra.id ?? level, projectName: extra.name ?? level, level, reason: null,
    counts: { overdue: extra.overdue ?? 0 }, riskyIssueCount: extra.risky ?? 0, topIssues: extra.topIssues ?? [],
    milestones: extra.milestones ?? [], progress: null, fetchedAt: null,
  }
}

test('health reasons: top issue, worst milestone, other risky issues', () => {
  const h = health('red', {
    risky: 3,
    topIssues: [{ number: 412, title: 'Export', radar: 'overdue', daysLeft: -3, htmlUrl: 'u' }],
    milestones: [{ title: 'v2.3', kind: 'lag', elapsedPct: 72, closedPct: 41 }],
  })
  assert.deepEqual(healthReasons(h, 3), [
    { kind: 'issue', number: 412, title: 'Export', radar: 'overdue', daysLeft: -3, url: 'u' },
    { kind: 'milestoneLag', title: 'v2.3', lagPct: 31 },
    { kind: 'riskyIssues', count: 2 },
  ])
  assert.equal(healthReasons(h).length, 2)
  assert.deepEqual(healthReasons(health('red', { milestones: [{ title: 'S', kind: 'overdue', lateDays: 4 }] })), [
    { kind: 'milestoneOverdue', title: 'S', lateDays: 4 },
  ])
  assert.deepEqual(healthReasons(health('green')), [])
})

test('project card buckets and level', () => {
  const c = projectCard([health('red'), health('orange'), health('green'), health('gray')])
  assert.deepEqual([c.risk.length, c.attention.length, c.ok.length, c.unknown.length], [1, 1, 1, 1])
  assert.equal(c.level, 'bad')
  assert.equal(projectCard([health('green')]).level, 'ok')
  assert.equal(projectCard([health('green'), health('gray')]).level, 'ok')
  assert.equal(projectCard([health('gray'), health('gray', { id: 'g2' })]).level, 'none', 'nothing judged is no verdict')
  assert.equal(projectCard([]).level, 'none')
})

test('unknown projects grouped by reason', () => {
  const g = (id, reason) => ({ ...health('gray', { id, name: id }), reason })
  assert.deepEqual(
    unknownByReason([g('a', 'no-board'), g('b', 'error'), g('c', 'no-board'), g('d', null)])
      .map((x) => [x.reason, x.projects.map((p) => p.projectId)]),
    [['no-board', ['a', 'c']], ['error', ['b']], ['loading', ['d']]],
  )
})

test('per-project signals and the traffic-light order (gray last)', () => {
  const signals = projectSignals({
    slack: [
      slackRow('waitingOnMe', { projectId: 'p1', waitingMs: 3_600_000, customer: { displayName: 'Tanaka' } }),
      slackRow('waitingOnMe', { id: 'b', projectId: 'p1', waitingMs: 7_200_000, customer: { displayName: 'Sato' } }),
      slackRow('waitingOnCustomer', { id: 'c', projectId: 'p1' }),
      slackRow('waitingOnMe', { id: 'd', projectId: null }),
    ],
    todos: [todo('overdue', { projectId: 'p2', daysLeft: -2 }), todo('overdue', { id: 'x', projectId: 'p2', daysLeft: -5 }), todo('today', { projectId: 'p2' })],
    prs: { rows: [ranked(1, { projectId: 'p1' }), ranked(2, { projectId: 'p1', draft: true })], bots: [] },
    runs: [{ run: { project_id: 'p3' }, pending: 1 }, { run: { project_id: 'p3' }, pending: 0 }],
  })
  const p1 = signals.get('p1')
  assert.equal(p1.customersWaiting, 2)
  assert.deepEqual(p1.longestWait, { name: 'Sato', ms: 7_200_000 })
  assert.equal(p1.prs, 1)
  assert.equal(signals.get('p2').overdueTodos, 2)
  assert.equal(signals.get('p2').worstTodoLateDays, 5)
  assert.deepEqual([signals.get('p3').sessions, signals.get('p3').sessionsWaiting], [2, 1])
  assert.equal(signals.size, 3)

  const rows = projectRows(
    [health('gray', { id: 'g', name: 'A' }), health('green', { id: 'p1', name: 'B' }), health('red', { id: 'r', name: 'C' }), health('orange', { id: 'o', name: 'D' })],
    signals,
  )
  assert.deepEqual(rows.map((r) => r.health.level), ['red', 'orange', 'green', 'gray'])
  assert.equal(rows[2].signals.customersWaiting, 2)
  assert.equal(rows[0].signals.prs, 0, 'no signals → zeros')
})

test('current milestone progress: earliest due with open issues', () => {
  const issue = (startDate) => ({ startDate })
  const p = currentMilestoneProgress([
    { title: 'Later', dueOn: '2026-12-20', openCount: 3, closedCount: 1, issues: [] },
    { title: 'Done', dueOn: '2026-11-01', openCount: 0, closedCount: 5, issues: [] },
    { title: 'Now', dueOn: '2026-11-29', openCount: 6, closedCount: 4, issues: [issue('2026-11-01'), issue(null)] },
    { title: 'Cut', dueOn: '2026-11-20', openCount: 2, closedCount: 0, countsKnown: false, issues: [] },
  ], NOW)
  assert.equal(p.title, 'Now')
  assert.equal(p.closedPct, 40)
  assert.ok(p.elapsedPct >= 50 && p.elapsedPct <= 55, String(p.elapsedPct))
  const noStart = currentMilestoneProgress([{ title: 'X', dueOn: '2026-11-29', openCount: 1, closedCount: 1, issues: [] }], NOW)
  assert.equal(noStart.elapsedPct, null)
  assert.equal(noStart.closedPct, 50)
  assert.equal(currentMilestoneProgress([{ title: 'Y', dueOn: null, openCount: 1, closedCount: 0, issues: [] }], NOW), null)
})

test('session activity: permission prompt, running tool, else nothing', () => {
  assert.deepEqual(sessionActivity([], [{ tool_name: 'Bash' }]), { kind: 'permission', tool: 'Bash' })
  assert.deepEqual(sessionActivity([{ kind: 'text' }, { kind: 'tool', name: 'Read', displayName: 'Read file' }], []), { kind: 'tool', tool: 'Read file' })
  assert.equal(sessionActivity([{ kind: 'tool', name: 'Read', result: { content: '' } }], []), null)
  assert.equal(sessionActivity([{ kind: 'tool', name: 'Read' }, { kind: 'result' }], []), null)
  assert.equal(sessionActivity(null, null), null)
})
