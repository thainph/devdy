// Unit tests for lib/projectHealth.ts (Work Dashboard "projects at risk").
// Run: node --test src/lib/__tests__/projectHealth.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { compareHealth, isAtRisk, projectHealth } from '../projectHealth.ts'
import { DEFAULT_THRESHOLDS, milestoneRisk } from '../deliveryRadar.ts'

// Monday 2026-11-16 10:00 local time.
const NOW = new Date(2026, 10, 16, 10, 0).getTime()
const TH = DEFAULT_THRESHOLDS

let seq = 0
function issue(extra = {}) {
  seq += 1
  return {
    repo: 'web', number: seq, title: `Issue ${seq}`, htmlUrl: `https://x/${seq}`,
    updatedAt: new Date(NOW - 3_600_000).toISOString(), assignees: ['alice'],
    startDate: null, deadline: null, status: 'In progress', statusColor: null, state: 'OPEN',
    ...extra,
  }
}

function milestone(extra = {}) {
  return { title: 'v1', dueOn: null, openCount: 1, closedCount: 0, issues: [], ...extra }
}

function input(board, extra = {}) {
  return {
    projectId: 'p1', projectName: 'Alpha', error: null, hasBoardUrl: true, gitlabOnly: false,
    board: board && {
      fetchedAt: '2026-11-16T00:00:00Z', boardLinked: true, truncated: false, skippedNonGithub: false, ...board,
    },
    ...extra,
  }
}

test('an overdue issue makes the project red, with its top issues', () => {
  const h = projectHealth(input({
    milestones: [milestone({ issues: [
      issue({ deadline: '2026-11-13', title: 'Late' }),
      issue({ deadline: '2026-11-17', title: 'Soon' }),
      issue({ deadline: '2026-12-30', title: 'Fine' }),
    ] })],
  }), NOW, TH)
  assert.equal(h.level, 'red')
  assert.equal(h.reason, null)
  assert.equal(h.counts.overdue, 1)
  assert.equal(h.riskyIssueCount, 2)
  assert.deepEqual(h.topIssues.map((i) => i.title), ['Late', 'Soon'])
  assert.equal(h.topIssues[0].daysLeft, -3)
  assert.ok(isAtRisk(h))
})

test('due soon / stale only → orange; nothing → green', () => {
  const orange = projectHealth(input({ milestones: [milestone({ issues: [
    issue({ updatedAt: new Date(NOW - 10 * 86_400_000).toISOString(), deadline: '2026-12-30' }),
  ] })] }), NOW, TH)
  assert.equal(orange.level, 'orange')
  const green = projectHealth(input({ milestones: [milestone({ issues: [issue({ deadline: '2026-12-30' })] })] }), NOW, TH)
  assert.equal(green.level, 'green')
  assert.ok(!isAtRisk(green))
})

test('milestone past its due date with open issues → red', () => {
  const r = milestoneRisk(milestone({ dueOn: '2026-11-10T00:00:00Z', openCount: 4 }), NOW, 20)
  assert.equal(r.kind, 'overdue')
  assert.equal(r.lateDays, 6)
  // Closed out: no risk.
  assert.equal(milestoneRisk(milestone({ dueOn: '2026-11-10', openCount: 0 }), NOW, 20), null)
  const h = projectHealth(input({ milestones: [milestone({ dueOn: '2026-11-10', openCount: 4 })] }), NOW, TH)
  assert.equal(h.level, 'red')
  assert.equal(h.milestones.length, 1)
})

test('milestone lag: elapsed % − closed % above the threshold → orange', () => {
  // Planned 2026-11-01 … end of 2026-11-29 (29 days); ~53% elapsed.
  const behind = milestone({
    dueOn: '2026-11-29', openCount: 8, closedCount: 2,
    issues: [issue({ startDate: '2026-11-01', deadline: '2026-12-30' })],
  })
  const r = milestoneRisk(behind, NOW, 20)
  assert.equal(r.kind, 'lag')
  assert.equal(r.closedPct, 20)
  assert.ok(r.elapsedPct >= 50 && r.elapsedPct <= 55, String(r.elapsedPct))
  // On pace.
  assert.equal(milestoneRisk({ ...behind, openCount: 5, closedCount: 5 }, NOW, 20), null)
  // No start date anywhere: only the due-date check applies.
  assert.equal(milestoneRisk({ ...behind, issues: [issue()] }, NOW, 20), null)
  // A bigger tolerance silences it.
  assert.equal(milestoneRisk(behind, NOW, 40), null)
  const h = projectHealth(input({ milestones: [behind] }), NOW, TH)
  assert.equal(h.level, 'orange')
})

test('gray with a reason when there is nothing to judge by', () => {
  assert.deepEqual(
    [
      projectHealth(input(null, { hasBoardUrl: false }), NOW, TH).reason,
      projectHealth(input(null, { gitlabOnly: true, hasBoardUrl: false }), NOW, TH).reason,
      projectHealth(input(null, { error: 'boom' }), NOW, TH).reason,
      projectHealth(input(null), NOW, TH).reason,
    ],
    ['no-board', 'gitlab', 'error', 'loading'],
  )
  // A truncated board with no signal is not "green".
  const cut = projectHealth(input({ truncated: true, milestones: [milestone({ issues: [issue({ deadline: '2026-12-30' })] })] }), NOW, TH)
  assert.equal(cut.level, 'gray')
  assert.equal(cut.reason, 'truncated')
  // …but a real signal on a truncated board still counts, with the caveat.
  const cutLate = projectHealth(input({ truncated: true, milestones: [milestone({ issues: [issue({ deadline: '2026-11-01' })] })] }), NOW, TH)
  assert.equal(cutLate.level, 'red')
  assert.equal(cutLate.reason, 'truncated')
  // Cached board + a failed refresh: judged on the cache, flagged.
  const stale = projectHealth(input({ milestones: [] }, { error: 'rate limited' }), NOW, TH)
  assert.equal(stale.level, 'gray')
  assert.equal(stale.reason, 'error')
})

test('without a linked board issue deadlines are ignored, milestones still count', () => {
  const h = projectHealth(input({ boardLinked: false, milestones: [
    milestone({ dueOn: '2026-11-10', openCount: 2, issues: [issue({ deadline: '2026-11-01' })] }),
  ] }), NOW, TH)
  assert.equal(h.counts.overdue, 0)
  assert.equal(h.level, 'red')
  assert.equal(h.reason, 'no-board')
})

test('compareHealth: red → orange → gray → green', () => {
  const mk = (level, name, overdue = 0) => ({
    projectId: name, projectName: name, level, reason: null, riskyIssueCount: 0, topIssues: [], milestones: [],
    fetchedAt: null, counts: { overdue, 'due-soon': 0, 'not-started': 0, stale: 0, 'no-deadline': 0, 'on-track': 0 },
  })
  const list = [mk('green', 'a'), mk('gray', 'b'), mk('red', 'c', 1), mk('orange', 'd'), mk('red', 'e', 3)]
  assert.deepEqual(list.sort(compareHealth).map((h) => h.projectName), ['e', 'c', 'd', 'b', 'a'])
})
