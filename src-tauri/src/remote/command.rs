//! Pure command-validation logic for the Remote Control host agent.
//!
//! These functions hold ZERO I/O so they are directly unit-testable and cover
//! the security-critical AC:
//! - AC-12 (FR-009): allow-list — only 5 actions accepted; others rejected.
//! - AC-17 (BR-016/SEC-011): a remote `respond_permission` may only be
//!   `allow_once`/`deny_once`; `allow_always`/`deny_always` are rejected.
//! - A remote `start_run` / `set_run_meta` may set any permission mode the
//!   desktop can (parity by explicit product decision; the trust boundary is
//!   device approval, not a locked-down remote mode).
//! - AC-10 (BR-009): idempotency — a `request_id` is answered at most once.
//! - AC-18 (BR-017): rate-limit 30 commands/minute per Controller.

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// CMD_RATE_LIMIT — max commands a Controller may send per minute (BR-017).
pub const CMD_RATE_LIMIT: u32 = 30;
/// The rate-limit window.
pub const CMD_RATE_WINDOW: Duration = Duration::from_secs(60);

/// The actions a Controller is allowed to invoke (FR-009). `list_runs` is a
/// read-only browse of run metadata (added for full desktop parity — the trust
/// boundary is device approval, not per-run share).
pub const ALLOW_LIST: [&str; 12] = [
    "respond_permission",
    "start_run",
    "request_history",
    "cancel_run",
    "send_chat_message",
    "set_run_meta",
    "list_runs",
    "list_projects",
    "list_slash_commands",
    "list_engine_models",
    "list_project_files",
    "list_plan_usage",
];

/// Whether `action` is in the frozen allow-list (AC-12).
pub fn is_allowed_action(action: &str) -> bool {
    ALLOW_LIST.contains(&action)
}

/// Map a remote permission decision string onto the internal
/// `respond_permission` decision (`allow`/`deny`), enforcing BR-016/SEC-011.
///
/// Only `allow_once` and `deny_once` are accepted from a Controller. The
/// "remember" variants (`allow_always`/`deny_always`) — and anything else —
/// return `Err`, so a remote device can never persist a permission grant
/// (AC-17). Plain `allow`/`deny` are also refused: a remote caller must be
/// explicit about the one-time scope.
pub fn map_remote_decision(decision: &str) -> Result<&'static str, RejectReason> {
    match decision {
        "allow_once" => Ok("allow"),
        "deny_once" => Ok("deny"),
        "allow_always" | "deny_always" => Err(RejectReason::RememberNotAllowed),
        _ => Err(RejectReason::BadDecision),
    }
}

/// Machine-readable rejection reasons written to the audit log (never a secret).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectReason {
    /// Action not in the allow-list (AC-12).
    NotAllowed,
    /// Decision was `allow_always`/`deny_always` (AC-17).
    RememberNotAllowed,
    /// Decision string was not a recognized one-time decision.
    BadDecision,
    /// `request_id` already answered (AC-10).
    Duplicate,
    /// Over the per-minute command cap (AC-18).
    RateLimited,
    /// Malformed payload (missing/blank required field).
    Malformed,
    /// No authenticated session for this room (retained for completeness; the
    /// OTP gate now rejects unauthenticated frames before the handler runs).
    #[allow(dead_code)]
    Unauthorized,
    /// Command carried a run_id that is not the single bound session run.
    WrongRun,
}

impl RejectReason {
    /// Stable wire/audit string.
    pub fn as_str(self) -> &'static str {
        match self {
            RejectReason::NotAllowed => "action_not_allowed",
            RejectReason::RememberNotAllowed => "remember_not_allowed",
            RejectReason::BadDecision => "bad_decision",
            RejectReason::Duplicate => "duplicate_request",
            RejectReason::RateLimited => "rate_limited",
            RejectReason::Malformed => "malformed",
            RejectReason::Unauthorized => "unauthorized",
            RejectReason::WrongRun => "wrong_run",
        }
    }
}

/// Idempotency guard for `respond_permission` (BR-009/AC-10).
///
/// Records every `request_id` already answered so a duplicate (arriving from the
/// same Controller, or racing an answer the Owner already gave at the Host) is
/// dropped rather than re-applied. This is intentionally monotone (never
/// forgets within a session): the number of distinct requests in one remote
/// session is small and bounded by RESP_TIMEOUT churn.
#[derive(Default)]
pub struct IdempotencyGuard {
    seen: HashSet<String>,
}

impl IdempotencyGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Try to claim `request_id`. Returns `true` on the FIRST call for that id
    /// (proceed), `false` on every later call (duplicate — drop, AC-10).
    pub fn claim(&mut self, request_id: &str) -> bool {
        self.seen.insert(request_id.to_string())
    }

    /// Release a claim when delivery to the local permission resolver failed.
    ///
    /// A request is only idempotently complete after the sidecar/broker accepts
    /// the response. Keeping a failed claim would turn a transient delivery
    /// error into a permanent `duplicate_request` and make Deny/Allow impossible
    /// to retry.
    pub fn release(&mut self, request_id: &str) {
        self.seen.remove(request_id);
    }

    /// Whether `request_id` has already been answered. Exercised by the unit
    /// tests; kept as part of the guard's API surface.
    #[allow(dead_code)]
    pub fn is_seen(&self, request_id: &str) -> bool {
        self.seen.contains(request_id)
    }
}

