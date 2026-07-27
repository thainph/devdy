//! Interop test: load the shared vector (`vectors/basic.json`) and prove the
//! Rust reference impl reproduces the exact `cipher`, session key and
//! fingerprint the TS impl must also produce (spec §6, AC-19).

use remote_e2e::{derive_effective_psk, host_fingerprint, hex_decode, Keypair, Session};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vector {
    psk_hex: String,
    sk_a_hex: String,
    pk_a_hex: String,
    sk_b_hex: String,
    pk_b_hex: String,
    nonce_hex: String,
    plaintext_utf8: String,
    session_key_hex: String,
    host_fingerprint_hex: String,
    cipher_b64: String,
}

fn load_vector() -> Vector {
    let raw = include_str!("../vectors/basic.json");
    serde_json::from_str(raw).expect("vector JSON parses")
}

fn kp(pk_hex: &str, sk_hex: &str) -> Keypair {
    Keypair::from_bytes(
        &hex_decode(pk_hex).expect("pk hex"),
        &hex_decode(sk_hex).expect("sk hex"),
    )
    .expect("keypair")
}

#[test]
fn session_key_matches_vector_both_roles() {
    let v = load_vector();
    let psk = hex_decode(&v.psk_hex).expect("psk");
    let a = kp(&v.pk_a_hex, &v.sk_a_hex);
    let b = kp(&v.pk_b_hex, &v.sk_b_hex);

    let sa = Session::new(&psk, &a, &b.public).expect("session a");
    let sb = Session::new(&psk, &b, &a.public).expect("session b");

    // Both roles derive the identical key...
    assert_eq!(sa.key_bytes(), sb.key_bytes());
    // ...and it matches the frozen vector.
    let expected = hex_decode(&v.session_key_hex).expect("key hex");
    assert_eq!(sa.key_bytes().to_vec(), expected);
}

#[test]
fn seal_is_deterministic_with_fixed_nonce() {
    let v = load_vector();
    let psk = hex_decode(&v.psk_hex).expect("psk");
    let a = kp(&v.pk_a_hex, &v.sk_a_hex);
    let b = kp(&v.pk_b_hex, &v.sk_b_hex);
    let nonce = hex_decode(&v.nonce_hex).expect("nonce");

    let sa = Session::new(&psk, &a, &b.public).expect("session a");
    let cipher = sa
        .seal_with_nonce(v.plaintext_utf8.as_bytes(), &nonce)
        .expect("seal");
    assert_eq!(cipher, v.cipher_b64, "Rust seal must reproduce vector cipher");
}

#[test]
fn open_vector_cipher_round_trips() {
    let v = load_vector();
    let psk = hex_decode(&v.psk_hex).expect("psk");
    let a = kp(&v.pk_a_hex, &v.sk_a_hex);
    let b = kp(&v.pk_b_hex, &v.sk_b_hex);

    // B opens the cipher A sealed.
    let sb = Session::new(&psk, &b, &a.public).expect("session b");
    let opened = sb.open(&v.cipher_b64).expect("open");
    assert_eq!(opened, v.plaintext_utf8.as_bytes());
}

#[test]
fn host_fingerprint_matches_vector() {
    let v = load_vector();
    let pk_a = hex_decode(&v.pk_a_hex).expect("pk a");
    let fp = host_fingerprint(&pk_a).expect("fingerprint");
    assert_eq!(fp, v.host_fingerprint_hex);
}

#[test]
fn tampered_cipher_is_rejected() {
    let v = load_vector();
    let psk = hex_decode(&v.psk_hex).expect("psk");
    let a = kp(&v.pk_a_hex, &v.sk_a_hex);
    let b = kp(&v.pk_b_hex, &v.sk_b_hex);
    let sb = Session::new(&psk, &b, &a.public).expect("session b");

    // Flip a byte in the base64 payload region.
    let mut bad = v.cipher_b64.clone();
    let ch = bad.pop().unwrap_or('A');
    bad.push(if ch == 'A' { 'B' } else { 'A' });
    assert!(sb.open(&bad).is_err(), "tampered cipher must fail auth");
}

#[test]
fn random_nonce_round_trip() {
    // Full handshake with fresh ephemeral keys (forward secrecy path).
    let psk = b"a-strong-pairing-psk-32-bytes!!!";
    let host = Keypair::generate();
    let ctrl = Keypair::generate();
    let s_host = Session::new(psk, &host, &ctrl.public).expect("host session");
    let s_ctrl = Session::new(psk, &ctrl, &host.public).expect("ctrl session");

    let msg = br#"{"kind":"cmd","action":"start_run","run_id":"r_2"}"#;
    let cipher = s_ctrl.seal(msg);
    let opened = s_host.open(&cipher).expect("open");
    assert_eq!(opened, msg);
}

#[test]
fn wrong_psk_fails_to_open() {
    let v = load_vector();
    let a = kp(&v.pk_a_hex, &v.sk_a_hex);
    let b = kp(&v.pk_b_hex, &v.sk_b_hex);
    let sb = Session::new(b"the-wrong-psk-entirely-nope", &b, &a.public).expect("session");
    assert!(sb.open(&v.cipher_b64).is_err(), "wrong psk must not decrypt");
}

// ---- two-layer (link_secret + OTP) effective-psk KDF ----

const LINK_SECRET: &str = "a3f1c0de9b8877665544332211009988ffeeddccbbaa99887766554433221100";

#[test]
fn effective_psk_is_deterministic_and_fixed_length() {
    let k1 = derive_effective_psk(LINK_SECRET, "482913");
    let k2 = derive_effective_psk(LINK_SECRET, "482913");
    assert_eq!(k1, k2, "same inputs → same key");
    assert_eq!(k1.len(), 32);
}

#[test]
fn effective_psk_differs_per_otp_and_per_link_secret() {
    let base = derive_effective_psk(LINK_SECRET, "482913");
    assert_ne!(base, derive_effective_psk(LINK_SECRET, "482914"), "otp matters");
    assert_ne!(
        base,
        derive_effective_psk("00112233445566778899aabbccddeeff", "482913"),
        "link_secret matters"
    );
}

#[test]
fn effective_psk_handshake_round_trips_both_roles() {
    // Both peers derive the same effective psk from (link_secret, otp), so a
    // full seal/open round-trip succeeds — the OTP-authenticated pairing path.
    let eff = derive_effective_psk(LINK_SECRET, "482913");
    let host = Keypair::generate();
    let ctrl = Keypair::generate();
    let s_host = Session::new(&eff, &host, &ctrl.public).expect("host session");
    let s_ctrl = Session::new(&eff, &ctrl, &host.public).expect("ctrl session");

    let msg = br#"{"kind":"cmd","action":"send_chat_message","run_id":"r_1"}"#;
    let cipher = s_ctrl.seal(msg);
    assert_eq!(s_host.open(&cipher).expect("open"), msg);
}

#[test]
fn wrong_otp_yields_unopenable_key() {
    // Controller enters the wrong OTP → different effective psk → the Host cannot
    // open the controller's first frame (this IS the OTP gate, no oracle).
    let host = Keypair::generate();
    let ctrl = Keypair::generate();
    let good = derive_effective_psk(LINK_SECRET, "482913");
    let bad = derive_effective_psk(LINK_SECRET, "999999");
    let s_host = Session::new(&good, &host, &ctrl.public).expect("host session");
    let s_ctrl = Session::new(&bad, &ctrl, &host.public).expect("ctrl session");

    let cipher = s_ctrl.seal(b"hello");
    assert!(s_host.open(&cipher).is_err(), "wrong OTP must not authenticate");
}
