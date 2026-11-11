// Unit tests for the shallow-reactivity contract of lib/streamEvents.ts: entries
// change by slot replacement only, and `scanNewEntries` walks only the new tail.
// Run: node --test src/lib/__tests__/streamEventsShallow.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { register } from 'node:module'

// streamEvents.ts imports siblings without an extension (Vite style); resolve
// those to `.ts` so Node's type stripping can load the module graph.
register(
  'data:text/javascript,' +
    encodeURIComponent(`
      export async function resolve(spec, ctx, next) {
        try { return await next(spec, ctx) } catch (e) {
          if (spec.startsWith('.') && !/\\.[cm]?[jt]s$/.test(spec)) return next(spec + '.ts', ctx)
          throw e
        }
      }`),
)
const { applyStreamEvent, createStreamState, scanNewEntries } = await import('../streamEvents.ts')

const toolUse = (id, name = 'Bash') => ({
  type: 'assistant',
  message: { content: [{ type: 'tool_use', id, name, input: {} }] },
  parent_tool_use_id: null,
})
const toolResult = (id, content, is_error = false) => ({
  type: 'user',
  message: { content: [{ type: 'tool_result', tool_use_id: id, content, is_error }] },
})

test('a tool result replaces the tool entry instead of mutating it', () => {
  const state = createStreamState()
  applyStreamEvent(state, toolUse('t1'))
  const before = state.entries[0]
  applyStreamEvent(state, toolResult('t1', 'ok'))
  const after = state.entries[0]
  assert.notEqual(after, before)
  assert.equal(before.result, undefined)
  assert.deepEqual(after.result, { content: 'ok', is_error: false })
  assert.equal(after.id, 't1')
})

test('command stdout replaces the preceding command entry', () => {
  const state = createStreamState()
  applyStreamEvent(state, { type: 'user', message: { content: '<command-name>/compact</command-name>' } })
  const before = state.entries[0]
  applyStreamEvent(state, {
    type: 'user',
    message: { content: '<local-command-stdout>Compacted</local-command-stdout>' },
  })
  assert.equal(state.entries.length, 1)
  assert.notEqual(state.entries[0], before)
  assert.equal(before.stdout, undefined)
  assert.deepEqual(state.entries[0], { kind: 'command', name: '/compact', stdout: 'Compacted' })
})

test('scanNewEntries reports the latest session facts and counts results incrementally', () => {
  const entries = [
    { kind: 'system', model: 'm1', sessionId: 's1' },
    { kind: 'text', text: 'hi' },
    { kind: 'result', text: '', is_error: false },
  ]
  const first = scanNewEntries(entries, 0)
  assert.deepEqual(first, { next: 3, sessionId: 's1', model: 'm1', results: 1 })

  // Nothing new → nothing reported.
  assert.deepEqual(scanNewEntries(entries, first.next), { next: 3, results: 0 })

  entries.push({ kind: 'system', model: 'm2', sessionId: 's2' }, { kind: 'result', text: '', is_error: true })
  assert.deepEqual(scanNewEntries(entries, first.next), { next: 5, sessionId: 's2', model: 'm2', results: 1 })
})

test('scanNewEntries clamps a start past a shrunk tail', () => {
  const entries = [{ kind: 'result', text: '', is_error: false }]
  assert.deepEqual(scanNewEntries(entries, 4), { next: 1, results: 0 })
})
