// Unit tests for lib/workDashboard.ts (Work Dashboard todos / sessions / strip).
// Run: node --test src/lib/__tests__/workDashboard.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  ageMs, attentionItems, dashboardRuns, dashboardTodos, inProject, shortDuration, todoGroup,
} from '../workDashboard.ts'

// Monday 2026-11-16 10:00 local time.
const NOW = new Date(2026, 10, 16, 10, 0).getTime()
const pad = (n) => String(n).padStart(2, '0')
const ymd = (offset) => {
  const d = new Date(2026, 10, 16 + offset)
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

let seq = 0
function item(todo = {}, extra = {}) {
  seq += 1
  const due = todo.due_date ?? null
  return {
    todo: { id: `t${seq}`, text: `Todo ${seq}`, done: false, project_id: 'p1', due_date: due, due_time: null, status: 'todo', ...todo },
    bucket: 'today', score: 0, reasons: [], start_by: null,
    effective_due: due ? new Date(`${due}T18:00:00`).toISOString() : null,
    ...extra,
  }
}

test('todo groups: overdue → today → due within 3 days → doing / waiting', () => {
  assert.equal(todoGroup(item({ due_date: ymd(-2) }), NOW), 'overdue')
  assert.equal(todoGroup(item({ due_date: ymd(0) }), NOW), 'today')
  assert.equal(todoGroup(item({ due_date: ymd(3) }), NOW), 'soon')
  assert.equal(todoGroup(item({ due_date: ymd(5) }), NOW), null)
  assert.equal(todoGroup(item({ due_date: ymd(5), status: 'doing' }), NOW), 'active')
  assert.equal(todoGroup(item({ status: 'waiting', due_date: ymd(-1) }), NOW), 'active')
  assert.equal(todoGroup(item({}), NOW), null)
  assert.equal(todoGroup(item({ due_date: ymd(-2) }, { bucket: 'snoozed' }), NOW), null)
  assert.equal(todoGroup(item({ due_date: ymd(-2), done: true }), NOW), null)
})

test('todos are ordered by group, then deadline', () => {
  const list = dashboardTodos([
    item({ text: 'doing', status: 'doing' }),
    item({ text: 'soon', due_date: ymd(2) }),
    item({ text: 'late1', due_date: ymd(-1) }),
    item({ text: 'today', due_date: ymd(0) }),
    item({ text: 'late5', due_date: ymd(-5) }),
  ], NOW)
  assert.deepEqual(list.map((t) => t.item.todo.text), ['late5', 'late1', 'today', 'soon', 'doing'])
  assert.equal(list[0].daysLeft, -5)
})

function run(id, extra = {}) {
  return {
    run_id: id, project_id: 'p1', project_name: 'Alpha', title: id, run_type: 'session', ref_number: null,
    engine: 'claude', model: null, status: 'running', started_at: '2026-11-16T09:00:00Z', created_at: '2026-11-16T08:00:00Z',
    role: null, conductor_run_id: null, pending_permissions: 0, waiting_since: null, ...extra,
  }
}

test('sessions: awaiting permission first, then the longest running; workers fold into their conductor', () => {
  const rows = dashboardRuns([
    run('short', { started_at: new Date(NOW - 60_000).toISOString() }),
    run('long', { started_at: new Date(NOW - 3_600_000).toISOString() }),
    run('cond', { role: 'conductor', started_at: new Date(NOW - 120_000).toISOString() }),
    run('worker', { conductor_run_id: 'cond', pending_permissions: 1, waiting_since: new Date(NOW - 30_000).toISOString() }),
    run('orphan', { conductor_run_id: 'gone', started_at: new Date(NOW - 10_000).toISOString() }),
  ], NOW)
  assert.deepEqual(rows.map((r) => r.run.run_id), ['cond', 'long', 'short', 'orphan'])
  assert.equal(rows[0].pending, 1)
  // The prompt belongs to the worker: "Review" must open the worker, not the conductor.
  assert.deepEqual(rows[0].waiting.map((w) => [w.runId, w.worker]), [['worker', true]])
  assert.deepEqual(rows[1].waiting, [])
  assert.equal(rows[0].workerCount, 1)
  assert.equal(rows[0].sinceMs, 30_000)
  assert.equal(rows[1].sinceMs, 3_600_000)
})

test('several waiting sessions in one row are listed, longest waiting first', () => {
  const rows = dashboardRuns([
    run('cond', { role: 'conductor', pending_permissions: 1, waiting_since: new Date(NOW - 10_000).toISOString() }),
    run('w1', { conductor_run_id: 'cond', pending_permissions: 2, waiting_since: new Date(NOW - 50_000).toISOString() }),
    run('w2', { conductor_run_id: 'cond' }),
  ], NOW)
  assert.equal(rows.length, 1)
  assert.equal(rows[0].pending, 3)
  assert.deepEqual(rows[0].waiting.map((w) => [w.runId, w.pending, w.worker]), [['w1', 2, true], ['cond', 1, false]])
  assert.equal(rows[0].sinceMs, 50_000)
})

test('project filter, ages and durations', () => {
  assert.ok(inProject('p1', null))
  assert.ok(inProject(null, null))
  assert.ok(!inProject(null, 'p1'))
  assert.ok(inProject('p1', 'p1'))
  assert.equal(ageMs(NOW - 5000, NOW), 5000)
  assert.equal(ageMs('nope', NOW), null)
  assert.equal(ageMs(null, NOW), null)
  assert.deepEqual(shortDuration(10_000), { value: 1, unit: 'm' })
  assert.deepEqual(shortDuration(3 * 3_600_000 + 5), { value: 3, unit: 'h' })
  assert.deepEqual(shortDuration(50 * 3_600_000), { value: 2, unit: 'd' })
})

test('attention strip keeps non-zero items in a fixed order', () => {
  assert.deepEqual(
    attentionItems({ projects: 1, slack: 0, runs: 2, prs: 3, todos: 4 }),
    [{ key: 'todos', count: 4 }, { key: 'prs', count: 3 }, { key: 'runs', count: 2 }, { key: 'projects', count: 1 }],
  )
})
