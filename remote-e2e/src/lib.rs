//! Devdy Remote Control — E2E crypto contract (spec: `docs/remote-e2e-spec.md`).
//!
//! Provides the shared, cross-compatible (Rust ↔ TS) encryption layer between a
//! Devdy Host and a Controller. The relay only ever sees the base64 `cipher`
//! produced here (SRS SEC-006 / CON-04 / AC-19). We NEVER implement crypto
//! primitives by hand — everything routes through `dryoc` (libsodium-compatible):
//!
//! - KEX:  X25519 ECDH, ephemeral per session (forward secrecy).
//! - KDF:  BLAKE2b keyed with the pairing `psk` (defeats a relay MITM).
//! - AEAD: XChaCha20-Poly1305-IETF, 24-byte nonce, never reused.
//!
//! Frame layout of the decoded `cipher` (see spec §4):
//! `version(1B) | nonce(24B) | aead_ct`, with the version byte bound as AEAD
//! associated data.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt,
};
use dryoc::classic::crypto_box::crypto_box_keypair;
use dryoc::classic::crypto_core::crypto_scalarmult;
use dryoc::classic::crypto_generichash::crypto_generichash;
use dryoc::rng::copy_randombytes;

/// Wire format version carried as the first byte of every frame (spec §2).
pub const E2E_VERSION: u8 = 0x01;
/// Domain-separation label mixed into the session-key KDF (spec §3).
pub const CONTEXT: &[u8] = b"devdy-remote-e2e/v1";
/// Domain-separation label for the host fingerprint (spec §3).
pub const FP_CONTEXT: &[u8] = b"devdy-host-fp/v1";
/// Domain-separation label for the two-layer (link_secret + OTP) psk KDF.
pub const OTP_KDF_CONTEXT: &[u8] = b"devdy-remote-otp/v1";

/// X25519 / AEAD key length.
pub const KEY_LEN: usize = 32;
/// XChaCha20-Poly1305 nonce length.
pub const NONCE_LEN: usize = 24;
/// Poly1305 tag length.
pub const TAG_LEN: usize = 16;
/// X25519 public/secret key length.
pub const PUB_LEN: usize = 32;

/// Errors surfaced by the E2E layer. No panics, no `unwrap` in library code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum E2eError {
    /// A key/psk/nonce was given with the wrong byte length.
    BadLength {
        /// Which field was wrong.
        what: &'static str,
        /// Expected byte length.
        expected: usize,
        /// Actual byte length received.
        got: usize,
    },
    /// The decoded frame was too short or structurally invalid.
    BadFrame,
    /// The frame carried a version this build does not understand.
    UnsupportedVersion(u8),
    /// The `cipher` string was not valid base64.
    Base64,
    /// X25519 produced an all-zero shared secret (low-order point) — rejected.
    WeakKex,
    /// AEAD decryption / authentication failed (tampered or wrong key).
    Decrypt,
}

impl core::fmt::Display for E2eError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            E2eError::BadLength {
                what,
                expected,
                got,
            } => write!(f, "bad length for {what}: expected {expected}, got {got}"),
            E2eError::BadFrame => write!(f, "malformed cipher frame"),
            E2eError::UnsupportedVersion(v) => write!(f, "unsupported e2e version: {v}"),
            E2eError::Base64 => write!(f, "cipher is not valid base64"),
            E2eError::WeakKex => write!(f, "x25519 shared secret is degenerate"),
            E2eError::Decrypt => write!(f, "aead authentication failed"),
        }
    }
}

impl std::error::Error for E2eError {}

/// An ephemeral X25519 keypair for one remote session.
#[derive(Clone)]
pub struct Keypair {
    /// Public key, safe to send over the relay.
    pub public: [u8; PUB_LEN],
    /// Secret key. Kept private; dropped at end of session (forward secrecy).
    pub secret: [u8; PUB_LEN],
}

impl Keypair {
    /// Generates a fresh ephemeral keypair using the OS RNG.
    #[must_use]
    pub fn generate() -> Self {
        let (public, secret) = crypto_box_keypair();
        Keypair { public, secret }
    }

    /// Reconstructs a keypair from raw bytes (used to load test vectors).
    ///
    /// # Errors
    /// Returns [`E2eError::BadLength`] if either slice is not [`PUB_LEN`] bytes.
    pub fn from_bytes(public: &[u8], secret: &[u8]) -> Result<Self, E2eError> {
        Ok(Keypair {
            public: to_array::<PUB_LEN>(public, "public key")?,
            secret: to_array::<PUB_LEN>(secret, "secret key")?,
        })
    }
}

