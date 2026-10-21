//! Run liveness: watches one live sidecar and decides when the user should be
//! told that something may be wrong. Pure state + decisions so the policy is
//! unit-testable; the drain loop in `sidecar.rs` feeds it events and carries out
//! its actions.
//!
//! Policy: this module NEVER stops a run or answers a prompt on suspicion — its
//! only outputs are notices (a banner with a stop action), notice clears and
//! permission reminders. The `Action` enum has no kill/deny variant on purpose.
//! Runs are only torn down on definitive evidence (process exited, stdout EOF,
//! the `_devdy_closed` shutdown protocol), which the drain handles itself.
//!
//! Why silence is only ever a hint: legitimate work can be silent for a long
//! time. The CLI emits a `tool_progress` heartbeat every 30s only for tools the
//! MAIN thread runs — never for tools inside a subagent, so a background
//! subagent running a 20-minute `terraform apply` produces no output at all, and
//! its messages even stream after the parent turn's `result`. Codex emits
//! nothing while a long command runs. The sidecar's own heartbeat (a plain Node
//! timer) is the strongest signal we have, yet even its absence can be a paused
//! process, so it too only raises a notice.
//!
//! Time: all ages are measured on the monotonic clock AND only across normal
//! watchdog ticks. A gap between two evaluations larger than `MAX_TICK_GAP`
//! (system sleep, App Nap, a starved runtime) is not counted: baselines restart
//! and waiting ages don't grow, so waking a laptop never produces a burst of
//! "stuck" notices.

use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::Instant;

/// How often the drain evaluates liveness.
pub const WATCHDOG_TICK: Duration = Duration::from_secs(15);
/// Heartbeat-capable sidecars emit `_devdy_heartbeat` every 30s from a plain
/// Node timer. No output at all for this long → "not responding" notice.
pub const SIDECAR_SILENCE_LIMIT: Duration = Duration::from_secs(3 * 60);
/// A freshly spawned sidecar that has produced no output at all (no ready line,
/// no heartbeat) within this window → "has not started" notice.
pub const STARTUP_WINDOW: Duration = Duration::from_secs(90);
/// An active main-thread turn with no engine output (no stream, no thinking
/// tokens, no tool heartbeat) and nothing waiting on the user for this long gets
/// a "may be stuck" notice. Real data: the slowest model response seen was
/// ~140s; main-thread tools heartbeat every 30s.
pub const QUIET_NOTICE_AFTER: Duration = Duration::from_secs(10 * 60);
/// A single main-thread tool still running after this long gets a notice. It
/// keeps heartbeating, so it is alive — but a hung MCP call heartbeats forever
/// too, and only the user can tell a long deploy from a stuck one.
pub const LONG_TOOL_NOTICE_AFTER: Duration = Duration::from_secs(30 * 60);
/// A permission / question prompt open this long (awake time) gets a banner and
/// a fresh native notification. The prompt itself stays until the user answers
/// or stops the run — it is never answered for them.
pub const PERMISSION_NOTICE_AFTER: Duration = Duration::from_secs(10 * 60);
/// After the first reminder, re-fire the native notification this often.
pub const PERMISSION_REMIND_EVERY: Duration = Duration::from_secs(30 * 60);
/// Once the sidecar reported `_devdy_closed` (or its process exited) the
/// session is over; if stdout hasn't hit EOF within this grace, the process
/// group is killed. Definitive evidence, not a guess.
pub const SIDECAR_EXIT_GRACE: Duration = Duration::from_secs(10);
/// A gap this large between two evaluations means we were not watching (sleep,
/// App Nap, starved runtime): restart baselines instead of judging the sidecar.
const MAX_TICK_GAP: Duration = Duration::from_secs(60);

/// What the drain should do after feeding an event or a tick. There is
/// deliberately no variant that stops a run or answers a prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show a non-destructive notice (keyed so it can be cleared later).
    Notice(Notice),
    /// Remove a notice previously shown under this key.
    Clear { key: String },
    /// Re-alert the user (native notification) about a still-open prompt and let
    /// the UI re-sync the pending prompts from the backend.
    RemindPermission { request_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// The active turn has produced nothing for `mins` minutes.
    Quiet { mins: u64 },
    /// `tool_name` (main thread) has been running for `mins` minutes.
    LongTool {
        tool_use_id: String,
        tool_name: String,
        mins: u64,
    },
    /// The sidecar produced nothing — not even its heartbeat — for `secs`.
    Unresponsive { secs: u64 },
    /// The sidecar produced nothing at all within `secs` of being spawned.
    NotStarted { secs: u64 },
    /// A sidecar that can't report liveness (no heartbeat capability) has had
    /// an active turn silent for `mins` minutes: we can't tell busy from frozen.
    Unmonitored { mins: u64 },
    /// A permission / question prompt has been waiting `mins` minutes.
    PermissionWaiting {
        request_id: String,
        tool_name: String,
        mins: u64,
    },
}

