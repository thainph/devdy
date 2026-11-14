// Unit tests for lib/remoteFocus.ts (Remote Controller Focus settings).
// Run: node --test src/lib/__tests__/remoteFocus.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  focusErrorCode,
  focusSettingsError,
  normaliseFocusSettings,
  parseFocusSettings,
  toggleWorkDay,
} from '../remoteFocus.ts'
import { DEFAULT_FOCUS_SETTINGS } from '../focus.ts'

test('focusErrorCode keeps snake_case codes, else failed', () => {
  assert.equal(focusErrorCode('project_required'), 'project_required')
  assert.equal(focusErrorCode('no_plan: nothing'), 'no_plan')
  assert.equal(focusErrorCode('cancelled'), 'cancelled')
  assert.equal(focusErrorCode('Something broke'), 'failed')
  assert.equal(focusErrorCode(null), 'failed')
  assert.equal(focusErrorCode(''), 'failed')
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

test('phoneSettings drops the desktop-only keys', async () => {
  const { phoneSettings } = await import('../remoteFocus.ts')
  const out = phoneSettings({ work_start: '08:00', ai_engine: 'codex', ai_model: 'x', calendar_account_id: 'a', calendar_ids: ['c'] })
  assert.deepEqual(out, { work_start: '08:00' })
})
