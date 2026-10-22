// Unit tests for the partial-message (typewriter) handling in lib/streamEvents.ts.
// Run: node --test src/lib/__tests__/streamEventsPartial.test.mjs
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
const { applyStreamEvent, createStreamState, parseStreamLog, settlePartials } = await import(
  '../streamEvents.ts'
)

const se = (event, extra = {}) => ({ type: 'stream_event', event, parent_tool_use_id: null, ...extra })
const start = (index, block) => se({ type: 'content_block_start', index, content_block: block })
const delta = (index, d) => se({ type: 'content_block_delta', index, delta: d })
const textDelta = (index, text) => delta(index, { type: 'text_delta', text })
const assistant = (content, extra = {}) => ({
  type: 'assistant',
  message: { content },
  parent_tool_use_id: null,
  ...extra,
})

function run(events) {
  const state = createStreamState()
  for (const e of events) applyStreamEvent(state, e)
  return state
}

test('text deltas build a streaming entry that the complete message replaces', () => {
  const state = createStreamState()
  applyStreamEvent(state, start(0, { type: 'text', text: '' }))
  applyStreamEvent(state, textDelta(0, 'Xin '))
  const before = state.entries[0]
  applyStreamEvent(state, textDelta(0, 'chào'))
  assert.deepEqual(state.entries, [{ kind: 'text', text: 'Xin chào', streaming: true }])
  assert.notEqual(state.entries[0], before, 'each delta yields a new entry object')

  applyStreamEvent(state, assistant([{ type: 'text', text: 'Xin chào!' }]))
  assert.deepEqual(state.entries, [{ kind: 'text', text: 'Xin chào!' }])
})

test('thinking + text + tool stream in order and settle in place', () => {
  const state = run([
    start(0, { type: 'thinking', thinking: '' }),
    delta(0, { type: 'thinking_delta', thinking: 'hmm' }),
    start(1, { type: 'text', text: '' }),
    textDelta(1, 'Đọc file'),
    assistant([
      { type: 'thinking', thinking: 'hmm.' },
      { type: 'text', text: 'Đọc file.' },
      { type: 'tool_use', id: 't1', name: 'Read', input: {} },
    ]),
  ])
  assert.deepEqual(
    state.entries.map(e => [e.kind, e.text ?? e.name, !!e.streaming]),
    [
      ['thinking', 'hmm.', false],
      ['text', 'Đọc file.', false],
      ['tool', 'Read', false],
    ],
  )
  assert.equal(state.toolIndex.get('t1'), 2)
})

test('per-block assistant messages map to their own streamed entries', () => {
  const state = run([
    start(0, { type: 'text', text: '' }),
    textDelta(0, 'A'),
    start(1, { type: 'text', text: '' }),
    textDelta(1, 'B'),
    assistant([{ type: 'text', text: 'A1' }]),
    textDelta(1, 'b'),
    assistant([{ type: 'text', text: 'B1' }]),
  ])
  assert.deepEqual(state.entries, [
    { kind: 'text', text: 'A1' },
    { kind: 'text', text: 'B1' },
  ])
})

test('an entry pushed mid-stream does not orphan the partial block', () => {
  const state = run([
    start(0, { type: 'text', text: '' }),
    textDelta(0, 'Hel'),
    { type: 'user', message: { content: 'follow-up' } },
    textDelta(0, 'lo'),
    assistant([{ type: 'text', text: 'Hello' }]),
  ])
  assert.deepEqual(
    state.entries.map(e => [e.kind, e.text]),
    [
      ['text', 'Hello'],
      ['user', 'follow-up'],
    ],
  )
})

test('subagent stream events are ignored; their messages arrive whole', () => {
  const state = run([
    start(0, { type: 'text', text: '' }),
    textDelta(0, 'main'),
    se({ type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }, { parent_tool_use_id: 'task1' }),
    assistant([{ type: 'text', text: 'sub' }], { parent_tool_use_id: 'task1' }),
    assistant([{ type: 'text', text: 'main!' }]),
  ])
  assert.deepEqual(state.entries, [
    { kind: 'text', text: 'main!' },
    { kind: 'text', text: 'sub' },
  ])
})

test('deltas without an open block (view attached mid-block) are dropped', () => {
  const state = run([textDelta(0, 'lost'), assistant([{ type: 'text', text: 'full' }])])
  assert.deepEqual(state.entries, [{ kind: 'text', text: 'full' }])
})

test('an interrupted block is settled by result or settlePartials', () => {
  const viaResult = run([
    start(0, { type: 'text', text: '' }),
    textDelta(0, 'cut'),
    { type: 'result', subtype: 'error_during_execution', is_error: true, result: '' },
  ])
  assert.deepEqual(viaResult.entries[0], { kind: 'text', text: 'cut' })

  const state = run([start(0, { type: 'text', text: '' }), textDelta(0, 'gone')])
  settlePartials(state.entries)
  assert.deepEqual(state.entries, [{ kind: 'text', text: 'gone' }])
})

test('a saved log (no deltas) parses exactly as before', () => {
  const log = [assistant([{ type: 'text', text: 'hi' }])].map(e => JSON.stringify(e)).join('\n')
  assert.deepEqual(parseStreamLog(log)?.entries, [{ kind: 'text', text: 'hi' }])
})
