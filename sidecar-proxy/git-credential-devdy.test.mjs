// Pure-function unit tests for the git credential helper (AC5).
// Run with: node --test sidecar-proxy/git-credential-devdy.test.mjs
import { test } from 'node:test'
import assert from 'node:assert'
import {
  parseCredentialInput,
  formatCredentialOutput,
} from './git-credential-devdy.mjs'

test('parseCredentialInput reads key=value pairs', () => {
  const p = parseCredentialInput('protocol=https\nhost=github.com\npath=a/b\n')
  assert.equal(p.protocol, 'https')
  assert.equal(p.host, 'github.com')
  assert.equal(p.path, 'a/b')
})

test('parseCredentialInput stops at the blank line', () => {
  const p = parseCredentialInput('host=github.com\n\nafter=ignored\n')
  assert.equal(p.host, 'github.com')
  assert.ok(!('after' in p))
})

test('parseCredentialInput tolerates CRLF and skips array/empty keys', () => {
  const p = parseCredentialInput('protocol=https\r\nhost=x\r\nwwwauth[]=Basic\r\n=noKey\r\n')
  assert.equal(p.protocol, 'https')
  assert.equal(p.host, 'x')
  assert.ok(!('wwwauth[]' in p))
  assert.ok(!('' in p))
})

test('formatCredentialOutput emits username/password + blank terminator', () => {
  assert.equal(
    formatCredentialOutput({ username: 'x-access-token', password: 'secret' }),
    'username=x-access-token\npassword=secret\n\n',
  )
})

test('formatCredentialOutput omits empty fields', () => {
  assert.equal(formatCredentialOutput({}), '\n')
  assert.equal(formatCredentialOutput({ password: 'p' }), 'password=p\n\n')
  assert.equal(formatCredentialOutput({ username: '', password: 't' }), 'password=t\n\n')
})
