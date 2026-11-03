// Unit tests for the Controller's full-Focus wire (controller/protocol.ts) and
// the AI / plan / settings side of the data store (controller/dataStore.ts).
// Run: node --test src/lib/__tests__/remoteFocusWire.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  focusAiSuggestCmd,
  focusPlanToCalendarCmd,
  parseStreamPayload,
  todoAddCmd,
} from '../../controller/protocol.ts'
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

const PLAN = {
  date: '2026-11-03',
  blocks: [{ id: 'b1', todo_id: 't1', start: '09:00', end: '10:00', note: '', calendar_event_id: null }],
  deferred: [],
  summary: 's',
  dropped: 0,
  created_at: 'c',
}

test('focus payloads parse; a bad result turns into failed', () => {
  const ok = parseStreamPayload({ kind: 'focus_ai_result', request_id: 'r1', ai_kind: 'plan_today', ok: true, result: PLAN })
  assert.equal(ok.ok, true)
  assert.equal(ok.result.blocks.length, 1)
  const bad = parseStreamPayload({ kind: 'focus_ai_result', request_id: 'r1', ai_kind: 'suggest', ok: true, result: {} })
  assert.deepEqual([bad.ok, bad.error], [false, 'failed'])
  const err = parseStreamPayload({ kind: 'focus_ai_result', request_id: 'r1', ai_kind: 'analyze', ok: false, error: 'busy' })
  assert.equal(err.error, 'busy')
  assert.equal(parseStreamPayload({ kind: 'focus_ai_result', request_id: 'r1', ai_kind: 'nope', ok: true }), null)

  assert.equal(parseStreamPayload({ kind: 'focus_day_plan', date: '2026-11-03', plan: null }).plan, null)
  assert.equal(parseStreamPayload({ kind: 'focus_day_plan', date: '2026-11-03', plan: { x: 1 } }), null)
  assert.equal(parseStreamPayload({ kind: 'focus_actual', todo_id: 't1', minutes: null }).minutes, null)
  assert.equal(parseStreamPayload({ kind: 'focus_actual', todo_id: 't1', minutes: 95 }).minutes, 95)
  assert.equal(parseStreamPayload({ kind: 'focus_settings', settings: DEFAULT_FOCUS_SETTINGS }).settings.work_start, '09:00')
})

test('command builders', () => {
  assert.deepEqual(focusAiSuggestCmd('r', { sourceUrl: 'https://x', projectId: null, title: ' ' }), {
    kind: 'cmd', action: 'focus_ai_suggest', request_id: 'r', source_url: 'https://x',
  })
  assert.equal(focusPlanToCalendarCmd('2026-11-03').block_ids, undefined)
  assert.deepEqual(focusPlanToCalendarCmd('2026-11-03', ['b1']).block_ids, ['b1'])
  assert.equal(todoAddCmd({ text: 'x', sourceUrl: ' https://a ' }).source_url, 'https://a')
  assert.equal(todoAddCmd({ text: 'x', sourceUrl: '' }).source_url, undefined)
})

test('an AI request resolves with its result and clears its spinner', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const running = data.analyzeTodo('t1')
  assert.equal(data.isAiRunning('todo:t1'), true)
  const cmd = sent[0].cmd
  assert.equal(cmd.action, 'focus_ai_analyze')
  assert.equal(cmd.item_id, 't1')
  sent[0].onAck(true)
  // A second one for the same todo is refused locally.
  assert.deepEqual(await data.analyzeTodo('t1'), { ok: false, code: 'busy' })
  data.apply(
    parseStreamPayload({
      kind: 'focus_ai_result', request_id: cmd.request_id, ai_kind: 'analyze', ok: true,
      result: { analysis: { estimate_min: 60 }, todo: { id: 't1' } },
    }),
  )
  const r = await running
  assert.equal(r.ok, true)
  assert.equal(r.result.estimate_min, 60)
  assert.equal(data.isAiRunning('todo:t1'), false)
  assert.equal(data.state.analyses.t1.estimate_min, 60)
})

test('cancel ends at once and ignores the late answer; failures are kept per key', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const p = data.planToday()
  const id = sent[0].cmd.request_id
  data.cancelAi('plan')
  assert.equal(sent[1].cmd.action, 'focus_ai_cancel')
  assert.equal(sent[1].cmd.request_id, id)
  assert.deepEqual(await p, { ok: false, code: 'cancelled' })
  assert.equal(data.state.aiErrors.plan, undefined)
  data.apply(parseStreamPayload({ kind: 'focus_ai_result', request_id: id, ai_kind: 'plan_today', ok: true, result: PLAN }))
  assert.equal(data.state.dayPlan, null)

  const s = data.suggestTodo('https://x', null, null)
  sent.at(-1).onAck(false, 'rate_limited')
  assert.deepEqual(await s, { ok: false, code: 'rate_limited' })
  assert.equal(data.state.aiErrors['suggest:https://x'], 'rate_limited')
})

test('start with AI yields the run id; a refusal its code', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const ok = data.startTodoAi('t1')
  sent[0].onAck(true, null, 'run-9')
  assert.equal((await ok).runId, 'run-9')
  const no = data.startTodoAi('t2')
  sent[1].onAck(false, 'project_required')
  const out = await no
  assert.deepEqual([out.ok, out.code], [false, 'project_required'])
})

test('estimate applied from AI is saved as estimate_source ai', async () => {
  const { sent, send } = fakeSend()
  const data = createDataStore(send, (r) => `err:${r}`)
  const base = { id: 't1', text: 'T', done: false, due_date: null, project_id: null, created_at: 'c' }
  const focusBase = { priority: 2, status: 'todo', estimate_min: null, due_time: null }
  void data.saveTodo({
    id: 't1', text: 'T', dueDate: '', projectId: '', base,
    focus: { dueTime: '', priority: 2, estimateMin: 150, status: 'todo', base: focusBase, aiEstimateMin: 150 },
  })
  assert.deepEqual(sent[0].cmd.patch, { estimate_min: 150, estimate_source: 'ai' })
})
