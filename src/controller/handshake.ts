/**
 * E2E handshake logic for the Controller (SRS §5 / spec §3), split out from the
 * transport so it is pure and unit-testable.
 *
 * The Controller receives the Host's {@link HsOffer} over a `control` frame,
 * MUST verify the Host fingerprint against the pinned link value (SEC-006), then
 * derives the session key with `@/remote/e2e` and answers with its own ephemeral
 * public key.
 *
 * KEY NOTE (interop-critical): the effective psk is a two-layer KDF over the
 * high-entropy `link_secret` (from the link) and a short `secret2` — the OTP the
 * user types (pairing) or the stored `session_secret` (reconnect). Both sides
 * feed the same bytes via {@link deriveEffectivePsk} (mirrors the Rust
 * `derive_effective_psk`), so a wrong OTP just yields a different key and every
 * `open` fails — there is no comparison oracle.
 */
import {
  Session,
  deriveEffectivePsk,
  generateKeypair,
  hostFingerprint,
  type Keypair,
} from '@/remote/e2e'
import { fromHex, parseHsOffer, toHex, type HsOffer } from './protocol'

/** A handshake failure with a user-facing reason (surfaced in the UI). */
export class HandshakeError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'HandshakeError'
  }
}

/**
 * The 32-byte effective psk fed to the KDF, derived from the high-entropy
 * `linkSecretHex` (from the link) and a short `secret2` (the OTP on pairing, or
 * the stored `session_secret` on reconnect). Mirrors the Rust
 * `derive_effective_psk` byte-for-byte.
 */
export function sessionSecretBytes(linkSecretHex: string, secret2: string): Uint8Array {
  return deriveEffectivePsk(linkSecretHex, secret2)
}

/** The result of a successful handshake: a live session + the answer to send. */
export interface HandshakeResult {
  session: Session
  /** Our ephemeral public key, lowercase hex, to send in the `hs_answer`. */
  controllerPubHex: string
}

/**
 * Verify a decoded {@link HsOffer} against the pinned link fingerprint and
 * derive the session key with the already-computed effective psk.
 *
 * Two independent checks defend against a relay MITM (SEC-006):
 *   1. The offered `host_fingerprint` must equal the link's `host_fingerprint`.
 *   2. `hostFingerprint(host_pub)` recomputed locally must equal that value too
 *      — so a relay can't ship a matching label with a swapped key.
 *
 * @param offer decoded Host offer (already shape-checked by {@link parseHsOffer})
 * @param linkFingerprint the `host_fingerprint` from the link (pinned)
 * @param effectivePsk the 32-byte effective psk (see {@link sessionSecretBytes})
 * @param keypair this Controller's ephemeral keypair (inject for tests)
 * @throws {HandshakeError} on any fingerprint mismatch or bad key.
 */
export function verifyAndDerive(
  offer: HsOffer,
  linkFingerprint: string,
  effectivePsk: Uint8Array,
  keypair: Keypair,
): HandshakeResult {
  // (1) The offered fingerprint must match what the link pinned.
  if (!constantTimeEqHex(offer.host_fingerprint, linkFingerprint)) {
    throw new HandshakeError(
      'Host fingerprint does not match the link — possible relay MITM. Connection refused.',
    )
  }

  // (2) Recompute the fingerprint from the offered public key; a relay that
  // forwarded a genuine label but a swapped key is caught here.
  let hostPub: Uint8Array
  try {
    hostPub = fromHex(offer.host_pub)
  } catch {
    throw new HandshakeError('Malformed host public key in handshake offer.')
  }
  const recomputed = hostFingerprint(hostPub)
  if (!constantTimeEqHex(recomputed, linkFingerprint)) {
    throw new HandshakeError(
      'Host public key does not match its fingerprint — possible relay MITM. Connection refused.',
    )
  }

  // Fingerprint verified — derive the shared session key.
  let session: Session
  try {
    session = Session.derive(effectivePsk, keypair, hostPub)
  } catch (e) {
    throw new HandshakeError(`Failed to derive session key: ${(e as Error).message}`)
  }
  return { session, controllerPubHex: toHex(keypair.publicKey) }
}

/**
 * Full handshake entry from a raw decoded control payload: shape-check the
 * offer then {@link verifyAndDerive}. Generates a fresh ephemeral keypair
 * unless one is injected (tests supply a deterministic keypair).
 *
 * @throws {HandshakeError} if the payload is not a valid offer or verification
 *   fails.
 */
export function handleOffer(
  offerJson: unknown,
  linkFingerprint: string,
  effectivePsk: Uint8Array,
  keypair: Keypair = generateKeypair(),
): HandshakeResult {
  const offer = parseHsOffer(offerJson)
  if (!offer) throw new HandshakeError('Malformed handshake offer from Host.')
  return verifyAndDerive(offer, linkFingerprint, effectivePsk, keypair)
}

/**
 * Verify the Host offer WITHOUT deriving the session key. Runs the two SEC-006
 * MITM checks (offered fingerprint == pinned, and recomputed fingerprint ==
 * pinned) and returns the Host's ephemeral public key bytes. Used by the
 * connection so it can answer the handshake and reach the ACTIVE room BEFORE the
 * OTP is known — the session key is derived later via {@link deriveSessionKey}.
 *
 * @throws {HandshakeError} on a malformed offer or any fingerprint mismatch.
 */
export function verifyOffer(offerJson: unknown, linkFingerprint: string): Uint8Array {
  const offer = parseHsOffer(offerJson)
  if (!offer) throw new HandshakeError('Malformed handshake offer from Host.')
  if (!constantTimeEqHex(offer.host_fingerprint, linkFingerprint)) {
    throw new HandshakeError(
      'Host fingerprint does not match the link — possible relay MITM. Connection refused.',
    )
  }
  let hostPub: Uint8Array
  try {
    hostPub = fromHex(offer.host_pub)
  } catch {
    throw new HandshakeError('Malformed host public key in handshake offer.')
  }
  if (!constantTimeEqHex(hostFingerprint(hostPub), linkFingerprint)) {
    throw new HandshakeError(
      'Host public key does not match its fingerprint — possible relay MITM. Connection refused.',
    )
  }
  return hostPub
}

/**
 * Derive the shared session key from a Host public key already verified by
 * {@link verifyOffer}, the effective psk (link_secret + OTP/token), and this
 * Controller's ephemeral keypair.
 *
 * @throws {HandshakeError} if key derivation fails.
 */
export function deriveSessionKey(
  hostPub: Uint8Array,
  effectivePsk: Uint8Array,
  keypair: Keypair,
): HandshakeResult {
  let session: Session
  try {
    session = Session.derive(effectivePsk, keypair, hostPub)
  } catch (e) {
    throw new HandshakeError(`Failed to derive session key: ${(e as Error).message}`)
  }
  return { session, controllerPubHex: toHex(keypair.publicKey) }
}

/**
 * Constant-time-ish comparison of two hex strings (case-insensitive). Avoids a
 * length/prefix timing signal when comparing fingerprints; not a hard security
 * boundary (the fingerprint isn't secret) but cheap and correct.
 */
function constantTimeEqHex(a: string, b: string): boolean {
  const x = a.toLowerCase()
  const y = b.toLowerCase()
  if (x.length !== y.length) return false
  let diff = 0
  for (let i = 0; i < x.length; i += 1) diff |= x.charCodeAt(i) ^ y.charCodeAt(i)
  return diff === 0
}
