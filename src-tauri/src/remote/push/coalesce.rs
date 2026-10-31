//! Pure Web Push decision logic: which run events count, how a burst of them
//! collapses into one notification, what each device receives, and when to
//! stay quiet. No I/O — the dispatcher in `mod.rs` feeds it.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// How long the first event of a burst waits for company before sending.
pub const COALESCE_WINDOW: Duration = Duration::from_secs(10);

/// The kinds of event a device can be notified about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushKind {
    /// A permission prompt / AskUserQuestion / broker approval is waiting.
    Pending,
    /// A run finished, or a multi-turn session finished its turn and waits.
    Done,
    /// A run failed.
    Failed,
}

impl PushKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PushKind::Pending => "pending",
            PushKind::Done => "done",
            PushKind::Failed => "failed",
        }
    }
}

/// Map a `run:done` status to a kind. A cancel is the user's own doing — not
/// worth a notification.
pub fn kind_of_done(status: &str) -> Option<PushKind> {
    match status {
        "done" => Some(PushKind::Done),
        "failed" => Some(PushKind::Failed),
        _ => None,
    }
}

/// Which kinds are switched on (desktop toggles, and per device).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushEvents {
    pub pending: bool,
    pub done: bool,
    pub failed: bool,
}

impl Default for PushEvents {
    fn default() -> Self {
        PushEvents { pending: true, done: true, failed: true }
    }
}

impl PushEvents {
    pub fn has(&self, kind: PushKind) -> bool {
        match kind {
            PushKind::Pending => self.pending,
            PushKind::Done => self.done,
            PushKind::Failed => self.failed,
        }
    }

    /// Both switched on.
    pub fn and(self, other: PushEvents) -> PushEvents {
        PushEvents {
            pending: self.pending && other.pending,
            done: self.done && other.done,
            failed: self.failed && other.failed,
        }
    }

    /// From the `events` column: a comma list of kind names.
    pub fn from_list(list: &str) -> PushEvents {
        let has = |k: &str| list.split(',').any(|x| x.trim() == k);
        PushEvents { pending: has("pending"), done: has("done"), failed: has("failed") }
    }

    pub fn to_list(self) -> String {
        [PushKind::Pending, PushKind::Done, PushKind::Failed]
            .into_iter()
            .filter(|k| self.has(*k))
            .map(PushKind::as_str)
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// What happened during one burst.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BurstCounts {
    /// New prompts seen (the notification reports how many are STILL open).
    pub new_pending: u32,
    pub done: u32,
    pub failed: u32,
}

/// Collapses events into bursts: the first event opens a window, later ones
/// join it, and the burst is handed out once the window has elapsed.
#[derive(Debug)]
pub struct Coalescer {
    window: Duration,
    burst: Option<(Instant, BurstCounts)>,
}

impl Coalescer {
    pub fn new(window: Duration) -> Self {
        Coalescer { window, burst: None }
    }

    pub fn note(&mut self, kind: PushKind, now: Instant) {
        let (_, c) = self.burst.get_or_insert((now + self.window, BurstCounts::default()));
        match kind {
            PushKind::Pending => c.new_pending += 1,
            PushKind::Done => c.done += 1,
            PushKind::Failed => c.failed += 1,
        }
    }

    /// When the open burst is due, if any.
    pub fn deadline(&self) -> Option<Instant> {
        self.burst.map(|(d, _)| d)
    }

    /// The burst, once due (it is then closed).
    pub fn take_due(&mut self, now: Instant) -> Option<BurstCounts> {
        match self.burst {
            Some((deadline, counts)) if now >= deadline => {
                self.burst = None;
                Some(counts)
            }
            _ => None,
        }
    }
}

/// The encrypted body's plaintext. Counts only — never a tool, command,
/// project or title; the service worker words it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PushMessage {
    pub v: u8,
    pub t: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed: Option<u32>,
}

impl PushMessage {
    pub fn test() -> Self {
        PushMessage { v: 1, t: "test", pending: None, done: None, failed: None }
    }
}

/// The message one device gets for a burst, or `None` when nothing it opted
/// into happened. `open_pending` is how many prompts are open NOW: a burst of
/// prompts all answered before the window closed sends nothing.
pub fn compose(counts: BurstCounts, open_pending: u32, events: PushEvents) -> Option<PushMessage> {
    let pending = if events.pending { open_pending } else { 0 };
    let done = if events.done { counts.done } else { 0 };
    let failed = if events.failed { counts.failed } else { 0 };
    let triggered = (counts.new_pending > 0 && pending > 0) || done > 0 || failed > 0;
    if !triggered {
        return None;
    }
    Some(PushMessage { v: 1, t: "summary", pending: Some(pending), done: Some(done), failed: Some(failed) })
}

