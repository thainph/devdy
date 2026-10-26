//! One-shot `ps` snapshot of every process on the machine.
//!
//! The tracker identifies a process by `(pid, start stamp)` so a pid the OS
//! recycled after a restart is never mistaken for one Devdy started. Reading
//! other processes' environment (`ps -E`) is not possible on recent macOS, so
//! descendants are found through the parent / process-group links sampled here.

use std::process::Command;

#[derive(Debug, Clone, PartialEq)]
pub struct PsRow {
    pub pid: u32,
    pub ppid: u32,
    pub pgid: u32,
    /// `lstart` exactly as `ps` prints it, whitespace-normalized
    /// ("Mon Oct 26 09:25:52 2026"). Used as the identity stamp.
    pub start: String,
    pub rss_kb: u64,
    pub cpu: f32,
    /// Process state (`stat`); a leading `Z` marks a zombie.
    pub stat: String,
    pub command: String,
}

impl PsRow {
    pub fn is_zombie(&self) -> bool {
        self.stat.starts_with('Z')
    }
}

const PS_FORMAT: &str = "pid=,ppid=,pgid=,lstart=,rss=,%cpu=,stat=,command=";

/// Every process currently running, or `None` when `ps` could not be run (the
/// caller must then keep its previous view instead of treating everything as
/// dead).
pub fn snapshot() -> Option<Vec<PsRow>> {
    #[cfg(unix)]
    {
        let bin = if std::path::Path::new("/bin/ps").exists() {
            "/bin/ps"
        } else {
            "ps"
        };
        let out = Command::new(bin)
            .args(["-axww", "-o", PS_FORMAT])
            .env("LC_ALL", "C")
            // UTC: the start stamp is our identity key and must not change
            // with a time-zone / DST change during the session.
            .env("TZ", "UTC")
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(parse(&String::from_utf8_lossy(&out.stdout)))
    }
    #[cfg(not(unix))]
    {
        let _ = Command::new;
        None
    }
}

pub fn parse(text: &str) -> Vec<PsRow> {
    text.lines().filter_map(parse_line).collect()
}

/// Split off the next whitespace-separated token.
fn take<'a>(s: &mut &'a str) -> Option<&'a str> {
    let t = s.trim_start();
    if t.is_empty() {
        return None;
    }
    let end = t.find(char::is_whitespace).unwrap_or(t.len());
    let (tok, rest) = t.split_at(end);
    *s = rest;
    Some(tok)
}

fn parse_line(line: &str) -> Option<PsRow> {
    let mut rest = line;
    let pid = take(&mut rest)?.parse().ok()?;
    let ppid = take(&mut rest)?.parse().ok()?;
    let pgid = take(&mut rest)?.parse().ok()?;
    let mut start_parts = Vec::with_capacity(5);
    for _ in 0..5 {
        start_parts.push(take(&mut rest)?);
    }
    let rss_kb = take(&mut rest)?.parse().ok()?;
    let cpu = take(&mut rest)?.replace(',', ".").parse().ok()?;
    let stat = take(&mut rest)?.to_string();
    Some(PsRow {
        pid,
        ppid,
        pgid,
        start: start_parts.join(" "),
        rss_kb,
        cpu,
        stat,
        command: rest.trim().to_string(),
    })
}

/// `lstart` (printed in UTC, see [`snapshot`]) as RFC 3339, for display.
pub fn start_to_rfc3339(start: &str) -> Option<String> {
    let ndt = chrono::NaiveDateTime::parse_from_str(start, "%a %b %e %H:%M:%S %Y").ok()?;
    Some(ndt.and_utc().to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rows_with_spaces_in_date_and_command() {
        let text = "  1     0     1 Mon Oct 26 09:25:52 2026      21568   0.0 Ss   /sbin/launchd\n\
                    4242  4200  4242 Tue Oct  6 10:01:02 2026   1234   12.5 S+   node /x/sidecar/index.mjs --flag a b\n\
                    garbage line\n";
        let rows = parse(text);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].pid, 1);
        assert_eq!(rows[0].command, "/sbin/launchd");
        assert_eq!(rows[1].pgid, 4242);
        assert_eq!(rows[1].start, "Tue Oct 6 10:01:02 2026");
        assert_eq!(rows[1].rss_kb, 1234);
        assert!((rows[1].cpu - 12.5).abs() < f32::EPSILON);
        assert_eq!(rows[1].command, "node /x/sidecar/index.mjs --flag a b");
        assert!(!rows[1].is_zombie());
    }

    #[test]
    fn detects_zombies() {
        let rows = parse("10 1 10 Mon Oct 26 09:25:52 2026 0 0.0 Z (node)\n");
        assert!(rows[0].is_zombie());
    }

    #[test]
    fn converts_start_stamp() {
        assert!(start_to_rfc3339("Tue Oct 6 10:01:02 2026").is_some());
        assert!(start_to_rfc3339("Mon Oct 26 09:25:52 2026").is_some());
        assert!(start_to_rfc3339("nonsense").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn live_snapshot_contains_self() {
        let rows = snapshot().expect("ps runs");
        let me = std::process::id();
        assert!(rows.iter().any(|r| r.pid == me));
    }
}
