// Unit tests for lib/ipc/mux.ts (remote pop-outs sharing the main
// remote-desktop window's connection) and lib/ipc/windows.ts (their labels).
// Run: node --test src/lib/__tests__/remoteMux.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { MuxHost, MuxPeer, decodeError, encodeError } from '../ipc/mux.ts'
import { IpcCore } from '../ipc/ipcCore.ts'
import {
  busEventName,
  fromRemoteLabel,
  isRemoteLabel,
  toRemoteLabel,
  withRemoteParam,
} from '../ipc/windows.ts'

const tick = () => new Promise((r) => setTimeout(r, 0))

/** A fake relay client: records listens, lets the test push events / settle invokes. */
function fakeClient() {
  const listeners = new Map() // event → Set<handler>
  const invokes = []
  return {
    listeners,
    invokes,
    invoke(cmd, args) {
      return new Promise((resolve, reject) => invokes.push({ cmd, args, resolve, reject }))
    },
    listen(event, handler) {
      if (!listeners.has(event)) listeners.set(event, new Set())
      listeners.get(event).add(handler)
      return Promise.resolve(() => listeners.get(event)?.delete(handler))
    },
    fire(event, payload) {
      for (const h of listeners.get(event) ?? []) h({ event, id: 0, payload })
    },
    count(event) {
      return listeners.get(event)?.size ?? 0
    },
  }
}

/** A host + peers wired back to back (synchronous delivery). */
function setup() {
  const client = fakeClient()
  const peers = new Map()
  const host = new MuxHost({
    client,
    send: (label, reply) => peers.get(label)?.peer.handle(reply),
  })
  const addPeer = (label) => {
    let id = 0
    const core = new IpcCore({
      send: (frame) => {
        host.handle(label, frame)
        return true
      },
      newId: () => `${label}-${++id}`,
    })
    const states = []
    const hellos = []
    const peer = new MuxPeer({
      core,
      sendHello: (n) => {
        hellos.push(n)
        host.handle(label, { t: 'hello', n })
      },
      onState: (s) => states.push(s),
    })
    const entry = { core, peer, states, hellos }
    peers.set(label, entry)
    return entry
  }
  return { client, host, addPeer, peers }
}

test('labels: remote pop-outs never share a label with local windows', () => {
  assert.equal(toRemoteLabel('main'), 'remote-desktop')
  assert.equal(toRemoteLabel('session-1ab'), 'remote-session-1ab')
  assert.equal(toRemoteLabel('remote-session-1ab'), 'remote-session-1ab')
  assert.equal(fromRemoteLabel('remote-desktop'), 'main')
  assert.equal(fromRemoteLabel('remote-fileviewer-x'), 'fileviewer-x')
  assert.equal(fromRemoteLabel('pulse'), 'pulse')
  assert.ok(isRemoteLabel('remote-pulse'))
  assert.ok(!isRemoteLabel('pulse'))
  assert.equal(busEventName('item:changed'), 'remote-desktop:bus:item:changed')
})

test('urls: remote=1 is added without disturbing the rest', () => {
  assert.equal(withRemoteParam('index.html?calendarWindow=1'), 'index.html?calendarWindow=1&remote=1')
  assert.equal(withRemoteParam('index.html'), 'index.html?remote=1')
  assert.equal(withRemoteParam('index.html?remote=1&a=b'), 'index.html?remote=1&a=b')
  assert.equal(withRemoteParam('index.html?a=1#x'), 'index.html?a=1&remote=1#x')
  const u = new URLSearchParams(withRemoteParam('index.html?path=a%20b%26c').split('?')[1])
  assert.equal(u.get('path'), 'a b&c')
})

test('errors cross the mux as data', () => {
  const e = decodeError(encodeError(new Error('remote_timeout')))
  assert.ok(e instanceof Error)
  assert.equal(e.message, 'remote_timeout')
  assert.deepEqual(decodeError(encodeError({ code: 'x' })), { code: 'x' })
  assert.equal(decodeError(encodeError('nope')), 'nope')
})

test('invokes are queued until the owner welcomes the peer, then forwarded', async () => {
  const { client, addPeer } = setup()
  const { core, peer } = addPeer('remote-pulse')
  const p = core.invoke('list_projects', { a: 1 })
  assert.equal(client.invokes.length, 0)
  peer.hello()
  assert.equal(client.invokes.length, 1)
  assert.deepEqual(client.invokes[0].args, { a: 1 })
  client.invokes[0].resolve(['p1'])
  assert.deepEqual(await p, ['p1'])

  const q = core.invoke('boom')
  client.invokes[1].reject('denied')
  await assert.rejects(q, (e) => e === 'denied')
  const r = core.invoke('slow')
  client.invokes[2].reject(new Error('remote_timeout'))
  await assert.rejects(r, /remote_timeout/)
})

test('two peers share one owner-side subscription per event', async () => {
  const { client, host, addPeer } = setup()
  const a = addPeer('remote-a')
  const b = addPeer('remote-b')
  a.peer.hello()
  b.peer.hello()
  const gotA = []
  const gotB = []
  const offA = a.core.listen('run:done', (e) => gotA.push(e.payload))
  b.core.listen('run:done', (e) => gotB.push(e.payload))
  await tick()
  assert.equal(client.count('run:done'), 2, 'one per peer; the real client refcounts towards B')
  client.fire('run:done', 1)
  assert.deepEqual(gotA, [1])
  assert.deepEqual(gotB, [1])
  offA()
  await tick()
  assert.equal(client.count('run:done'), 1)
  assert.deepEqual(host.eventsOf('remote-a'), [])
  client.fire('run:done', 2)
  assert.deepEqual(gotA, [1])
  assert.deepEqual(gotB, [1, 2])
})

test('a hello retry keeps subscriptions; a new hello starts over', async () => {
  const { client, host, addPeer } = setup()
  const { core, peer, hellos } = addPeer('remote-a')
  peer.hello()
  core.listen('x', () => {})
  await tick()
  assert.equal(client.count('x'), 1)
  peer.retryHello() // already welcomed: nothing sent
  host.handle('remote-a', { t: 'hello', n: hellos[0] }) // a late duplicate of the first hello
  await tick()
  assert.equal(client.count('x'), 1)
  // The owner restarted: the peer re-announces and re-subscribes.
  peer.hello()
  await tick()
  assert.equal(client.count('x'), 1)
  assert.deepEqual(host.eventsOf('remote-a'), ['x'])
})

test('a reloaded pop-out (new page, same label) does not pass as a hello retry', () => {
  const sent = []
  const page = () => new MuxPeer({ core: { linkUp() {}, linkDown() {} }, sendHello: (n) => sent.push(n) })
  page().hello()
  page().hello()
  assert.notEqual(sent[0], sent[1])
})

test('owner going down fails in-flight calls; peers leaving release subscriptions', async () => {
  const { client, host, addPeer } = setup()
  const { core, peer } = addPeer('remote-a')
  peer.hello()
  const p = core.invoke('slow')
  core.listen('x', () => {})
  await tick()
  peer.down()
  await assert.rejects(p, /remote_connection_lost/)
  assert.equal(peer.connected, false)
  host.drop('remote-a')
  await tick()
  assert.equal(client.count('x'), 0)
  assert.deepEqual(host.peerLabels, [])
})

test('state changes reach every peer', () => {
  const { host, addPeer } = setup()
  const a = addPeer('remote-a')
  a.peer.hello()
  host.setState({ status: 'connected', auth: 'authenticated', lastError: null })
  assert.equal(a.states.at(-1).status, 'connected')
})
