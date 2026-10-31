// Unit tests for the Controller's data wire (controller/protocol.ts) and the
// Todo / Note / switch store (controller/dataStore.ts).
// Run: node --test src/lib/__tests__/remoteData.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { hostHas, noteGetCmd, parseStreamPayload, projectToggleSetCmd } from '../../controller/protocol.ts'
import { createDataStore } from '../../controller/dataStore.ts'

test('session_token carries host_features; an older Host sends none', () => {
  const p = parseStreamPayload({
    kind: 'session_token', token: 't', run_id: 'r', rendezvous_code: 'c', host_features: ['data', 'inbox', 7],
  })
  assert.deepEqual(p.host_features, ['data', 'inbox'])
  const old = parseStreamPayload({ kind: 'session_token', token: 't', run_id: 'r', rendezvous_code: 'c' })
  assert.deepEqual(old.host_features, [])
  assert.equal(hostHas(p.host_features, 'data'), true)
  assert.equal(hostHas(p.host_features, 'remember'), false)
  assert.equal(hostHas(null, 'data'), false)
})

test('broker prompts parse as once-only; resolved carries remembered', () => {
  const req = parseStreamPayload({ kind: 'permission_request', run_id: 'r', request_id: 'q', tool: 'gh', input: {}, once_only: true })
  assert.equal(req.once_only, true)
  const plain = parseStreamPayload({ kind: 'permission_request', run_id: 'r', request_id: 'q', tool: 'Bash', input: {} })
  assert.equal(plain.once_only, false)
  const res = parseStreamPayload({ kind: 'permission_resolved', run_id: 'r', request_id: 'q', accepted: true, remembered: false })
  assert.equal(res.remembered, false)
})

test('note_list rows are summaries; note carries the full body', () => {
  const list = parseStreamPayload({
    kind: 'note_list',
    notes: [{ id: 'n1', title: 'T', preview: 'body…', updated_at: 'u' }, { id: 'bad', title: 'x', content: 'full', updated_at: 'u' }],
  })
  assert.deepEqual(list.notes.map((n) => n.id), ['n1'])
  const one = parseStreamPayload({ kind: 'note', note: { id: 'n1', title: 'T', content: 'full', updated_at: 'u' } })
  assert.equal(one.note.content, 'full')
  assert.equal(parseStreamPayload({ kind: 'note', note: { id: 'n1' } }), null)
  assert.deepEqual(noteGetCmd('n1'), { kind: 'cmd', action: 'note_get', item_id: 'n1' })
  assert.equal(projectToggleSetCmd('p', 'builtin_mcp', 'ignored', true).item_id, undefined)
})

/** A fake transport: records commands, lets the test answer acks. */
function fakeSend() {
  const sent = []
  const send = (cmd, onAck) => {
    sent.push({ cmd, onAck })
    return true
  }
  return { sent, send }
}

test('saveTodo sends only the fields changed since the sheet opened', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const base = { id: 't1', text: 'Old', done: false, due_date: '2026-11-01', project_id: 'p1', created_at: 'c' }
  // The desktop renamed it meanwhile; the phone only moved the date.
  data.apply({ kind: 'todo_list', todos: [{ ...base, text: 'Renamed on desktop' }] })
  const saving = data.saveTodo({ id: 't1', text: 'Old', dueDate: '2026-11-02', projectId: 'p1', base })
  sent.at(-1).onAck(true)
  assert.equal(await saving, true)
  assert.deepEqual(sent.at(-1).cmd.patch, { due_date: '2026-11-02' })
})

test('a refused or unanswered read stops the spinner', () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  data.loadTodos()
  sent.at(-1).onAck(false, 'host_ack_timeout')
  assert.equal(data.state.failed.todos, true)
  assert.equal(data.state.error, 'err:load_timeout')
  data.loadTodos()
  assert.equal(data.state.failed.todos, false, 'a retry clears the failure')
})