pub const QUIET_KEY: &str = "quiet";
pub const UNRESPONSIVE_KEY: &str = "unresponsive";
pub const NOT_STARTED_KEY: &str = "not_started";
pub const UNMONITORED_KEY: &str = "unmonitored";

impl Notice {
    pub fn key(&self) -> String {
        match self {
            Notice::Quiet { .. } => QUIET_KEY.to_string(),
            Notice::LongTool { tool_use_id, .. } => format!("tool:{}", tool_use_id),
            Notice::Unresponsive { .. } => UNRESPONSIVE_KEY.to_string(),
            Notice::NotStarted { .. } => NOT_STARTED_KEY.to_string(),
            Notice::Unmonitored { .. } => UNMONITORED_KEY.to_string(),
            Notice::PermissionWaiting { request_id, .. } => format!("perm:{}", request_id),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Notice::Quiet { .. } => "quiet",
            Notice::LongTool { .. } => "long_tool",
            Notice::Unresponsive { .. } => "unresponsive",
            Notice::NotStarted { .. } => "not_started",
            Notice::Unmonitored { .. } => "unmonitored",
            Notice::PermissionWaiting { .. } => "permission_waiting",
        }
    }

    /// Parameters for the UI's localized rendering of the notice.
    pub fn params(&self) -> Value {
        match self {
            Notice::Quiet { mins } | Notice::Unmonitored { mins } => {
                serde_json::json!({ "mins": mins })
            }
            Notice::LongTool {
                tool_name, mins, ..
            }
            | Notice::PermissionWaiting {
                tool_name, mins, ..
            } => {
                serde_json::json!({ "mins": mins, "tool": tool_name })
            }
            Notice::Unresponsive { secs } | Notice::NotStarted { secs } => {
                serde_json::json!({ "mins": secs / 60, "secs": secs })
            }
        }
    }

    pub fn message(&self) -> String {
        match self {
            Notice::Quiet { mins } => format!(
                "No activity from the session for {} minutes. It may still be working (e.g. a long model response), or it may be stuck — stop it if it should have finished.",
                mins
            ),
            Notice::LongTool { tool_name, mins, .. } => format!(
                "{} has been running for {} minutes. Fine if it is a long job; stop the session if it should have finished.",
                tool_name, mins
            ),
            Notice::Unresponsive { secs } => format!(
                "The session bridge has not responded for {} minutes (not even its heartbeat). It may be frozen — stop the session if it doesn't recover.",
                secs / 60
            ),
            Notice::NotStarted { secs } => format!(
                "The session has not started after {} seconds. Stop it and try again if it doesn't come up.",
                secs
            ),
            Notice::Unmonitored { mins } => format!(
                "No activity from the session for {} minutes, and this session can't report whether it is still alive. It may be working or stuck — stop it if it should have finished.",
                mins
            ),
            Notice::PermissionWaiting { tool_name, mins, .. } => format!(
                "A permission request for {} has been waiting for your answer for {} minutes. The session is paused until you answer or stop it.",
                tool_name, mins
            ),
        }
    }
}

/// One open permission prompt as seen by the watchdog.
struct PermissionWait {
    /// Awake time this prompt has been open (sleep / tick gaps excluded).
    age: Duration,
    noticed: bool,
    next_remind: Duration,
}

/// Liveness state of one sidecar.
pub struct Liveness {
    spawned_at: Instant,
    /// Any line from the sidecar yet (ready, heartbeat, engine, stderr…).
    got_output: bool,
    /// Last stdout/stderr line of any kind (heartbeats included).
    last_output: Instant,
    /// Last message from the engine itself (heartbeats excluded).
    last_engine_event: Instant,
    /// The sidecar declared (or proved) that it heartbeats. Only then does its
    /// silence mean anything.
    heartbeat_expected: bool,
    last_eval: Option<Instant>,
    /// An `api_retry` announced a backoff; quiet notices wait until it passes.
    retry_grace_until: Option<Instant>,
    /// Keys of the notices currently shown.
    shown: HashSet<String>,
    /// Engine silence after which the next "unmonitored" warning is due
    /// (sidecars without a heartbeat only; repeats while the silence lasts).
    unmonitored_next: Duration,
    long_tools_noticed: HashSet<String>,
    permissions: HashMap<String, PermissionWait>,
}

