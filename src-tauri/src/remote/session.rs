//! Per-session bound-remote model (redesign): one link + OTP + session token.
//!
//! Each time the Owner starts remote control for a run, the host mints a
//! [`BoundSession`]: a high-entropy `link_secret` (carried out-of-band in the
//! session link/QR), a relay `rendezvous_code`, a fresh ephemeral keypair (with
//! its `host_fingerprint`), and a `session_secret` used for OTP-less reconnect.
//!
//! When a controller joins, the host mints a short 6-digit `otp` shown on the
//! desktop. The controller derives the E2E key from `KDF(link_secret, otp)`;
//! a wrong OTP yields a different key and every frame fails to open (no oracle).
//! On reconnect the controller uses `KDF(link_secret, session_secret)` instead.
//!
//! Only ONE `BoundSession` exists at a time (single bound run/room).

use rand::RngCore;
use remote_e2e::{host_fingerprint, Keypair};
use std::time::{Duration, Instant};

/// OTP validity window: 120 s after it is minted (short out-of-band code).
pub const OTP_TTL: Duration = Duration::from_secs(120);
/// Max OTP entry attempts before the controller must request a fresh link/OTP.
pub const OTP_MAX_ATTEMPTS: u8 = 5;

/// Bytes of entropy for the `link_secret` (E2E KDF key material). 32B = 256-bit.
const LINK_SECRET_BYTES: usize = 32;
/// Bytes of entropy for the relay `rendezvous_code` (join routing key).
const RENDEZVOUS_BYTES: usize = 16;
/// Bytes of entropy for the durable `session_secret` (reconnect KDF input).
const SESSION_SECRET_BYTES: usize = 32;

/// The single bound remote session held by the Host agent.
///
/// No `Debug` derive: holds `link_secret`, `session_secret`, the ephemeral
/// secret key and the (transient) `otp` — none may ever be `{:?}`-logged.
pub struct BoundSession {
    /// The single run this session controls (all commands are scoped to it).
    pub run_id: String,
    /// Relay rendezvous code announced as a persistent pair.
    pub rendezvous_code: String,
    /// High-entropy link secret (hex, 64 chars) — the E2E KDF key. Travels
    /// out-of-band in the link only; never sent to the relay or persisted server.
    pub link_secret: String,
    /// This session's ephemeral X25519 keypair (forward secrecy).
    pub keypair: Keypair,
    /// Host fingerprint (hex) derived from `keypair.public`.
    pub host_fingerprint: String,
    /// Current 6-digit OTP shown on the desktop, empty until a controller joins.
    pub otp: String,
    /// Absolute deadline after which the current OTP is invalid.
    pub otp_expires_at: Instant,
    /// Remaining OTP attempts before the controller must get a fresh link/OTP.
    pub otp_attempts_left: u8,
    /// Whether the first sealed frame has authenticated this session.
    pub authenticated: bool,
    /// Durable session secret (hex) mixed into the reconnect KDF in place of OTP.
    pub session_secret: String,
    /// The controller's ephemeral public key, captured from the handshake answer
    /// and used at the OTP gate to build the candidate E2E sessions.
    pub controller_pub: Option<Vec<u8>>,
}

impl BoundSession {
    /// Whether the current OTP has expired (past its TTL).
    pub fn otp_expired(&self, now: Instant) -> bool {
        now >= self.otp_expires_at
    }

    /// Mint a fresh 6-digit OTP with a full attempt budget and TTL anchored at
    /// `now`. Called when a controller joins the rendezvous.
    pub fn mint_otp(&mut self, now: Instant) {
        self.otp = random_otp();
        self.otp_expires_at = now + OTP_TTL;
        self.otp_attempts_left = OTP_MAX_ATTEMPTS;
    }

    /// Mint an OTP only when none is currently live (never minted, or expired).
    /// A live code is REUSED so it stays valid across controller (re)joins and
    /// on-demand reveals — otherwise a code shown to the user would silently
    /// rotate out from under them the moment a controller connects, and reusing
    /// it (rather than resetting the attempt budget every join) is also the
    /// safer choice against OTP brute-force.
    pub fn ensure_otp(&mut self, now: Instant) {
        if self.otp.is_empty() || self.otp_expired(now) {
            self.mint_otp(now);
        }
    }

    /// Replace the reconnect secret. Every session token handed out so far stops
    /// opening the gate, so its holder must sign in again (OTP or master
    /// password), which hands out the new one. The link, rendezvous code and
    /// keypair are unchanged, so a live room is not affected by itself.
    pub fn rotate_session_secret(&mut self) {
        self.session_secret = hex(&random_bytes(SESSION_SECRET_BYTES));
    }

    /// Seconds until the current OTP expires; 0 when absent or already expired.
    pub fn otp_remaining_secs(&self, now: Instant) -> u64 {
        if self.otp.is_empty() || now >= self.otp_expires_at {
            return 0;
        }
        (self.otp_expires_at - now).as_secs()
    }
}

