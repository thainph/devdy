//! Generates the shared cross-language test vector (spec §6).
//!
//! Run: `cargo run --example gen_vectors > vectors/basic.json`
//! The keys/psk/nonce here are FIXED TEST DATA — never production secrets.

use remote_e2e::{host_fingerprint, hex_decode, Keypair, Session};

// TEST VECTOR INPUTS — deterministic, hardcoded on purpose (test data only).
const PSK_HEX: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
const SK_A_HEX: &str = "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a";
const SK_B_HEX: &str = "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb";
const NONCE_HEX: &str = "404142434445464748494a4b4c4d4e4f5051525354555657";
const PLAINTEXT: &str = "{\"kind\":\"stream\",\"run_id\":\"r_1\",\"line\":\"hello from host\"}";

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let psk = hex_decode(PSK_HEX)?;
    let sk_a = hex_decode(SK_A_HEX)?;
    let sk_b = hex_decode(SK_B_HEX)?;
    let nonce = hex_decode(NONCE_HEX)?;

    // Derive public keys from the fixed secrets via X25519 base.
    let kp_a = derive_keypair(&sk_a)?;
    let kp_b = derive_keypair(&sk_b)?;

    // Both sides derive the SAME session key.
    let sess_a = Session::new(&psk, &kp_a, &kp_b.public)?;
    let sess_b = Session::new(&psk, &kp_b, &kp_a.public)?;
    assert_eq!(sess_a.key_bytes(), sess_b.key_bytes(), "session keys must match");

    // Deterministic cipher using the fixed nonce (A seals).
    let cipher = sess_a.seal_with_nonce(PLAINTEXT.as_bytes(), &nonce)?;
    // Sanity: B opens it.
    let opened = sess_b.open(&cipher)?;
    assert_eq!(opened, PLAINTEXT.as_bytes());

    let fp = host_fingerprint(&kp_a.public)?; // treat A as Host in this vector.

    println!(
        "{{\n  \"_note\": \"TEST VECTOR — DO NOT USE IN PRODUCTION. Keys/psk/nonce are fixed test data.\",\n  \"version\": 1,\n  \"psk_hex\": \"{}\",\n  \"sk_a_hex\": \"{}\",\n  \"pk_a_hex\": \"{}\",\n  \"sk_b_hex\": \"{}\",\n  \"pk_b_hex\": \"{}\",\n  \"nonce_hex\": \"{}\",\n  \"plaintext_utf8\": {},\n  \"session_key_hex\": \"{}\",\n  \"host_fingerprint_hex\": \"{}\",\n  \"cipher_b64\": \"{}\"\n}}",
        PSK_HEX,
        SK_A_HEX,
        hexs(&kp_a.public),
        SK_B_HEX,
        hexs(&kp_b.public),
        NONCE_HEX,
        serde_json::to_string(PLAINTEXT)?,
        hexs(&sess_a.key_bytes()),
        fp,
        cipher,
    );
    Ok(())
}

/// X25519 base-point multiply to get the public key from a fixed secret.
fn derive_keypair(secret: &[u8]) -> Result<Keypair, Box<dyn std::error::Error>> {
    use dryoc::classic::crypto_core::crypto_scalarmult_base;
    let sk: [u8; 32] = secret.try_into().map_err(|_| "bad secret len")?;
    let mut pk = [0u8; 32];
    crypto_scalarmult_base(&mut pk, &sk);
    Ok(Keypair::from_bytes(&pk, &sk)?)
}
