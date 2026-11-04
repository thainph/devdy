//! Cross-implementation check: `vectors/libsodium_compat.json` was captured
//! from the previous libsodium-wrappers TS impl, and the current @noble TS impl
//! is pinned to it by `src/lib/__tests__/remoteE2e.test.mjs`. This test proves
//! the Rust (dryoc) impl agrees with the same bytes, so all three are
//! wire-compatible.

use dryoc::classic::crypto_core::{crypto_scalarmult, crypto_scalarmult_base};
use remote_e2e::{derive_effective_psk, hex_decode, host_fingerprint, E2eError, Keypair, Session};
use serde::Deserialize;

#[derive(Deserialize)]
struct Kp {
    sk: String,
    pk: String,
}

#[derive(Deserialize)]
struct Dh {
    sk: String,
    pk: String,
    dh: String,
}

#[derive(Deserialize)]
struct Sess {
    a: usize,
    b: usize,
    psk_hex: String,
    key_hex: String,
}

#[derive(Deserialize)]
struct Seal {
    nonce_hex: String,
    plaintext_hex: String,
    cipher_b64: String,
}

#[derive(Deserialize)]
struct Fp {
    pk: String,
    fp: String,
}

#[derive(Deserialize)]
struct Psk {
    link: String,
    secret2: String,
    psk_hex: String,
}

#[derive(Deserialize)]
struct OpenCase {
    cipher_b64: String,
    plaintext_hex: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct Vectors {
    keypairs: Vec<Kp>,
    dh: Vec<Dh>,
    weak_peer_keys: Vec<String>,
    sessions: Vec<Sess>,
    seals: Vec<Seal>,
    fingerprints: Vec<Fp>,
    effective_psk: Vec<Psk>,
    open_cases: Vec<OpenCase>,
}

fn load() -> Vectors {
    serde_json::from_str(include_str!("../vectors/libsodium_compat.json")).expect("vector JSON")
}

fn h(s: &str) -> Vec<u8> {
    hex_decode(s).expect("hex")
}

fn arr(s: &str) -> [u8; 32] {
    h(s).try_into().expect("32 bytes")
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn kp(v: &Vectors, i: usize) -> Keypair {
    Keypair::from_bytes(&h(&v.keypairs[i].pk), &h(&v.keypairs[i].sk)).expect("keypair")
}

#[test]
fn base_mult_and_dh_match() {
    let v = load();
    for k in &v.keypairs {
        let mut pk = [0u8; 32];
        crypto_scalarmult_base(&mut pk, &arr(&k.sk));
        assert_eq!(hexs(&pk), k.pk);
    }
    for d in &v.dh {
        let mut out = [0u8; 32];
        crypto_scalarmult(&mut out, &arr(&d.sk), &arr(&d.pk)).expect("dh");
        assert_eq!(hexs(&out), d.dh, "{} x {}", d.sk, d.pk);
    }
}

#[test]
fn weak_peer_keys_rejected() {
    let v = load();
    for pk in &v.weak_peer_keys {
        let r = Session::new(&[0u8], &kp(&v, 0), &h(pk));
        assert!(matches!(r, Err(E2eError::WeakKex)), "{pk}");
    }
}

#[test]
fn sessions_match() {
    let v = load();
    for s in &v.sessions {
        let psk = h(&s.psk_hex);
        if psk.len() < 16 {
            // Known (pre-existing) divergence: dryoc enforces BLAKE2b's 16-byte
            // minimum key, libsodium/noble accept 1..=64. Unreachable in the
            // protocol — the effective psk is always 32 bytes — so just pin
            // that Rust rejects it rather than deriving a different key.
            assert!(Session::new(&psk, &kp(&v, s.a), &kp(&v, s.b).public).is_err());
            continue;
        }
        let ka = Session::new(&psk, &kp(&v, s.a), &kp(&v, s.b).public).expect("a");
        let kb = Session::new(&psk, &kp(&v, s.b), &kp(&v, s.a).public).expect("b");
        assert_eq!(hexs(&ka.key_bytes()), s.key_hex);
        assert_eq!(hexs(&kb.key_bytes()), s.key_hex);
    }
}

#[test]
fn seals_and_opens_match() {
    let v = load();
    let psk = h(&v.sessions[0].psk_hex);
    let sa = Session::new(&psk, &kp(&v, 0), &kp(&v, 1).public).expect("a");
    let sb = Session::new(&psk, &kp(&v, 1), &kp(&v, 0).public).expect("b");
    for s in &v.seals {
        let c = sa
            .seal_with_nonce(&h(&s.plaintext_hex), &h(&s.nonce_hex))
            .expect("seal");
        assert_eq!(c, s.cipher_b64);
        assert_eq!(hexs(&sb.open(&s.cipher_b64).expect("open")), s.plaintext_hex);
    }
    for c in &v.open_cases {
        match (sb.open(&c.cipher_b64), &c.plaintext_hex, &c.error) {
            (Ok(pt), Some(want), None) => assert_eq!(&hexs(&pt), want),
            (Err(e), None, Some(want)) => assert_eq!(&e.to_string(), want, "{:?}", c.cipher_b64),
            (r, _, _) => panic!("mismatch for {:?}: {:?}", c.cipher_b64, r.map(|p| hexs(&p))),
        }
    }
}

#[test]
fn fingerprints_and_effective_psk_match() {
    let v = load();
    for f in &v.fingerprints {
        assert_eq!(host_fingerprint(&h(&f.pk)).expect("fp"), f.fp);
    }
    for p in &v.effective_psk {
        assert_eq!(hexs(&derive_effective_psk(&p.link, &p.secret2)), p.psk_hex);
    }
}