/// A derived session, holding the symmetric key for seal/open.
///
/// The key is derived once from the ECDH shared secret + `psk`; individual
/// messages get a fresh random nonce.
pub struct Session {
    key: [u8; KEY_LEN],
}

impl Session {
    /// Runs the handshake (spec §3) and derives the session key.
    ///
    /// `psk` is the pairing pre-shared key (out-of-band via QR); `my_keypair`
    /// is this side's ephemeral keypair; `their_public` is the peer's ephemeral
    /// public key received over the relay.
    ///
    /// # Errors
    /// - [`E2eError::BadLength`] if `psk` or `their_public` has the wrong size.
    /// - [`E2eError::WeakKex`] if the peer sent a low-order public key.
    pub fn new(psk: &[u8], my_keypair: &Keypair, their_public: &[u8]) -> Result<Self, E2eError> {
        if psk.is_empty() {
            return Err(E2eError::BadLength {
                what: "psk",
                expected: 1,
                got: 0,
            });
        }
        let their_public = to_array::<PUB_LEN>(their_public, "peer public key")?;

        // dh = X25519(my_secret, their_public)
        let mut dh = [0u8; PUB_LEN];
        crypto_scalarmult(&mut dh, &my_keypair.secret, &their_public)
            .map_err(|_| E2eError::WeakKex)?;

        // Sort the two public keys so both roles derive the same key.
        let (lo, hi) = sort_pair(&my_keypair.public, &their_public);

        // material = CONTEXT || pk_lo || pk_hi || dh
        let mut material = Vec::with_capacity(CONTEXT.len() + PUB_LEN * 3);
        material.extend_from_slice(CONTEXT);
        material.extend_from_slice(&lo);
        material.extend_from_slice(&hi);
        material.extend_from_slice(&dh);

        // K = BLAKE2b(key = psk, input = material)
        let mut key = [0u8; KEY_LEN];
        crypto_generichash(&mut key, &material, Some(psk)).map_err(|_| E2eError::BadFrame)?;

        Ok(Session { key })
    }

    /// The derived 32-byte session key (exposed for test-vector checks only).
    #[must_use]
    pub fn key_bytes(&self) -> [u8; KEY_LEN] {
        self.key
    }

    /// Seals `plaintext` into a base64 `cipher` with a fresh random nonce.
    #[must_use]
    pub fn seal(&self, plaintext: &[u8]) -> String {
        let mut nonce = [0u8; NONCE_LEN];
        copy_randombytes(&mut nonce);
        self.seal_with_nonce(plaintext, &nonce)
            .unwrap_or_default()
    }

    /// Seals with a caller-supplied nonce. Deterministic — used by test vectors.
    ///
    /// # Errors
    /// [`E2eError::BadLength`] if `nonce` is not [`NONCE_LEN`] bytes.
    pub fn seal_with_nonce(&self, plaintext: &[u8], nonce: &[u8]) -> Result<String, E2eError> {
        let nonce = to_array::<NONCE_LEN>(nonce, "nonce")?;
        let ad = [E2E_VERSION];

        let mut aead_ct = vec![0u8; plaintext.len() + TAG_LEN];
        crypto_aead_xchacha20poly1305_ietf_encrypt(
            &mut aead_ct,
            plaintext,
            Some(&ad),
            &nonce,
            &self.key,
        )
        .map_err(|_| E2eError::BadFrame)?;

        let mut frame = Vec::with_capacity(1 + NONCE_LEN + aead_ct.len());
        frame.push(E2E_VERSION);
        frame.extend_from_slice(&nonce);
        frame.extend_from_slice(&aead_ct);
        Ok(B64.encode(frame))
    }

    /// Opens a base64 `cipher` back into plaintext bytes.
    ///
    /// # Errors
    /// - [`E2eError::Base64`] if not valid base64.
    /// - [`E2eError::BadFrame`] if the frame is too short.
    /// - [`E2eError::UnsupportedVersion`] if the version byte is unknown.
    /// - [`E2eError::Decrypt`] if authentication fails.
    pub fn open(&self, cipher_b64: &str) -> Result<Vec<u8>, E2eError> {
        let frame = B64.decode(cipher_b64).map_err(|_| E2eError::Base64)?;
        if frame.len() < 1 + NONCE_LEN + TAG_LEN {
            return Err(E2eError::BadFrame);
        }
        let version = frame[0];
        if version != E2E_VERSION {
            return Err(E2eError::UnsupportedVersion(version));
        }
        let nonce = to_array::<NONCE_LEN>(&frame[1..1 + NONCE_LEN], "nonce")?;
        let aead_ct = &frame[1 + NONCE_LEN..];
        let ad = [E2E_VERSION];

        let mut plaintext = vec![0u8; aead_ct.len() - TAG_LEN];
        crypto_aead_xchacha20poly1305_ietf_decrypt(
            &mut plaintext,
            aead_ct,
            Some(&ad),
            &nonce,
            &self.key,
        )
        .map_err(|_| E2eError::Decrypt)?;
        Ok(plaintext)
    }
}

