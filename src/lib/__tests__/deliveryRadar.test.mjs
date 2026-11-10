// Unit tests for lib/deliveryRadar.ts (issue classification + per-person rollup).
// Run: node --test src/lib/__tests__/deliveryRadar.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEFAULT_THRESHOLDS,
  buildGroups,
  classifyAll,
  classifyIssue,
  countByStatus,
  daysUntil,
  filterIssues,
  defaultStage,
  isDone,
  nestIssues,
  stageOf,
} from '../deliveryRadar.ts'

// 2026-11-07 10:00 local time.
const NOW = new Date(2026, 10, 7, 10, 0).getTime()
const TH = { dueSoonDays: 3, staleDays: 5 }

let seq = 0
function issue(extra = {}) {
  seq += 1
  return {
    repo: 'web',
    number: seq,
    title: `Issue ${seq}`,
    htmlUrl: `https://github.com/acme/web/issues/${seq}`,
    updatedAt: new Date(NOW - 3_600_000).toISOString(),
    assignees: ['alice'],
    startDate: null,
    deadline: null,
    status: 'In progress',
    statusColor: null,
    state: 'OPEN',
    projectId: 'p1',
    projectName: 'Alpha',
    ...extra,
  }
}

test('daysUntil counts whole local days, negative when late', () => {
  assert.equal(daysUntil('2026-11-07', NOW), 0)
  assert.equal(daysUntil('2026-11-06', NOW), -1)
  assert.equal(daysUntil('2026-10-29', NOW), -9)
  assert.equal(daysUntil('2026-11-10', NOW), 3)
  assert.equal(daysUntil(null, NOW), null)
  assert.equal(daysUntil('soon', NOW), null)
})

test('classifyIssue: deadline today is due soon, yesterday is overdue', () => {
  assert.equal(classifyIssue(issue({ deadline: '2026-11-06' }), NOW, TH), 'overdue')
  assert.equal(classifyIssue(issue({ deadline: '2026-11-07' }), NOW, TH), 'due-soon')
  assert.equal(classifyIssue(issue({ deadline: '2026-11-10' }), NOW, TH), 'due-soon')
  assert.equal(classifyIssue(issue({ deadline: '2026-11-11' }), NOW, TH), 'on-track')
})

test('classifyIssue: not started, stale, no deadline', () => {
  assert.equal(
    classifyIssue(issue({ deadline: '2026-12-01', startDate: '2026-11-05', status: 'Todo' }), NOW, TH),
    'not-started',
  )
  // A future start date is not late yet.
  assert.equal(
    classifyIssue(issue({ deadline: '2026-12-01', startDate: '2026-11-09', status: 'Todo' }), NOW, TH),
    'on-track',
  )
  const old = new Date(NOW - 6 * 86_400_000).toISOString()
  assert.equal(classifyIssue(issue({ deadline: '2026-12-01', updatedAt: old }), NOW, TH), 'stale')
  assert.equal(classifyIssue(issue(), NOW, TH), 'no-deadline')
  // Overdue wins over stale.
  assert.equal(classifyIssue(issue({ deadline: '2026-11-01', updatedAt: old }), NOW, TH), 'overdue')
})

test('classifyAll drops done issues and duplicates', () => {
  const a = issue({ deadline: '2026-11-01' })
  const out = classifyAll(
    [a, { ...a }, issue({ status: 'Done' }), issue({ state: 'CLOSED' })],
    NOW,
    DEFAULT_THRESHOLDS,
  )
  assert.equal(out.length, 1)
  assert.equal(out[0].radar, 'overdue')
  assert.equal(out[0].daysLeft, -6)
  // The same repo#number in another project is a different issue.
  assert.equal(classifyAll([a, { ...a, projectId: 'p2' }], NOW, TH).length, 2)
})

