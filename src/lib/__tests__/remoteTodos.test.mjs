// Unit tests for lib/remoteTodos.ts (Remote Controller Todo / Note screens).
// Run: node --test src/lib/__tests__/remoteTodos.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  activeDueChip,
  addDays,
  countTodos,
  doneTodos,
  dueFromChip,
  filterByProject,
  groupOpenTodos,
  joinTodoText,
  noteDisplayTitle,
  noteListPreview,
  notePreview,
  searchTodos,
  splitTodoText,
  todoGroup,
} from '../remoteTodos.ts'

const todo = (id, over = {}) => ({ id, text: id, done: false, created_at: '2026-10-01T00:00:00Z', ...over })

test('splitTodoText: first line is the title, the rest the note', () => {
  assert.deepEqual(splitTodoText('Ship it'), { title: 'Ship it', note: '' })
  assert.deepEqual(splitTodoText(' Ship it \n\n line 2\nline 3 '), { title: 'Ship it', note: 'line 2\nline 3' })
})

test('joinTodoText round-trips and needs a title', () => {
  assert.equal(joinTodoText(' Ship ', ' details '), 'Ship\ndetails')
  assert.equal(joinTodoText('Ship', '  '), 'Ship')
  assert.equal(joinTodoText('  ', 'note only'), '')
  // A pasted newline in the title cannot push text into the note.
  assert.equal(joinTodoText('a\nb', ''), 'a b')
  const { title, note } = splitTodoText(joinTodoText('T', 'n1\nn2'))
  assert.deepEqual({ title, note }, { title: 'T', note: 'n1\nn2' })
})

test('filterByProject and countTodos', () => {
  const list = [todo('a', { project_id: 'p1' }), todo('b', { project_id: 'p2', done: true }), todo('c')]
  assert.deepEqual(filterByProject(list, null).map((t) => t.id), ['a', 'b', 'c'])
  assert.deepEqual(filterByProject(list, 'p2').map((t) => t.id), ['b'])
  assert.deepEqual(countTodos(list), { active: 2, done: 1 })
})

test('todoGroup buckets against today', () => {
  const today = '2026-10-31'
  assert.equal(todoGroup(todo('a', { due_date: '2026-10-30' }), today), 'overdue')
  assert.equal(todoGroup(todo('a', { due_date: today }), today), 'today')
  assert.equal(todoGroup(todo('a', { due_date: '2026-11-02' }), today), 'upcoming')
  assert.equal(todoGroup(todo('a'), today), 'none')
  assert.equal(todoGroup(todo('a', { due_date: 'garbage' }), today), 'none')
})

test('groupOpenTodos: order, sorting, empty groups dropped, done excluded', () => {
  const today = '2026-10-31'
  const groups = groupOpenTodos(
    [
      todo('n1'),
      todo('o2', { due_date: '2026-10-29' }),
      todo('o1', { due_date: '2026-10-20' }),
      todo('t1', { due_date: today }),
      todo('d1', { due_date: '2026-10-01', done: true }),
      todo('n2'),
    ],
    today,
  )
  assert.deepEqual(
    groups.map((g) => [g.key, g.todos.map((t) => t.id)]),
    [
      ['overdue', ['o1', 'o2']],
      ['today', ['t1']],
      ['none', ['n1', 'n2']],
    ],
  )
})

test('doneTodos: most recently completed first', () => {
  const list = doneTodos([
    todo('a', { done: true, completed_at: '2026-10-01T00:00:00Z' }),
    todo('b'),
    todo('c', { done: true, completed_at: '2026-10-05T00:00:00Z' }),
  ])
  assert.deepEqual(list.map((t) => t.id), ['c', 'a'])
})

test('addDays crosses months and years', () => {
  assert.equal(addDays('2026-10-31', 1), '2026-11-01')
  assert.equal(addDays('2026-12-28', 7), '2027-01-04')
  assert.equal(addDays('2026-03-01', -1), '2026-02-28')
  assert.equal(addDays('bad', 1), 'bad')
})

test('due chips', () => {
  const today = '2026-10-31'
  assert.equal(dueFromChip('today', today), today)
  assert.equal(dueFromChip('tomorrow', today), '2026-11-01')
  assert.equal(dueFromChip('next_week', today), '2026-11-07')
  assert.equal(dueFromChip('clear', today), '')
  assert.equal(activeDueChip('2026-11-01', today), 'tomorrow')
  assert.equal(activeDueChip('', today), 'clear')
  assert.equal(activeDueChip('2026-12-25', today), null)
})

test('noteDisplayTitle falls back to the first content line', () => {
  assert.equal(noteDisplayTitle({ title: ' Plan ', content: 'x' }), 'Plan')
  assert.equal(noteDisplayTitle({ title: '', content: '\n## Heading\nbody' }), 'Heading')
  assert.equal(noteDisplayTitle({ title: '', content: '' }), '')
})

test('notePreview flattens Markdown and truncates', () => {
  assert.equal(notePreview('# Title\n- **one**\n- [x] two\n\n> quote'), 'Title one two quote')
  const long = notePreview('a'.repeat(300), 10)
  assert.equal(long.length, 10)
  assert.ok(long.endsWith('…'))
})

test('noteListPreview skips the line used as the title', () => {
  assert.equal(noteListPreview({ title: '', content: '# Go-live\n- Backup\n- Alert' }), 'Backup Alert')
  assert.equal(noteListPreview({ title: 'T', content: 'body' }), 'body')
  assert.equal(noteListPreview({ title: '', content: '' }), '')
})

test('searchTodos matches every word in title + note, ignoring case and accents', () => {
  const todos = [
    { id: 'a', text: 'Sửa lỗi đăng nhập\nkiểm tra token', done: false, created_at: 'c' },
    { id: 'b', text: 'Deploy relay', done: true, created_at: 'c' },
  ]
  assert.deepEqual(searchTodos(todos, '').map((t) => t.id), ['a', 'b'])
  assert.deepEqual(searchTodos(todos, '  dang NHAP ').map((t) => t.id), ['a'])
  assert.deepEqual(searchTodos(todos, 'token sua').map((t) => t.id), ['a'])
  assert.deepEqual(searchTodos(todos, 'relay').map((t) => t.id), ['b'])
  assert.deepEqual(searchTodos(todos, 'relay token'), [])
})
