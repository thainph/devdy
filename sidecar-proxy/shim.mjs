// Devdy gh/glab/aws shim core (GĐ3).
//
// A tiny wrapper placed at the FRONT of the sidecar PATH under the names
// `gh`/`glab`/`aws`. Every invocation asks the Devdy broker (over a per-run Unix
// socket) whether the call is allowed and, if so, receives credentials for the
// project's linked account. The real binary is then executed with credentials
// injected ONLY into that child process's environment.
//
// Hard rules (frozen):
//   - NEVER log/write credentials anywhere (no console, no stderr, no file).
//   - Fail-closed: any misconfiguration/denial/error => non-zero exit, the real
//     binary is NOT run.
//   - Self-exclude the shim dir from PATH when resolving the real binary so the
//     shim never re-invokes itself (infinite recursion).
//   - Strip DEVDY_BROKER_SOCK / DEVDY_PROJECT_ID from the child env so the
//     socket path never leaks to child tools.

import net from 'node:net'
import { spawn } from 'node:child_process'
import path from 'node:path'
import fs from 'node:fs'

// Distinct exit codes so failures are debuggable without leaking anything.
const EXIT_NOT_CONFIGURED = 97 // broker env missing
const EXIT_BROKER_UNREACHABLE = 98 // could not talk to broker
const EXIT_DENIED = 99 // broker denied by policy / fail-closed
const EXIT_REAL_NOT_FOUND = 96 // real tool not on PATH
const EXIT_EXEC_FAILED = 95 // spawning the real binary failed

/**
 * Ask the broker for a decision. Sends one NDJSON request line, reads one
 * NDJSON response line.
 * @param {string} sockPath
 * @param {{project_id: string, tool: string, argv: string[], host: string|null}} req
 * @returns {Promise<{decision: string, reason?: string, token?: string, aws_access_key_id?: string, aws_secret_access_key?: string, aws_session_token?: string, aws_profile?: string, aws_region?: string}>}
 */
export function ask(sockPath, req) {
  return new Promise((resolve, reject) => {
    const conn = net.createConnection(sockPath)
    let buf = ''
    let settled = false
    const done = (fn, arg) => {
      if (settled) return
      settled = true
      try {
        conn.destroy()
      } catch {
        // ignore
      }
      fn(arg)
    }
    conn.on('connect', () => {
      conn.write(JSON.stringify(req) + '\n')
    })
    conn.on('data', (d) => {
      buf += d.toString('utf8')
      const nl = buf.indexOf('\n')
      if (nl >= 0) {
        try {
          done(resolve, JSON.parse(buf.slice(0, nl)))
        } catch (e) {
          done(reject, e)
        }
      }
    })
    conn.on('error', (e) => done(reject, e))
    conn.on('close', () => {
      if (!settled) done(reject, new Error('broker closed connection'))
    })
    // Inactivity timeout. A write op that needs approval keeps the socket idle
    // while the broker awaits the user's modal decision, so this MUST be at least
    // as long as the broker's approval window (approver.rs APPROVAL_TIMEOUT).
    // 15s was shorter than a human's reaction time and turned "slow to approve"
    // into a spurious "broker timeout" fail-closed deny mid-session.
    conn.setTimeout(300000, () => done(reject, new Error('broker timeout')))
  })
}

/**
 * Resolve the real tool binary by scanning PATH with the shim's own
 * directory removed, so we never re-invoke the shim.
 * @param {string} tool
 * @returns {string|null}
 */
function resolveReal(tool) {
  // process.argv[1] is the shim script path. Its dir is the shim dir.
  const shimDir = path.resolve(path.dirname(process.argv[1] || ''))
  const dirs = (process.env.PATH || '')
    .split(path.delimiter)
    .filter((d) => d && path.resolve(d) !== shimDir)
  for (const d of dirs) {
    const candidate = path.join(d, tool)
    try {
      fs.accessSync(candidate, fs.constants.X_OK)
      return candidate
    } catch {
      // not here; keep looking
    }
  }
  return null
}

/**
 * Drop any `--profile <name>` / `--profile=<name>` tokens from an `aws` argv.
 *
 * Inside a run the account is decided by the broker (the project's linked AWS
 * account), never by a CLI flag — creds arrive as injected access keys and
 * AWS_PROFILE is stripped. A stray `--profile foo` would make the real `aws`
 * look `foo` up in the sandbox config (which only holds `[default]`) and fail
 * with "The config profile (foo) could not be found". Silently ignoring the
 * flag lets the agent pass it without breaking.
 * @param {string[]} argv
 * @returns {{argv: string[], stripped: boolean}}
 */
export function stripAwsProfileArgs(argv) {
  const out = []
  let stripped = false
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]
    if (a === '--profile') {
      stripped = true
      if (i + 1 < argv.length) i++ // also drop the value token
      continue
    }
    if (a.startsWith('--profile=')) {
      stripped = true
      continue
    }
    out.push(a)
  }
  return { argv: out, stripped }
}

/**
 * Entry point for a shim. `tool` is 'gh', 'glab', or 'aws'.
 * @param {string} tool
 */