test('buildGroups counts multi-assignee issues for everyone, late people first, unassigned last', () => {
  const issues = classifyAll(
    [
      issue({ assignees: ['bob'], deadline: '2026-11-05' }),
      issue({ assignees: ['alice', 'bob'], deadline: '2026-10-29', projectName: 'Beta', projectId: 'p2' }),
      issue({ assignees: ['carol'], deadline: '2026-11-08' }),
      issue({ assignees: [], deadline: '2026-10-01' }),
    ],
    NOW,
    TH,
  )
  const groups = buildGroups(issues, 'person')
  assert.deepEqual(groups.map((g) => g.key), ['bob', 'alice', 'carol', null])
  const bob = groups[0]
  assert.equal(bob.counts.overdue, 2)
  assert.equal(bob.worstLateDays, 9)
  assert.deepEqual(bob.related, ['Alpha', 'Beta'])
  // Most late issue first within the group.
  assert.equal(bob.issues[0].daysLeft, -9)
  assert.equal(groups[2].counts['due-soon'], 1)
  assert.equal(groups[2].worstLateDays, null)
  // Unique issue count is unaffected by the per-person duplication.
  assert.equal(countByStatus(issues).overdue, 3)
})

test('buildGroups by project lists assignees as related', () => {
  const issues = classifyAll(
    [
      issue({ assignees: ['bob'], deadline: '2026-11-05' }),
      issue({ assignees: ['alice'], projectId: 'p2', projectName: 'Beta' }),
    ],
    NOW,
    TH,
  )
  const groups = buildGroups(issues, 'project')
  assert.deepEqual(groups.map((g) => g.label), ['Alpha', 'Beta'])
  assert.deepEqual(groups[0].related, ['bob'])
})

test('filterIssues by status and search text', () => {
  const issues = classifyAll(
    [
      issue({ title: 'Fix login timeout', deadline: '2026-11-01', number: 123 }),
      issue({ title: 'Export CSV', assignees: ['dave'] }),
    ],
    NOW,
    TH,
  )
  assert.equal(filterIssues(issues, { status: 'overdue' }).length, 1)
  assert.equal(filterIssues(issues, { query: 'login' }).length, 1)
  assert.equal(filterIssues(issues, { query: '#123' }).length, 1)
  assert.equal(filterIssues(issues, { query: 'dav' }).length, 1)
  assert.equal(filterIssues(issues, { status: 'overdue', query: 'csv' }).length, 0)
})

test('defaultStage guesses dev / QA / done from the status name', () => {
  for (const s of ['Ready for QA', 'In QA', 'Testing', 'UAT', 'Verified', 'Staging', 'Deployed']) {
    assert.equal(defaultStage(s), 'qa', s)
  }
  for (const s of ['Ready for release', 'Released', 'Done', 'Closed', 'Hoàn thành', 'Canceled', 'Cancelled', "Won't fix", 'QA done']) {
    assert.equal(defaultStage(s), 'done', s)
  }
  for (const s of ['Todo', 'In progress', 'In review', 'Changes Requested', 'Blocked', '', null]) {
    assert.equal(defaultStage(s), 'dev', String(s))
  }
})

test('a picked stage overrides the guess, case-insensitively, for every project', () => {
  const stages = { 'ready for qa': 'done', 'in review': 'qa' }
  assert.equal(stageOf('Ready for QA', stages), 'done')
  assert.equal(stageOf('In Review', stages), 'qa')
  assert.equal(stageOf('Testing', stages), 'qa')
  const qa = issue({ status: 'Ready for QA', deadline: '2026-11-01' })
  assert.equal(isDone(qa), true)
  assert.equal(isDone(qa, { 'ready for qa': 'dev' }), false)
  assert.equal(isDone(issue({ state: 'CLOSED', status: 'Todo' }), {}), true)
  // Late but handed to QA: not overdue for the dev.
  assert.equal(classifyAll([qa], NOW, TH).length, 0)
  assert.equal(classifyAll([qa], NOW, TH, { stages: { 'ready for qa': 'dev' } })[0].radar, 'overdue')
  assert.equal(classifyAll([{ ...qa, projectId: 'p2' }], NOW, TH, { stages: { 'ready for qa': 'dev' } }).length, 1)
})

