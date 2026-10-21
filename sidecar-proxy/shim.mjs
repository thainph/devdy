// Devdy gh/glab/aws/az/terraform shim core (GĐ3).
//
// A tiny wrapper placed at the FRONT of the sidecar PATH under the names
// `gh`/`glab`/`aws`/`az`/`terraform`. Every invocation asks the Devdy broker (over
// a per-run Unix socket) whether the call is allowed and, if so, receives
// credentials for the project's linked account. The real binary is then executed
// with credentials injected ONLY into that child process's environment.
//
// Azure: the broker returns the linked account's isolated AZURE_CONFIG_DIR (the
// Devdy-managed az session, incl. its plaintext token cache, lives there) plus
// tenant/subscription ids; the
// sidecar itself only ever sees an empty AZURE_CONFIG_DIR. terraform gets AWS
// and/or Azure (azurerm uses Azure CLI auth) depending on what is linked.
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
import { scanTerraformExternalCode, terraformWorkDir } from './terraform-scan.mjs'

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
 * @param {{project_id: string, tool: string, argv: string[], host: string|null, run_id?: string|null, tf_external_code?: boolean, tf_external_hits?: string[]}} req
 * @returns {Promise<{decision: string, reason?: string, token?: string, aws_access_key_id?: string, aws_secret_access_key?: string, aws_session_token?: string, aws_profile?: string, aws_region?: string, azure_config_dir?: string, azure_tenant_id?: string, azure_subscription_id?: string}>}
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
    // No inactivity timeout: a write op that needs approval keeps the socket
    // idle while the user decides, and a visible prompt must never be silently
    // denied (the broker re-alerts the user instead). This process's lifetime is
    // bounded by its caller (e.g. the Bash tool timeout); when it dies, the
    // broker sees the disconnect and withdraws the prompt.
  })
}

/**
 * Resolve the real tool binary by scanning PATH with the shim's own
 * directory removed, so we never re-invoke the shim.
 * @param {string} tool
 * @returns {string|null}
 */
