// Unit tests for lib/remoteFocus.ts (Remote Controller full Focus screens).
// Run: node --test src/lib/__tests__/remoteFocus.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  PLAN_KEY,
  focusErrorCode,
  focusSettingsError,
  newRequestId,
  normaliseFocusSettings,
  parseAiAnalyzeResult,
  parseAiSuggestion,
  parseDayPlan,
  parseFocusSettings,
  pendingBlocks,
  shouldAskActual,
  suggestAiKey,
  todoAiKey,
  toggleWorkDay,
} from '../remoteFocus.ts'
import { DEFAULT_FOCUS_SETTINGS } from '../focus.ts'

test('request keys and ids', () => {
  assert.equal(PLAN_KEY, 'plan')
  assert.equal(todoAiKey('t1'), 'todo:t1')
  assert.equal(suggestAiKey('  https://x/1 '), 'suggest:https://x/1')
  const a = newRequestId()
  const b = newRequestId()
  assert.notEqual(a, b)
  assert.ok(a.length > 0 && a.length <= 128)
})

test('focusErrorCode keeps snake_case codes, else failed', () => {
  assert.equal(focusErrorCode('project_required'), 'project_required')
  assert.equal(focusErrorCode('no_plan: nothing'), 'no_plan')
  assert.equal(focusErrorCode('cancelled'), 'cancelled')
  assert.equal(focusErrorCode('Something broke'), 'failed')
  assert.equal(focusErrorCode(null), 'failed')
  assert.equal(focusErrorCode(''), 'failed')
})

test('shouldAskActual needs a started, estimated, open todo', () => {
  assert.equal(shouldAskActual({ done: false, started_at: '2026-11-03T08:00:00Z', estimate_min: 60 }), true)
  assert.equal(shouldAskActual({ done: false, started_at: null, estimate_min: 60 }), false)
  assert.equal(shouldAskActual({ done: false, started_at: '2026-11-03T08:00:00Z', estimate_min: null }), false)
  assert.equal(shouldAskActual({ done: true, started_at: '2026-11-03T08:00:00Z', estimate_min: 60 }), false)
})

test('parseDayPlan drops malformed rows and defaults fields', () => {
  const plan = parseDayPlan({
    date: '2026-11-03',
    blocks: [
      { id: 'b1', todo_id: 't1', start: '09:00', end: '10:00', note: 'x', calendar_event_id: null },
      { id: 'b2', todo_id: 't2', start: '10:00', end: '11:00', calendar_event_id: 'ev' },
      { id: 'bad' },
      null,
    ],
    deferred: [{ todo_id: 't3', reason: 'later' }, { reason: 'no id' }],
    summary: 'ok',
    dropped: 1,
    created_at: '2026-11-03T07:00:00Z',
  })
  assert.equal(plan.blocks.length, 2)
  assert.equal(plan.blocks[1].note, '')
  assert.deepEqual(plan.deferred, [{ todo_id: 't3', reason: 'later' }])
  assert.deepEqual(pendingBlocks(plan), ['b1'])
  assert.equal(parseDayPlan({ date: 1 }), null)
  assert.equal(parseDayPlan(null), null)
  assert.deepEqual(pendingBlocks(null), [])
})

test('parseFocusSettings defaults and filters', () => {
  const s = parseFocusSettings({
    work_start: '08:30',
    work_end: '17:00',
    work_days: [1, 2, 9, 'x'],
    notify_kinds: { morning: false, junk: 'yes' },
  })
  assert.equal(s.work_start, '08:30')
  assert.deepEqual(s.work_days, [1, 2])
  assert.deepEqual(s.notify_kinds, { morning: false })
  assert.equal(s.notify_enabled, true)
  assert.equal(s.morning_time, '09:00')
  assert.equal(s.calendar_account_id, null)
  assert.equal(parseFocusSettings({ work_start: '08:00' }), null)
})

test('normalise / validate settings', () => {
  const n = normaliseFocusSettings({ ...DEFAULT_FOCUS_SETTINGS, work_days: [5, 1, 1] })
  assert.deepEqual(n.work_days, [1, 5])
  assert.equal(n.notify_kinds.morning, true)
  assert.equal(focusSettingsError(DEFAULT_FOCUS_SETTINGS), null)
  assert.equal(focusSettingsError({ ...DEFAULT_FOCUS_SETTINGS, work_end: '08:00' }), 'hours')
  assert.equal(focusSettingsError({ ...DEFAULT_FOCUS_SETTINGS, work_start: '9:00' }), 'hours')
  assert.equal(focusSettingsError({ ...DEFAULT_FOCUS_SETTINGS, work_days: [] }), 'days')
  assert.equal(focusSettingsError({ ...DEFAULT_FOCUS_SETTINGS, morning_time: '' }), 'morning')
  assert.deepEqual(toggleWorkDay([1, 3], 2), [1, 2, 3])
  assert.deepEqual(toggleWorkDay([1, 2, 3], 2), [1, 3])
})

test('parseAiSuggestion / parseAiAnalyzeResult', () => {
  const s = parseAiSuggestion({ title: 'Fix', estimate_min: 90, priority: 1, due_date: '2026-11-04' })
  assert.equal(s.priority, 1)
  assert.equal(s.due_time, null)
  assert.equal(s.reason, '')
  assert.equal(parseAiSuggestion({ title: 'x' }), null)

  const r = parseAiAnalyzeResult({
    analysis: { estimate_min: 120, estimate_low_min: 90, estimate_high_min: 180, suggested_priority: 7, steps: ['a', 1] },
    todo: { id: 't1' },
  })
  assert.equal(r.analysis.estimate_min, 120)
  assert.equal(r.analysis.suggested_priority, null)
  assert.deepEqual(r.analysis.steps, ['a'])
  assert.equal(r.analysis.confidence, 'medium')
  assert.equal(r.todo.id, 't1')
  assert.equal(parseAiAnalyzeResult({ analysis: {} }), null)
})

test('phoneSettings drops the desktop-only keys', async () => {
  const { phoneSettings } = await import('../remoteFocus.ts')
  const out = phoneSettings({ work_start: '08:00', ai_engine: 'codex', ai_model: 'x', calendar_account_id: 'a', calendar_ids: ['c'] })
  assert.deepEqual(out, { work_start: '08:00' })
})

test('parseActual takes 1 min to 10 days', async () => {
  const { parseActual } = await import('../remoteFocus.ts')
  assert.equal(parseActual(''), null)
  assert.equal(parseActual('3'), 3)
  assert.equal(parseActual('50h'), 3000)
  assert.equal(parseActual('1h30'), 90)
  assert.equal(parseActual('0'), undefined)
  assert.equal(parseActual('241h'), undefined)
  assert.equal(parseActual('soon'), undefined)
})
