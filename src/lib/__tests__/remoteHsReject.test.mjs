// Unit tests for the Host's gate rejection in controller/protocol.ts — the wire
// must match `HandshakePayload::HsReject` in src-tauri/src/remote/protocol.rs.
// Run: node --test src/lib/__tests__/remoteHsReject.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import { isHsReject, parseHsOffer } from '../../controller/protocol.ts'

test('hs_reject is recognised and never mistaken for an offer', () => {
  const reject = JSON.parse('{"kind":"hs_reject"}')
  assert.equal(isHsReject(reject), true)
  assert.equal(parseHsOffer(reject), null)
})

test('anything else is not a rejection', () => {
  const hex = 'a'.repeat(64)
  assert.equal(isHsReject({ kind: 'hs_offer', host_pub: hex, host_fingerprint: hex }), false)
  assert.equal(isHsReject(null), false)
  assert.equal(isHsReject('hs_reject'), false)
  assert.equal(isHsReject({}), false)
})
