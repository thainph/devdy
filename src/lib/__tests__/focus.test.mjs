// Unit tests for lib/focus.ts (Focus board helpers: estimates, due labels,
// buckets, filters, snooze presets, sources).
// Run: node --test src/lib/__tests__/focus.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  BUCKET_ORDER,
  aiErrorCode,
  applySuggestion,
  blockMinutes,
  FILTER_NONE,
  calibrationHint,
  draftToInput,
  dueLabel,
  emptyTodoDraft,
  filterItems,
  formatEstimate,
  fromLocalInput,
  githubRef,
  groupByBucket,
  groupByProject,
  groupByWeek,
  guessSourceType,
  parseEstimate,
  parseReason,
  snoozePresets,
  sourceShortLabel,
  spanOf,
  toLocalInput,
  topItems,
  weekDays,
} from '../focus.ts'

const local = (y, m, d, h = 0, min = 0) => new Date(y, m - 1, d, h, min)
// Friday 2026-10-30 10:00 local.
const NOW = local(2026, 10, 30, 10).getTime()

function todo(over = {}) {
  return {
    id: 't1', text: 'x', done: false, position: 0, created_at: '2026-10-01T00:00:00Z',
    project_id: null, due_date: null, due_time: null, priority: 2, estimate_min: null,
    estimate_source: null, status: 'todo', snoozed_until: null, remind_at: null,
    source_type: null, source_ref: null, source_url: null, started_at: null,
    completed_at: null, actual_min: null, ai_plan: null, ai_analyzed_at: null,
    updated_at: null, linked_run_count: 0, ...over,
  }
}
function item(over = {}, t = {}) {
  return {
    todo: todo(t), bucket: 'no_due', score: 0, reasons: [], start_by: null,
    effective_due: null, effective_estimate_min: 60, estimate_missing: true, ...over,
  }
}

test('parseEstimate reads minutes, hours and mixed forms', () => {
  assert.equal(parseEstimate('30m'), 30)
  assert.equal(parseEstimate('2h'), 120)
  assert.equal(parseEstimate('1h30'), 90)
  assert.equal(parseEstimate('1h30m'), 90)
  assert.equal(parseEstimate('1h05'), 65)
  assert.equal(parseEstimate('90'), 90)
  assert.equal(parseEstimate('1.5h'), 90)
  assert.equal(parseEstimate(' 2 H '), 120)
  assert.equal(parseEstimate('45p'), 45)
})

test('parseEstimate: empty is null, junk / out of range is undefined', () => {
  assert.equal(parseEstimate(''), null)
  assert.equal(parseEstimate('   '), null)
  assert.equal(parseEstimate(null), null)
  assert.equal(parseEstimate('abc'), undefined)
  assert.equal(parseEstimate('2d'), undefined)
  assert.equal(parseEstimate('3'), undefined) // < 5 min
  assert.equal(parseEstimate('41h'), undefined) // > 2400 min
})

test('formatEstimate round-trips with parseEstimate', () => {
  assert.equal(formatEstimate(30), '30m')
  assert.equal(formatEstimate(120), '2h')
  assert.equal(formatEstimate(90), '1h30')
  assert.equal(formatEstimate(65), '1h05')
  assert.equal(formatEstimate(null), '')
  for (const m of [5, 30, 60, 90, 125, 2400]) assert.equal(parseEstimate(formatEstimate(m)), m)
})

test('spanOf picks the largest sensible unit', () => {
  assert.deepEqual(spanOf(45), { unit: 'm', value: 45 })
  assert.deepEqual(spanOf(150), { unit: 'h', value: 3 })
  assert.deepEqual(spanOf(-3000), { unit: 'd', value: 2 })
})

test('dueLabel: overdue wins', () => {
  const it = item(
    { bucket: 'overdue', effective_due: local(2026, 10, 29, 18).toISOString() },
    { due_date: '2026-10-29' },
  )
  assert.deepEqual(dueLabel(it, NOW), { kind: 'overdue', minutes: 16 * 60 })
})

test('dueLabel: start_by today for a todo not started', () => {
  const it = item(
    {
      bucket: 'today',
      effective_due: local(2026, 10, 30, 17).toISOString(),
      start_by: local(2026, 10, 30, 14).toISOString(),
    },
    { due_date: '2026-10-30', due_time: '17:00' },
  )
  assert.deepEqual(dueLabel(it, NOW), { kind: 'start_by', time: '14:00', late: false })
  // Already started: the deadline itself.
  it.todo.status = 'doing'
  assert.deepEqual(dueLabel(it, NOW), { kind: 'due', day: 'today', date: '2026-10-30', time: '17:00' })
})