/// Lowercase hex encoding.
fn hex(bytes: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(H[(b >> 4) as usize] as char);
        s.push(H[(b & 0x0f) as usize] as char);
    }
    s
}

/// Generate `n` bytes from the OS CSPRNG.
fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut buf);
    buf
}

/// A cryptographically-random 6-digit numeric OTP (zero-padded).
fn random_otp() -> String {
    let n = rand::thread_rng().next_u32() % 1_000_000;
    format!("{n:06}")
}

/// Generate a fresh [`BoundSession`] for `run_id`: strong random `link_secret`,
/// `rendezvous_code` and `session_secret`, a fresh ephemeral keypair and the
/// derived `host_fingerprint`. The OTP is minted later (on controller join).
///
/// # Errors
/// Propagates a fingerprint-derivation error (only on a malformed key, which
/// cannot happen for a freshly generated keypair).
pub fn generate(run_id: &str) -> Result<BoundSession, String> {
    let link_secret = hex(&random_bytes(LINK_SECRET_BYTES));
    let rendezvous_code = hex(&random_bytes(RENDEZVOUS_BYTES));
    let session_secret = hex(&random_bytes(SESSION_SECRET_BYTES));
    let keypair = Keypair::generate();
    let host_fingerprint =
        host_fingerprint(&keypair.public).map_err(|e| format!("fingerprint: {e}"))?;
    Ok(BoundSession {
        run_id: run_id.to_string(),
        rendezvous_code,
        link_secret,
        keypair,
        host_fingerprint,
        otp: String::new(),
        otp_expires_at: Instant::now(),
        otp_attempts_left: OTP_MAX_ATTEMPTS,
        authenticated: false,
        session_secret,
        controller_pub: None,
    })
}

/// Rebuild a [`BoundSession`] from a persisted [`crate::secrets::RemoteSession`]
/// on agent start, so the controller can reconnect (via the session_secret) with
/// no OTP. The `link_secret` is NOT persisted (the controller holds it), so this
/// leaves it empty — reconnect uses `KDF(link_secret, session_secret)` where the
/// controller supplies `link_secret`; the host only needs `session_secret` to
/// try the reconnect key (both sides recompute the same key from their halves).
///
/// # Errors
/// Returns an error if the stored keypair hex is malformed.
pub fn rehydrate(cred: &crate::secrets::RemoteSession) -> Result<BoundSession, String> {
    let public = crate::remote::pairing_unhex(&cred.host_pub)
        .ok_or_else(|| "bad host_pub hex".to_string())?;
    let secret = crate::remote::pairing_unhex(&cred.host_secret)
        .ok_or_else(|| "bad host_secret hex".to_string())?;
    let keypair = Keypair::from_bytes(&public, &secret).map_err(|e| format!("keypair: {e}"))?;
    let host_fingerprint =
        host_fingerprint(&keypair.public).map_err(|e| format!("fingerprint: {e}"))?;
    Ok(BoundSession {
        run_id: cred.run_id.clone(),
        rendezvous_code: cred.rendezvous_code.clone(),
        // Empty for credentials persisted before it was stored; such a session
        // can only be re-entered via OTP / password login.
        link_secret: cred.link_secret.clone(),
        keypair,
        host_fingerprint,
        otp: String::new(),
        otp_expires_at: Instant::now(),
        otp_attempts_left: OTP_MAX_ATTEMPTS,
        authenticated: false,
        session_secret: cred.session_secret.clone(),
        controller_pub: None,
    })
}