/// Computes the hex host fingerprint from the host's ephemeral public key
/// (spec §3). Controllers compare this against the value read from the QR to
/// authenticate the Host and detect a relay MITM (SEC-006).
///
/// # Errors
/// [`E2eError::BadLength`] if `host_public` is not [`PUB_LEN`] bytes.
pub fn host_fingerprint(host_public: &[u8]) -> Result<String, E2eError> {
    let host_public = to_array::<PUB_LEN>(host_public, "host public key")?;
    let mut input = Vec::with_capacity(FP_CONTEXT.len() + PUB_LEN);
    input.extend_from_slice(FP_CONTEXT);
    input.extend_from_slice(&host_public);
    let mut out = [0u8; 32];
    crypto_generichash(&mut out, &input, None).map_err(|_| E2eError::BadFrame)?;
    Ok(hex_encode(&out))
}

/// Derives the effective pre-shared key fed to [`Session::new`] from a
/// high-entropy `link_secret_hex` (carried out-of-band in the session link) and
/// a short `secret2` (the OTP shown on the Host during pairing, or the durable
/// `session_secret` on token reconnect). Both peers MUST feed byte-identical
/// inputs, so the OTP/session-secret is a KDF key rather than a compared value:
/// a wrong OTP simply yields a different key and every `open` fails (no oracle).
///
/// `K_psk = BLAKE2b-256(key = link_secret_hex bytes, input = OTP_KDF_CONTEXT || secret2 bytes)`
///
/// The high-entropy `link_secret_hex` (64 hex chars) is the keyed-hash key —
/// BLAKE2b requires a key of 16..=64 bytes, which a short OTP could not satisfy,
/// so the OTP/session-secret goes in the input instead. Returns a fixed 32-byte
/// key (always non-empty, always ≤ BLAKE2b's key ceiling) that plugs straight
/// into [`Session::new`] as the `psk`.
#[must_use]
pub fn derive_effective_psk(link_secret_hex: &str, secret2: &str) -> [u8; KEY_LEN] {
    let mut input = Vec::with_capacity(OTP_KDF_CONTEXT.len() + secret2.len());
    input.extend_from_slice(OTP_KDF_CONTEXT);
    input.extend_from_slice(secret2.as_bytes());
    let mut out = [0u8; KEY_LEN];
    // link_secret_hex (64 bytes) is the keyed-hash key. On the (unreachable)
    // error path `out` stays zeroed, which just makes the derived session key
    // wrong → open fails safely.
    let _ = crypto_generichash(&mut out, &input, Some(link_secret_hex.as_bytes()));
    out
}

// ---- helpers ----

/// Converts a slice into a fixed array, erroring on length mismatch.
fn to_array<const N: usize>(slice: &[u8], what: &'static str) -> Result<[u8; N], E2eError> {
    slice.try_into().map_err(|_| E2eError::BadLength {
        what,
        expected: N,
        got: slice.len(),
    })
}

/// Lexicographic sort of two public keys → deterministic KDF input.
fn sort_pair(a: &[u8; PUB_LEN], b: &[u8; PUB_LEN]) -> ([u8; PUB_LEN], [u8; PUB_LEN]) {
    if a <= b {
        (*a, *b)
    } else {
        (*b, *a)
    }
}

/// Lowercase hex encoding (no external dep).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

/// Decodes a lowercase/uppercase hex string into bytes (for loading vectors).
///
/// # Errors
/// Returns [`E2eError::BadFrame`] on odd length or non-hex characters.
pub fn hex_decode(s: &str) -> Result<Vec<u8>, E2eError> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err(E2eError::BadFrame);
    }
    let mut out = Vec::with_capacity(bytes.len() / 2);
    for pair in bytes.chunks_exact(2) {
        let hi = hex_val(pair[0])?;
        let lo = hex_val(pair[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn hex_val(c: u8) -> Result<u8, E2eError> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(E2eError::BadFrame),
    }
}
