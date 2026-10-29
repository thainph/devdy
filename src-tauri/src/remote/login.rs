//! Password login: reach the Host from the bare controller page with only the
//! owner's master password — for when the session link is lost.
//!
//! While a master password is set, the agent also listens on the well-known
//! [`LOGIN_CODE`] in the relay's `login` slot. A Controller joins it, both sides
//! run the usual X25519 handshake, and the E2E key is
//! `derive_effective_psk(LOGIN_LINK_SECRET, hex(PBKDF2(password)))` — so only a
//! peer that knows the password can open or produce a frame. The first frame
//! that opens proves the password; the Host answers with a `login_granted`
//! carrying a normal session link + token, and the Controller continues on the
//! regular session path (fingerprint pinned from then on).
//!
//! Brute force is bounded at the Host: [`MAX_FAILURES`] wrong passwords per
//! [`FAILURE_WINDOW`], after which attempts are refused without even trying.
//! PBKDF2 makes an offline guess against a captured frame expensive.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use remote_e2e::derive_effective_psk;

/// Rendezvous code of the password-login slot. Public by design: knowing it
/// only lets a peer *try* a password, which the Host rate limits.
pub const LOGIN_CODE: &str = "devdy-login-v1";
/// Relay listen slot for [`LOGIN_CODE`].
pub const LOGIN_SLOT: &str = "login";
/// Public stand-in for the link secret in the login KDF (hex of
/// `"devdy remote login v1 public key"`). Must match the controller.
pub const LOGIN_LINK_SECRET: &str = "64657664792072656d6f7465206c6f67696e207631207075626c6963206b6579";
/// PBKDF2-HMAC-SHA256 salt and rounds. Must match the controller.
const PBKDF2_SALT: &[u8] = b"devdy-remote-login-v1";
pub const PBKDF2_ROUNDS: u32 = 210_000;

/// Wrong passwords allowed per window before login is refused outright.
pub const MAX_FAILURES: usize = 5;
pub const FAILURE_WINDOW: Duration = Duration::from_secs(300);

/// Stretched password → hex, the `secret2` fed to the login KDF.
fn stretch(password: &str) -> String {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<sha2::Sha256>(password.as_bytes(), PBKDF2_SALT, PBKDF2_ROUNDS, &mut out);
    crate::remote::pairing_hex(&out)
}

/// The login PSK for `password`. PBKDF2 is deliberately slow, so the result is
/// cached for the current password (it only changes when the owner sets a new
/// one). Run it off the async runtime.
pub fn login_psk(password: &str) -> [u8; 32] {
    static CACHE: Mutex<Option<(String, [u8; 32])>> = Mutex::new(None);
    if let Ok(guard) = CACHE.lock() {
        if let Some((pw, psk)) = guard.as_ref() {
            if pw == password {
                return *psk;
            }
        }
    }
    let psk = derive_effective_psk(LOGIN_LINK_SECRET, &stretch(password));
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((password.to_string(), psk));
    }
    psk
}

/// Sliding-window counter of failed login attempts (process-wide, so
/// re-dialing the relay does not reset it).
pub struct FailureLimiter {
    failures: VecDeque<Instant>,
}

impl FailureLimiter {
    pub const fn new() -> Self {
        Self {
            failures: VecDeque::new(),
        }
    }

    fn prune(&mut self, now: Instant) {
        while self
            .failures
            .front()
            .is_some_and(|t| now.duration_since(*t) >= FAILURE_WINDOW)
        {
            self.failures.pop_front();
        }
    }

    /// Whether another attempt may be tried now.
    pub fn allow(&mut self, now: Instant) -> bool {
        self.prune(now);
        self.failures.len() < MAX_FAILURES
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.prune(now);
        self.failures.push_back(now);
    }
}

pub static FAILURES: Mutex<FailureLimiter> = Mutex::new(FailureLimiter::new());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psk_is_deterministic_and_password_bound() {
        let a = login_psk("correct horse");
        assert_eq!(a, login_psk("correct horse"));
        assert_ne!(a, login_psk("correct horsf"));
    }

    /// Cross-language vector: `src/controller/__tests__/login.test.mjs` asserts
    /// the controller derives the same bytes.
    #[test]
    fn psk_matches_the_controller_vector() {
        assert_eq!(
            crate::remote::pairing_hex(&login_psk("devdy-test-password")),
            LOGIN_PSK_VECTOR
        );
    }
    const LOGIN_PSK_VECTOR: &str = "c2ee56d76cc322ce322e386031f956429595a64a04195e648ba9f27fa070e57f";

    #[test]
    fn limiter_blocks_after_max_failures_then_recovers() {
        let mut l = FailureLimiter::new();
        let t0 = Instant::now();
        for _ in 0..MAX_FAILURES {
            assert!(l.allow(t0));
            l.record_failure(t0);
        }
        assert!(!l.allow(t0));
        assert!(l.allow(t0 + FAILURE_WINDOW));
    }
}
