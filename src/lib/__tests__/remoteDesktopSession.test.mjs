// Unit tests for lib/remoteDesktopSession.ts (the remote-desktop sign-in record).
// Run: node --test src/lib/__tests__/remoteDesktopSession.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { parseRemoteDesktopSession } from '../remoteDesktopSession.ts'

const link = {
  relay_url: 'wss://relay.example.com/ws',
  host_fingerprint: 'ab'.repeat(32),
  rendezvous_code: 'code',
  run_id: 'run-1',
  link_secret: 'cd'.repeat(32),
}

test('parses a stored record and drops unknown fields', () => {
  const raw = JSON.stringify({ link: { ...link, extra: 1 }, token: 'tok', device: 'mini', more: true })
  assert.deepEqual(parseRemoteDesktopSession(raw), { link, token: 'tok', device: 'mini' })
})

test('device defaults to unnamed', () => {
  assert.equal(parseRemoteDesktopSession(JSON.stringify({ link, token: 'tok' })).device, '')
})

test('rejects missing or malformed records', () => {
  assert.equal(parseRemoteDesktopSession(null), null)
  assert.equal(parseRemoteDesktopSession('not json'), null)
  assert.equal(parseRemoteDesktopSession('null'), null)
  assert.equal(parseRemoteDesktopSession(JSON.stringify({ link, token: '' })), null)
  assert.equal(parseRemoteDesktopSession(JSON.stringify({ token: 'tok' })), null)
  assert.equal(
    parseRemoteDesktopSession(JSON.stringify({ link: { ...link, link_secret: '' }, token: 'tok' })),
    null,
  )
})