/// Stay quiet while a Controller is connected and on screen: it already shows
/// everything live. A phone that reported itself hidden (screen locked, app in
/// the background) still gets notified.
pub fn suppressed(controller_connected: bool, controller_visible: bool) -> bool {
    controller_connected && controller_visible
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: PushEvents = PushEvents { pending: true, done: true, failed: true };

    #[test]
    fn a_burst_waits_for_the_window_then_closes() {
        let t0 = Instant::now();
        let mut c = Coalescer::new(COALESCE_WINDOW);
        assert_eq!(c.deadline(), None);
        c.note(PushKind::Pending, t0);
        c.note(PushKind::Done, t0 + Duration::from_secs(4));
        c.note(PushKind::Pending, t0 + Duration::from_secs(9));
        // The window does not slide: it is anchored on the first event.
        assert_eq!(c.deadline(), Some(t0 + COALESCE_WINDOW));
        assert_eq!(c.take_due(t0 + Duration::from_secs(9)), None);
        assert_eq!(
            c.take_due(t0 + COALESCE_WINDOW),
            Some(BurstCounts { new_pending: 2, done: 1, failed: 0 })
        );
        assert_eq!(c.deadline(), None, "one notification per burst");
        assert_eq!(c.take_due(t0 + Duration::from_secs(60)), None);
        c.note(PushKind::Failed, t0 + Duration::from_secs(61));
        assert_eq!(c.deadline(), Some(t0 + Duration::from_secs(61) + COALESCE_WINDOW));
    }

    #[test]
    fn done_status_maps_to_a_kind() {
        assert_eq!(kind_of_done("done"), Some(PushKind::Done));
        assert_eq!(kind_of_done("failed"), Some(PushKind::Failed));
        assert_eq!(kind_of_done("cancelled"), None);
    }

    #[test]
    fn compose_reports_counts_only() {
        let m = compose(BurstCounts { new_pending: 1, done: 2, failed: 0 }, 3, ALL).unwrap();
        assert_eq!(
            serde_json::to_value(&m).unwrap(),
            serde_json::json!({ "v": 1, "t": "summary", "pending": 3, "done": 2, "failed": 0 })
        );
    }

    #[test]
    fn prompts_answered_before_the_window_closed_send_nothing() {
        assert_eq!(compose(BurstCounts { new_pending: 2, done: 0, failed: 0 }, 0, ALL), None);
    }

    #[test]
    fn a_done_burst_still_mentions_open_prompts() {
        let m = compose(BurstCounts { new_pending: 0, done: 1, failed: 0 }, 2, ALL).unwrap();
        assert_eq!((m.pending, m.done), (Some(2), Some(1)));
    }

    #[test]
    fn switched_off_kinds_neither_trigger_nor_show() {
        let only_failed = PushEvents { pending: false, done: false, failed: true };
        assert_eq!(compose(BurstCounts { new_pending: 1, done: 3, failed: 0 }, 1, only_failed), None);
        let m = compose(BurstCounts { new_pending: 1, done: 3, failed: 1 }, 1, only_failed).unwrap();
        assert_eq!((m.pending, m.done, m.failed), (Some(0), Some(0), Some(1)));
    }

    #[test]
    fn device_and_desktop_switches_combine() {
        let device = PushEvents::from_list("pending,failed");
        let desktop = PushEvents { pending: true, done: true, failed: false };
        assert_eq!(device.and(desktop), PushEvents { pending: true, done: false, failed: false });
        assert_eq!(PushEvents::default().to_list(), "pending,done,failed");
        assert_eq!(PushEvents::from_list(" done ,bogus"), PushEvents { pending: false, done: true, failed: false });
    }

    #[test]
    fn only_a_connected_visible_controller_silences_pushes() {
        assert!(suppressed(true, true));
        assert!(!suppressed(true, false), "phone reported hidden");
        assert!(!suppressed(false, true));
        assert!(!suppressed(false, false));
    }

    #[test]
    fn test_message_has_no_counts() {
        assert_eq!(serde_json::to_value(PushMessage::test()).unwrap(), serde_json::json!({ "v": 1, "t": "test" }));
    }
}
