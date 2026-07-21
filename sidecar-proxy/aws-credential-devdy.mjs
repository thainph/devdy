// Devdy AWS credential_process helper.
//
// AWS SDKs/CLI can invoke this helper from a per-run AWS_CONFIG_FILE as
// `credential_process`. The helper asks Devdy's broker over the same socket as
// the gh/glab/aws shims and prints AWS Version 1 credential JSON only when the
// project has a linked keys account and policy allows the request.
//
// Hard rules:
//   - NEVER log/write credentials except the single credential_process JSON on
//     stdout after broker allow.
//   - Fail-closed: broker missing/unreachable/deny/profile-only => exit non-zero
//     and print nothing.

import { ask } from './shim.mjs'

/**
 * @param {{aws_access_key_id?: string, aws_secret_access_key?: string, aws_session_token?: string}} resp
 * @returns {null|{Version: 1, AccessKeyId: string, SecretAccessKey: string, SessionToken?: string}}
 */
export function credentialFromBrokerResponse(resp) {
  if (!resp || typeof resp !== 'object') return null
  if (!resp.aws_access_key_id || !resp.aws_secret_access_key) return null
  const out = {
    Version: 1,
    AccessKeyId: resp.aws_access_key_id,
    SecretAccessKey: resp.aws_secret_access_key,
  }
  if (resp.aws_session_token) out.SessionToken = resp.aws_session_token
  return out
}

/**
 * @param {{Version: 1, AccessKeyId: string, SecretAccessKey: string, SessionToken?: string}} cred
 * @returns {string}
 */
export function formatCredentialProcessOutput(cred) {
  return JSON.stringify(cred) + '\n'
}

export async function run() {
  const sock = process.env.DEVDY_BROKER_SOCK
  const projectId = process.env.DEVDY_PROJECT_ID
  if (!sock || !projectId) return 1

  let resp
  try {
    resp = await ask(sock, {
      project_id: projectId,
      tool: 'aws',
      argv: ['credential', 'get'],
      host: null,
      run_id: process.env.DEVDY_RUN_ID || null,
    })
  } catch {
    return 1
  }

  if (!resp || resp.decision !== 'allow') return 1
  const cred = credentialFromBrokerResponse(resp)
  if (!cred) return 1
  process.stdout.write(formatCredentialProcessOutput(cred))
  return 0
}
