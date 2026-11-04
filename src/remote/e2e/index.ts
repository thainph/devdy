/**
 * Devdy Remote Control — E2E crypto contract (TS reference impl).
 *
 * Cross-compatible with the Rust reference impl in `remote-e2e/` (crate
 * `dryoc`). Both sides use the same libsodium-compatible primitives (X25519,
 * BLAKE2b, XChaCha20-Poly1305 IETF), so the same test vector yields the same
 * `cipher` bytes.
 *
 * Spec: `docs/remote-e2e-spec.md` (SEC-006 / CON-04 / AC-19). We NEVER
 * implement crypto primitives by hand — everything routes through the audited,
 * tree-shakeable `@noble/*` libraries (pure JS, no WASM init). Only the
 * base64/hex codecs are local, and they reproduce libsodium's strict
 * `base64_variants.ORIGINAL` decoder (padding required, canonical trailing bits)
 * so wire behaviour is byte-identical to the previous libsodium-wrappers impl.
 *
 * Frame layout of the decoded `cipher` (spec §4):
 *   version(1B) | nonce(24B) | aead_ct    (version byte bound as AEAD AD)
 */
import { xchacha20poly1305 } from '@noble/ciphers/chacha.js'
import { randomBytes } from '@noble/ciphers/utils.js'
import { x25519 } from '@noble/curves/ed25519.js'
import { blake2b } from '@noble/hashes/blake2.js'

/** Wire format version — first byte of every frame (spec §2). */
export const E2E_VERSION = 0x01
/** Domain-separation label mixed into the session-key KDF (spec §3). */
const CONTEXT = 'devdy-remote-e2e/v1'
/** Domain-separation label for the host fingerprint (spec §3). */
const FP_CONTEXT = 'devdy-host-fp/v1'
/** Domain-separation label for the two-layer (link_secret + OTP) psk KDF. */
const OTP_KDF_CONTEXT = 'devdy-remote-otp/v1'

/** X25519 / AEAD key length. */
export const KEY_LEN = 32
/** XChaCha20-Poly1305 nonce length. */
export const NONCE_LEN = 24
/** Poly1305 tag length. */
export const TAG_LEN = 16
/** X25519 public/secret key length. */
export const PUB_LEN = 32

/**
 * Kept for API compatibility: the previous libsodium impl needed its WASM
 * runtime initialized before any call. The `@noble/*` primitives are pure JS
 * and need no init, so this resolves immediately.
 */
export async function ready(): Promise<void> {}

/** An ephemeral X25519 keypair for one remote session. */
export interface Keypair {
  /** Public key, safe to send over the relay. */
  readonly publicKey: Uint8Array
  /** Secret key. Kept private; dropped at end of session (forward secrecy). */
  readonly secretKey: Uint8Array
}

/** Error thrown for any E2E-layer failure. Strict, no `any`. */
export class E2eError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'E2eError'
  }
}

/**
 * Generates a fresh ephemeral keypair. Call {@link ready} first.
 */
export function generateKeypair(): Keypair {
  // Same as libsodium `crypto_box_keypair`: 32 random bytes as the secret,
  // public = X25519(secret, basepoint) (clamping happens inside the ladder).
  const secretKey = randomBytes(PUB_LEN)
  return { publicKey: x25519.getPublicKey(secretKey), secretKey }
}

/**
 * Reconstructs a keypair from raw bytes (used to load test vectors).
 * @throws {E2eError} if either key is not {@link PUB_LEN} bytes.
 */
export function keypairFromBytes(
  publicKey: Uint8Array,
  secretKey: Uint8Array,
): Keypair {
  assertLen(publicKey, PUB_LEN, 'public key')
  assertLen(secretKey, PUB_LEN, 'secret key')
  return { publicKey, secretKey }
}

/**
 * A derived session holding the symmetric key for seal/open.
 * The key is derived once from the ECDH shared secret + `psk`; each message
 * gets a fresh random nonce.
 */
export class Session {
  private readonly key: Uint8Array

  private constructor(key: Uint8Array) {
    this.key = key
  }

  /**
   * Runs the handshake (spec §3) and derives the session key.
   *
   * @param psk pairing pre-shared key (out-of-band via QR)
   * @param myKeypair this side's ephemeral keypair
   * @param theirPublic peer's ephemeral public key received over the relay
   * @throws {E2eError} on bad lengths or a degenerate ECDH result.
   */
  static derive(
    psk: Uint8Array,
    myKeypair: Keypair,
    theirPublic: Uint8Array,
  ): Session {
    if (psk.length === 0) {
      throw new E2eError('psk must not be empty')
    }
    assertLen(theirPublic, PUB_LEN, 'peer public key')

    // dh = X25519(my_secret, their_public)
    // Like libsodium `crypto_scalarmult` (and dryoc → Rust `WeakKex`), reject
    // low-order peer keys / an all-zero shared secret. noble throws on the
    // low-order blocklist; the explicit zero check is belt-and-braces.
    let dh: Uint8Array
    try {
      dh = x25519.getSharedSecret(myKeypair.secretKey, theirPublic)
    } catch {
      throw new E2eError('x25519 shared secret is degenerate')
    }
    if (isAllZero(dh)) {
      throw new E2eError('x25519 shared secret is degenerate')
    }

    // Sort the two public keys so both roles derive the same key.
    const [lo, hi] = sortPair(myKeypair.publicKey, theirPublic)

    // material = CONTEXT || pk_lo || pk_hi || dh
    const material = concat([textBytes(CONTEXT), lo, hi, dh])

    // K = BLAKE2b(key = psk, input = material)
    const key = generichash(KEY_LEN, material, psk)
    return new Session(key)
  }