impl Liveness {
    pub fn new(now: Instant) -> Self {
        Self {
            spawned_at: now,
            got_output: false,
            last_output: now,
            last_engine_event: now,
            heartbeat_expected: false,
            last_eval: Some(now),
            retry_grace_until: None,
            shown: HashSet::new(),
            unmonitored_next: QUIET_NOTICE_AFTER,
            long_tools_noticed: HashSet::new(),
            permissions: HashMap::new(),
        }
    }

    /// Mark the sidecar alive; clears the "not responding / not started" notices.
    fn mark_output(&mut self, now: Instant) -> Vec<Action> {
        self.last_output = now;
        self.got_output = true;
        let mut actions = Vec::new();
        for key in [UNRESPONSIVE_KEY, NOT_STARTED_KEY] {
            if self.shown.remove(key) {
                actions.push(Action::Clear {
                    key: key.to_string(),
                });
            }
        }
        actions
    }

    /// `_devdy_ready`. `heartbeat` is the sidecar's declared capability; a build
    /// without it is never judged on silence.
    pub fn on_ready(&mut self, now: Instant, heartbeat: bool) -> Vec<Action> {
        if heartbeat {
            self.heartbeat_expected = true;
        }
        self.mark_output(now)
    }

    /// The sidecar's own `_devdy_heartbeat`.
    pub fn on_heartbeat(&mut self, now: Instant) -> Vec<Action> {
        self.heartbeat_expected = true;
        self.mark_output(now)
    }

    /// Any other output that proves the sidecar alive but is not engine
    /// progress (stderr, sidecar control lines).
    pub fn on_output(&mut self, now: Instant) -> Vec<Action> {
        self.mark_output(now)
    }

    /// A raw engine message (stream-json from the CLI / codex bridge).
    pub fn on_engine_event(&mut self, now: Instant, v: &Value) -> Vec<Action> {
        let mut actions = self.mark_output(now);
        self.last_engine_event = now;
        self.unmonitored_next = QUIET_NOTICE_AFTER;
        for key in [QUIET_KEY, UNMONITORED_KEY] {
            if self.shown.remove(key) {
                actions.push(Action::Clear {
                    key: key.to_string(),
                });
            }
        }
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let subtype = v.get("subtype").and_then(|t| t.as_str()).unwrap_or("");
        match (kind, subtype) {
            ("system", "api_retry") => {
                let delay_ms = v
                    .get("retry_delay_ms")
                    .and_then(|d| d.as_f64())
                    .unwrap_or(0.0)
                    .max(0.0);
                self.retry_grace_until = Some(now + Duration::from_millis(delay_ms as u64));
            }
            ("tool_progress", _) => {
                // Heartbeat frames carry the real tool id in `parent_tool_use_id`
                // (their own id is `<id>-heartbeat-<n>`).
                let elapsed = v
                    .get("elapsed_time_seconds")
                    .and_then(|e| e.as_f64())
                    .unwrap_or(0.0);
                let id = v
                    .get("parent_tool_use_id")
                    .and_then(|p| p.as_str())
                    .or_else(|| v.get("tool_use_id").and_then(|p| p.as_str()));
                if let Some(id) = id {
                    if elapsed >= LONG_TOOL_NOTICE_AFTER.as_secs_f64()
                        && self.long_tools_noticed.insert(id.to_string())
                    {
                        actions.push(Action::Notice(Notice::LongTool {
                            tool_use_id: id.to_string(),
                            tool_name: v
                                .get("tool_name")
                                .and_then(|n| n.as_str())
                                .unwrap_or("A tool")
                                .to_string(),
                            mins: (elapsed as u64) / 60,
                        }));
                    }
                }
            }
            ("user", _) if !self.long_tools_noticed.is_empty() => {
                for id in tool_result_ids(v) {
                    if self.long_tools_noticed.remove(&id) {
                        actions.push(Action::Clear {
                            key: format!("tool:{}", id),
                        });
                    }
                }
            }
            // The turn is over: whatever it was running is too.
            ("result", _) => {
                for id in self.long_tools_noticed.drain() {
                    actions.push(Action::Clear {
                        key: format!("tool:{}", id),
                    });
                }
            }
            _ => {}
        }
        actions
    }

