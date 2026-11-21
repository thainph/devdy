// Unit tests for lib/itemListView.ts (the Todos / Notes sidebar's filter menu).
// Run: node --test src/lib/__tests__/itemListView.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  ITEM_NO_PROJECT,
  clearItemFilters,
  defaultItemListView,
  isFiltering,
  itemFilterSummary,
  itemStatusCounts,
  matchesItemView,
  parseItemListDisplay,
  viewDiffersFromDefault,
} from '../itemListView.ts'

const row = (id, projectId, over = {}) => ({ id, projectId, done: false, linkedRunCount: 0, ...over })

const TODOS = [
  row('a', 'p1'),
  row('b', 'p1'),
  row('c', 'p2'),
  row('d', null),
  row('e', 'p1', { done: true }),
]
const RADAR = { a: 'overdue', b: 'today', c: 'overdue', d: 'waiting' }

test('defaults: grouped, no filter, per-kind sort', () => {
  assert.deepEqual(defaultItemListView('todo'), { status: '', project: '', sort: 'urgency', grouped: true, showDone: false })
  assert.equal(defaultItemListView('note').sort, 'updated')
  assert.equal(viewDiffersFromDefault('todo', defaultItemListView('todo')), false)
})

test('viewDiffersFromDefault: filters, sort, grouping, show done', () => {
  const d = defaultItemListView('todo')
  assert.equal(viewDiffersFromDefault('todo', { ...d, status: 'overdue' }), true)
  assert.equal(viewDiffersFromDefault('todo', { ...d, project: 'p1' }), true)
  assert.equal(viewDiffersFromDefault('todo', { ...d, sort: 'due' }), true)
  assert.equal(viewDiffersFromDefault('todo', { ...d, grouped: false }), true)
  assert.equal(viewDiffersFromDefault('todo', { ...d, showDone: true }), true)
  // Notes have no "show done".
  assert.equal(viewDiffersFromDefault('note', { ...defaultItemListView('note'), showDone: true }), false)
})

test('isFiltering / clearItemFilters only look at status and project', () => {
  const d = defaultItemListView('todo')
  assert.equal(isFiltering({ ...d, sort: 'due', grouped: false }), false)
  assert.equal(isFiltering({ ...d, project: ITEM_NO_PROJECT }), true)
  const cleared = clearItemFilters({ ...d, status: 'today', project: 'p1', sort: 'due' })
  assert.deepEqual(cleared, { ...d, sort: 'due' })
})

test('matchesItemView: project, done and lateness', () => {
  const d = defaultItemListView('todo')
  const ids = (view) => TODOS.filter((r) => matchesItemView('todo', r, view, RADAR)).map((r) => r.id)
  assert.deepEqual(ids(d), ['a', 'b', 'c', 'd'])
  assert.deepEqual(ids({ ...d, showDone: true }), ['a', 'b', 'c', 'd', 'e'])
  assert.deepEqual(ids({ ...d, project: 'p1' }), ['a', 'b'])
  assert.deepEqual(ids({ ...d, project: ITEM_NO_PROJECT }), ['d'])
  assert.deepEqual(ids({ ...d, status: 'overdue' }), ['a', 'c'])
  assert.deepEqual(ids({ ...d, status: 'overdue', project: 'p2' }), ['c'])
  // No lateness known yet: a status filter matches nothing.
  assert.deepEqual(TODOS.filter((r) => matchesItemView('todo', r, { ...d, status: 'today' }, null)).length, 0)
})

test('matchesItemView: notes linked to a session, done ignored', () => {
  const notes = [row('n1', 'p1', { linkedRunCount: 2 }), row('n2', 'p1'), row('n3', null, { done: true })]
  const d = defaultItemListView('note')
  assert.deepEqual(notes.filter((r) => matchesItemView('note', r, d, null)).map((r) => r.id), ['n1', 'n2', 'n3'])
  assert.deepEqual(
    notes.filter((r) => matchesItemView('note', r, { ...d, status: 'linked' }, null)).map((r) => r.id),
    ['n1'],
  )
})

test('itemStatusCounts: within the project scope, before the status filter', () => {
  const d = defaultItemListView('todo')
  const all = itemStatusCounts('todo', TODOS, { ...d, status: 'today' }, RADAR)
  assert.equal(all[''], 4)
  assert.equal(all.overdue, 2)
  assert.equal(all.today, 1)
  assert.equal(all.waiting, 1)
  assert.equal(all['due-soon'], 0)
  assert.equal(all.linked, 0)
  const p1 = itemStatusCounts('todo', TODOS, { ...d, project: 'p1', showDone: true }, RADAR)
  assert.equal(p1[''], 3)
  assert.equal(p1.overdue, 1)
  const notes = itemStatusCounts('note', [row('n1', 'p1', { linkedRunCount: 1 }), row('n2', 'p2')], defaultItemListView('note'), null)
  assert.equal(notes[''], 2)
  assert.equal(notes.linked, 1)
  assert.equal(notes.overdue, 0)
})

test('itemFilterSummary: status then project, empty when not filtering', () => {
  const labels = { status: (s) => `S:${s}`, project: (p) => `P:${p}` }
  const d = defaultItemListView('todo')
  assert.deepEqual(itemFilterSummary(d, labels), [])
  assert.deepEqual(itemFilterSummary({ ...d, status: 'overdue', project: 'p1' }, labels), ['S:overdue', 'P:p1'])
  assert.deepEqual(itemFilterSummary({ ...d, project: 'p1' }, labels), ['P:p1'])
})

test('parseItemListDisplay keeps only valid values for the kind', () => {
  assert.deepEqual(parseItemListDisplay('todo', null), { sort: 'urgency', grouped: true, showDone: false })
  assert.deepEqual(parseItemListDisplay('todo', { sort: 'due', grouped: false, showDone: true }), {
    sort: 'due',
    grouped: false,
    showDone: true,
  })
  // A note sort on todos, and "show done" on notes, fall back.
  assert.equal(parseItemListDisplay('todo', { sort: 'title' }).sort, 'urgency')
  assert.equal(parseItemListDisplay('note', { showDone: true }).showDone, false)
  assert.equal(parseItemListDisplay('note', { sort: 'title', grouped: 'yes' }).grouped, true)
})
