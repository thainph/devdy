// Unit tests for lib/agentState.ts (agent-reactive ambient background).
// Run: node --test src/lib/__tests__/agentState.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  AGENT_TRANSIENT_MS,
  deriveAgentState,
  detectAgentTransient,
  mergeAgentTransient,
  resolveAgentState,
  steadyAgentState,
} from '../agentState.ts'

const run = (id, status, waiting = false) => ({ id, status, waiting })

test('steady: idle with no runs or only finished ones', () => {
  assert.equal(steadyAgentState([]), 'idle')
  assert.equal(steadyAgentState([run('a', 'done'), run('b', 'cancelled'), run('c', 'failed')]), 'idle')
})

test('steady: any running run → running', () => {
  assert.equal(steadyAgentState([run('a', 'done'), run('b', 'running')]), 'running')
})

test('steady: a pending prompt beats running, in any position', () => {
  assert.equal(steadyAgentState([run('a', 'running'), run('b', 'running', true)]), 'waiting')
  assert.equal(steadyAgentState([run('a', 'running', true), run('b', 'running')]), 'waiting')
})

test('transient: running → done flashes done', () => {
  assert.equal(detectAgentTransient([run('a', 'running')], [run('a', 'done')]), 'done')
})

test('transient: running → failed flashes error', () => {
  assert.equal(detectAgentTransient([run('a', 'running')], [run('a', 'failed')]), 'error')
})

test('transient: cancelled flashes nothing', () => {
  assert.equal(detectAgentTransient([run('a', 'running')], [run('a', 'cancelled')]), null)
})

test('transient: error wins over a simultaneous done', () => {
  const prev = [run('a', 'running'), run('b', 'running')]
  assert.equal(detectAgentTransient(prev, [run('a', 'done'), run('b', 'failed')]), 'error')
  assert.equal(detectAgentTransient(prev, [run('a', 'failed'), run('b', 'done')]), 'error')
})

test('transient: runs that appear already finished do not flash', () => {
  assert.equal(detectAgentTransient([], [run('a', 'done')]), null)
  assert.equal(detectAgentTransient([run('x', 'running')], [run('x', 'running'), run('a', 'failed')]), null)
})

test('transient: no change, a removed run, or a status that stays terminal flashes nothing', () => {
  assert.equal(detectAgentTransient([run('a', 'running')], [run('a', 'running')]), null)
  assert.equal(detectAgentTransient([run('a', 'running')], []), null)
  assert.equal(detectAgentTransient([run('a', 'done')], [run('a', 'done')]), null)
  // Resumed: done → running is a steady-state change, not a flash.
  assert.equal(detectAgentTransient([run('a', 'done')], [run('a', 'running')]), null)
})

test('transient: answering a prompt then finishing still flashes', () => {
  assert.equal(detectAgentTransient([run('a', 'running', true)], [run('a', 'done')]), 'done')
})

test('merge: done does not cut an error flash short; error replaces done', () => {
  assert.equal(mergeAgentTransient('error', 'done'), 'error')
  assert.equal(mergeAgentTransient('done', 'error'), 'error')
  assert.equal(mergeAgentTransient(null, 'done'), 'done')
  assert.equal(mergeAgentTransient('done', 'done'), 'done')
})

test('resolve: waiting outranks a flash; a flash outranks running/idle', () => {
  assert.equal(resolveAgentState('waiting', 'error'), 'waiting')
  assert.equal(resolveAgentState('running', 'done'), 'done')
  assert.equal(resolveAgentState('idle', 'error'), 'error')
  assert.equal(resolveAgentState('running', null), 'running')
  assert.equal(resolveAgentState('idle', null), 'idle')
})

test('derive: combines steady + transient', () => {
  assert.deepEqual(
    deriveAgentState([run('a', 'running'), run('b', 'running')], [run('a', 'done'), run('b', 'running')]),
    { steady: 'running', transient: 'done' },
  )
})

test('transient durations: error lingers longer than done', () => {
  assert.ok(AGENT_TRANSIENT_MS.error > AGENT_TRANSIENT_MS.done)
})
