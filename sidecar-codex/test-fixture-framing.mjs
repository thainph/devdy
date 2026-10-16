#!/usr/bin/env node
// JSON-RPC fixture for test-framing-seed.mjs: records thread/start,
// thread/resume and turn/start params to DEVDY_FRAMING_RPC_OUT (NDJSON).
import { appendFileSync } from 'node:fs'

const out = process.env.DEVDY_FRAMING_RPC_OUT
let buffer = ''
const send = (m) => process.stdout.write(JSON.stringify(m) + '\n')

process.stdin.on('data', (chunk) => {
  buffer += chunk.toString()
  let nl
  while ((nl = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, nl).trim()
    buffer = buffer.slice(nl + 1)
    if (!line) continue
    let m
    try { m = JSON.parse(line) } catch { continue }
    if (['thread/start', 'thread/resume', 'turn/start'].includes(m.method)) {
      appendFileSync(out, JSON.stringify({ method: m.method, params: m.params }) + '\n')
    }
    if (m.id === undefined) continue
    if (m.method === 'thread/start') {
      send({ jsonrpc: '2.0', id: m.id, result: { thread: { id: 'fixture-thread' }, model: 'fixture' } })
    } else if (m.method === 'thread/resume') {
      send({ jsonrpc: '2.0', id: m.id, result: { thread: { id: m.params.threadId }, model: 'fixture' } })
    } else if (m.method === 'turn/start') {
      send({ jsonrpc: '2.0', id: m.id, result: { turn: { id: `turn-${m.id}` } } })
    } else {
      send({ jsonrpc: '2.0', id: m.id, result: {} })
    }
  }
})