  /** The derived 32-byte session key (exposed for test-vector checks only). */
  keyBytes(): Uint8Array {
    return this.key
  }

  /** Seals `plaintext` into a base64 `cipher` with a fresh random nonce. */
  seal(plaintext: Uint8Array): string {
    const nonce = randomBytes(NONCE_LEN)
    return this.sealWithNonce(plaintext, nonce)
  }

  /**
   * Seals with a caller-supplied nonce. Deterministic — used by test vectors.
   * @throws {E2eError} if `nonce` is not {@link NONCE_LEN} bytes.
   */
  sealWithNonce(plaintext: Uint8Array, nonce: Uint8Array): string {
    assertLen(nonce, NONCE_LEN, 'nonce')
    const ad = new Uint8Array([E2E_VERSION])
    // Combined mode: ciphertext || 16-byte tag, same as libsodium's
    // `crypto_aead_xchacha20poly1305_ietf_encrypt`.
    const aeadCt = xchacha20poly1305(this.key, nonce, ad).encrypt(plaintext)
    const frame = concat([new Uint8Array([E2E_VERSION]), nonce, aeadCt])
    return toBase64(frame)
  }

  /**
   * Opens a base64 `cipher` back into plaintext bytes.
   * @throws {E2eError} on bad base64, short/invalid frame, unsupported version,
   *   or authentication failure.
   */
  open(cipherB64: string): Uint8Array {
    let frame: Uint8Array
    try {
      frame = fromBase64(cipherB64)
    } catch {
      throw new E2eError('cipher is not valid base64')
    }
    if (frame.length < 1 + NONCE_LEN + TAG_LEN) {
      throw new E2eError('malformed cipher frame')
    }
    const version = frame[0]
    if (version !== E2E_VERSION) {
      throw new E2eError(`unsupported e2e version: ${version}`)
    }
    const nonce = frame.subarray(1, 1 + NONCE_LEN)
    const aeadCt = frame.subarray(1 + NONCE_LEN)
    const ad = new Uint8Array([E2E_VERSION])
    try {
      return xchacha20poly1305(this.key, nonce, ad).decrypt(aeadCt)
    } catch {
      throw new E2eError('aead authentication failed')
    }
  }
}

/**
 * Convenience wrapper mirroring the Rust `Session::new` free-function style.
 */
export function deriveSession(
  psk: Uint8Array,
  myKeypair: Keypair,
  theirPublic: Uint8Array,
): Session {
  return Session.derive(psk, myKeypair, theirPublic)
}

/**
 * Computes the hex host fingerprint from the host's ephemeral public key
 * (spec §3). Controllers compare this against the QR value to authenticate the
 * Host and detect a relay MITM (SEC-006).
 * @throws {E2eError} if `hostPublic` is not {@link PUB_LEN} bytes.
 */
export function hostFingerprint(hostPublic: Uint8Array): string {
  assertLen(hostPublic, PUB_LEN, 'host public key')
  const input = concat([textBytes(FP_CONTEXT), hostPublic])
  const out = generichash(32, input, null)
  return toHex(out)
}

/**
 * Derives the effective pre-shared key fed to {@link deriveSession} from a
 * high-entropy `linkSecretHex` (carried out-of-band in the session link) and a
 * short `secret2` (the OTP typed by the user during pairing, or the durable
 * `sessionSecret` on token reconnect). Mirrors the Rust `derive_effective_psk`
 * byte-for-byte. Because `secret2` is a KDF key (not a compared value), a wrong
 * OTP just yields a different key and every `open` fails — no comparison oracle.
 *
 * The high-entropy `linkSecretHex` (64 hex chars) is the keyed-hash key —
 * BLAKE2b requires a 16..64-byte key, which a short OTP could not satisfy — so
 * the OTP/session-secret goes in the input instead.
 *
 * `K_psk = BLAKE2b-256(key = linkSecretHex bytes, input = OTP_KDF_CONTEXT || secret2 bytes)`
 */
