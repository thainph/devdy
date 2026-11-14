// Unit tests for the Controller's Focus wire (controller/protocol.ts): the
// working hours & reminders settings, and the todo fields the Host accepts.
// Run: node --test src/lib/__tests__/remoteFocusWire.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { focusSettingsSetCmd, parseStreamPayload, todoAddCmd } from '../../controller/protocol.ts'
import { createDataStore } from '../../controller/dataStore.ts'
import { DEFAULT_FOCUS_SETTINGS } from '../focus.ts'

function fakeSend() {
  const sent = []
  const send = (cmd, onAck) => {
    sent.push({ cmd, onAck })
    return true
  }
  return { sent, send }
}

test('focus_settings parses; the removed Focus board / AI payloads do not', () => {
  assert.equal(parseStreamPayload({ kind: 'focus_settings', settings: DEFAULT_FOCUS_SETTINGS }).settings.work_start, '09:00')
  assert.equal(parseStreamPayload({ kind: 'focus_settings', settings: { work_start: 1 } }), null)
  assert.equal(parseStreamPayload({ kind: 'focus_board', board: { now: 'n', items: [], top: [] } }), null)
  assert.equal(parseStreamPayload({ kind: 'focus_ai_result', request_id: 'r', ai_kind: 'suggest', ok: false, error: 'x' }), null)
})

test('command builders', () => {
  assert.deepEqual(focusSettingsSetCmd({ work_start: '08:00' }), {
    kind: 'cmd', action: 'focus_settings_set', settings: { work_start: '08:00' },
  })
  assert.equal(todoAddCmd({ text: 'x', sourceUrl: ' https://a ' }).source_url, 'https://a')
  assert.equal(todoAddCmd({ text: 'x', sourceUrl: '' }).source_url, undefined)
})

test('saving settings leaves out the desktop-only keys; the answer lands in state', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const done = data.saveSettings({ ...DEFAULT_FOCUS_SETTINGS, work_start: '08:00' })
  const cmd = sent[0].cmd
  assert.equal(cmd.action, 'focus_settings_set')
  assert.equal(cmd.settings.work_start, '08:00')
  for (const k of ['ai_engine', 'ai_model', 'calendar_account_id', 'calendar_ids']) assert.equal(k in cmd.settings, false)
  sent[0].onAck(true)
  assert.equal((await done).ok, true)
  data.apply({ kind: 'focus_settings', settings: { ...DEFAULT_FOCUS_SETTINGS, work_start: '08:00' } })
  assert.equal(data.state.settings.work_start, '08:00')
})

test('a todo edit sends only the changed fields', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const base = { id: 't1', text: 'T', done: false, due_date: null, project_id: null, created_at: 'c' }
  void data.saveTodo({ id: 't1', text: 'T2', dueDate: '', projectId: '', base })
  assert.deepEqual(sent[0].cmd.patch, { text: 'T2' })
})
