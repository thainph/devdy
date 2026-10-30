//! Group crypto for sync (design §9.2, on-demand-sync.md §1).
//!
//! Every device of a sync group holds the same 32-byte group key. The AEAD
//! key `enc` is derived from it with keyed BLAKE2b (domain-separated) and
//! seals every peer message the relay forwards: `nonce(24) ||
//! XChaCha20-Poly1305(enc, nonce, plaintext, aad = "devdy-sync-v1")`, base64
//! on the wire. (The relay store's per-row op keys are gone with the store.)
//!
//! Primitives come from `dryoc` (libsodium-compatible), like `remote-e2e`.

use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt,
};
use dryoc::classic::crypto_generichash::crypto_generichash;
use dryoc::rng::copy_randombytes;

/// AEAD associated data bound into every sealed message.
pub const AAD: &[u8] = b"devdy-sync-v1";
pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const ENC_CONTEXT: &[u8] = b"devdy-sync-v1/enc";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    BadKey,
    /// Too short, or authentication failed (tampered / wrong key).
    Open,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::BadKey => "invalid group key",
            CryptoError::Open => "sealed payload failed authentication",
        })
    }
}

impl std::error::Error for CryptoError {}

/// A fresh random 32-byte group key.
pub fn generate_group_key() -> [u8; KEY_LEN] {
    let mut k = [0u8; KEY_LEN];
    copy_randombytes(&mut k);
    k
}

/// Sub-key derived from the group key. Never logged (no `Debug`).
#[derive(Clone)]
pub struct SyncKeys {
    enc: [u8; KEY_LEN],
}

impl SyncKeys {
    pub fn derive(group_key: &[u8]) -> Result<Self, CryptoError> {
        if group_key.len() != KEY_LEN {
            return Err(CryptoError::BadKey);
        }
        let sub = |ctx: &[u8]| -> Result<[u8; KEY_LEN], CryptoError> {
            let mut out = [0u8; KEY_LEN];
            crypto_generichash(&mut out, ctx, Some(group_key)).map_err(|_| CryptoError::BadKey)?;
            Ok(out)
        };
        Ok(SyncKeys { enc: sub(ENC_CONTEXT)? })
    }

    /// `nonce || ciphertext` with a fresh random nonce.
    pub fn seal_bytes(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        copy_randombytes(&mut nonce);
        self.seal_with_nonce(plaintext, &nonce)
    }

    fn seal_with_nonce(&self, plaintext: &[u8], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
        self.seal_aad(plaintext, nonce, AAD)
    }

    fn seal_aad(&self, plaintext: &[u8], nonce: &[u8; NONCE_LEN], aad: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; NONCE_LEN + plaintext.len() + TAG_LEN];
        out[..NONCE_LEN].copy_from_slice(nonce);
        // Only fails on buffer-size mismatches, which the allocation above rules out.
        let _ = crypto_aead_xchacha20poly1305_ietf_encrypt(
            &mut out[NONCE_LEN..],
            plaintext,
            Some(aad),
            nonce,
            &self.enc,
        );
        out
    }

    pub fn open_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        self.open_aad(sealed, AAD)
    }

    fn open_aad(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::Open);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&sealed[..NONCE_LEN]);
        let ct = &sealed[NONCE_LEN..];
        let mut pt = vec![0u8; ct.len() - TAG_LEN];
        crypto_aead_xchacha20poly1305_ietf_decrypt(&mut pt, ct, Some(aad), &nonce, &self.enc)
            .map_err(|_| CryptoError::Open)?;
        Ok(pt)
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> SyncKeys {
        SyncKeys::derive(&[7u8; KEY_LEN]).unwrap()
    }

    #[test]
    fn subkey_is_pinned() {
        let k = keys();
        assert_ne!(k.enc, [7u8; KEY_LEN]);
        // Regression vector: changing the KDF would split the group's devices.
        assert_eq!(hex::encode(k.enc), ENC_VECTOR);
    }
    const ENC_VECTOR: &str = "d21a22a755edef8eea0c7a410a55759b4e695e9eff4463a4f0e6b22fb75a26d4";

    #[test]
    fn seal_is_pinned_for_a_fixed_nonce() {
        let sealed = keys().seal_with_nonce(b"devdy", &[1u8; NONCE_LEN]);
        assert_eq!(&sealed[..NONCE_LEN], &[1u8; NONCE_LEN]);
        assert_eq!(hex::encode(&sealed[NONCE_LEN..]), SEAL_VECTOR);
    }
    const SEAL_VECTOR: &str = "7207afd1f22126487959f3cc859e5c0dc067a18b54";

    #[test]
    fn rejects_bad_key_length() {
        assert!(SyncKeys::derive(&[0u8; 16]).is_err());
    }

    #[test]
    fn tamper_is_detected() {
        let k = keys();
        let sealed = k.seal_bytes(b"secret row");
        for i in [0, NONCE_LEN, sealed.len() - 1] {
            let mut bad = sealed.clone();
            bad[i] ^= 0x01;
            assert_eq!(k.open_bytes(&bad), Err(CryptoError::Open), "flip at {i}");
        }
        assert_eq!(k.open_bytes(&sealed[..sealed.len() - 1]), Err(CryptoError::Open));
        assert_eq!(k.open_bytes(&sealed[..10]), Err(CryptoError::Open));
    }

    #[test]
    fn wrong_key_or_aad_fails() {
        let sealed = keys().seal_bytes(b"row");
        let other = SyncKeys::derive(&[8u8; KEY_LEN]).unwrap();
        assert_eq!(other.open_bytes(&sealed), Err(CryptoError::Open));

        // Same key, different AAD → must not open.
        let k = keys();
        let nonce = [3u8; NONCE_LEN];
        let mut ct = vec![0u8; 3 + TAG_LEN];
        crypto_aead_xchacha20poly1305_ietf_encrypt(&mut ct, b"row", Some(b"other-aad"), &nonce, &k.enc)
            .unwrap();
        let mut framed = nonce.to_vec();
        framed.extend_from_slice(&ct);
        assert_eq!(k.open_bytes(&framed), Err(CryptoError::Open));
    }
}