function resolveReal(tool) {
  const dirs = pathWithoutDir(process.env.PATH, shimDir()).split(path.delimiter)
  for (const d of dirs) {
    if (!d) continue
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

// Non-prefixed managed-identity env honored by Azure SDKs / azurerm.
const AZURE_IDENTITY_ENV = ['MSI_ENDPOINT', 'MSI_SECRET', 'IDENTITY_ENDPOINT', 'IDENTITY_HEADER']

// Terraform CLI env that could change what a policy-approved command does
// (`TF_CLI_ARGS_init=-migrate-state` smuggles flags past the policy) or load
// unvetted config/providers.
const TERRAFORM_UNSAFE_ENV = ['TF_CLI_CONFIG_FILE', 'TERRAFORM_CONFIG', 'TF_PLUGIN_CACHE_DIR']

/**
 * Drop inherited env that could redirect an az/terraform child away from the
 * broker-provided account, BEFORE our own values are injected:
 *   - every `AZURE_*` (az reads config from AZURE_<SECTION>_<KEY>, e.g.
 *     AZURE_EXTENSION_DIR loads arbitrary Python) and every `ARM_*` (azurerm
 *     auth/endpoint overrides such as ARM_METADATA_HOSTNAME), plus MSI/identity
 *     endpoints;
 *   - terraform only: `TF_CLI_ARGS*`, CLI config / plugin cache overrides.
 * Mutates and returns `env`. Never inspects values.
 * @param {Record<string, string|undefined>} env
 * @param {string} tool
 */
export function scrubCloudEnv(env, tool) {
  if (tool !== 'az' && tool !== 'terraform') return env
  for (const key of Object.keys(env)) {
    if (key.startsWith('AZURE_') || key.startsWith('ARM_')) delete env[key]
    if (tool === 'terraform' && key.startsWith('TF_CLI_ARGS')) delete env[key]
  }
  for (const key of AZURE_IDENTITY_ENV) delete env[key]
  if (tool === 'terraform') {
    for (const key of TERRAFORM_UNSAFE_ENV) delete env[key]
  }
  return env
}

/**
 * The shim's own directory (process.argv[1] is the shim script path).
 * @returns {string}
 */
function shimDir() {
  return path.resolve(path.dirname(process.argv[1] || ''))
}

/**
 * PATH with the shim dir removed. Used for children that shell out to a
 * shimmed tool themselves (terraform's azurerm runs `az account
 * get-access-token`, which must reach the REAL az, not the policy-gated shim).
 * @param {string|undefined} pathValue
 * @param {string} dir
 * @returns {string}
 */
export function pathWithoutDir(pathValue, dir) {
  return (pathValue || '')
    .split(path.delimiter)
    .filter((d) => d && path.resolve(d) !== dir)
    .join(path.delimiter)
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
 * Entry point for a shim. `tool` is 'gh', 'glab', 'aws', 'az', or 'terraform'.
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

  const req = {
    project_id: projectId,
    tool,
    argv,
    host: null,
    run_id: runId,
  }
  if (tool === 'terraform') {
    // Code that runs outside Terraform (data.external, local-exec, data.http)
    // would execute with the linked credentials during plan/validate; the
    // broker asks the user when it is present. Fail-closed on scan errors.
    let scan
    try {
      scan = scanTerraformExternalCode(terraformWorkDir(argv, process.cwd()))
    } catch {
      scan = { detected: true, hits: ['.:scan-error'] }
    }
    req.tf_external_code = scan.detected
    req.tf_external_hits = scan.hits
  }

  let resp
  try {
    resp = await ask(sock, req)
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

  const hasAws = Boolean(
    (resp.aws_access_key_id && resp.aws_secret_access_key) || resp.aws_profile,
  )
  const hasAzure = Boolean(resp.azure_config_dir)

  // The sidecar's AZURE_CONFIG_DIR is Devdy's empty sandbox; keep it for a
  // terraform child without a linked Azure account so azurerm never falls back
  // to ~/.azure.
  const sandboxAzureConfigDir = childEnv.AZURE_CONFIG_DIR
  scrubCloudEnv(childEnv, tool)

  if (tool === 'terraform' && !hasAws && !hasAzure) {
    process.stderr.write(`devdy: ${tool} — credential unavailable\n`)
    process.exit(EXIT_DENIED)
  }

  if (tool === 'az') {
    if (!hasAzure) {
      process.stderr.write(`devdy: ${tool} — credential unavailable\n`)
      process.exit(EXIT_DENIED)
    }
    // The account dir holds the Devdy-managed az session with the project's
    // default subscription; nothing else is needed.
    childEnv.AZURE_CONFIG_DIR = resp.azure_config_dir
  } else if (tool === 'aws' || tool === 'terraform') {
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
    }
    // terraform: same AWS env as the aws CLI — the AWS provider/S3 backend read
    // the standard env credential chain (or AWS_PROFILE for SSO profiles). AWS is
    // optional for terraform (an Azure-only project has none).
    if (tool === 'aws' || hasAws) {
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
    }
    if (tool === 'terraform' && hasAzure) {
      // azurerm (and the azurerm state backend) authenticate via Azure CLI:
      // they run `az account get-access-token` against this config dir.
      childEnv.AZURE_CONFIG_DIR = resp.azure_config_dir
      if (resp.azure_tenant_id) childEnv.ARM_TENANT_ID = resp.azure_tenant_id
      if (resp.azure_subscription_id) childEnv.ARM_SUBSCRIPTION_ID = resp.azure_subscription_id
    } else if (tool === 'terraform' && sandboxAzureConfigDir) {
      childEnv.AZURE_CONFIG_DIR = sandboxAzureConfigDir
    }
    if (tool === 'terraform') {
      // Drop the shim dir from the child's PATH: azurerm shells out to `az
      // account get-access-token`, which via the az shim would fail (broker env
      // is stripped from this child, and policy denies get-access-token).
      childEnv.PATH = pathWithoutDir(childEnv.PATH, shimDir())
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
