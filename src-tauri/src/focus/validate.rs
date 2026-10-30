//! Todo field checks (spec BR-01, pure). `sidecar-mcp/lib/store.mjs` applies
//! the same rules to MCP writes.

pub const STATUSES: [&str; 3] = ["todo", "doing", "waiting"];
pub const ESTIMATE_RANGE: std::ops::RangeInclusive<i64> = 5..=2400;
/// Recorded actual time: up to ten 24 h days.
pub const ACTUAL_RANGE: std::ops::RangeInclusive<i64> = 1..=14_400;

/// Trimmed, non-empty. Long text is kept whole (the UI clips the title).
pub fn text(s: &str) -> Result<String, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("text is required".into());
    }
    Ok(s.to_string())
}

/// `YYYY-MM-DD` (blank = none).
pub fn due_date(s: Option<&str>) -> Result<Option<String>, String> {
    match s.map(str::trim) {
        None | Some("") => Ok(None),
        Some(d) => Ok(Some(
            chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d")
                .map_err(|_| format!("invalid due date: {d}"))?
                .format("%Y-%m-%d")
                .to_string(),
        )),
    }
}

/// 24 h `HH:MM` (blank = none).
pub fn due_time(s: Option<&str>) -> Result<Option<String>, String> {
    match s.map(str::trim) {
        None | Some("") => Ok(None),
        Some(t) => {
            let ok = t.len() == 5 && chrono::NaiveTime::parse_from_str(t, "%H:%M").is_ok();
            if ok {
                Ok(Some(t.to_string()))
            } else {
                Err(format!("invalid due_time (expected HH:MM): {t}"))
            }
        }
    }
}

pub fn priority(p: i64) -> Result<i64, String> {
    if (1..=3).contains(&p) {
        Ok(p)
    } else {
        Err("priority must be 1, 2 or 3".into())
    }
}

pub fn estimate(m: i64) -> Result<i64, String> {
    if ESTIMATE_RANGE.contains(&m) {
        Ok(m)
    } else {
        Err("estimate_min must be between 5 and 2400 minutes".into())
    }
}

pub fn actual(m: i64) -> Result<i64, String> {
    if ACTUAL_RANGE.contains(&m) {
        Ok(m)
    } else {
        Err("actual_min must be between 1 and 14400 minutes".into())
    }
}

pub fn status(s: &str) -> Result<String, String> {
    let s = s.trim();
    if STATUSES.contains(&s) {
        Ok(s.to_string())
    } else {
        Err("status must be todo, doing or waiting".into())
    }
}

/// RFC3339 instant (blank = none), normalised.
pub fn instant(key: &str, s: Option<&str>) -> Result<Option<String>, String> {
    match s.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) => chrono::DateTime::parse_from_rfc3339(v)
            .map(|d| Some(d.to_rfc3339()))
            .map_err(|_| format!("{key} must be an RFC3339 date-time")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields() {
        assert_eq!(text("  a "), Ok("a".into()));
        assert!(text(" ").is_err());
        assert_eq!(due_date(Some(" 2026-10-30 ")), Ok(Some("2026-10-30".into())));
        assert_eq!(due_date(Some("")), Ok(None));
        assert!(due_date(Some("2026-13-01")).is_err());
        assert_eq!(due_time(Some("23:59")), Ok(Some("23:59".into())));
        assert!(due_time(Some("24:00")).is_err());
        assert!(due_time(Some("9:5")).is_err());
        assert!(priority(0).is_err() && priority(2).is_ok());
        assert!(estimate(5).is_ok() && estimate(2400).is_ok() && estimate(2401).is_err());
        assert!(status("doing").is_ok() && status("done").is_err());
        assert_eq!(
            instant("x", Some("2026-10-28T14:00:00Z")),
            Ok(Some("2026-10-28T14:00:00+00:00".into()))
        );
        assert!(instant("x", Some("2026-10-28")).is_err());
    }
}