test('a tester is done only at the done stage', () => {
  assert.equal(isDone(issue({ status: 'Ready for QA' }), null, 'tester'), false)
  assert.equal(isDone(issue({ status: 'Ready for release' }), null, 'tester'), true)
  assert.equal(isDone(issue({ status: 'Ready for QA' }), { 'ready for qa': 'done' }, 'tester'), true)
})

test('an issue in QA leaves the developer but stays with the tester', () => {
  const rules = { testers: ['Tina'] }
  const [qa] = classifyAll(
    [issue({ assignees: ['dan', 'tina'], status: 'Ready for QA', deadline: '2026-11-01' })],
    NOW, TH, rules,
  )
  assert.deepEqual(qa.activeAssignees, ['tina'])
  const groups = buildGroups([qa], 'person')
  assert.deepEqual(groups.map((g) => g.key), ['tina'])
  assert.deepEqual(buildGroups([qa], 'project')[0].related, ['tina'])
  // Ready for release: done for both roles.
  assert.equal(classifyAll([issue({ assignees: ['dan', 'tina'], status: 'Ready for release' })], NOW, TH, rules).length, 0)
  // Still in progress: both still owe work.
  assert.deepEqual(
    classifyAll([issue({ assignees: ['dan', 'tina'] })], NOW, TH, rules)[0].activeAssignees,
    ['dan', 'tina'],
  )
  // A status picked as done counts for the tester too.
  const custom = { ...rules, stages: { 'ready for qa': 'done' } }
  assert.equal(classifyAll([issue({ assignees: ['tina'], status: 'Ready for QA' })], NOW, TH, custom).length, 0)
})

function parentOf(n) {
  return { owner: 'acme', repo: 'web', number: n, title: `Issue ${n}`, htmlUrl: '' }
}

test('nestIssues puts sub-issues under their parent, ordered by the most urgent subtree', () => {
  const calm = issue({ number: 100, deadline: '2026-12-20' })
  const epic = issue({ number: 1, title: 'Epic' })
  const story = issue({ number: 2, parent: parentOf(1), deadline: '2026-12-01' })
  const lateTask = issue({ number: 3, parent: parentOf(2), deadline: '2026-11-01' })
  const task = issue({ number: 4, parent: parentOf(1), deadline: '2026-12-10' })
  const orphan = issue({ number: 5, parent: parentOf(99), deadline: '2026-12-15' })
  const rows = nestIssues(classifyAll([calm, epic, story, lateTask, task, orphan], NOW, TH))
  assert.deepEqual(
    rows.map((r) => [r.issue.number, r.depth, r.hasChildren]),
    [
      // The epic has no deadline but its late task pulls it to the top.
      [1, 0, true],
      [2, 1, true],
      [3, 2, false],
      [4, 1, false],
      [5, 0, false], // parent not in the list: top level
      [100, 0, false],
    ],
  )
})

test('nestIssues survives a parent cycle and buildGroups exposes rows', () => {
  const a = issue({ number: 1, parent: parentOf(2) })
  const b = issue({ number: 2, parent: parentOf(1) })
  assert.equal(nestIssues(classifyAll([a, b], NOW, TH)).length, 2)
  const groups = buildGroups(classifyAll([issue({ number: 7 }), issue({ number: 8, parent: parentOf(7) })], NOW, TH), 'person')
  assert.deepEqual(groups[0].rows.map((r) => r.depth), [0, 1])
})

test('filterIssues matches the parent title', () => {
  const issues = classifyAll([issue({ parent: { ...parentOf(1), title: 'Checkout epic' } }), issue()], NOW, TH)
  assert.equal(filterIssues(issues, { query: 'checkout' }).length, 1)
})