test('dueLabel: tomorrow / later date, none without a deadline or when done', () => {
  const tomorrow = item(
    { effective_due: local(2026, 10, 31, 18).toISOString(), start_by: local(2026, 10, 31, 16).toISOString() },
    { due_date: '2026-10-31' },
  )
  assert.deepEqual(dueLabel(tomorrow, NOW), { kind: 'due', day: 'tomorrow', date: '2026-10-31', time: null })
  const later = item({ effective_due: local(2026, 11, 5, 18).toISOString() }, { due_date: '2026-11-05' })
  assert.equal(dueLabel(later, NOW).day, 'date')
  assert.equal(dueLabel(item(), NOW), null)
  assert.equal(dueLabel(item({}, { due_date: '2026-10-29', done: true }), NOW), null)
})

test('groupByBucket keeps BUCKET_ORDER and sorts by score', () => {
  const groups = groupByBucket([
    item({ bucket: 'today', score: 50 }, { id: 'a' }),
    item({ bucket: 'doing', score: 999 }, { id: 'b' }),
    item({ bucket: 'today', score: -10 }, { id: 'c' }),
    item({ bucket: 'done', score: 0 }, { id: 'd' }),
  ])
  assert.deepEqual(groups.map((g) => g.bucket), ['doing', 'today', 'done'])
  assert.deepEqual(groups[1].items.map((i) => i.todo.id), ['c', 'a'])
  assert.equal(BUCKET_ORDER[0], 'doing')
  assert.equal(BUCKET_ORDER.at(-1), 'done')
})

test('score ties fall back to earlier due, then position', () => {
  const groups = groupByBucket([
    item({ bucket: 'today', score: 1, effective_due: '2026-10-30T10:00:00Z' }, { id: 'late', position: 0 }),
    item({ bucket: 'today', score: 1, effective_due: '2026-10-30T08:00:00Z' }, { id: 'early', position: 5 }),
    item({ bucket: 'today', score: 1 }, { id: 'nodue', position: 1 }),
  ])
  assert.deepEqual(groups[0].items.map((i) => i.todo.id), ['early', 'late', 'nodue'])
})

test('topItems resolves ids in order, skipping filtered-out ones', () => {
  const items = [item({}, { id: 'a' }), item({}, { id: 'b' })]
  assert.deepEqual(topItems(items, ['b', 'x', 'a']).map((i) => i.todo.id), ['b', 'a'])
})

test('groupByProject: active items only, no-project column last, totals', () => {
  const cols = groupByProject([
    item({ bucket: 'overdue', score: -100, effective_estimate_min: 30 }, { id: 'a', project_id: 'p1' }),
    item({ bucket: 'today', score: 10, effective_estimate_min: 60 }, { id: 'b', project_id: 'p1' }),
    item({ bucket: 'today', score: -500 }, { id: 'c', project_id: null }),
    item({ bucket: 'later', score: 5 }, { id: 'd', project_id: 'p2' }),
    item({ bucket: 'done', score: -999 }, { id: 'e', project_id: 'p3' }),
  ])
  assert.deepEqual(cols.map((c) => c.projectId), ['p1', 'p2', null])
  assert.equal(cols[0].estimateMin, 90)
  assert.equal(cols[0].overdue, 1)
})

test('filterItems by project / source, including "none"', () => {
  const items = [
    item({}, { id: 'a', project_id: 'p1', source_type: 'slack' }),
    item({}, { id: 'b', project_id: null, source_type: null }),
    item({}, { id: 'c', project_id: 'p2', source_type: 'github' }),
  ]
  const ids = (f) => filterItems(items, f).map((i) => i.todo.id)
  assert.deepEqual(ids({ projectId: '', sourceType: '' }), ['a', 'b', 'c'])
  assert.deepEqual(ids({ projectId: 'p1', sourceType: '' }), ['a'])
  assert.deepEqual(ids({ projectId: FILTER_NONE, sourceType: '' }), ['b'])
  assert.deepEqual(ids({ projectId: '', sourceType: 'github' }), ['c'])
  assert.deepEqual(ids({ projectId: '', sourceType: FILTER_NONE }), ['b'])
})

test('weekDays: Monday → Sunday of the current week', () => {
  assert.deepEqual(weekDays(NOW), [
    '2026-10-26', '2026-10-27', '2026-10-28', '2026-10-29', '2026-10-30', '2026-10-31', '2026-11-01',
  ])
  // A Sunday belongs to the week that started the Monday before.
  assert.equal(weekDays(local(2026, 11, 1, 12).getTime())[0], '2026-10-26')
})

