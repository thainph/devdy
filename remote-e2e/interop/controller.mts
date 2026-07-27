/**
 * Controller protocol/handshake proof (U4). Standalone tsx runner — no vitest.
 *
 *   npx tsx remote-e2e/interop/controller.mts
 *
 * Exit 0 = all assertions passed. Proves the security-critical behaviours QA
 * cares about:
 *   1. A Controller derives the SAME session key as the Host, given the Host's
 *      PSK convention (ASCII bytes of the hex `psk` string — matches
 *      `agent.rs`: `Session::new(psk.as_bytes(), …)`), and can seal/open both
 *      ways.
 *   2. host_fingerprint MISMATCH → handshake REFUSED (SEC-006 / FR-003 error).
 *   3. A swapped host key whose label still matches the QR → REFUSED (the
 *      Controller recomputes the fingerprint from the offered key).
 *   4. Only `allow_once` / `deny_once` are representable in a respond_permission
 *      command (BR-016 / SEC-011 / AC-17); the builder cannot emit `*_always`.
 *   5. Pairing payload parsing accepts JSON, a wss deep-link, and rejects a
 *      non-wss relay URL / missing fields.
 *
 * Lives OUTSIDE `src/**` on purpose: the app tsconfig targets the browser and
 * doesn't type Node APIs used by a runner. It exercises the same browser-clean
 * modules the app ships.
 */
import { ready } from '../../src/remote/e2e/index.ts'
import {
  generateKeypair,
  hostFingerprint,
  Session,
} from '../../src/remote/e2e/index.ts'
import {
  hsAnswerJson,
  parseHsOffer,
  parsePairingPayload,
  PairingParseError,
  respondPermissionCmd,
  toHex,
  fromHex,
  type HsOffer,
} from '../../src/controller/protocol.ts'
import {
  handleOffer,
  HandshakeError,
  pskBytes,
  verifyAndDerive,
} from '../../src/controller/handshake.ts'

let passed = 0
function assert(cond: boolean, msg: string): void {
  if (!cond) {
    console.error(`FAIL: ${msg}`)
    process.exitCode = 1
    throw new Error(msg)
  }
  passed += 1
  console.log(`ok - ${msg}`)
}
function assertThrows(fn: () => void, msg: string): void {
  let threw = false
  try {
    fn()
  } catch {
    threw = true
  }
  assert(threw, msg)
}

