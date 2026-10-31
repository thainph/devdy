//! Web Push message crypto: RFC 8291 payload encryption (`aes128gcm`, RFC 8188)
//! and RFC 8292 VAPID authorization (an ES256 JWT).
//!
//! Pure functions over byte slices — no I/O, no key storage — so the RFC test
//! vectors can drive them directly.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use p256::{PublicKey, SecretKey};
use rand::RngCore;
use sha2::Sha256;

/// Record size advertised in the `aes128gcm` header. One record holds the
/// whole (tiny) payload.
const RECORD_SIZE: u32 = 4096;

/// How long a VAPID JWT is valid. RFC 8292 caps it at 24h, but some push
/// services are stricter; a fresh token is signed for every send anyway.
pub const VAPID_TTL_SECS: u64 = 3600;

/// base64url without padding (what every Web Push field uses).
pub fn b64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decode base64url, tolerating `=` padding and the standard alphabet some
/// browsers hand out.
pub fn b64url_decode(s: &str) -> Option<Vec<u8>> {
    let cleaned: String = s
        .trim()
        .trim_end_matches('=')
        .chars()
        .map(|c| match c {
            '+' => '-',
            '/' => '_',
            c => c,
        })
        .collect();
    URL_SAFE_NO_PAD.decode(cleaned).ok()
}

/// A fresh P-256 private key (the VAPID key, or an ephemeral sender key).
pub fn generate_secret_key() -> SecretKey {
    let mut rng = rand::rngs::OsRng;
    loop {
        let mut bytes = [0u8; 32];
        rng.fill_bytes(&mut bytes);
        // Out-of-range scalars (zero / ≥ n) are astronomically rare; retry.
        if let Ok(k) = SecretKey::from_slice(&bytes) {
            return k;
        }
    }
}

/// The uncompressed SEC1 encoding (65 bytes, leading 0x04) of a public key.
pub fn public_bytes(key: &PublicKey) -> Vec<u8> {
    key.to_encoded_point(false).as_bytes().to_vec()
}

/// Parse a subscription's `p256dh` (uncompressed point, base64url).
pub fn parse_public_key(b64: &str) -> Option<PublicKey> {
    let bytes = b64url_decode(b64)?;
    if bytes.len() != 65 || bytes[0] != 0x04 {
        return None;
    }
    PublicKey::from_sec1_bytes(&bytes).ok()
}

/// Parse a subscription's `auth` secret (16 bytes, base64url).
pub fn parse_auth_secret(b64: &str) -> Option<[u8; 16]> {
    b64url_decode(b64)?.try_into().ok()
}

/// Encrypt `plaintext` for a subscription (RFC 8291 §3.4) with a fresh
/// ephemeral key and salt. Returns the full `aes128gcm` request body.
pub fn encrypt(ua_public: &PublicKey, auth_secret: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let as_secret = generate_secret_key();
    let mut salt = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    encrypt_with(&as_secret, &salt, ua_public, auth_secret, plaintext)
}