/// Fixed-window rate limiter, 30 commands/minute per Controller (BR-017/AC-18).
///
/// Fixed window (reset on the minute boundary from first hit) is sufficient for
/// an anti-abuse cap and is trivial to reason about. Every REJECTED command
/// still gets audited by the caller.
pub struct RateLimiter {
    max: u32,
    window: Duration,
    window_start: Instant,
    count: u32,
}

impl RateLimiter {
    /// A limiter with the frozen defaults (30/min).
    pub fn new(now: Instant) -> Self {
        RateLimiter {
            max: CMD_RATE_LIMIT,
            window: CMD_RATE_WINDOW,
            window_start: now,
            count: 0,
        }
    }

    /// Construct with explicit limits (used by tests).
    #[allow(dead_code)]
    pub fn with_limits(max: u32, window: Duration, now: Instant) -> Self {
        RateLimiter {
            max,
            window,
            window_start: now,
            count: 0,
        }
    }

    /// Record one command attempt at `now`. Returns `true` if it is within the
    /// cap (allow), `false` if the cap is exceeded (block + audit, AC-18).
    pub fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window_start) >= self.window {
            self.window_start = now;
            self.count = 0;
        }
        if self.count >= self.max {
            return false;
        }
        self.count += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- AC-12: allow-list ----
    #[test]
    fn allow_list_accepts_exactly_the_allowed_actions() {
        for a in [
            "respond_permission",
            "start_run",
            "request_history",
            "cancel_run",
            "send_chat_message",
            "set_run_meta",
            "list_runs",
            "list_projects",
            "list_slash_commands",
            "list_engine_models",
            "list_project_files",
            "list_plan_usage",
        ] {
            assert!(is_allowed_action(a), "{a} should be allowed");
        }
    }

    #[test]
    fn allow_list_rejects_anything_else() {
        for a in [
            "",
            "open_terminal",
            "read_file",
            "delete_run", // removed in the single-run redesign
            "RESPOND_PERMISSION", // case-sensitive
            "start_run ",         // trailing space is a different string
        ] {
            assert!(!is_allowed_action(a), "{a} must be rejected");
        }
    }

    // ---- AC-17: reject allow_always / deny_always from a Controller ----
    #[test]
    fn remote_decision_maps_once_variants() {
        assert_eq!(map_remote_decision("allow_once"), Ok("allow"));
        assert_eq!(map_remote_decision("deny_once"), Ok("deny"));
    }

    #[test]
    fn remote_decision_rejects_remember_variants() {
        assert_eq!(
            map_remote_decision("allow_always"),
            Err(RejectReason::RememberNotAllowed)
        );
        assert_eq!(
            map_remote_decision("deny_always"),
            Err(RejectReason::RememberNotAllowed)
        );
    }

    #[test]
    fn remote_decision_rejects_bare_and_unknown() {
        // Even plain allow/deny must be explicit-once from remote.
        assert_eq!(map_remote_decision("allow"), Err(RejectReason::BadDecision));
        assert_eq!(map_remote_decision("deny"), Err(RejectReason::BadDecision));
        assert_eq!(map_remote_decision("ask"), Err(RejectReason::BadDecision));
        assert_eq!(map_remote_decision(""), Err(RejectReason::BadDecision));
    }

    // ---- AC-10: idempotency ----
    #[test]
    fn idempotency_claims_once_then_drops_duplicates() {
        let mut g = IdempotencyGuard::new();
        assert!(g.claim("p_9"), "first claim proceeds");
        assert!(!g.claim("p_9"), "second claim is a duplicate");
        assert!(!g.claim("p_9"), "still a duplicate");
        assert!(g.is_seen("p_9"));
        assert!(g.claim("p_10"), "a different id is independent");
    }

    #[test]
    fn failed_delivery_can_release_idempotency_claim_for_retry() {
        let mut g = IdempotencyGuard::new();
        assert!(g.claim("p_retry"));
        g.release("p_retry");
        assert!(!g.is_seen("p_retry"));
        assert!(g.claim("p_retry"), "a failed delivery must remain retryable");
    }

    // ---- AC-18: rate-limit 30/min ----
    #[test]
    fn rate_limiter_allows_up_to_max_then_blocks() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::new(t0);
        for i in 0..CMD_RATE_LIMIT {
            assert!(rl.allow(t0), "command {i} within cap should pass");
        }
        // 31st command inside the same window is blocked.
        assert!(!rl.allow(t0), "over-cap command must be blocked");
        assert!(!rl.allow(t0), "still blocked");
    }

    #[test]
    fn rate_limiter_resets_after_window() {
        let t0 = Instant::now();
        let mut rl = RateLimiter::with_limits(3, Duration::from_secs(60), t0);
        assert!(rl.allow(t0));
        assert!(rl.allow(t0));
        assert!(rl.allow(t0));
        assert!(!rl.allow(t0), "cap reached");
        // Advance past the window → counter resets.
        let t1 = t0 + Duration::from_secs(61);
        assert!(rl.allow(t1), "new window allows again");
        assert!(rl.allow(t1));
    }
}
