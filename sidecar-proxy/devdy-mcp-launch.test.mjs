// Tests for devdy-mcp-launch.mjs against a fake broker socket.
//
//   node --test sidecar-proxy/devdy-mcp-launch.test.mjs

import test from 'node:test'
import assert from 'node:assert/strict'
import net from 'node:net'
import os from 'node:os'
import path from 'node:path'
import fs from 'node:fs'
import { spawn } from 'node:child_process'
import { fileURLToPath } from 'node:url'
import { parseArgs } from './devdy-mcp-launch.mjs'

const LAUNCHER = path.join(path.dirname(fileURLToPath(import.meta.url)), 'devdy-mcp-launch.mjs')

/** Fake broker: records requests, answers each with `reply(req)`. */
async function fakeBroker(reply) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'devdy-launch-'))
  const sock = path.join(dir, 'b.sock')
  const requests = []
  const server = net.createServer((c) => {
    let buf = ''
    c.on('data', (d) => {
      buf += d
      const nl = buf.indexOf('\n')
      if (nl < 0) return
      const req = JSON.parse(buf.slice(0, nl))
      requests.push(req)
      c.end(JSON.stringify(reply(req)) + '\n')
    })
  })
  await new Promise((r) => server.listen(sock, r))
  return { sock, requests, close: () => new Promise((r) => server.close(r)) }
}

function launch(args, env) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [LAUNCHER, ...args], {
      env: { PATH: process.env.PATH, ...env },
      stdio: ['ignore', 'pipe', 'pipe'],
    })
    let stdout = ''
    let stderr = ''
    child.stdout.on('data', (d) => (stdout += d))
    child.stderr.on('data', (d) => (stderr += d))
    child.on('close', (code) => resolve({ code, stdout, stderr }))
  })
}

const brokerEnv = (sock) => ({ DEVDY_BROKER_SOCK: sock, DEVDY_PROJECT_ID: 'p1', DEVDY_RUN_ID: 'r1' })

test('parseArgs accepts only the two documented forms', () => {
  assert.deepEqual(parseArgs(['headers', 's1']), { mode: 'headers', server: 's1' })
  assert.deepEqual(parseArgs(['env', 's1', '--', 'npx', '-y', 'pkg']), {
    mode: 'env', server: 's1', command: 'npx', args: ['-y', 'pkg'],
  })
  assert.equal(parseArgs(['env', 's1', 'npx']), null)
  assert.equal(parseArgs(['env', 's1', '--']), null)
  assert.equal(parseArgs(['headers', 's1', 'extra']), null)
  assert.equal(parseArgs(['other', 's1']), null)
  assert.equal(parseArgs(['headers']), null)
})

test('headers mode prints the broker headers as JSON', async () => {
  const b = await fakeBroker(() => ({ decision: 'allow', env: {}, headers: { Authorization: 'Bearer t' } }))
  const r = await launch(['headers', '@conductor'], brokerEnv(b.sock))
  await b.close()
  assert.equal(r.code, 0)
  assert.deepEqual(JSON.parse(r.stdout), { Authorization: 'Bearer t' })
  assert.deepEqual(b.requests, [{ project_id: 'p1', run_id: 'r1', tool: 'mcp', argv: [], host: null, server: '@conductor' }])
})

test('env mode runs the real server with the broker env and passes stdio through', async () => {
  const b = await fakeBroker(() => ({ decision: 'allow', env: { API_KEY: 'k-123' }, headers: {} }))
  const r = await launch(
    ['env', 's1', '--', process.execPath, '-e', 'process.stdout.write(`${process.env.API_KEY}|${process.env.DEVDY_BROKER_SOCK}`); process.exit(7)'],
    brokerEnv(b.sock),
  )
  await b.close()
  assert.equal(r.stdout, 'k-123|undefined', 'secret env in, broker coordinates out')
  assert.equal(r.code, 7, 'exit code is the real server\'s')
})

test('a denial runs nothing and prints no secret', async () => {
  const b = await fakeBroker(() => ({ decision: 'deny', reason: 'MCP server is not enabled for this project' }))
  const marker = path.join(os.tmpdir(), `devdy-launch-ran-${process.pid}`)
  const r = await launch(
    ['env', 's1', '--', process.execPath, '-e', `require('fs').writeFileSync(${JSON.stringify(marker)}, '')`],
    brokerEnv(b.sock),
  )
  await b.close()
  assert.equal(r.code, 99)
  assert.match(r.stderr, /not enabled for this project/)
  assert.equal(fs.existsSync(marker), false, 'the real server must not start')
})

test('missing broker env fails closed', async () => {
  const r = await launch(['headers', 's1'], {})
  assert.equal(r.code, 97)
  assert.equal(r.stdout, '')
})

test('an unreachable broker fails closed', async () => {
  const r = await launch(['headers', 's1'], brokerEnv(path.join(os.tmpdir(), 'devdy-no-such.sock')))
  assert.equal(r.code, 98)
  assert.equal(r.stdout, '')
})
