// Unit tests for lib/permissionSurface.ts (in-app vs window permission prompts).
// Run: node --test src/lib/__tests__/permissionSurface.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  desiredWindowRuns,
  floatWhenBackgroundEnabled,
  permissionSurface,
  promote,
  resolveForeground,
  stackEntries,
  stackLayout,
  summarizePermissionInput,
} from '../permissionSurface.ts'

test('setting defaults to on', () => {
  assert.equal(floatWhenBackgroundEnabled(undefined), true)
  assert.equal(floatWhenBackgroundEnabled(''), true)
  assert.equal(floatWhenBackgroundEnabled('true'), true)
  assert.equal(floatWhenBackgroundEnabled('false'), false)
})

test('surface follows the foreground window', () => {
  assert.equal(permissionSurface('main', true), 'inline')
  assert.equal(permissionSurface('main', false), 'inline')
  assert.equal(permissionSurface('other', false), 'window')
  assert.equal(permissionSurface('background', true), 'window')
  assert.equal(permissionSurface('background', false), 'none')
})

test('desired windows per surface', () => {
  const pending = ['a', 'b']
  assert.deepEqual([...desiredWindowRuns('window', pending, [])], ['a', 'b'])
  assert.deepEqual([...desiredWindowRuns('inline', pending, ['a'])], [])
  // none: keep an open window for a still-pending run, open nothing new
  assert.deepEqual([...desiredWindowRuns('none', pending, ['b', 'z'])], ['b'])
})

test('popped-out runs keep a window even while the main window is in front', () => {
  const pending = ['a', 'b']
  assert.deepEqual([...desiredWindowRuns('inline', pending, [], ['b'])], ['b'])
  // a popped-out run whose request is gone gets no window
  assert.deepEqual([...desiredWindowRuns('inline', pending, ['b'], ['z'])], [])
  assert.deepEqual([...desiredWindowRuns('none', pending, ['a'], ['b'])], ['a', 'b'])
  assert.deepEqual([...desiredWindowRuns('window', pending, [], ['z'])], ['a', 'b'])
})

test('a popped-out window the user closed is not reopened, whatever the surface', () => {
  const pending = ['a', 'b']
  // Devdy in the background: every other pending run still gets its window.
  assert.deepEqual([...desiredWindowRuns('window', pending, [], [], ['a'])], ['b'])
  assert.deepEqual([...desiredWindowRuns('none', pending, ['a', 'b'], [], ['a'])], ['b'])
  assert.deepEqual([...desiredWindowRuns('inline', pending, [], ['a'], ['a'])], [])
})

test('focus on a popped-out permission window keeps the main window in front', () => {
  const popped = new Set(['permission-a'])
  assert.deepEqual(resolveForeground('background', true, [], popped), { foreground: 'main', held: false })
  // From the main window into the popped-out prompt: held, the dock stays.
  assert.deepEqual(resolveForeground('main', false, ['permission-a'], popped), { foreground: 'main', held: true })
  // Into any other Devdy window (a session window, a non-popped prompt): other.
  assert.deepEqual(resolveForeground('main', false, ['session-x'], popped), { foreground: 'other', held: false })
  assert.deepEqual(resolveForeground('main', false, ['permission-b'], popped), { foreground: 'other', held: false })
  // A popped-out window focused while Devdy was elsewhere doesn't pull the requests in-app.
  assert.deepEqual(resolveForeground('background', false, ['permission-a'], popped), { foreground: 'other', held: false })
  // Leaving to another app ends the hold.
  assert.deepEqual(resolveForeground('main', false, [], popped), { foreground: 'background', held: false })
})

const e = (runId, requestId = `${runId}-1`) => ({ runId, requestId })

test('stack skips docked runs and popped-out requests', () => {
  const all = [e('a'), e('b'), e('c'), e('d', 'd-2')]
  assert.deepEqual(stackEntries(all, new Set(), new Set()).map((x) => x.runId), ['a', 'b', 'c', 'd'])
  assert.deepEqual(stackEntries(all, new Set(['b']), new Set(['c-1'])).map((x) => x.runId), ['a', 'd'])
  // popping out is per request: a newer request from the same run shows again
  assert.deepEqual(stackEntries(all, new Set(), new Set(['d-1'])).map((x) => x.runId), ['a', 'b', 'c', 'd'])
})

test('stack keeps arrival order and caps visible cards', () => {
  const l = stackLayout([e('a'), e('b'), e('c'), e('d')], new Set(), [])
  assert.deepEqual(l.visible.map((x) => x.runId), ['a', 'b', 'c'])
  assert.deepEqual(l.overflow.map((x) => x.runId), ['d'])
  assert.deepEqual(l.collapsed, [])
})

test('promoted runs lead, collapsed requests are set aside', () => {
  const l = stackLayout([e('a'), e('b'), e('c'), e('d')], new Set(['b-1']), ['d', 'c'])
  assert.deepEqual(l.visible.map((x) => x.runId), ['d', 'c', 'a'])
  assert.deepEqual(l.overflow, [])
  assert.deepEqual(l.collapsed.map((x) => x.runId), ['b'])
})

test('a new request from a collapsed run shows again', () => {
  const l = stackLayout([e('a', 'a-2')], new Set(['a-1']), [])
  assert.deepEqual(l.visible.map((x) => x.requestId), ['a-2'])
})

test('promote dedupes and bounds', () => {
  assert.deepEqual(promote(['a', 'b'], 'b'), ['b', 'a'])
  assert.deepEqual(promote(['a', 'b', 'c'], 'd', 3), ['d', 'a', 'b'])
})

test('input summary picks the meaningful field', () => {
  assert.equal(summarizePermissionInput('Bash', { command: 'ls  -la\n /tmp' }), 'ls -la /tmp')
  assert.equal(summarizePermissionInput('Edit', { file_path: '/a/b.ts', old_string: 'x' }), '/a/b.ts')
  assert.equal(
    summarizePermissionInput('AskUserQuestion', {
      questions: [{ question: 'Which DB?' }, { question: 'Port?' }],
    }),
    'Which DB? (+1)',
  )
  assert.equal(summarizePermissionInput('ExitPlanMode', { plan: '\n# Plan\n- step' }), 'Plan')
  assert.equal(summarizePermissionInput('X', { a: 1 }), '{"a":1}')
  assert.equal(summarizePermissionInput('X', null), '')
  assert.equal(summarizePermissionInput('Bash', { command: 'a'.repeat(50) }, 10), `${'a'.repeat(9)}…`)
})
