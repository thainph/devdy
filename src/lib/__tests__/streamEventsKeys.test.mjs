// Unit tests for lib/streamEvents.ts stable entry keys (StreamLog's v-for key)
// and image-block placeholders in tool results.
// Run: node --test src/lib/__tests__/streamEventsKeys.test.mjs
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
const { applyStreamEvent, createStreamState, entryKey, settlePartials } = await import('../streamEvents.ts')

const delta = (text) => ({ type: 'stream_event', event: { type: 'content_block_delta', delta: { type: 'text_delta', text } } })

test('keys are stable, unique, and invisible to JSON', () => {
  const s = createStreamState()
  applyStreamEvent(s, { type: 'user', message: { content: 'hi' } })
  applyStreamEvent(s, { type: 'assistant', message: { content: [{ type: 'text', text: 'yo' }] } })
  const [a, b] = s.entries.map(entryKey)
  assert.notEqual(a, b)
  assert.equal(entryKey(s.entries[0]), a)
  assert.equal(JSON.stringify(s.entries[0]), '{"kind":"user","text":"hi"}')
})

test('a streamed entry keeps its key through deltas, the complete block and settling', () => {
  const s = createStreamState()
  applyStreamEvent(s, { type: 'stream_event', event: { type: 'content_block_start', content_block: { type: 'text', text: '' } } })
  const k = entryKey(s.entries[0])
  applyStreamEvent(s, delta('Hel'))
  applyStreamEvent(s, delta('lo'))
  assert.equal(entryKey(s.entries[0]), k)
  applyStreamEvent(s, { type: 'assistant', message: { content: [{ type: 'text', text: 'Hello' }] } })
  assert.equal(s.entries[0].streaming, undefined)
  assert.equal(entryKey(s.entries[0]), k)

  const s2 = createStreamState()
  applyStreamEvent(s2, { type: 'stream_event', event: { type: 'content_block_start', content_block: { type: 'text', text: 'x' } } })
  const k2 = entryKey(s2.entries[0])
  settlePartials(s2.entries)
  assert.equal(entryKey(s2.entries[0]), k2)
})

test('a tool entry keeps its key when its result attaches', () => {
  const s = createStreamState()
  applyStreamEvent(s, { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't1', name: 'Bash', input: { command: 'ls' } }] } })
  const k = entryKey(s.entries[0])
  applyStreamEvent(s, { type: 'user', message: { content: [{ type: 'tool_result', tool_use_id: 't1', content: 'ok' }] } })
  assert.equal(s.entries[0].result.content, 'ok')
  assert.equal(entryKey(s.entries[0]), k)
})

test('image blocks in a tool result become a placeholder, not base64', () => {
  const s = createStreamState()
  applyStreamEvent(s, { type: 'assistant', message: { content: [{ type: 'tool_use', id: 't1', name: 'Read', input: {} }] } })
  const data = 'A'.repeat(100_000)
  applyStreamEvent(s, {
    type: 'user',
    message: {
      content: [{
        type: 'tool_result',
        tool_use_id: 't1',
        content: [
          { type: 'text', text: 'see:' },
          { type: 'image', source: { type: 'base64', media_type: 'image/png', data } },
          { type: 'image', source: { type: 'url', url: 'https://x' } },
        ],
      }],
    },
  })
  assert.equal(s.entries[0].result.content, 'see:\n[image image/png]\n[image]')
})
