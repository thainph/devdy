// Unit tests for lib/ipc/ipcCore.ts (remote-desktop IPC bookkeeping).
// Run: node --test src/lib/__tests__/ipcCore.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { IpcCore, ERR_CLOSED, ERR_CONNECTION_LOST, ERR_TIMEOUT } from '../ipc/ipcCore.ts'

/** A core over a fake link with manual timers. */
function setup({ canSend = () => true } = {}) {
  const sent = []
  const timers = new Map()
  let nextTimer = 0
  let nextId = 0
  const core = new IpcCore({
    send: (frame) => {
      if (!canSend(frame)) return false
      sent.push(frame)
      return true
    },
    newId: () => `id${++nextId}`,
    timeoutMs: 1000,
    setTimer: (fn) => {
      const h = ++nextTimer
      timers.set(h, fn)
      return h
    },
    clearTimer: (h) => timers.delete(h),
    onHandlerError: () => {},
  })
  const fireTimers = () => {
    for (const [h, fn] of [...timers]) {
      timers.delete(h)
      fn()
    }
  }
  return { core, sent, timers, fireTimers }
}

const settled = (p) =>
  p.then(
    (v) => ({ ok: true, v }),
    (e) => ({ ok: false, e }),
  )

test('invoke queues while down and flushes in order on linkUp', async () => {
  const { core, sent } = setup()
  const a = core.invoke('a', { x: 1 })
  const b = core.invoke('b')
  assert.equal(sent.length, 0)
  core.linkUp()
  assert.deepEqual(sent, [
    { kind: 'cmd', action: 'ipc_invoke', id: 'id1', cmd: 'a', args: { x: 1 } },
    { kind: 'cmd', action: 'ipc_invoke', id: 'id2', cmd: 'b', args: {} },
  ])
  core.handleResult('id2', true, 'B', undefined)
  core.handleResult('id1', false, undefined, 'boom')
  assert.equal(await b, 'B')
  assert.deepEqual(await settled(a), { ok: false, e: 'boom' })
  assert.equal(core.pendingCount, 0)
})

test('invoke sends immediately while up; late and unknown results are ignored', async () => {
  const { core, sent } = setup()
  core.linkUp()
  const p = core.invoke('x')
  assert.equal(sent.length, 1)
  core.handleResult('nope', true, 1, undefined)
  core.handleResult('id1', true, { v: 2 }, undefined)
  core.handleResult('id1', true, 3, undefined)
  assert.deepEqual(await p, { v: 2 })
})

test('rejects with the error value as-is', async () => {
  const { core } = setup()
  core.linkUp()
  const p = core.invoke('x')
  const err = { code: 'not_found', message: 'gone' }
  core.handleResult('id1', false, undefined, err)
  const r = await settled(p)
  assert.equal(r.e, err)
})

test('a call queued too long rejects with remote_timeout and is never sent', async () => {
  const { core, sent, fireTimers } = setup()
  const p = core.invoke('slow')
  fireTimers()
  const r = await settled(p)
  assert.equal(r.e.message, ERR_TIMEOUT)
  assert.equal(core.pendingCount, 0)
  core.linkUp()
  assert.equal(sent.length, 0)
})

test('a sent call has no timeout: it waits for its result however long', async () => {
  const { core, timers } = setup()
  core.linkUp()
  const p = core.invoke('git_push')
  assert.equal(timers.size, 0)
  core.handleResult('id1', true, 'pushed', undefined)
  assert.equal(await p, 'pushed')
})

test('flushing a queued call disarms its queue timeout', async () => {
  const { core, timers, fireTimers } = setup()
  const p = core.invoke('slow')
  assert.equal(timers.size, 1)
  core.linkUp()
  assert.equal(timers.size, 0)
  fireTimers()
  assert.equal(core.pendingCount, 1)
  core.handleResult('id1', true, null, undefined)
  assert.equal(await p, null)
})

test('a call whose send failed keeps its queue timeout', async () => {
  const { core, sent, fireTimers } = setup({ canSend: () => false })
  core.linkUp()
  const p = core.invoke('x')
  assert.equal(sent.length, 0)
  fireTimers()
  assert.equal((await settled(p)).e.message, ERR_TIMEOUT)
})

