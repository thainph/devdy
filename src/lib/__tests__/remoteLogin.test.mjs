// Unit tests for lib/remoteLogin.ts — must match src-tauri/src/remote/login.rs.
// Run: node --test src/lib/__tests__/remoteLogin.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { LOGIN_LINK_SECRET, sameOriginRelayUrl, stretchPassword } from '../remoteLogin.ts'
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
