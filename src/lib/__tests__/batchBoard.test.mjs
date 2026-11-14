// Unit tests for lib/batchBoard.ts (Batch Board lanes and card text).
// Run: node --test src/lib/__tests__/batchBoard.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { commitMessageFor, diffParts, groupByLane, groupQueueByProject, laneOf, lastActivity, roleLabel } from '../batchBoard.ts'

const item = (over = {}) => ({
  runId: 'r1',
  title: '[implementer] Fix login',
  role: 'worker',
  projectId: 'p',
  projectName: 'shop',
  engine: 'claude',
  status: 'done',
  createdAt: '',
  startedAt: null,
  finishedAt: null,
  costUsd: 0,
  todo: { id: 't', text: 'Fix login\nmore', status: 'waiting', done: false },
  worktree: null,
  diff: null,
  ...over,
})

test('a pending prompt puts the worker in "needs" whatever its status', () => {
  assert.equal(laneOf(item({ status: 'running' }), { pending: 1, attention: 0 }), 'needs')
  assert.equal(laneOf(item({ status: 'done' }), { pending: 2, attention: 1 }), 'needs')
})

test('running workers split on liveness notices; the live status wins over the snapshot', () => {
  assert.equal(laneOf(item({ status: 'running' }), undefined), 'running')
  assert.equal(laneOf(item({ status: 'running' }), { pending: 0, attention: 1 }), 'alert')
  assert.equal(laneOf(item({ status: 'done' }), { pending: 0, attention: 0, status: 'running' }), 'running')
  assert.equal(laneOf(item({ status: 'fetched' }), undefined), 'running')
})

test('finished workers wait for review until the todo is done or the worktree removed', () => {
  assert.equal(laneOf(item(), undefined), 'review')
  assert.equal(laneOf(item({ todo: { id: 't', text: 'x', status: 'todo', done: true } }), undefined), 'closed')
  assert.equal(laneOf(item({ worktree: { removedAt: '2026-01-01' } }), undefined), 'closed')
  assert.equal(laneOf(item({ status: 'failed' }), undefined), 'failed')
  assert.equal(laneOf(item({ status: 'cancelled' }), undefined), 'failed')
})

test('groupByLane skips the conductor and keeps spawn order', () => {
  const lanes = groupByLane(
    [item({ runId: 'c', role: 'conductor', status: 'running' }), item({ runId: 'a' }), item({ runId: 'b', status: 'running' }), item({ runId: 'd' })],
    (id) => (id === 'b' ? { pending: 1, attention: 0 } : undefined),
  )
  assert.deepEqual(lanes.needs.map((i) => i.runId), ['b'])
  assert.deepEqual(lanes.review.map((i) => i.runId), ['a', 'd'])
  assert.equal(lanes.running.length, 0)
})

test('roleLabel reads the [label] prefix', () => {
  assert.equal(roleLabel('[implementer] Fix'), 'implementer')
  assert.equal(roleLabel('Fix'), null)
  assert.equal(roleLabel(null), null)
})

test('lastActivity takes the newest tool, text or result line', () => {
  assert.equal(lastActivity([{ kind: 'text', text: 'hello\n world' }, { kind: 'tool', name: 'Bash' }]), '⚙ Bash')
  assert.equal(lastActivity([{ kind: 'tool', name: 'Bash', displayName: 'Run tests' }, { kind: 'thinking', text: 'x' }]), '⚙ Run tests')
  assert.equal(lastActivity([{ kind: 'system' }, { kind: 'text', text: '   ' }]), null)
  assert.equal(lastActivity([{ kind: 'text', text: 'a'.repeat(10) }], 5), 'aaaa…')
})

test('diffParts hides an empty diff', () => {
  assert.equal(diffParts(null), null)
  assert.equal(diffParts({ commits: 0, files: 0, insertions: 0, deletions: 0, uncommitted: 0 }), null)
  assert.equal(diffParts({ commits: 0, files: 0, insertions: 0, deletions: 0, uncommitted: 2 })?.uncommitted, 2)
})

test('commitMessageFor uses the todo first line, capped', () => {
  assert.equal(commitMessageFor(item()), 'Fix login')
  assert.equal(commitMessageFor(item({ todo: null, title: null })), 'Devdy task')
  assert.equal(commitMessageFor(item({ todo: { id: 't', text: 'x'.repeat(100), status: '', done: false } })).length, 72)
})

test('groupQueueByProject: one group per project, A–Z, todos in added order', () => {
  const q = (id, projectId, projectName) => ({ todo: { id, text: id, status: 'todo', done: false }, projectId, projectName, addedAt: '' })
  const groups = groupQueueByProject([q('a', 'p2', 'shop'), q('b', 'p1', 'admin'), q('c', 'p2', 'shop'), q('x', null, null)])
  assert.deepEqual(
    groups.map((g) => [g.projectId, g.projectName, g.todos.map((t) => t.todo.id)]),
    [['p1', 'admin', ['b']], ['p2', 'shop', ['a', 'c']]],
  )
})

test('laneOf: a discarded result (todo back to "todo") is closed, not waiting for review', () => {
  assert.equal(laneOf(item({ todo: { id: 't', text: 'x', status: 'todo', done: false } }), undefined), 'closed')
  assert.equal(laneOf(item({ todo: { id: 't', text: 'x', status: 'waiting', done: false } }), undefined), 'review')
  // Still running: it stays in its live lane whatever the todo says.
  assert.equal(laneOf(item({ status: 'running', todo: { id: 't', text: 'x', status: 'todo', done: false } }), undefined), 'running')
  // A failed / not yet started run keeps its own lane.
  assert.equal(laneOf(item({ status: 'failed', todo: { id: 't', text: 'x', status: 'todo', done: false } }), undefined), 'failed')
  assert.equal(laneOf(item({ status: 'fetched', todo: { id: 't', text: 'x', status: 'todo', done: false } }), undefined), 'running')
})