test('linkDown rejects in-flight calls but keeps queued ones', async () => {
  let up = true
  const { core, sent } = setup({ canSend: () => up })
  core.linkUp()
  const inflight = core.invoke('a')
  up = false
  const unsent = core.invoke('b') // send fails → stays queued
  core.linkDown()
  const queued = core.invoke('c')
  assert.equal((await settled(inflight)).e.message, ERR_CONNECTION_LOST)
  assert.equal(core.pendingCount, 2)
  up = true
  core.linkUp()
  assert.deepEqual(
    sent.map((f) => f.cmd),
    ['a', 'b', 'c'],
  )
  core.handleResult('id2', true, 'b', undefined)
  core.handleResult('id3', true, 'c', undefined)
  assert.equal(await unsent, 'b')
  assert.equal(await queued, 'c')
})

test('listen is refcounted per event', () => {
  const { core, sent } = setup()
  core.linkUp()
  const off1 = core.listen('run://x', () => {})
  const off2 = core.listen('run://x', () => {})
  assert.deepEqual(sent, [{ kind: 'cmd', action: 'ipc_listen', event: 'run://x' }])
  off1()
  off1() // idempotent
  assert.equal(sent.length, 1)
  off2()
  assert.deepEqual(sent.at(-1), { kind: 'cmd', action: 'ipc_unlisten', event: 'run://x' })
  assert.deepEqual(core.events, [])
})

test('same handler listened twice unlistens once per call', () => {
  const { core } = setup()
  const seen = []
  const h = (e) => seen.push(e.payload)
  const off1 = core.listen('e', h)
  core.listen('e', h)
  off1()
  core.handleEvent('e', 1)
  assert.deepEqual(seen, [1])
})

test('events dispatch as Tauri-like objects and a throwing handler does not stop others', () => {
  const { core } = setup()
  const seen = []
  core.listen('e', () => {
    throw new Error('bad')
  })
  core.listen('e', (ev) => seen.push(ev))
  core.handleEvent('e', { n: 1 })
  core.handleEvent('other', 2)
  assert.deepEqual(seen, [{ event: 'e', id: 0, payload: { n: 1 } }])
})

test('subscriptions made while down are sent on linkUp, and re-sent on every re-auth', () => {
  const { core, sent } = setup()
  core.listen('a', () => {})
  core.listen('b', () => {})
  assert.equal(sent.length, 0)
  core.linkUp()
  core.linkDown()
  core.linkUp()
  assert.deepEqual(
    sent.map((f) => `${f.action}:${f.event}`),
    ['ipc_listen:a', 'ipc_listen:b', 'ipc_listen:a', 'ipc_listen:b'],
  )
})

test('re-subscriptions go out before queued invokes', () => {
  const { core, sent } = setup()
  core.listen('a', () => {})
  core.invoke('x')
  core.linkUp()
  assert.deepEqual(
    sent.map((f) => f.action),
    ['ipc_listen', 'ipc_invoke'],
  )
})

test('unlisten while down sends nothing and is not re-subscribed', () => {
  const { core, sent } = setup()
  core.linkUp()
  const off = core.listen('a', () => {})
  core.linkDown()
  off()
  core.linkUp()
  assert.deepEqual(
    sent.map((f) => f.action),
    ['ipc_listen'],
  )
})

test('close rejects everything, clears subscriptions and refuses new work', async () => {
  const { core, sent, timers } = setup()
  core.linkUp()
  const a = core.invoke('a')
  core.linkDown()
  const b = core.invoke('b')
  const seen = []
  core.listen('e', (ev) => seen.push(ev))
  core.close()
  assert.equal((await settled(a)).e.message, ERR_CONNECTION_LOST)
  assert.equal((await settled(b)).e.message, ERR_CLOSED)
  assert.equal(timers.size, 0)
  core.handleEvent('e', 1)
  assert.deepEqual(seen, [])
  assert.equal((await settled(core.invoke('c'))).e.message, ERR_CLOSED)
  const before = sent.length
  core.linkUp()
  assert.equal(sent.length, before)
})