test('groupByWeek places open todos on their due date with totals', () => {
  const cols = groupByWeek([
    item({ bucket: 'today', effective_estimate_min: 30 }, { id: 'a', due_date: '2026-10-30' }),
    item({ bucket: 'today', effective_estimate_min: 90 }, { id: 'b', due_date: '2026-10-30' }),
    item({ bucket: 'done' }, { id: 'c', due_date: '2026-10-30' }),
    item({ bucket: 'later' }, { id: 'd', due_date: '2026-11-20' }),
  ], NOW)
  assert.equal(cols.length, 7)
  assert.deepEqual(cols[4].items.map((i) => i.todo.id), ['a', 'b'])
  assert.equal(cols[4].estimateMin, 120)
  assert.equal(cols.reduce((n, c) => n + c.items.length, 0), 2)
})

test('snoozePresets: +1h, 14:00 only before it, tomorrow and next Monday at work start', () => {
  const p = Object.fromEntries(snoozePresets(NOW, '09:30').map((x) => [x.preset, Date.parse(x.at)]))
  assert.equal(p.hour, NOW + 3_600_000)
  assert.equal(p.afternoon, local(2026, 10, 30, 14).getTime())
  assert.equal(p.tomorrow, local(2026, 10, 31, 9, 30).getTime())
  assert.equal(p.nextWeek, local(2026, 11, 2, 9, 30).getTime())
  const late = snoozePresets(local(2026, 10, 30, 15).getTime()).map((x) => x.preset)
  assert.deepEqual(late, ['hour', 'tomorrow', 'nextWeek'])
  // On a Monday "next week" is the following Monday, not today.
  const monday = snoozePresets(local(2026, 10, 26, 8).getTime()).find((x) => x.preset === 'nextWeek')
  assert.equal(Date.parse(monday.at), local(2026, 11, 2, 9).getTime())
})

test('local datetime input round-trip', () => {
  const iso = local(2026, 10, 30, 14, 5).toISOString()
  assert.equal(toLocalInput(iso), '2026-10-30T14:05')
  assert.equal(fromLocalInput('2026-10-30T14:05'), iso)
  assert.equal(fromLocalInput(''), null)
  assert.equal(toLocalInput(null), '')
})

test('calibrationHint uses the measured factor, only when enabled with ≥5 samples', () => {
  const c = (over) => ({ factor: 1.4, measured: 1.4, samples: 5, enabled: true, ...over })
  assert.equal(calibrationHint(c()), 1.4)
  assert.equal(calibrationHint(c({ samples: 4 })), null)
  assert.equal(calibrationHint(c({ measured: 1.02 })), null)
  assert.equal(calibrationHint(c({ measured: 0.66, factor: 0.66 })), 0.7)
  assert.equal(calibrationHint(c({ enabled: false, factor: 1 })), null)
  assert.equal(calibrationHint(null), null)
})

test('parseReason decodes every backend reason kind', () => {
  for (const kind of ['doing', 'waiting', 'no_due', 'done']) {
    assert.deepEqual(parseReason({ kind, value: null }), { kind })
  }
  assert.deepEqual(parseReason({ kind: 'due', value: '2026-10-30T15:00' }), { kind: 'due', date: '2026-10-30', time: '15:00' })
  assert.deepEqual(parseReason({ kind: 'snoozed', value: '2026-11-02T09:00' }), { kind: 'snoozed', date: '2026-11-02', time: '09:00' })
  assert.deepEqual(parseReason({ kind: 'snoozed', value: null }), { kind: 'snoozed', date: null, time: null })
  for (const kind of ['overdue', 'work_left', 'late_by', 'estimate', 'estimate_missing']) {
    assert.deepEqual(parseReason({ kind, value: 90 }), { kind, minutes: 90 })
  }
  assert.deepEqual(parseReason({ kind: 'priority', value: 1 }), { kind: 'priority', n: 1 })
  assert.deepEqual(parseReason({ kind: 'age_days', value: 4 }), { kind: 'age_days', n: 4 })
})

test('parseReason falls back to unknown for new kinds or bad values', () => {
  assert.deepEqual(parseReason({ kind: 'calendar', value: 'busy' }), { kind: 'unknown', name: 'calendar', value: 'busy' })
  assert.deepEqual(parseReason({ kind: 'mystery', value: null }), { kind: 'unknown', name: 'mystery', value: null })
  assert.deepEqual(parseReason({ kind: 'work_left', value: null }), { kind: 'unknown', name: 'work_left', value: null })
  assert.deepEqual(parseReason({ kind: 'due', value: 'soon' }), { kind: 'unknown', name: 'due', value: 'soon' })
})

