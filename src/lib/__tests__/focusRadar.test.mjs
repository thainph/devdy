// Unit tests for lib/focusRadar.ts (todo lateness + per-project rollup).
// Run: node --test src/lib/__tests__/focusRadar.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { classifyTodo, classifyTodos, countFocus, filterFocus, groupByProject } from '../focusRadar.ts'

// Saturday 2026-11-07 10:00 local time.
const NOW = new Date(2026, 10, 7, 10, 0).getTime()

let seq = 0
function item(todo = {}, extra = {}) {
  seq += 1
  const due = todo.due_date ?? null
  return {
    todo: {
      id: `t${seq}`,
      text: `Todo ${seq}`,
      done: false,
      project_id: 'p1',
      due_date: due,
      due_time: null,
      status: 'todo',
      source_ref: null,
      ...todo,
    },
    bucket: 'today',
    score: 0,
    reasons: [],
    start_by: null,
    // End of the working day unless a time is given.
    effective_due: due ? new Date(`${due}T${todo.due_time ?? '18:00'}:00`).toISOString() : null,
    effective_estimate_min: 30,
    estimate_missing: false,
    ...extra,
  }
}

test('classifyTodo by deadline', () => {
  assert.equal(classifyTodo(item({ due_date: '2026-11-05' }), NOW), 'overdue')
  assert.equal(classifyTodo(item({ due_date: '2026-11-07' }), NOW), 'today')
  // Due earlier today → already overdue.
  assert.equal(classifyTodo(item({ due_date: '2026-11-07', due_time: '09:00' }), NOW), 'overdue')
  assert.equal(classifyTodo(item({ due_date: '2026-11-14' }), NOW), 'due-soon')
  assert.equal(classifyTodo(item({ due_date: '2026-11-15' }), NOW), 'later')
  assert.equal(classifyTodo(item(), NOW), 'no-due')
})

test('doing past its deadline is overdue; waiting is its own status', () => {
  assert.equal(classifyTodo(item({ due_date: '2026-11-01', status: 'doing' }, { bucket: 'doing' }), NOW), 'overdue')
  assert.equal(classifyTodo(item({ due_date: '2026-11-01', status: 'waiting' }, { bucket: 'waiting' }), NOW), 'waiting')
})

test('classifyTodos drops done todos', () => {
  const out = classifyTodos([item({ done: true }, { bucket: 'done' }), item({ due_date: '2026-11-06' })], NOW)
  assert.equal(out.length, 1)
  assert.equal(out[0].daysLeft, -1)
})

test('groupByProject: most late project first, no project last', () => {
  const items = classifyTodos(
    [
      item({ project_id: 'a', due_date: '2026-11-06' }),
      item({ project_id: 'b', due_date: '2026-11-01' }),
      item({ project_id: 'b', due_date: '2026-11-08' }),
      item({ project_id: null, due_date: '2026-10-01' }),
      item({ project_id: 'c' }),
    ],
    NOW,
  )
  const groups = groupByProject(items)
  assert.deepEqual(groups.map((g) => g.projectId), ['b', 'a', 'c', null])
  assert.equal(groups[0].worstLateDays, 6)
  assert.equal(groups[0].counts['due-soon'], 1)
  assert.equal(groups[0].items[0].radar, 'overdue')
  assert.equal(countFocus(items).overdue, 3)
})

test('filterFocus by status and text', () => {
  const items = classifyTodos([item({ text: 'Send quote to client', due_date: '2026-11-01' }), item({ text: 'Plan sprint' })], NOW)
  assert.equal(filterFocus(items, { status: 'overdue' }).length, 1)
  assert.equal(filterFocus(items, { query: 'QUOTE' }).length, 1)
  assert.equal(filterFocus(items, { status: 'no-due', query: 'quote' }).length, 0)
})

test('lateness helpers order the Todos list', async () => {
  const { latenessById, compareLateness, groupLateness, compareGroupLateness } = await import('../focusRadar.ts')
  const a = item({ due_date: '2026-11-01' })
  const b = item({ due_date: '2026-11-06' })
  const c = item({ due_date: '2026-11-20' })
  const done = item({ done: true, due_date: '2026-10-01' }, { bucket: 'done' })
  const by = latenessById([a, b, c, done], NOW)
  assert.equal(by[done.todo.id], undefined)
  const order = [done, c, b, a].map((x) => x.todo.id).sort((x, y) => compareLateness(by[x], by[y]))
  assert.deepEqual(order, [a.todo.id, b.todo.id, c.todo.id, done.todo.id])
  const late = groupLateness([by[a.todo.id], by[b.todo.id], undefined])
  assert.deepEqual(late, { overdue: 2, worstLateDays: 6 })
  const calm = groupLateness([by[c.todo.id]])
  assert.ok(compareGroupLateness(late, calm) < 0)
  assert.equal(compareGroupLateness(calm, groupLateness([])), 0)
})
