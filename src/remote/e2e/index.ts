/**
 * Devdy Remote Control — E2E crypto contract (TS reference impl).
 *
 * Cross-compatible with the Rust reference impl in `remote-e2e/` (crate
 * `dryoc`). Both sides use the same libsodium-compatible primitives, so the
 * same test vector yields the same `cipher` bytes.
 *
 * Spec: `docs/remote-e2e-spec.md` (SEC-006 / CON-04 / AC-19). We NEVER
 * implement crypto primitives by hand — everything routes through
 * `libsodium-wrappers`.
 *
 * Frame layout of the decoded `cipher` (spec §4):
 *   version(1B) | nonce(24B) | aead_ct    (version byte bound as AEAD AD)
 */
import sodium from 'libsodium-wrappers'

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

/** Ensures libsodium's WASM/asm runtime is initialized before any call. */
export async function ready(): Promise<void> {
  await sodium.ready
}

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
  const kp = sodium.crypto_box_keypair()
  return { publicKey: kp.publicKey, secretKey: kp.privateKey }
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
    let dh: Uint8Array
    try {
      dh = sodium.crypto_scalarmult(myKeypair.secretKey, theirPublic)
    } catch {
      throw new E2eError('x25519 shared secret is degenerate')
    }

    // Sort the two public keys so both roles derive the same key.
    const [lo, hi] = sortPair(myKeypair.publicKey, theirPublic)

    // material = CONTEXT || pk_lo || pk_hi || dh
    const material = concat([textBytes(CONTEXT), lo, hi, dh])

    // K = BLAKE2b(key = psk, input = material)
    const key = sodium.crypto_generichash(KEY_LEN, material, psk)
    return new Session(key)
  }

  /** The derived 32-byte session key (exposed for test-vector checks only). */
  keyBytes(): Uint8Array {
    return this.key
  }

  /** Seals `plaintext` into a base64 `cipher` with a fresh random nonce. */
  seal(plaintext: Uint8Array): string {
    const nonce = sodium.randombytes_buf(NONCE_LEN)
    return this.sealWithNonce(plaintext, nonce)
  }

  /**
   * Seals with a caller-supplied nonce. Deterministic — used by test vectors.
   * @throws {E2eError} if `nonce` is not {@link NONCE_LEN} bytes.
   */
  sealWithNonce(plaintext: Uint8Array, nonce: Uint8Array): string {
    assertLen(nonce, NONCE_LEN, 'nonce')
    const ad = new Uint8Array([E2E_VERSION])
    const aeadCt = sodium.crypto_aead_xchacha20poly1305_ietf_encrypt(
      plaintext,
      ad,
      null,
      nonce,
      this.key,
    )
    const frame = concat([new Uint8Array([E2E_VERSION]), nonce, aeadCt])
    return sodium.to_base64(frame, sodium.base64_variants.ORIGINAL)
  }

  /**
   * Opens a base64 `cipher` back into plaintext bytes.
   * @throws {E2eError} on bad base64, short/invalid frame, unsupported version,
   *   or authentication failure.
   */
  open(cipherB64: string): Uint8Array {
    let frame: Uint8Array
    try {
      frame = sodium.from_base64(cipherB64, sodium.base64_variants.ORIGINAL)
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
      return sodium.crypto_aead_xchacha20poly1305_ietf_decrypt(
        null,
        aeadCt,
        ad,
        nonce,
        this.key,
      )
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
  const out = sodium.crypto_generichash(32, input, null)
  return sodium.to_hex(out)
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
  return sodium.crypto_generichash(KEY_LEN, input, textBytes(linkSecretHex))
}

// ---- helpers ----

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
