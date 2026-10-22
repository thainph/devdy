// Pure-function unit tests for the gh/glab/aws/az/terraform shim env handling.
// Run with: node --test sidecar-proxy/shim.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { scrubCloudEnv, pathWithoutDir, azureMfaRequired } from './shim.mjs'

const inherited = () => ({
  PATH: '/usr/bin',
  AWS_REGION: 'ap-northeast-1',
  AZURE_CONFIG_DIR: '/sandbox/_empty',
  AZURE_EXTENSION_DIR: '/evil/ext',
  AZURE_CLIENT_SECRET: 'x',
  ARM_METADATA_HOSTNAME: 'evil.example',
  ARM_ENVIRONMENT: 'usgovernment',
  MSI_ENDPOINT: 'http://169.254.169.254',
  IDENTITY_HEADER: 'h',
  TF_CLI_ARGS: '-no-color',
  TF_CLI_ARGS_init: '-migrate-state',
  TF_CLI_CONFIG_FILE: '/evil/.terraformrc',
  TERRAFORM_CONFIG: '/evil/.terraformrc',
  TF_PLUGIN_CACHE_DIR: '/evil/plugins',
  TF_LOG: 'INFO',
})

test('az: drops every AZURE_* / ARM_* / identity var, keeps unrelated env', () => {
  const env = scrubCloudEnv(inherited(), 'az')
  for (const k of Object.keys(env)) {
    assert.ok(!k.startsWith('AZURE_') && !k.startsWith('ARM_'), k)
  }
  assert.equal(env.MSI_ENDPOINT, undefined)
  assert.equal(env.IDENTITY_HEADER, undefined)
  assert.equal(env.PATH, '/usr/bin')
  assert.equal(env.AWS_REGION, 'ap-northeast-1')
  // TF vars are terraform-only concerns.
  assert.equal(env.TF_CLI_ARGS_init, '-migrate-state')
})

test('terraform: also drops TF_CLI_ARGS*, CLI config and plugin cache overrides', () => {
  const env = scrubCloudEnv(inherited(), 'terraform')
  for (const k of ['TF_CLI_ARGS', 'TF_CLI_ARGS_init', 'TF_CLI_CONFIG_FILE', 'TERRAFORM_CONFIG', 'TF_PLUGIN_CACHE_DIR', 'ARM_METADATA_HOSTNAME', 'ARM_ENVIRONMENT', 'AZURE_EXTENSION_DIR']) {
    assert.equal(env[k], undefined, k)
  }
  assert.equal(env.TF_LOG, 'INFO')
})

test('other tools are untouched', () => {
  const env = scrubCloudEnv(inherited(), 'gh')
  assert.equal(env.AZURE_EXTENSION_DIR, '/evil/ext')
})

test('pathWithoutDir removes only the given dir', () => {
  assert.equal(pathWithoutDir('/shim:/usr/bin:/shim/:/bin', '/shim'), '/usr/bin:/bin')
})

test('azureMfaRequired spots Entra / ARM MFA rejections only', () => {
  assert.ok(azureMfaRequired('ERROR: AADSTS50076: Due to a configuration change made by your administrator, you must use multi-factor authentication'))
  assert.ok(azureMfaRequired('AADSTS50079: The user is required to use multi-factor authentication.'))
  assert.ok(azureMfaRequired("ERROR: (RequestDisallowedByAzure) Resource 'vm' was disallowed by Azure: ... without authenticating through MFA."))
  assert.ok(!azureMfaRequired('ERROR: (RequestDisallowedByAzure) Resource was disallowed by policy.'))
  assert.ok(!azureMfaRequired('AADSTS700082: The refresh token has expired due to inactivity.'))
  assert.ok(!azureMfaRequired('ERROR: (ResourceNotFound) not found'))
})