export function deriveEffectivePsk(
  linkSecretHex: string,
  secret2: string,
): Uint8Array {
  const input = concat([textBytes(OTP_KDF_CONTEXT), textBytes(secret2)])
  return generichash(KEY_LEN, input, textBytes(linkSecretHex))
}

// ---- helpers ----

/**
 * libsodium `crypto_generichash` semantics: BLAKE2b with `outLen` output and
 * an optional key, where a null/empty key means unkeyed. Keys longer than 64
 * bytes throw (as libsodium does).
 */
function generichash(
  outLen: number,
  input: Uint8Array,
  key: Uint8Array | null,
): Uint8Array {
  return blake2b(input, {
    dkLen: outLen,
    key: key && key.length > 0 ? key : undefined,
  })
}

function isAllZero(bytes: Uint8Array): boolean {
  let acc = 0
  for (const b of bytes) {
    acc |= b
  }
  return acc === 0
}

function toHex(bytes: Uint8Array): string {
  let out = ''
  for (const b of bytes) {
    out += b.toString(16).padStart(2, '0')
  }
  return out
}

const B64_ALPHABET =
  'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/'
const B64_LOOKUP: Int16Array = (() => {
  const t = new Int16Array(128).fill(-1)
  for (let i = 0; i < B64_ALPHABET.length; i += 1) {
    t[B64_ALPHABET.charCodeAt(i)] = i
  }
  return t
})()

/** Standard (RFC 4648 §4) base64 with `=` padding — libsodium `ORIGINAL`. */
function toBase64(bytes: Uint8Array): string {
  let out = ''
  let i = 0
  for (; i + 2 < bytes.length; i += 3) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8) | bytes[i + 2]
    out +=
      B64_ALPHABET[(n >> 18) & 63] +
      B64_ALPHABET[(n >> 12) & 63] +
      B64_ALPHABET[(n >> 6) & 63] +
      B64_ALPHABET[n & 63]
  }
  const rest = bytes.length - i
  if (rest === 1) {
    const n = bytes[i] << 16
    out += B64_ALPHABET[(n >> 18) & 63] + B64_ALPHABET[(n >> 12) & 63] + '=='
  } else if (rest === 2) {
    const n = (bytes[i] << 16) | (bytes[i + 1] << 8)
    out +=
      B64_ALPHABET[(n >> 18) & 63] +
      B64_ALPHABET[(n >> 12) & 63] +
      B64_ALPHABET[(n >> 6) & 63] +
      '='
  }
  return out
}

/**
 * Strict standard-base64 decode matching libsodium `sodium_base642bin` with
 * `ORIGINAL` and no ignore set (and the Rust `base64` STANDARD engine):
 * padding required, no whitespace / URL-safe chars, and the unused trailing
 * bits of the last symbol must be zero. Throws on any violation.
 */
function fromBase64(s: string): Uint8Array {
  if (s.length % 4 !== 0) {
    throw new Error('bad base64 length')
  }
  let pad = 0
  if (s.endsWith('==')) {
    pad = 2
  } else if (s.endsWith('=')) {
    pad = 1
  }
  const dataLen = s.length - pad
  const out = new Uint8Array((dataLen * 3) >> 2)
  let acc = 0
  let accBits = 0
  let o = 0
  for (let i = 0; i < dataLen; i += 1) {
    const c = s.charCodeAt(i)
    const v = c < 128 ? B64_LOOKUP[c] : -1
    if (v < 0) {
      throw new Error('bad base64 character')
    }
    acc = ((acc << 6) | v) & 0xfff
    accBits += 6
    if (accBits >= 8) {
      accBits -= 8
      out[o] = (acc >> accBits) & 0xff
      o += 1
    }
  }
  if ((acc & ((1 << accBits) - 1)) !== 0) {
    throw new Error('non-canonical base64')
  }
  return out
}

function assertLen(bytes: Uint8Array, expected: number, what: string): void {
  if (bytes.length !== expected) {
    throw new E2eError(
      `bad length for ${what}: expected ${expected}, got ${bytes.length}`,
    )
  }
}

function sortPair(
  a: Uint8Array,
  b: Uint8Array,
): [Uint8Array, Uint8Array] {
  return compareBytes(a, b) <= 0 ? [a, b] : [b, a]
}

/** Lexicographic (unsigned) byte comparison, matching Rust's `[u8]` ordering. */
function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const n = Math.min(a.length, b.length)
  for (let i = 0; i < n; i += 1) {
    if (a[i] !== b[i]) {
      return a[i] < b[i] ? -1 : 1
    }
  }
  return a.length - b.length
}

function concat(parts: readonly Uint8Array[]): Uint8Array {
  let total = 0
  for (const p of parts) {
    total += p.length
  }
  const out = new Uint8Array(total)
  let offset = 0
  for (const p of parts) {
    out.set(p, offset)
    offset += p.length
  }
  return out
}

function textBytes(s: string): Uint8Array {
  return new TextEncoder().encode(s)
}