test('a stale note-list failure is ignored once the filter moved on', () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  data.loadNotes(null, 'old')
  const stale = sent.at(-1)
  data.loadNotes(null, 'new')
  stale.onAck(false, 'host_ack_timeout')
  assert.equal(data.state.failed.notes, false)
  // …and a reply for the old filter is dropped.
  data.apply({ kind: 'note_list', notes: [], project_id: null, query: 'old' })
  assert.equal(data.state.notes, null)
})

test('note_get fills the editor only for the note it asked for', () => {
  const { send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  data.loadNote('n1')
  data.apply({ kind: 'note', note: { id: 'n2', title: '', content: 'x', updated_at: 'u' } })
  assert.equal(data.state.note, null)
  data.apply({ kind: 'note', note: { id: 'n1', title: 'T', content: 'full', updated_at: 'u' } })
  assert.equal(data.state.note.content, 'full')
})

test('a refused switch re-fetches only while that project is still shown', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const toggles = (pid) => ({
    kind: 'project_toggles', project_id: pid, mcp_servers: [], skills: [{ id: 's1', name: 's', enabled: false }],
    rules: [], builtin_devdy: { enabled: true, global: true },
  })
  data.loadToggles('p1')
  data.apply(toggles('p1'))
  data.setToggle('skill', 's1', true)
  const set = sent.at(-1)
  data.loadToggles('p2')
  const before = sent.length
  set.onAck(false, 'failed')
  await new Promise((r) => setTimeout(r, 0))
  assert.equal(sent.length, before, 'no re-fetch of p1 while p2 is shown')
})

test('project_toggles carries servers + accounts; an older Host sends none', () => {
  const p = parseStreamPayload({
    kind: 'project_toggles', project_id: 'p1', mcp_servers: [], skills: [], rules: [],
    builtin_devdy: { enabled: true, global: true },
    servers: [{ id: 'v1', name: 'prod', enabled: true }],
    github_accounts: [{ id: 'g1', name: 'work', enabled: false }, { id: 'bad' }],
    claude_accounts: [{ id: 'c1', name: 'main', enabled: false, is_default: true }],
  })
  assert.deepEqual(p.servers, [{ id: 'v1', name: 'prod', enabled: true }])
  assert.deepEqual(p.github_accounts.map((a) => a.id), ['g1'])
  assert.deepEqual(p.claude_accounts, [{ id: 'c1', name: 'main', enabled: false, is_default: true }])
  const old = parseStreamPayload({
    kind: 'project_toggles', project_id: 'p1', mcp_servers: [], skills: [], rules: [], builtin_devdy: {},
  })
  for (const k of ['servers', 'github_accounts', 'gitlab_accounts', 'aws_accounts', 'azure_accounts', 'claude_accounts']) {
    assert.deepEqual(old[k], [], k)
  }
})

test('switching a Claude account on switches the others off', () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  data.loadToggles('p1')
  data.apply(parseStreamPayload({
    kind: 'project_toggles', project_id: 'p1', mcp_servers: [], skills: [], rules: [], builtin_devdy: {},
    servers: [{ id: 'v1', name: 'prod', enabled: false }],
    claude_accounts: [
      { id: 'c1', name: 'main', enabled: true, is_default: true },
      { id: 'c2', name: 'alt', enabled: false, is_default: false },
    ],
  }))
  data.setToggle('claude_account', 'c2', true)
  assert.deepEqual(data.state.toggles.claude_accounts.map((a) => a.enabled), [false, true])
  assert.equal(sent.at(-1).cmd.toggle_kind, 'claude_account')
  data.setToggle('server', 'v1', true)
  assert.equal(data.state.toggles.servers[0].enabled, true)
})

test('appearance needs both fields', () => {
  assert.deepEqual(parseStreamPayload({ kind: 'appearance', theme: 'dark', color_theme: 'ocean' }), {
    kind: 'appearance', theme: 'dark', color_theme: 'ocean',
  })
  assert.equal(parseStreamPayload({ kind: 'appearance', theme: 'dark' }), null)
})
