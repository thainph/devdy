// Unit tests for the remote-desktop bridge path: lib/ipc/bridgeOutbox.ts
// (ordered delivery), lib/ipc/bridgeEvents.ts (denylist), lib/ipc/resyncCore.ts
// (reload throttle) and the ipc_* parsing in controller/protocol.ts.
// Run: node --test src/lib/__tests__/remoteBridge.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { BridgeOutbox } from '../ipc/bridgeOutbox.ts'
import { isBridgeDeniedEvent } from '../ipc/bridgeEvents.ts'
import { resyncDelay, RESYNC_MIN_GAP_MS } from '../ipc/resyncCore.ts'
import { parseStreamPayload } from '../../controller/protocol.ts'

/** A batch sender whose calls settle by hand, recording what is in flight. */
function manualSender() {
  const batches = []
  const pending = []
  let inFlight = 0
  let maxInFlight = 0
  const sendBatch = (items) => {
    batches.push(items)
    inFlight += 1
    maxInFlight = Math.max(maxInFlight, inFlight)
    return new Promise((resolve, reject) => {
      pending.push({
        resolve: () => {
          inFlight -= 1
          resolve()
        },
        reject: (e) => {
          inFlight -= 1
          reject(e)
        },
      })
    })
  }
  return { sendBatch, batches, pending, maxInFlight: () => maxInFlight }
}

const tick = () => new Promise((r) => setImmediate(r))
const ev = (n) => ({ kind: 'event', event: 'e', payload: n })
const res = (id) => ({ kind: 'result', id, ok: true, value: null, error: null })

test('outbox: one batch in flight; items keep their order across batches', async () => {
  const s = manualSender()
  const box = new BridgeOutbox({ sendBatch: s.sendBatch })
  box.push(res('a'))
  box.push(ev(1))
  box.push(res('b'))
  box.push(ev(2))
  assert.deepEqual(s.batches, [[res('a')]])
  assert.equal(box.size, 3)
  s.pending.shift().resolve()
  await tick()
  assert.deepEqual(s.batches[1], [ev(1), res('b'), ev(2)])
  box.push(ev(3))
  s.pending.shift().resolve()
  await tick()
  assert.deepEqual(s.batches[2], [ev(3)])
  s.pending.shift().resolve()
  await tick()
  assert.equal(s.maxInFlight(), 1)
  assert.deepEqual(s.batches.flat(), [res('a'), ev(1), res('b'), ev(2), ev(3)])
})

test('outbox: a failed batch is reported and the queue carries on', async () => {
  const s = manualSender()
  const errors = []
  const box = new BridgeOutbox({ sendBatch: s.sendBatch, onError: (e) => errors.push(e) })
  box.push(ev(1))
  box.push(ev(2))
  s.pending.shift().reject(new Error('ipc down'))
  await tick()
  assert.equal(errors.length, 1)
  assert.deepEqual(s.batches[1], [ev(2)])
  s.pending.shift().resolve()
  await tick()
  box.push(ev(3))
  assert.deepEqual(s.batches[2], [ev(3)], 'idle again: the next push sends right away')
})

test('bridge denylist: menu / tray / own channel stay local, app events pass', () => {
  for (const e of ['menu://action', 'tray:open-processes', 'tray://open-run', 'remote-bridge://request', 'tauri://focus']) {
    assert.equal(isBridgeDeniedEvent(e), true, e)
  }
  for (const e of ['run://status', 'remote://desktop-attached', 'todos://changed']) {
    assert.equal(isBridgeDeniedEvent(e), false, e)
  }
})

test('resync throttle: first reload now, then at most one per gap', () => {
  assert.equal(resyncDelay(1_000_000, null), 0)
  assert.equal(resyncDelay(1_000_000, 1_000_000 - RESYNC_MIN_GAP_MS), 0)
  assert.equal(resyncDelay(1_000_000, 1_000_000 - 10_000), RESYNC_MIN_GAP_MS - 10_000)
  // A clock that went backwards must not postpone forever.
  assert.equal(resyncDelay(1_000_000, 2_000_000), 0)
})

test('ipc_result: a missing value is null, never undefined', () => {
  assert.deepEqual(parseStreamPayload({ kind: 'ipc_result', id: 'a', ok: true }), {
    kind: 'ipc_result',
    id: 'a',
    ok: true,
    value: null,
    error: undefined,
  })
  assert.equal(parseStreamPayload({ kind: 'ipc_result', id: 'a', ok: true, value: 0 }).value, 0)
})

test('ipc_resync parses', () => {
  assert.deepEqual(parseStreamPayload({ kind: 'ipc_resync' }), { kind: 'ipc_resync' })
})
