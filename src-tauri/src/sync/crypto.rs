//! Group crypto for sync (design §9.1 / §9.2).
//!
//! Every device of a sync group holds the same 32-byte group key. Two sub-keys
//! are derived from it with keyed BLAKE2b (domain-separated), so the AEAD key
//! and the op-key MAC key are never the same bytes:
//!
//! - `enc` seals ops and blobs: `nonce(24) || XChaCha20-Poly1305(enc, nonce,
//!   plaintext, aad = "devdy-sync-v1")`, base64 on the wire for ops.
//! - `mac` computes `op_key(tbl, pk)`, the opaque per-row key the relay uses to
//!   supersede older ops without seeing content.
//!
//! Primitives come from `dryoc` (libsodium-compatible), like `remote-e2e`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt,
};
use dryoc::classic::crypto_generichash::crypto_generichash;
use dryoc::rng::copy_randombytes;
use sha2::{Digest, Sha256};

use super::op::Op;

/// AEAD associated data bound into every sealed op and blob.
pub const AAD: &[u8] = b"devdy-sync-v1";
pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const ENC_CONTEXT: &[u8] = b"devdy-sync-v1/enc";
const MAC_CONTEXT: &[u8] = b"devdy-sync-v1/mac";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CryptoError {
    BadKey,
    Base64,
    /// Too short, or authentication failed (tampered / wrong key).
    Open,
    Json,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::BadKey => "invalid group key",
            CryptoError::Base64 => "sealed payload is not valid base64",
            CryptoError::Open => "sealed payload failed authentication",
            CryptoError::Json => "sealed payload is not a valid op",
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

/// Sub-keys derived from the group key. Never logged (no `Debug`).
#[derive(Clone)]
pub struct SyncKeys {
    enc: [u8; KEY_LEN],
    mac: [u8; KEY_LEN],
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
        Ok(SyncKeys {
            enc: sub(ENC_CONTEXT)?,
            mac: sub(MAC_CONTEXT)?,
        })
    }

    /// `nonce || ciphertext` with a fresh random nonce.
    pub fn seal_bytes(&self, plaintext: &[u8]) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        copy_randombytes(&mut nonce);
        self.seal_with_nonce(plaintext, &nonce)
    }

    fn seal_with_nonce(&self, plaintext: &[u8], nonce: &[u8; NONCE_LEN]) -> Vec<u8> {
        let mut out = vec![0u8; NONCE_LEN + plaintext.len() + TAG_LEN];
        out[..NONCE_LEN].copy_from_slice(nonce);
        // Only fails on buffer-size mismatches, which the allocation above rules out.
        let _ = crypto_aead_xchacha20poly1305_ietf_encrypt(
            &mut out[NONCE_LEN..],
            plaintext,
            Some(AAD),
            nonce,
            &self.enc,
        );
        out
    }

    pub fn open_bytes(&self, sealed: &[u8]) -> Result<Vec<u8>, CryptoError> {
        if sealed.len() < NONCE_LEN + TAG_LEN {
            return Err(CryptoError::Open);
        }
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&sealed[..NONCE_LEN]);
        let ct = &sealed[NONCE_LEN..];
        let mut pt = vec![0u8; ct.len() - TAG_LEN];
        crypto_aead_xchacha20poly1305_ietf_decrypt(&mut pt, ct, Some(AAD), &nonce, &self.enc)
            .map_err(|_| CryptoError::Open)?;
        Ok(pt)
    }

    /// The wire `blob` for an op: base64(nonce || seal(json(op))).
    pub fn seal_op(&self, op: &Op) -> Result<String, CryptoError> {
        let json = serde_json::to_vec(op).map_err(|_| CryptoError::Json)?;
        Ok(B64.encode(self.seal_bytes(&json)))
    }

    pub fn open_op(&self, blob_b64: &str) -> Result<Op, CryptoError> {
        let sealed = B64.decode(blob_b64).map_err(|_| CryptoError::Base64)?;
        let pt = self.open_bytes(&sealed)?;
        serde_json::from_slice(&pt).map_err(|_| CryptoError::Json)
    }

    /// Opaque row key: `hex(blake2b(key = mac, tbl || 0x00 || canonical_json(pk)))`.
    pub fn op_key(&self, tbl: &str, pk: &serde_json::Value) -> String {
        let mut input = Vec::with_capacity(tbl.len() + 64);
        input.extend_from_slice(tbl.as_bytes());
        input.push(0);
        input.extend_from_slice(canonical_json(pk).as_bytes());
        let mut out = [0u8; 32];
        let _ = crypto_generichash(&mut out, &input, Some(&self.mac));
        hex::encode(out)
    }
}