export async function run(tool) {
  const sock = process.env.DEVDY_BROKER_SOCK
  const projectId = process.env.DEVDY_PROJECT_ID
  // GĐ7: the app-wide broker serves many runs from one socket; run_id tells it
  // which run's modal to route an `Ask` to. Absent → broker fail-closed denies
  // any `Ask` (reads/allows still work).
  const runId = process.env.DEVDY_RUN_ID || null
  const argv = process.argv.slice(2) // drop [node, shimPath]

  if (!sock || !projectId) {
    process.stderr.write(
      `devdy: broker not configured — ${tool} unavailable in this run\n`,
    )
    process.exit(EXIT_NOT_CONFIGURED)
  }

  let resp
  try {
    resp = await ask(sock, {
      project_id: projectId,
      tool,
      argv,
      host: null,
      run_id: runId,
    })
  } catch {
    process.stderr.write(
      `devdy: broker unreachable — ${tool} denied (fail-closed)\n`,
    )
    process.exit(EXIT_BROKER_UNREACHABLE)
  }

  if (!resp || resp.decision !== 'allow') {
    const reason = (resp && resp.reason) || 'denied by policy'
    process.stderr.write(`devdy: ${tool} — ${reason}\n`)
    process.exit(EXIT_DENIED)
  }

  const realBin = resolveReal(tool)
  if (!realBin) {
    process.stderr.write(
      `devdy: real ${tool} not found on PATH (install it to use this run)\n`,
    )
    process.exit(EXIT_REAL_NOT_FOUND)
  }

  // Credentials go ONLY into the child env. Never logged, never persisted.
  const childEnv = { ...process.env }
  let execArgv = argv
  const originalAwsConfigFile = childEnv.DEVDY_ORIGINAL_AWS_CONFIG_FILE
  const originalAwsCredentialsFile = childEnv.DEVDY_ORIGINAL_AWS_SHARED_CREDENTIALS_FILE
  delete childEnv.DEVDY_BROKER_SOCK
  delete childEnv.DEVDY_PROJECT_ID
  delete childEnv.DEVDY_RUN_ID
  delete childEnv.DEVDY_ORIGINAL_AWS_CONFIG_FILE
  delete childEnv.DEVDY_ORIGINAL_AWS_SHARED_CREDENTIALS_FILE

  if (tool === 'aws') {
    // The broker (project-linked account) decides which identity is used, so a
    // `--profile` flag on the command line can only point at a profile the
    // sandbox config does not contain. Drop it instead of letting `aws` fail.
    const sanitized = stripAwsProfileArgs(argv)
    if (sanitized.stripped) {
      process.stderr.write(
        'devdy: ignoring --profile for aws; using the account linked to this project\n',
      )
    }
    execArgv = sanitized.argv
    if (resp.aws_region) {
      childEnv.AWS_REGION = resp.aws_region
      childEnv.AWS_DEFAULT_REGION = resp.aws_region
    }
    if (resp.aws_access_key_id && resp.aws_secret_access_key) {
      childEnv.AWS_ACCESS_KEY_ID = resp.aws_access_key_id
      childEnv.AWS_SECRET_ACCESS_KEY = resp.aws_secret_access_key
      if (resp.aws_session_token) childEnv.AWS_SESSION_TOKEN = resp.aws_session_token
      else delete childEnv.AWS_SESSION_TOKEN
      delete childEnv.AWS_PROFILE
      delete childEnv.AWS_DEFAULT_PROFILE
    } else if (resp.aws_profile) {
      childEnv.AWS_PROFILE = resp.aws_profile
      if (originalAwsConfigFile) childEnv.AWS_CONFIG_FILE = originalAwsConfigFile
      else delete childEnv.AWS_CONFIG_FILE
      if (originalAwsCredentialsFile) childEnv.AWS_SHARED_CREDENTIALS_FILE = originalAwsCredentialsFile
      else delete childEnv.AWS_SHARED_CREDENTIALS_FILE
      delete childEnv.AWS_SDK_LOAD_CONFIG
      delete childEnv.AWS_ACCESS_KEY_ID
      delete childEnv.AWS_SECRET_ACCESS_KEY
      delete childEnv.AWS_SESSION_TOKEN
    } else {
      process.stderr.write(`devdy: ${tool} — credential unavailable\n`)
      process.exit(EXIT_DENIED)
    }
  } else if (resp.token) {
    if (tool === 'gh') {
      childEnv.GH_TOKEN = resp.token
    } else {
      childEnv.GITLAB_TOKEN = resp.token
    }
  }

  const child = spawn(realBin, execArgv, { stdio: 'inherit', env: childEnv })
  child.on('error', () => {
    process.stderr.write(`devdy: failed to exec ${tool}\n`)
    process.exit(EXIT_EXEC_FAILED)
  })
  child.on('exit', (code, signal) => {
    // Propagate the real binary's exit code; a signal maps to a generic non-zero.
    process.exit(signal ? 1 : code ?? 1)
  })
}
