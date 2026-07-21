// Pure-function unit tests for the AWS credential_process helper.
// Run with: node --test sidecar-proxy/aws-credential-devdy.test.mjs
import { test } from 'node:test'
import assert from 'node:assert'
import {
  credentialFromBrokerResponse,
  formatCredentialProcessOutput,
} from './aws-credential-devdy.mjs'

test('credentialFromBrokerResponse maps keys response', () => {
  const cred = credentialFromBrokerResponse({
    aws_access_key_id: 'AKIA...',
    aws_secret_access_key: 'secret',
    aws_session_token: 'session',
  })
  assert.deepEqual(cred, {
    Version: 1,
    AccessKeyId: 'AKIA...',
    SecretAccessKey: 'secret',
    SessionToken: 'session',
  })
})

test('credentialFromBrokerResponse rejects profile-only response', () => {
  assert.equal(credentialFromBrokerResponse({ aws_profile: 'work-sso' }), null)
})

test('formatCredentialProcessOutput emits JSON line', () => {
  assert.equal(
    formatCredentialProcessOutput({
      Version: 1,
      AccessKeyId: 'AKIA...',
      SecretAccessKey: 'secret',
    }),
    '{"Version":1,"AccessKeyId":"AKIA...","SecretAccessKey":"secret"}\n',
  )
})