/// The persisted credential after a session-secret rotation: it carries the
/// bound session's new secret when it belongs to that session, else a fresh
/// random one (nothing in memory to match, e.g. the agent is stopped). Either
/// way the token a controller stored no longer matches it.
pub fn rotated_credential(
    mut cred: crate::secrets::RemoteSession,
    bound: Option<&BoundSession>,
) -> crate::secrets::RemoteSession {
    cred.session_secret = match bound {
        Some(b) if b.rendezvous_code == cred.rendezvous_code => b.session_secret.clone(),
        _ => hex(&random_bytes(SESSION_SECRET_BYTES)),
    };
    cred
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred_of(b: &BoundSession) -> crate::secrets::RemoteSession {
        crate::secrets::RemoteSession {
            run_id: b.run_id.clone(),
            rendezvous_code: b.rendezvous_code.clone(),
            session_secret: b.session_secret.clone(),
            host_pub: hex(&b.keypair.public),
            host_secret: hex(&b.keypair.secret),
            link_secret: b.link_secret.clone(),
            idle_expires_at: 42,
        }
    }

    /// The reconnect key a token holder derives (see `try_authenticate_first_frame`).
    fn reconnect_psk(link_secret: &str, token: &str) -> [u8; 32] {
        remote_e2e::derive_effective_psk(link_secret, token)
    }

    #[test]
    fn rotation_invalidates_the_old_token_but_keeps_the_session() {
        let mut s = generate("run-a").unwrap();
        let old_token = s.session_secret.clone();
        let (link, code, fp) = (s.link_secret.clone(), s.rendezvous_code.clone(), s.host_fingerprint.clone());
        s.rotate_session_secret();
        assert_ne!(s.session_secret, old_token);
        assert_eq!(s.session_secret.len(), SESSION_SECRET_BYTES * 2);
        // The old token no longer derives the host's reconnect key.
        assert_ne!(reconnect_psk(&link, &old_token), reconnect_psk(&link, &s.session_secret));
        // Same link: a new sign-in lands on the same session and gets the new token.
        assert_eq!(s.link_secret, link);
        assert_eq!(s.rendezvous_code, code);
        assert_eq!(s.host_fingerprint, fp);
    }

    #[test]
    fn rotated_credential_follows_the_bound_session() {
        let mut s = generate("run-a").unwrap();
        let cred = cred_of(&s);
        s.rotate_session_secret();
        let rotated = rotated_credential(cred, Some(&s));
        assert_eq!(rotated.session_secret, s.session_secret);
        assert_eq!(rotated.rendezvous_code, s.rendezvous_code);
        assert_eq!(rotated.link_secret, s.link_secret);
        assert_eq!(rotated.idle_expires_at, 42);
        // A restart rehydrates the rotated secret, not the old one.
        assert_eq!(rehydrate(&rotated).unwrap().session_secret, s.session_secret);
    }

    #[test]
    fn rotated_credential_without_a_matching_session_gets_a_fresh_secret() {
        let s = generate("run-a").unwrap();
        let other = generate("run-b").unwrap();
        let old = s.session_secret.clone();
        let rotated = rotated_credential(cred_of(&s), None);
        assert_ne!(rotated.session_secret, old);
        assert_eq!(rotated.session_secret.len(), SESSION_SECRET_BYTES * 2);
        let rotated = rotated_credential(cred_of(&s), Some(&other));
        assert_ne!(rotated.session_secret, old);
        assert_ne!(rotated.session_secret, other.session_secret);
    }

    #[test]
    fn generate_produces_distinct_strong_secrets() {
        let a = generate("run-a").unwrap();
        let b = generate("run-b").unwrap();
        // link_secret = 32B hex = 64 chars; rendezvous = 16B hex = 32 chars.
        assert_eq!(a.link_secret.len(), LINK_SECRET_BYTES * 2);
        assert_eq!(a.rendezvous_code.len(), RENDEZVOUS_BYTES * 2);
        assert_eq!(a.session_secret.len(), SESSION_SECRET_BYTES * 2);
        assert_eq!(a.host_fingerprint.len(), 64); // BLAKE2b-256 hex
        assert_ne!(a.link_secret, b.link_secret);
        assert_ne!(a.rendezvous_code, b.rendezvous_code);
        assert_ne!(a.session_secret, b.session_secret);
        assert_ne!(a.host_fingerprint, b.host_fingerprint);
        assert_eq!(a.run_id, "run-a");
        assert!(!a.authenticated);
        assert!(a.otp.is_empty(), "otp is minted on join, not at generate");
    }

    #[test]
    fn mint_otp_produces_six_digits_and_resets_budget() {
        let mut s = generate("run-a").unwrap();
        s.otp_attempts_left = 1;
        let now = Instant::now();
        s.mint_otp(now);
        assert_eq!(s.otp.len(), 6, "6-digit OTP");
        assert!(s.otp.chars().all(|c| c.is_ascii_digit()));
        assert_eq!(s.otp_attempts_left, OTP_MAX_ATTEMPTS);
        assert!(!s.otp_expired(now));
        assert!(!s.otp_expired(now + Duration::from_secs(119)));
        assert!(s.otp_expired(now + OTP_TTL));
    }

    #[test]
    fn rehydrate_rebuilds_with_matching_fingerprint() {
        let kp = Keypair::generate();
        let pub_hex = hex(&kp.public);
        let sec_hex = hex(&kp.secret);
        let fp = host_fingerprint(&kp.public).unwrap();
        let cred = crate::secrets::RemoteSession {
            run_id: "run-x".to_string(),
            rendezvous_code: "rv_reuse".to_string(),
            session_secret: "00112233445566778899aabbccddeeff".to_string(),
            host_pub: pub_hex,
            host_secret: sec_hex,
            link_secret: String::new(),
            idle_expires_at: 0,
        };
        let s = rehydrate(&cred).expect("rehydrate ok");
        assert_eq!(s.run_id, "run-x");
        assert_eq!(s.rendezvous_code, "rv_reuse");
        assert_eq!(s.host_fingerprint, fp);
        assert!(!s.authenticated);
    }

    #[test]
    fn rehydrate_rejects_bad_key_hex() {
        let cred = crate::secrets::RemoteSession {
            run_id: "r".to_string(),
            rendezvous_code: "rv".to_string(),
            session_secret: "aa".to_string(),
            host_pub: "zz".to_string(),
            host_secret: "zz".to_string(),
            link_secret: String::new(),
            idle_expires_at: 0,
        };
        assert!(rehydrate(&cred).is_err());
    }
}