    /// Periodic evaluation.
    ///
    /// * `turn_active` — a main-thread turn is streaming (see
    ///   `is_main_turn_activity` in sidecar.rs);
    /// * `pending` — open permission / question prompts as `(request_id,
    ///   tool_name)` (the CLI pauses its heartbeats while parked on the user);
    /// * `quiet_notices` — whether engine silence is meaningful for this engine
    ///   (Claude heartbeats its tools; Codex does not).
    pub fn tick(
        &mut self,
        now: Instant,
        turn_active: bool,
        pending: &[(String, String)],
        quiet_notices: bool,
    ) -> Vec<Action> {
        let mut actions = Vec::new();

        // Awake time since the previous evaluation. A large gap is not counted:
        // we weren't watching, so nothing measured across it means anything.
        let prev = self.last_eval.replace(now);
        let step = match prev {
            Some(p) => {
                let gap = now.saturating_duration_since(p);
                if gap > MAX_TICK_GAP {
                    self.last_output = now;
                    self.last_engine_event = now;
                    if !self.got_output {
                        self.spawned_at = now;
                    }
                    Duration::ZERO
                } else {
                    gap
                }
            }
            None => Duration::ZERO,
        };

        self.tick_permissions(step, pending, &mut actions);
        let awaiting_user = !pending.is_empty();
        if awaiting_user {
            // The engine is parked on the user (the CLI even pauses its tool
            // heartbeats). That time is not engine silence: once answered, the
            // quiet clock must start from the answer, not from before the prompt.
            self.last_engine_event = now;
        }

        // Nothing at all since spawn.
        if !self.got_output
            && !self.shown.contains(NOT_STARTED_KEY)
            && now.saturating_duration_since(self.spawned_at) >= STARTUP_WINDOW
        {
            let secs = now.saturating_duration_since(self.spawned_at).as_secs();
            self.shown.insert(NOT_STARTED_KEY.to_string());
            actions.push(Action::Notice(Notice::NotStarted { secs }));
        }

        // A heartbeating sidecar that went fully silent.
        let silent = now.saturating_duration_since(self.last_output);
        if self.heartbeat_expected
            && !self.shown.contains(UNRESPONSIVE_KEY)
            && silent >= SIDECAR_SILENCE_LIMIT
        {
            self.shown.insert(UNRESPONSIVE_KEY.to_string());
            actions.push(Action::Notice(Notice::Unresponsive {
                secs: silent.as_secs(),
            }));
        }

        let quiet = now.saturating_duration_since(self.last_engine_event);
        let in_retry_backoff = self.retry_grace_until.is_some_and(|t| now < t);

        // A sidecar that can't report liveness: silence in an active turn is the
        // only hint we have, for every engine. Warn (never act), and repeat while
        // the silence lasts so it stays visible.
        if !self.heartbeat_expected {
            if self.got_output
                && turn_active
                && !awaiting_user
                && !in_retry_backoff
                && quiet >= self.unmonitored_next
            {
                self.shown.insert(UNMONITORED_KEY.to_string());
                self.unmonitored_next = quiet + QUIET_NOTICE_AFTER;
                actions.push(Action::Notice(Notice::Unmonitored {
                    mins: quiet.as_secs() / 60,
                }));
            } else if self.shown.contains(UNMONITORED_KEY) && (!turn_active || awaiting_user) {
                self.shown.remove(UNMONITORED_KEY);
                self.unmonitored_next = QUIET_NOTICE_AFTER;
                actions.push(Action::Clear {
                    key: UNMONITORED_KEY.to_string(),
                });
            }
            return actions;
        }

        // A quiet active turn (only meaningful while the bridge itself is alive).
        if quiet_notices
            && turn_active
            && !awaiting_user
            && !in_retry_backoff
            && !self.shown.contains(QUIET_KEY)
            && !self.shown.contains(UNRESPONSIVE_KEY)
            && quiet >= QUIET_NOTICE_AFTER
        {
            self.shown.insert(QUIET_KEY.to_string());
            actions.push(Action::Notice(Notice::Quiet {
                mins: quiet.as_secs() / 60,
            }));
        } else if self.shown.contains(QUIET_KEY) && (!turn_active || awaiting_user) {
            // About the active turn only; moot once nothing runs or the user is asked.
            self.shown.remove(QUIET_KEY);
            actions.push(Action::Clear {
                key: QUIET_KEY.to_string(),
            });
        }
        actions
    }

