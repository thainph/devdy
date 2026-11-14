// Unit tests for lib/focus.ts (the "+ Todo" form helpers: estimates,
// "✨ Suggest", sources).
// Run: node --test src/lib/__tests__/focus.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  aiErrorCode,
  applySuggestion,
  draftToInput,
  emptyTodoDraft,
  formatEstimate,
  githubRef,
  guessSourceType,
  parseEstimate,
  sourceShortLabel,
} from '../focus.ts'

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
