// Devdy git credential helper (GĐ4).
//
// git invokes a credential helper as `git-credential-<name> <action>` where
// action ∈ {get, store, erase}. This helper routes `get` through the Devdy
// broker (over the same per-run Unix socket as the gh/glab shim) and returns the
// project's token as the credential password. `store`/`erase` are no-ops (Devdy
// never persists credentials to disk).
//
// Hard rules (frozen):
//   - NEVER log/write the token anywhere (no console, no stderr, no file). The
//     ONLY output is the two `username=`/`password=` lines on stdout for `get`.
//   - Fail-closed = exit 0 + print NOTHING (git treats "no credential" cleanly,
//     so the run does not crash — it just fails HTTPS auth). We never exit
//     non-zero, so a broker/misconfig problem never aborts `git fetch`.
//   - Token is NEVER put in an env var or a URL — only the broker holds it and it
//     is written once to stdout as `password=<token>`.
//   - The helper never invokes real git and never reads `credential.helper`
//     recursively.

import { ask } from './shim.mjs'

/**
 * Parse git credential stdin (lines of `key=value`, terminated by a blank line)
 * into a plain object. Unknown keys are kept; array-valued keys (`wwwauth[]`)
 * are ignored. Pure function — no I/O — so it is unit-testable (AC5).
 * @param {string} text
 * @returns {Record<string,string>}
 */
export function parseCredentialInput(text) {
  const out = {}
  if (typeof text !== 'string') return out
  for (const rawLine of text.split('\n')) {
    const line = rawLine.replace(/\r$/, '')
    if (line === '') break // blank line terminates the request
    const eq = line.indexOf('=')
    if (eq <= 0) continue // no key, or empty key → skip
    const key = line.slice(0, eq)
    const value = line.slice(eq + 1)
    if (key.endsWith('[]')) continue // multi-valued attrs (e.g. wwwauth[]) unused
    out[key] = value
  }
  return out
}

/**
 * Format a git credential response. Only emits keys with non-empty values,
 * followed by a terminating blank line (git protocol). Pure function.
 * @param {{username?: string, password?: string}} cred
 * @returns {string}
 */
export function formatCredentialOutput(cred) {
  let out = ''
  if (cred && typeof cred.username === 'string' && cred.username !== '') {
    out += `username=${cred.username}\n`
  }
  if (cred && typeof cred.password === 'string' && cred.password !== '') {
    out += `password=${cred.password}\n`
  }
  out += '\n'
  return out
}

/**
 * Read all of stdin as a UTF-8 string.
 * @returns {Promise<string>}
 */
function readStdin() {
  return new Promise((resolve) => {
    let buf = ''
    if (process.stdin.isTTY) {
      resolve('')
      return
    }
    process.stdin.setEncoding('utf8')
    process.stdin.on('data', (d) => {
      buf += d
    })
    process.stdin.on('end', () => resolve(buf))
    process.stdin.on('error', () => resolve(buf))
  })
}

/**
 * Credential helper orchestration. Always resolves; never throws to the caller.
 * Fail-closed = print nothing (exit 0). The entry point calls process.exit(0).
 * @param {string|undefined} action
 */
export async function run(action) {
  // store / erase → no-op. Devdy never persists credentials.
  if (action !== 'get') return

  const sock = process.env.DEVDY_BROKER_SOCK
  const projectId = process.env.DEVDY_PROJECT_ID
  // Not configured → fail-closed (print nothing → git auth fails, run survives).
  if (!sock || !projectId) return

  const input = await readStdin()
  const fields = parseCredentialInput(input)
  const protocol = fields.protocol
  const host = fields.host
  // Only serve HTTPS with a concrete host; anything else → print nothing.
  if (protocol !== 'https' || !host) return

  let resp
  try {
    resp = await ask(sock, {
      project_id: projectId,
      tool: 'git',
      argv: ['credential', 'get'],
      host,
      run_id: process.env.DEVDY_RUN_ID || null,
    })
  } catch {
    // Broker unreachable → fail-closed, print nothing, do NOT crash the run.
    return
  }

  // deny / no token → fail-closed (print nothing).
  if (!resp || resp.decision !== 'allow' || !resp.token) return

  process.stdout.write(
    formatCredentialOutput({
      username: resp.username || 'x-access-token',
      password: resp.token,
    }),
  )
}