    fn tick_permissions(
        &mut self,
        step: Duration,
        pending: &[(String, String)],
        actions: &mut Vec<Action>,
    ) {
        // Prompts that are no longer open: drop them (and their banner).
        let open: HashSet<&str> = pending.iter().map(|(id, _)| id.as_str()).collect();
        let gone: Vec<String> = self
            .permissions
            .keys()
            .filter(|id| !open.contains(id.as_str()))
            .cloned()
            .collect();
        for id in gone {
            if let Some(w) = self.permissions.remove(&id) {
                if w.noticed {
                    actions.push(Action::Clear {
                        key: format!("perm:{}", id),
                    });
                }
            }
        }

        for (id, tool) in pending {
            let w = self
                .permissions
                .entry(id.clone())
                .or_insert(PermissionWait {
                    age: Duration::ZERO,
                    noticed: false,
                    next_remind: PERMISSION_NOTICE_AFTER,
                });
            w.age += step;
            if w.age < w.next_remind {
                continue;
            }
            if !w.noticed {
                w.noticed = true;
                actions.push(Action::Notice(Notice::PermissionWaiting {
                    request_id: id.clone(),
                    tool_name: tool.clone(),
                    mins: w.age.as_secs() / 60,
                }));
            }
            actions.push(Action::RemindPermission {
                request_id: id.clone(),
            });
            w.next_remind = w.age + PERMISSION_REMIND_EVERY;
        }
    }
}

