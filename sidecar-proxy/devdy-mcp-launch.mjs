#!/usr/bin/env node
// Devdy MCP credential launcher.
//
// A run's MCP config is passed to the engine CLI on its command line and lands
// in process environments, where any process of the same user can read it. So
// Devdy puts no secret in it; a server that needs one is pointed at this script,
// which asks the broker (same socket and fail-closed rules as the gh shim) at the
// moment the secret is needed:
//
//   devdy-mcp-launch.mjs env <server> -- <command> [args...]
//     Fetch the server's env, then run the real stdio server with it. stdio is
//     inherited, so the MCP stream goes straight through.
//
//   devdy-mcp-launch.mjs headers <server>
//     Print the server's HTTP headers as one JSON object on stdout — the
//     contract of Claude's `headersHelper` (the Codex sidecar uses it too).
//
// <server> is a user server's id, or `@conductor` for the loopback conductor.
//
// Hard rules (as for shim.mjs): never write a secret anywhere but the child env
// / the helper's stdout; any denial or error → non-zero exit, nothing is run.

import { spawn } from 'node:child_process'
import { constants } from 'node:os'
import { pathToFileURL } from 'node:url'
import { ask } from './shim.mjs'

const EXIT_USAGE = 2
const EXIT_NOT_CONFIGURED = 97
const EXIT_BROKER_UNREACHABLE = 98
const EXIT_DENIED = 99
const EXIT_EXEC_FAILED = 95

const BROKER_ENV = ['DEVDY_BROKER_SOCK', 'DEVDY_PROJECT_ID', 'DEVDY_RUN_ID']

const fail = (code, msg) => {
  process.stderr.write(`devdy-mcp-launch: ${msg}\n`)
  process.exit(code)
}

/** Parse argv (without node + script). Exported for tests. */
export function parseArgs(argv) {
  const [mode, server, ...rest] = argv
  if (!server || (mode !== 'env' && mode !== 'headers')) return null
  if (mode === 'headers') return rest.length ? null : { mode, server }
  if (rest[0] !== '--' || !rest[1]) return null
  return { mode, server, command: rest[1], args: rest.slice(2) }
}

async function fetchCredentials(server) {
  const sock = process.env.DEVDY_BROKER_SOCK
  const projectId = process.env.DEVDY_PROJECT_ID
  if (!sock || !projectId) fail(EXIT_NOT_CONFIGURED, 'broker env missing')
  let resp
  try {
    resp = await ask(sock, {
      project_id: projectId,
      run_id: process.env.DEVDY_RUN_ID || null,
      tool: 'mcp',
      argv: [],
      host: null,
      server,
    })
  } catch {
    fail(EXIT_BROKER_UNREACHABLE, 'credential broker unreachable')
  }
  if (!resp || resp.decision !== 'allow') {
    fail(EXIT_DENIED, `denied: ${(resp && resp.reason) || 'no reason'}`)
  }
  return { env: resp.env || {}, headers: resp.headers || {} }
}

async function main() {
  const parsed = parseArgs(process.argv.slice(2))
  if (!parsed) {
    fail(EXIT_USAGE, 'usage: devdy-mcp-launch.mjs env <server> -- <command> [args...] | headers <server>')
  }
  const creds = await fetchCredentials(parsed.server)

  if (parsed.mode === 'headers') {
    process.stdout.write(JSON.stringify(creds.headers) + '\n')
    return
  }

  // A third-party server gets its own env, not a way to ask the broker for
  // other servers' secrets (as shim.mjs does for the real gh/aws).
  const env = { ...process.env, ...creds.env }
  for (const k of BROKER_ENV) delete env[k]
  const child = spawn(parsed.command, parsed.args, { stdio: 'inherit', env })
  for (const sig of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(sig, () => child.kill(sig))
  }
  child.on('error', (e) => fail(EXIT_EXEC_FAILED, `failed to start ${parsed.command}: ${e.code || e.message}`))
  child.on('exit', (code, signal) => {
    process.exit(signal ? 128 + (constants.signals[signal] || 0) : (code ?? 0))
  })
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) main()
