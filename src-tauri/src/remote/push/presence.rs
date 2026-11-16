//! Whether the user is at the desktop right now. While they are, the desktop
//! already shows its own notifications, so the phone is not pushed (the
//! `away_only` switch, on by default).

use std::time::Duration;

/// No keyboard / mouse / trackpad input for this long counts as away.
pub const AWAY_AFTER: Duration = Duration::from_secs(120);

/// Pure rule: at the desktop while the screen is unlocked and there was input
/// within [`AWAY_AFTER`].
pub fn in_use(screen_locked: bool, idle: Duration) -> bool {
    !screen_locked && idle < AWAY_AFTER
}

/// Time since the last user input event on this Mac (any kind).
#[cfg(target_os = "macos")]
fn input_idle() -> Duration {
    use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};
    // `kCGAnyInputEventType`.
    let any = CGEventType(u32::MAX);
    let secs = CGEventSource::seconds_since_last_event_type(CGEventSourceStateID::HIDSystemState, any);
    Duration::try_from_secs_f64(secs).unwrap_or(Duration::MAX)
}

/// Whether the user is at the desktop. Off macOS there is no reliable signal,
/// so the user always counts as away (the phone keeps being notified).
#[cfg(target_os = "macos")]
pub fn desktop_in_use() -> bool {
    in_use(crate::secret_vault::screen_locked(), input_idle())
}

#[cfg(not(target_os = "macos"))]
pub fn desktop_in_use() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_input_on_an_unlocked_screen_means_at_the_desktop() {
        assert!(in_use(false, Duration::from_secs(5)));
        assert!(!in_use(false, AWAY_AFTER), "idle long enough");
        assert!(!in_use(true, Duration::ZERO), "a locked screen is away");
    }
}