async function main(): Promise<void> {
  await ready()

  // A pairing psk is a hex string; both ends key BLAKE2b with its ASCII bytes.
  const pskHex = 'a'.repeat(64)

  // Host and Controller ephemeral keypairs.
  const host = generateKeypair()
  const controller = generateKeypair()
  const qrFingerprint = hostFingerprint(host.publicKey)

  // ---- (1) same session key + seal/open both ways ----
  const offer: HsOffer = {
    kind: 'hs_offer',
    host_pub: toHex(host.publicKey),
    host_fingerprint: qrFingerprint,
  }
  const parsed = parseHsOffer(offer)
  assert(parsed !== null, 'parseHsOffer accepts a well-formed offer')

  const result = verifyAndDerive(offer, qrFingerprint, pskHex, controller)
  // Host derives with the ASCII-of-hex psk and the Controller's public key.
  const hostSession = Session.derive(pskBytes(pskHex), host, controller.publicKey)
  const ctrlOpensHost = result.session.open(hostSession.seal(new TextEncoder().encode('hi-from-host')))
  assert(
    new TextDecoder().decode(ctrlOpensHost) === 'hi-from-host',
    'Controller opens a frame sealed by the Host (identical session key)',
  )
  const hostOpensCtrl = hostSession.open(result.session.seal(new TextEncoder().encode('hi-from-ctrl')))
  assert(
    new TextDecoder().decode(hostOpensCtrl) === 'hi-from-ctrl',
    'Host opens a frame sealed by the Controller',
  )
  assert(result.controllerPubHex === toHex(controller.publicKey), 'answer carries controller pub')
  // The answer JSON is well-formed and only carries the public key.
  const answer = JSON.parse(hsAnswerJson(result.controllerPubHex))
  assert(answer.kind === 'hs_answer' && typeof answer.controller_pub === 'string', 'hs_answer shape')

  // ---- (2) fingerprint mismatch → refuse (SEC-006) ----
  const wrongFp = 'b'.repeat(64)
  assertThrows(
    () => verifyAndDerive(offer, wrongFp, pskHex, controller),
    'fingerprint mismatch is REFUSED (MITM guard, SEC-006)',
  )
  // ...and via the full offer entry point too.
  let mitmMsg = ''
  try {
    handleOffer(offer, wrongFp, pskHex, controller)
  } catch (e) {
    mitmMsg = e instanceof HandshakeError ? e.message : ''
  }
  assert(mitmMsg.length > 0, 'handleOffer rejects on fingerprint mismatch with a HandshakeError')

  // ---- (3) swapped key, matching label → refuse (recomputed fp differs) ----
  const attacker = generateKeypair()
  const spoofed: HsOffer = {
    kind: 'hs_offer',
    host_pub: toHex(attacker.publicKey), // relay swapped in its own key
    host_fingerprint: qrFingerprint, // but kept the genuine label
  }
  assertThrows(
    () => verifyAndDerive(spoofed, qrFingerprint, pskHex, controller),
    'swapped host key with a genuine label is REFUSED (recomputed fingerprint)',
  )

  // ---- (4) only allow_once / deny_once representable (AC-17 / BR-016) ----
  const allow = respondPermissionCmd('r_1', 'p_9', 'allow_once')
  const deny = respondPermissionCmd('r_1', 'p_9', 'deny_once')
  assert(allow.decision === 'allow_once', 'respond_permission allow_once')
  assert(deny.decision === 'deny_once', 'respond_permission deny_once')
  // The decision type union has exactly two members; a non-once value is not a
  // valid RemoteDecision. Prove at runtime no builder path yields `*_always`.
  const emitted = [allow.decision, deny.decision]
  assert(
    !emitted.some((d) => d === 'allow_always' || d === 'deny_always' || d === 'allow' || d === 'deny'),
    'no builder path emits allow_always/deny_always',
  )

  // ---- (5) pairing payload parsing ----
  const jsonPayload = JSON.stringify({
    relay_url: 'wss://relay.example/ws',
    pair_code: 'deadbeef',
    psk: pskHex,
    host_fingerprint: qrFingerprint,
  })
  const p1 = parsePairingPayload(jsonPayload)
  assert(p1.relay_url === 'wss://relay.example/ws' && p1.pair_code === 'deadbeef', 'parses JSON payload')

  const link = `https://ctrl.example/#${encodeURIComponent(jsonPayload)}`
  const p2 = parsePairingPayload(link)
  assert(p2.psk === pskHex, 'parses a wss deep-link with JSON in the fragment')

  const linkParams =
    `https://ctrl.example/#relay_url=${encodeURIComponent('wss://relay.example/ws')}` +
    `&pair_code=deadbeef&psk=${pskHex}&host_fingerprint=${qrFingerprint}`
  const p3 = parsePairingPayload(linkParams)
  assert(p3.host_fingerprint === qrFingerprint, 'parses a deep-link with query-style params')

  assertThrows(
    () =>
      parsePairingPayload(
        JSON.stringify({ relay_url: 'ws://insecure/ws', pair_code: 'x', psk: pskHex, host_fingerprint: qrFingerprint }),
      ),
    'rejects a non-wss relay_url (CON-03 / SEC-009)',
  )
  assertThrows(() => parsePairingPayload('{"relay_url":"wss://r/ws"}'), 'rejects missing fields')
  assert(new PairingParseError('x') instanceof Error, 'PairingParseError is an Error subclass')

  // Sanity: hex round-trips (matches Rust pairing_hex/unhex).
  assert(toHex(fromHex('00ff')) === '00ff', 'hex round-trips lowercase')

  console.log(`\nCONTROLLER OK: ${passed} assertions passed`)
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})