test('applySuggestion fills the form without overwriting the user', () => {
  const s = { title: 'Gửi báo cáo', due_date: '2026-10-30', due_time: '17:00', due_evidence: 'x', priority: 1, estimate_min: 90, reason: '' }
  assert.deepEqual(applySuggestion(emptyTodoDraft({ sourceUrl: 'u' }), s), emptyTodoDraft({
    sourceUrl: 'u', text: 'Gửi báo cáo', dueDate: '2026-10-30', dueTime: '17:00', priority: 1, estimate: '1h30',
  }))
  const mine = applySuggestion(emptyTodoDraft({ text: 'Mine', dueDate: '2026-11-02' }), s)
  assert.equal(mine.text, 'Mine')
  assert.equal(mine.dueDate, '2026-11-02')
  assert.equal(mine.dueTime, '')
  assert.equal(applySuggestion(emptyTodoDraft(), { ...s, due_date: null, due_time: null }).dueDate, '')
})

test('aiErrorCode splits coded command errors', () => {
  assert.deepEqual(aiErrorCode('project_required: pick a project'), { code: 'project_required', detail: 'pick a project' })
  assert.deepEqual(aiErrorCode('cancelled'), { code: 'cancelled', detail: '' })
  assert.deepEqual(aiErrorCode(new Error('Boom')), { code: null, detail: 'Boom' })
})

test('blockMinutes', () => {
  assert.equal(blockMinutes({ start: '10:05', end: '11:30' }), 85)
  assert.equal(blockMinutes({ start: '11:00', end: '10:00' }), 0)
})

test('draftToInput maps the capture form to add_todo args', () => {
  assert.equal(draftToInput(emptyTodoDraft()), null)
  assert.equal(draftToInput(emptyTodoDraft({ text: 'x', estimate: 'soon' })), null)
  assert.deepEqual(
    draftToInput(emptyTodoDraft({ text: '  Fix  ', dueTime: '15:00', estimate: '1h30', sourceUrl: ' https://x.y ' }), 'r1'),
    {
      text: 'Fix', projectId: null, runId: 'r1', dueDate: null, dueTime: null, priority: 2,
      estimateMin: 90, sourceUrl: 'https://x.y',
    },
  )
  const withDue = draftToInput(emptyTodoDraft({ text: 'a', projectId: 'p', dueDate: '2026-11-02', dueTime: '15:00', priority: 1 }))
  assert.equal(withDue.dueTime, '15:00')
  assert.equal(withDue.projectId, 'p')
  assert.equal(withDue.estimateMin, null)
})

test('source helpers', () => {
  assert.equal(guessSourceType('https://acme.slack.com/archives/C1/p123'), 'slack')
  assert.equal(guessSourceType('https://github.com/a/b/pull/3'), 'github')
  assert.equal(guessSourceType('https://gitlab.example.com/g/p/-/issues/3'), 'gitlab')
  assert.equal(guessSourceType('https://example.com'), 'url')
  assert.equal(guessSourceType('not a url'), 'url')
  assert.equal(guessSourceType(''), null)
  assert.equal(githubRef('https://github.com/Acme/Repo/issues/012'), 'acme/repo#12')
  assert.equal(githubRef('https://github.com/acme/repo/pull/7/files'), 'acme/repo#7')
  assert.equal(githubRef('https://github.com/acme/repo'), null)
  assert.equal(sourceShortLabel('github', 'github:acme/repo#12'), '#12')
  assert.equal(sourceShortLabel('github', null, 'https://github.com/a/b/issues/4'), '#4')
  assert.equal(sourceShortLabel('gitlab', 'gitlab:g/p!8'), '!8')
  assert.equal(sourceShortLabel('slack', 'slack:T/C/1.2'), null)
})

test('rebaseForm takes changes for untouched fields only', async () => {
  const { rebaseForm } = await import('../focus.ts')
  const base = { text: 'Fix', status: 'todo', priority: 2, estimate: '1h' }
  const form = { ...base, estimate: '2h' } // the user edited the estimate
  const next = { ...base, status: 'doing', estimate: '30m' } // Start with AI + another window
  assert.deepEqual(rebaseForm(form, base, next), { text: 'Fix', status: 'doing', priority: 2, estimate: '2h' })
  // Nothing edited: the form becomes the new todo (not dirty).
  assert.deepEqual(rebaseForm(base, base, next), next)
})
