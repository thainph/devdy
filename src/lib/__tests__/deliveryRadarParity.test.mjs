// The built-in MCP server carries a plain-JS copy of the radar logic
// (sidecar-mcp/lib/radar.mjs). Run both on the same issues so they cannot drift.
// Run: node --test src/lib/__tests__/deliveryRadarParity.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import * as ts from '../deliveryRadar.ts'
import * as js from '../../../sidecar-mcp/lib/radar.mjs'

const NOW = new Date(2026, 10, 7, 10, 0).getTime()
const TH = { dueSoonDays: 3, staleDays: 5 }
const DAY = 86_400_000
const ymd = (offsetDays) => {
  const d = new Date(NOW + offsetDays * DAY)
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`
}

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

const epic = issue({ title: 'Epic', deadline: ymd(10), assignees: [] })
const ISSUES = [
  epic,
  issue({ deadline: ymd(-4), parent: { owner: 'acme', repo: 'web', number: epic.number, title: 'Epic', htmlUrl: '' } }),
  issue({ deadline: ymd(0), assignees: ['bob', 'carol'] }),
  issue({ deadline: ymd(2), status: 'Ready for QA', assignees: ['alice', 'carol'] }),
  issue({ startDate: ymd(-1), status: 'Todo', assignees: ['bob'] }),
  issue({ updatedAt: new Date(NOW - 9 * DAY).toISOString() }),
  issue({ status: 'Done' }),
  issue({ deadline: ymd(20), projectId: 'p2', projectName: 'Beta', assignees: ['dave'] }),
  issue({ state: 'CLOSED' }),
  issue({ status: 'Testing', assignees: [] }),
  issue({ status: 'Custom column', deadline: ymd(-1), assignees: ['Carol'] }),
]
const RULES = [{}, { testers: ['carol'] }, { testers: ['CAROL'], stages: { 'custom column': 'done', testing: 'dev' } }]

test('classifyAll / buildGroups / filterIssues match the TypeScript radar', () => {
  for (const rules of RULES) {
    const a = ts.classifyAll(ISSUES, NOW, TH, rules)
    const b = js.classifyAll(ISSUES, NOW, TH, rules)
    assert.deepEqual(b, a)
    for (const mode of ['person', 'project']) assert.deepEqual(js.buildGroups(b, mode), ts.buildGroups(a, mode))
    assert.deepEqual(js.nestIssues(b), ts.nestIssues(a))
    assert.deepEqual(js.countByStatus(b), ts.countByStatus(a))
    for (const opts of [{ status: 'overdue' }, { query: 'epic' }, { query: '#3' }, { query: 'bob' }]) {
      assert.deepEqual(js.filterIssues(b, opts), ts.filterIssues(a, opts))
    }
  }
})

test('stage defaults and thresholds match', () => {
  for (const s of ['', 'Todo', 'In progress', 'Ready for QA', 'UAT', 'Staging', 'Released', "Won't fix", 'Hoàn thành']) {
    assert.equal(js.defaultStage(s), ts.defaultStage(s), s)
  }
  assert.deepEqual(js.DEFAULT_THRESHOLDS, ts.DEFAULT_THRESHOLDS)
  assert.deepEqual(js.RADAR_STATUSES, ts.RADAR_STATUSES)
})

test('milestoneRisk matches the TypeScript radar', () => {
  const milestones = [
    { title: 'late', dueOn: `${ymd(-3)}T00:00:00Z`, openCount: 2, closedCount: 1, issues: [] },
    { title: 'closed out', dueOn: ymd(-3), openCount: 0, closedCount: 4, issues: [] },
    { title: 'behind', dueOn: ymd(10), openCount: 8, closedCount: 1, issues: [issue({ startDate: ymd(-20) })] },
    { title: 'on pace', dueOn: ymd(10), openCount: 2, closedCount: 8, issues: [issue({ startDate: ymd(-20) })] },
    { title: 'no start', dueOn: ymd(10), openCount: 8, closedCount: 0, issues: [issue()] },
    { title: 'no due', dueOn: null, openCount: 8, closedCount: 0, issues: [] },
    { title: 'counts unknown, late', dueOn: ymd(-2), openCount: 1, closedCount: 0, countsKnown: false, issues: [] },
    { title: 'counts unknown, lag', dueOn: ymd(10), openCount: 8, closedCount: 0, countsKnown: false, issues: [issue({ startDate: ymd(-20) })] },
  ]
  for (const lag of [0, 20, 60]) {
    const a = milestones.map((m) => ts.milestoneRisk(m, NOW, lag))
    assert.deepEqual(milestones.map((m) => js.milestoneRisk(m, NOW, lag)), a)
    const risks = a.filter(Boolean)
    assert.deepEqual([...risks].sort(js.compareMilestoneRisks), [...risks].sort(ts.compareMilestoneRisks))
  }
})
