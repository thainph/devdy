// Unit tests for the milestone view (mode B) of lib/deliveryRadar.ts.
// Run: node --test src/lib/__tests__/deliveryRadarMilestones.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEFAULT_THRESHOLDS,
  buildMilestoneViews,
  compareMilestones,
  countMilestones,
  filterMilestones,
  milestoneElapsedPct,
} from '../deliveryRadar.ts'

// 2026-11-07 10:00 local time.
const NOW = new Date(2026, 10, 7, 10, 0).getTime()
const TH = { ...DEFAULT_THRESHOLDS, dueSoonDays: 3, staleDays: 5, milestoneLagPct: 20 }
const DAY = 86_400_000
const ymd = (offsetDays) => {
  const d = new Date(NOW + offsetDays * DAY)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}
const PROJECT = { id: 'p1', name: 'Alpha' }

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
    deadline: ymd(20),
    status: 'In progress',
    statusColor: null,
    state: 'OPEN',
    ...extra,
  }
}

function ms(extra = {}) {
  const issues = extra.issues ?? [issue()]
  return { title: 'Sprint', dueOn: ymd(30), openCount: issues.length, closedCount: 0, countsKnown: true, issues, ...extra }
}

const view = (m, rules) => buildMilestoneViews(PROJECT, [m], NOW, TH, rules)[0]

test('health: done when closed on GitHub or nothing left open', () => {
  assert.equal(view(ms({ closedAt: '2026-11-01T00:00:00Z' })).health, 'done')
  assert.equal(view(ms({ issues: [], openCount: 0, closedCount: 4 })).health, 'done')
  // Unknown totals: an empty list says nothing about being done.
  assert.notEqual(view(ms({ issues: [], openCount: 0, countsKnown: false })).health, 'done')
})

test('health: overdue, then lag', () => {
  assert.equal(view(ms({ dueOn: ymd(-2) })).health, 'overdue')
  // 10 days planned, 8 elapsed, 1 of 10 closed → lag.
  const lag = view(ms({
    dueOn: ymd(1), openCount: 9, closedCount: 1, issues: [issue({ startDate: ymd(-8) })],
  }))
  assert.equal(lag.health, 'lag')
  assert.ok(lag.elapsedPct > 70 && lag.elapsedPct < 90)
  assert.equal(lag.closedPct, 10)
})

test('health: at risk when an issue is late or due after the milestone', () => {
  assert.equal(view(ms({ issues: [issue({ deadline: ymd(-1) })] })).health, 'at-risk')
  const after = view(ms({ dueOn: ymd(10), issues: [issue({ deadline: ymd(12) })] }))
  assert.equal(after.health, 'at-risk')
  assert.equal(after.afterDue.length, 1)
})

test('health: due soon, on track, no due date, no milestone', () => {
  assert.equal(view(ms({ dueOn: ymd(2), issues: [issue({ deadline: ymd(1) })] })).health, 'due-soon')
  assert.equal(view(ms()).health, 'on-track')
  assert.equal(view(ms({ dueOn: null })).health, 'no-due')
  const none = view(ms({ title: 'No milestone', dueOn: null, countsKnown: false }))
  assert.equal(none.noMilestone, true)
  assert.equal(none.health, 'no-due')
  assert.equal(none.closedPct, null)
})

test('progress counts every open issue; the list leaves hidden people out', () => {
  const v = view(
    ms({
      openCount: 3,
      closedCount: 1,
      issues: [
        issue({ assignees: ['alice'] }),
        issue({ assignees: ['bob'] }),
        issue({ assignees: ['carol'], status: 'Ready for QA' }),
      ],
    }),
    { hidden: ['bob'] },
  )
  assert.equal(v.issues.length, 1) // carol's part is done (dev at QA), bob is hidden
  assert.equal(v.hiddenCount, 1)
  assert.equal(v.pastDevCount, 1)
  assert.equal(v.closedPct, 25)
  assert.equal(v.pastDevPct, 25)
  assert.deepEqual(v.people, ['alice'])
})

test('elapsed share needs a start date', () => {
  assert.equal(milestoneElapsedPct(ms(), NOW), null)
  assert.equal(milestoneElapsedPct(ms({ dueOn: ymd(9), issues: [issue({ startDate: ymd(-10) })] }), NOW) > 50, true)
})

test('sort, count and filter', () => {
  const views = buildMilestoneViews(PROJECT, [
    ms({ title: 'Later' }),
    ms({ title: 'No milestone', dueOn: null, countsKnown: false }),
    ms({ title: 'Late', dueOn: ymd(-1) }),
    ms({ title: 'Shipped', closedAt: '2026-11-01T00:00:00Z' }),
    ms({ title: 'Soon', dueOn: ymd(1), issues: [issue({ deadline: ymd(1), title: 'Login form' })] }),
  ], NOW, TH).sort(compareMilestones)
  assert.deepEqual(views.map((v) => v.title), ['Late', 'Soon', 'Later', 'Shipped', 'No milestone'])
  const counts = countMilestones(views)
  assert.equal(counts.overdue, 1)
  assert.equal(counts['no-due'], 0) // the no-milestone bucket is not a milestone
  assert.deepEqual(filterMilestones(views, { health: 'due-soon' }).map((v) => v.title), ['Soon'])
  const byIssue = filterMilestones(views, { query: 'login' })
  assert.deepEqual(byIssue.map((v) => v.title), ['Soon'])
  assert.equal(byIssue[0].issues.length, 1)
})