/// `tool_use_id`s answered by the `tool_result` blocks of a `user` message.
fn tool_result_ids(v: &Value) -> Vec<String> {
    v.get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
                .filter_map(|b| b.get("tool_use_id").and_then(|i| i.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn perm(id: &str) -> Vec<(String, String)> {
        vec![(id.to_string(), "Bash".to_string())]
    }

    /// Drive `l` in watchdog ticks from `start` for `total`, optionally feeding a
    /// sidecar heartbeat every 30s; returns the end time and every action.
    fn run_ticks(
        l: &mut Liveness,
        start: Instant,
        total: Duration,
        heartbeat: bool,
        turn_active: bool,
        pending: &[(String, String)],
    ) -> (Instant, Vec<Action>) {
        let mut now = start;
        let mut out = Vec::new();
        let mut since_beat = Duration::ZERO;
        while now.saturating_duration_since(start) < total {
            now += WATCHDOG_TICK;
            since_beat += WATCHDOG_TICK;
            if heartbeat && since_beat >= secs(30) {
                out.extend(l.on_heartbeat(now));
                since_beat = Duration::ZERO;
            }
            out.extend(l.tick(now, turn_active, pending, true));
        }
        (now, out)
    }

    fn ready(t0: Instant) -> Liveness {
        let mut l = Liveness::new(t0);
        l.on_ready(t0, true);
        l.on_heartbeat(t0);
        l
    }

    fn notices(actions: &[Action]) -> Vec<&Notice> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Notice(n) => Some(n),
                _ => None,
            })
            .collect()
    }

    // A background subagent running a long command: no engine output at all for
    // an hour, no turn active, sidecar heartbeating → nothing happens.
    #[test]
    fn long_silent_background_work_produces_nothing() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let (_, actions) = run_ticks(&mut l, t0, secs(3600), true, false, &[]);
        assert!(actions.is_empty(), "{actions:?}");
    }

    // Inside an active turn, silence only ever produces ONE notice.
    #[test]
    fn quiet_active_turn_gets_one_notice() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let (_, actions) = run_ticks(&mut l, t0, secs(3600), true, true, &[]);
        assert_eq!(actions, vec![Action::Notice(Notice::Quiet { mins: 10 })]);
    }

    #[test]
    fn quiet_notice_clears_on_next_engine_event() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let (now, _) = run_ticks(&mut l, t0, secs(11 * 60), true, true, &[]);
        let actions = l.on_engine_event(now, &json!({"type": "assistant", "message": {}}));
        assert_eq!(
            actions,
            vec![Action::Clear {
                key: QUIET_KEY.into()
            }]
        );
    }

    // An announced API backoff postpones the quiet notice until it has elapsed.
    #[test]
    fn api_retry_backoff_defers_quiet_notice() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        l.on_engine_event(
            t0,
            &json!({"type": "system", "subtype": "api_retry", "retry_delay_ms": 15.0 * 60.0 * 1000.0}),
        );
        let (now, actions) = run_ticks(&mut l, t0, secs(14 * 60), true, true, &[]);
        assert!(actions.is_empty(), "{actions:?}");
        let (_, actions) = run_ticks(&mut l, now, secs(2 * 60), true, true, &[]);
        assert!(matches!(
            notices(&actions).as_slice(),
            [Notice::Quiet { .. }]
        ));
    }

    // The sidecar's heartbeat stops → a notice (never a kill), cleared as soon
    // as it is heard from again.
    #[test]
    fn missing_heartbeat_is_a_notice_and_recovers() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let (now, actions) = run_ticks(&mut l, t0, secs(30 * 60), false, true, &[]);
        assert_eq!(notices(&actions).len(), 1, "{actions:?}");
        assert!(matches!(notices(&actions)[0], Notice::Unresponsive { secs } if *secs >= 180));
        // While unresponsive, no separate quiet notice piles on.
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Notice(Notice::Quiet { .. }))));
        assert_eq!(
            l.on_heartbeat(now + secs(1)),
            vec![Action::Clear {
                key: UNRESPONSIVE_KEY.into()
            }]
        );
    }

    // A sidecar that can't heartbeat is never flagged "unresponsive", and an
    // idle one (no active turn) produces nothing at all.
    #[test]
    fn legacy_sidecar_idle_produces_nothing() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        l.on_ready(t0, false);
        let (_, actions) = run_ticks(&mut l, t0, secs(3600), false, false, &[]);
        assert!(actions.is_empty(), "{actions:?}");
    }

    // …but a silent ACTIVE turn on it gets a repeating "can't monitor" warning
    // (every 10 min of silence, any engine), cleared by the next engine event.
    #[test]
    fn legacy_sidecar_silent_turn_warns_periodically() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        l.on_ready(t0, false);
        // quiet_notices = true here, but the legacy rule applies regardless.
        let (now, actions) = run_ticks(&mut l, t0, secs(35 * 60), false, true, &[]);
        assert_eq!(
            actions,
            vec![
                Action::Notice(Notice::Unmonitored { mins: 10 }),
                Action::Notice(Notice::Unmonitored { mins: 20 }),
                Action::Notice(Notice::Unmonitored { mins: 30 }),
            ]
        );
        assert_eq!(
            l.on_engine_event(now, &json!({"type": "assistant", "message": {}})),
            vec![Action::Clear {
                key: UNMONITORED_KEY.into()
            }]
        );
        // Not while waiting on the user.
        let (_, actions) = run_ticks(&mut l, now, secs(30 * 60), false, true, &perm("p"));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Notice(Notice::Unmonitored { .. }))));
    }

    // Nothing at all after spawn → "not started" notice at the startup window,
    // cleared by the first line.
    #[test]
    fn startup_window_notice() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        let (now, actions) = run_ticks(&mut l, t0, STARTUP_WINDOW, false, false, &[]);
        assert!(
            matches!(notices(&actions).as_slice(), [Notice::NotStarted { secs }] if *secs >= 90)
        );
        assert_eq!(
            l.on_ready(now, true),
            vec![Action::Clear {
                key: NOT_STARTED_KEY.into()
            }]
        );
    }

    #[test]
    fn no_startup_notice_once_the_sidecar_spoke() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        l.on_heartbeat(t0 + secs(20));
        let (_, actions) = run_ticks(&mut l, t0, secs(120), true, false, &[]);
        assert!(actions.is_empty(), "{actions:?}");
    }

    // System sleep / a starved drain: the gap is not read as silence.
    #[test]
    fn tick_gap_rebaselines_heartbeat_silence() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        assert!(l.tick(t0 + WATCHDOG_TICK, false, &[], true).is_empty());
        // Next evaluation 2 hours later with no output in between (laptop slept).
        assert!(l.tick(t0 + secs(7200), false, &[], true).is_empty());
        // Heartbeats resume right after wake → still nothing.
        let (_, actions) = run_ticks(&mut l, t0 + secs(7200), secs(600), true, false, &[]);
        assert!(actions.is_empty(), "{actions:?}");
    }

    // A pending prompt is never answered: after the notice threshold it gets a
    // banner + reminder, then only reminders, and the banner clears once answered.
    #[test]
    fn permission_wait_notice_and_reminders() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let p = perm("req-1");
        let (now, actions) = run_ticks(&mut l, t0, secs(9 * 60), true, true, &p);
        assert!(actions.is_empty(), "{actions:?}");
        let (now, actions) = run_ticks(&mut l, now, secs(60), true, true, &p);
        assert_eq!(
            actions,
            vec![
                Action::Notice(Notice::PermissionWaiting {
                    request_id: "req-1".into(),
                    tool_name: "Bash".into(),
                    mins: 10
                }),
                Action::RemindPermission {
                    request_id: "req-1".into()
                },
            ]
        );
        // 2 more hours: one reminder per 30 minutes, no extra banners.
        let (now, actions) = run_ticks(&mut l, now, secs(2 * 3600), true, true, &p);
        assert_eq!(actions.len(), 4, "{actions:?}");
        assert!(actions
            .iter()
            .all(|a| matches!(a, Action::RemindPermission { .. })));
        // Answered → banner cleared.
        let (_, actions) = run_ticks(&mut l, now, WATCHDOG_TICK, true, true, &[]);
        assert_eq!(
            actions,
            vec![Action::Clear {
                key: "perm:req-1".into()
            }]
        );
    }

    // Waiting age is awake time: a sleep in the middle doesn't count.
    #[test]
    fn permission_wait_pauses_across_sleep() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let p = perm("req-2");
        let (now, actions) = run_ticks(&mut l, t0, secs(5 * 60), true, true, &p);
        assert!(actions.is_empty());
        // Laptop sleeps 3 hours (one evaluation after a huge gap).
        let woke = now + secs(3 * 3600);
        assert!(l.tick(woke, true, &p, true).is_empty());
        // 4 more awake minutes: 9 total → still nothing.
        let (now, actions) = run_ticks(&mut l, woke, secs(4 * 60), true, true, &p);
        assert!(actions.is_empty(), "{actions:?}");
        // Past 10 awake minutes → notice.
        let (_, actions) = run_ticks(&mut l, now, secs(75), true, true, &p);
        assert!(matches!(
            notices(&actions).as_slice(),
            [Notice::PermissionWaiting { .. }]
        ));
    }

    // Waiting on the user is not "quiet".
    #[test]
    fn no_quiet_notice_while_awaiting_user() {
        let t0 = Instant::now();
        let mut l = ready(t0);
        let (_, actions) = run_ticks(&mut l, t0, secs(3600), true, true, &perm("q"));
        assert!(!actions
            .iter()
            .any(|a| matches!(a, Action::Notice(Notice::Quiet { .. }))));
    }

    #[test]
    fn long_tool_notice_once_then_cleared_by_its_result() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        let beat = |elapsed: u64| {
            json!({
                "type": "tool_progress", "tool_name": "Bash",
                "tool_use_id": format!("toolu_1-heartbeat-{}", elapsed / 30),
                "parent_tool_use_id": "toolu_1", "elapsed_time_seconds": elapsed, "heartbeat": true,
            })
        };
        assert!(l.on_engine_event(t0, &beat(600)).is_empty());
        assert_eq!(
            l.on_engine_event(t0, &beat(1800)),
            vec![Action::Notice(Notice::LongTool {
                tool_use_id: "toolu_1".into(),
                tool_name: "Bash".into(),
                mins: 30
            })]
        );
        // Only once per tool.
        assert!(l.on_engine_event(t0, &beat(1830)).is_empty());
        let done = json!({"type": "user", "message": {"content": [
            {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}
        ]}});
        assert_eq!(
            l.on_engine_event(t0, &done),
            vec![Action::Clear {
                key: "tool:toolu_1".into()
            }]
        );
    }

    #[test]
    fn turn_end_clears_long_tool_notices() {
        let t0 = Instant::now();
        let mut l = Liveness::new(t0);
        l.on_engine_event(
            t0,
            &json!({"type": "tool_progress", "tool_name": "mcp__x__deploy",
                    "parent_tool_use_id": "toolu_9", "elapsed_time_seconds": 1900}),
        );
        assert_eq!(
            l.on_engine_event(t0, &json!({"type": "result", "subtype": "success"})),
            vec![Action::Clear {
                key: "tool:toolu_9".into()
            }]
        );
    }
}
