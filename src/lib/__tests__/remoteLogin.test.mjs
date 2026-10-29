// Unit tests for lib/remoteLogin.ts — must match src-tauri/src/remote/login.rs.
// Run: node --test src/lib/__tests__/remoteLogin.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  DEVICE_ALIAS_RE,
  LOGIN_LINK_SECRET,
  loginCodeFor,
  relayWsUrl,
  sameOriginRelayUrl,
  stretchPassword,
} from '../remoteLogin.ts'
import { deriveEffectivePsk, ready } from '../../remote/e2e/index.ts'

const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('')

test('stretchPassword is PBKDF2-HMAC-SHA256 with the shared salt and rounds', async () => {
  assert.equal(
    await stretchPassword('devdy-test-password'),
    'e8a8778886105840cd3ab9ce1fc1d60e2224dc27e695782498bf3afcb64aa745',
  )
})

test('login PSK matches the Rust host vector', async () => {
  await ready()
  const psk = deriveEffectivePsk(LOGIN_LINK_SECRET, await stretchPassword('devdy-test-password'))
  assert.equal(hex(psk), 'c2ee56d76cc322ce322e386031f956429595a64a04195e648ba9f27fa070e57f')
})

test('sameOriginRelayUrl follows the page scheme', () => {
  assert.equal(sameOriginRelayUrl({ protocol: 'https:', host: 'relay.example.com' }), 'wss://relay.example.com/ws')
  assert.equal(sameOriginRelayUrl({ protocol: 'http:', host: 'localhost:8080' }), 'ws://localhost:8080/ws')
})

test('loginCodeFor mirrors login::login_code', () => {
  assert.equal(loginCodeFor(), 'devdy-login-v1')
  assert.equal(loginCodeFor(''), 'devdy-login-v1')
  assert.equal(loginCodeFor('  '), 'devdy-login-v1')
  assert.equal(loginCodeFor('mac-mini'), 'devdy-login-v1/mac-mini')
})

test('DEVICE_ALIAS_RE mirrors login::valid_alias', () => {
  assert.ok(DEVICE_ALIAS_RE.test(''))
  assert.ok(DEVICE_ALIAS_RE.test('mac-mini-2'))
  assert.ok(DEVICE_ALIAS_RE.test('a'.repeat(32)))
  assert.ok(!DEVICE_ALIAS_RE.test('a'.repeat(33)))
  assert.ok(!DEVICE_ALIAS_RE.test('Mac'))
  assert.ok(!DEVICE_ALIAS_RE.test('mac/mini'))
})

test('relayWsUrl keeps ws(s) URLs and maps http(s) to the /ws endpoint', () => {
  assert.equal(relayWsUrl('wss://relay.example.com/ws'), 'wss://relay.example.com/ws')
  assert.equal(relayWsUrl('ws://localhost:8080/ws'), 'ws://localhost:8080/ws')
  assert.equal(relayWsUrl('https://relay.example.com'), 'wss://relay.example.com/ws')
  assert.equal(relayWsUrl('http://localhost:8080/'), 'ws://localhost:8080/ws')
  assert.equal(relayWsUrl(''), null)
  assert.equal(relayWsUrl('not a url'), null)
})