/// Content address of a sealed blob (§9.2: sha256 of the sealed bytes).
#[allow(dead_code)] // file sync (wave 2)
pub fn blob_sha(sealed: &[u8]) -> String {
    hex::encode(Sha256::digest(sealed))
}

/// JSON with object keys sorted at every level and no whitespace, so the same
/// value always hashes the same regardless of map ordering.
pub fn canonical_json(v: &serde_json::Value) -> String {
    let mut s = String::new();
    write_canonical(v, &mut s);
    s
}

fn write_canonical(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push(':');
                write_canonical(&map[k], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::op::OpKind;
    use serde_json::json;

    fn keys() -> SyncKeys {
        SyncKeys::derive(&[7u8; KEY_LEN]).unwrap()
    }

    fn sample_op() -> Op {
        Op {
            v: 1,
            schema: 55,
            hlc: serde_json::from_value(json!("0192a3b4c5d60001-dev-a")).unwrap(),
            tbl: "notes".into(),
            pk: json!("n-1"),
            kind: OpKind::Upsert,
            row: json!({ "id": "n-1", "title": "hello" }).as_object().cloned(),
        }
    }

    #[test]
    fn subkeys_are_pinned_and_distinct() {
        let k = keys();
        assert_ne!(k.enc, k.mac);
        assert_ne!(k.enc, [7u8; KEY_LEN]);
        // Regression vector: changing the KDF would orphan every stored op.
        assert_eq!(hex::encode(k.enc), ENC_VECTOR);
        assert_eq!(hex::encode(k.mac), MAC_VECTOR);
    }
    const ENC_VECTOR: &str = "d21a22a755edef8eea0c7a410a55759b4e695e9eff4463a4f0e6b22fb75a26d4";
    const MAC_VECTOR: &str = "5f3627b0ccb1968ecde58343c5a491c98eaaa7fe833f8f8e63a4baa9832d839b";

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
    fn op_roundtrips() {
        let k = keys();
        let op = sample_op();
        let blob = k.seal_op(&op).unwrap();
        let back = k.open_op(&blob).unwrap();
        assert_eq!(serde_json::to_value(&back).unwrap(), serde_json::to_value(&op).unwrap());
        // Fresh nonce each time.
        assert_ne!(blob, k.seal_op(&op).unwrap());
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
        assert_eq!(k.open_op("not base64!").err(), Some(CryptoError::Base64));
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

    #[test]
    fn non_op_plaintext_is_rejected() {
        let k = keys();
        let blob = B64.encode(k.seal_bytes(b"{\"nope\":1}"));
        assert_eq!(k.open_op(&blob).err(), Some(CryptoError::Json));
    }

    #[test]
    fn canonical_json_sorts_keys_recursively() {
        let a = json!({ "b": 1, "a": { "z": [ { "y": 2, "x": 1 } ], "c": "s" } });
        assert_eq!(canonical_json(&a), r#"{"a":{"c":"s","z":[{"x":1,"y":2}]},"b":1}"#);
    }

    #[test]
    fn op_key_is_stable_and_order_independent() {
        let k = keys();
        let pk1: serde_json::Value = serde_json::from_str(r#"{"project_id":"p","note_id":"n"}"#).unwrap();
        let pk2: serde_json::Value = serde_json::from_str(r#"{"note_id":"n","project_id":"p"}"#).unwrap();
        assert_eq!(k.op_key("note_links", &pk1), k.op_key("note_links", &pk2));
        assert_eq!(k.op_key("notes", &json!("n-1")).len(), 64);
        assert_ne!(k.op_key("notes", &json!("n-1")), k.op_key("todos", &json!("n-1")));
        assert_ne!(k.op_key("notes", &json!("n-1")), k.op_key("notes", &json!("n-2")));
        // The tbl/pk separator keeps ("ab","c") and ("a","bc") apart.
        assert_ne!(k.op_key("ab", &json!("c")), k.op_key("a", &json!("bc")));
        // Keyed: another group computes different keys for the same row.
        let other = SyncKeys::derive(&[9u8; KEY_LEN]).unwrap();
        assert_ne!(k.op_key("notes", &json!("n-1")), other.op_key("notes", &json!("n-1")));
        assert_eq!(k.op_key("notes", &json!("n-1")), OP_KEY_VECTOR);
    }
    const OP_KEY_VECTOR: &str = "4d11de4a125b2a75a851c77f0ad6685a4e01de291161164fdf3197b09e2b47ab";

    #[test]
    fn blob_sha_is_sha256_hex() {
        assert_eq!(
            blob_sha(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