/// [`encrypt`] with the sender key and salt given (the RFC test vector pins both).
pub fn encrypt_with(
    as_secret: &SecretKey,
    salt: &[u8; 16],
    ua_public: &PublicKey,
    auth_secret: &[u8; 16],
    plaintext: &[u8],
) -> Result<Vec<u8>, String> {
    let as_public = public_bytes(&as_secret.public_key());
    let ua_public_bytes = public_bytes(ua_public);
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua_public.as_affine());

    // IKM = HKDF(auth_secret, ecdh_secret, "WebPush: info" 0x00 ua_public as_public, 32)
    let mut key_info = Vec::with_capacity(14 + 65 + 65);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(&ua_public_bytes);
    key_info.extend_from_slice(&as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth_secret), shared.raw_secret_bytes())
        .expand(&key_info, &mut ikm)
        .map_err(|e| e.to_string())?;

    // RFC 8188: CEK and nonce from (salt, IKM).
    let hk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    hk.expand(b"Content-Encoding: aes128gcm\0", &mut cek)
        .map_err(|e| e.to_string())?;
    let mut nonce = [0u8; 12];
    hk.expand(b"Content-Encoding: nonce\0", &mut nonce)
        .map_err(|e| e.to_string())?;

    // A single, last record: plaintext + 0x02 delimiter, no padding.
    let mut record = Vec::with_capacity(plaintext.len() + 1);
    record.extend_from_slice(plaintext);
    record.push(0x02);
    if record.len() + 16 > RECORD_SIZE as usize {
        return Err("push payload too large".into());
    }
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|e| e.to_string())?;
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), record.as_slice())
        .map_err(|e| e.to_string())?;

    // Header: salt(16) | rs(4, BE) | idlen(1) | keyid (the sender public key).
    let mut body = Vec::with_capacity(16 + 4 + 1 + 65 + sealed.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(&as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// The `Authorization` header value for a push to `audience` (the endpoint's
/// origin): `vapid t=<jwt>, k=<public key>` (RFC 8292 §3).
pub fn vapid_authorization(key: &SecretKey, audience: &str, subject: &str, now_unix: u64) -> String {
    let jwt = vapid_jwt(key, audience, subject, now_unix + VAPID_TTL_SECS);
    let k = b64url(&public_bytes(&key.public_key()));
    format!("vapid t={jwt}, k={k}")
}

/// An ES256 JWT `{aud, exp, sub}` signed with the VAPID key.
pub fn vapid_jwt(key: &SecretKey, audience: &str, subject: &str, exp: u64) -> String {
    let header = b64url(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({ "aud": audience, "exp": exp, "sub": subject });
    let claims = b64url(claims.to_string().as_bytes());
    let signing_input = format!("{header}.{claims}");
    let signature: Signature = SigningKey::from(key).sign(signing_input.as_bytes());
    // JWS ES256 wants the raw r || s (64 bytes), not DER.
    format!("{signing_input}.{}", b64url(&signature.to_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use p256::ecdsa::signature::Verifier;
    use p256::ecdsa::VerifyingKey;

    fn secret(b64: &str) -> SecretKey {
        SecretKey::from_slice(&b64url_decode(b64).unwrap()).unwrap()
    }

    /// RFC 8291 Appendix A, byte for byte.
    #[test]
    fn rfc8291_test_vector() {
        let as_secret = secret("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw");
        assert_eq!(
            b64url(&public_bytes(&as_secret.public_key())),
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        let ua_public =
            parse_public_key("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4")
                .unwrap();
        let auth = parse_auth_secret("BTBZMqHH6r4Tts7J_aSIgg").unwrap();
        let salt: [u8; 16] = b64url_decode("DGv6ra1nlYgDCS1FRnbzlw").unwrap().try_into().unwrap();
        let body = encrypt_with(
            &as_secret,
            &salt,
            &ua_public,
            &auth,
            b"When I grow up, I want to be a watermelon",
        )
        .unwrap();
        assert_eq!(
            b64url(&body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn fresh_encryptions_differ_but_share_the_header_shape() {
        let ua = generate_secret_key().public_key();
        let auth = [7u8; 16];
        let a = encrypt(&ua, &auth, b"{}").unwrap();
        let b = encrypt(&ua, &auth, b"{}").unwrap();
        assert_ne!(a, b, "fresh salt + ephemeral key every time");
        // salt 16 + rs 4 + idlen 1 + key 65 + (2 + 1 delimiter + 16 tag)
        assert_eq!(a.len(), 16 + 4 + 1 + 65 + 3 + 16);
        assert_eq!(&a[16..20], &4096u32.to_be_bytes());
        assert_eq!(a[20], 65);
    }

    #[test]
    fn vapid_jwt_has_the_es256_shape_and_verifies() {
        let key = generate_secret_key();
        let jwt = vapid_jwt(&key, "https://fcm.googleapis.com", "mailto:a@b.c", 1_700_000_000);
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value = serde_json::from_slice(&b64url_decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header, serde_json::json!({ "typ": "JWT", "alg": "ES256" }));
        let claims: serde_json::Value = serde_json::from_slice(&b64url_decode(parts[1]).unwrap()).unwrap();
        assert_eq!(
            claims,
            serde_json::json!({ "aud": "https://fcm.googleapis.com", "exp": 1_700_000_000u64, "sub": "mailto:a@b.c" })
        );
        let sig_bytes = b64url_decode(parts[2]).unwrap();
        assert_eq!(sig_bytes.len(), 64, "raw r||s, not DER");
        let sig = Signature::from_slice(&sig_bytes).unwrap();
        let vk = VerifyingKey::from(&SigningKey::from(&key));
        vk.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).unwrap();
    }

    #[test]
    fn authorization_header_carries_the_public_key() {
        let key = generate_secret_key();
        let h = vapid_authorization(&key, "https://web.push.apple.com", "mailto:a@b.c", 100);
        let k = h.split(", k=").nth(1).unwrap();
        assert_eq!(b64url_decode(k).unwrap(), public_bytes(&key.public_key()));
        let jwt = h.strip_prefix("vapid t=").unwrap().split(',').next().unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&b64url_decode(jwt.split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!(claims["exp"], 100 + VAPID_TTL_SECS);
    }

    #[test]
    fn subscription_keys_are_validated() {
        assert!(parse_public_key("AAAA").is_none());
        assert!(parse_auth_secret("BTBZMqHH6r4Tts7J_aSIgg").is_some());
        assert!(parse_auth_secret("BTBZMqHH6r4Tts7J_aSI").is_none());
        // Standard alphabet + padding is accepted too.
        assert!(parse_auth_secret("BTBZMqHH6r4Tts7J/aSIgg==").is_some());
    }
}
