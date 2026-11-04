//! Host presence: which authenticated Hosts are online, for `list_hosts`.
//!
//! A Host opts in by sending `alias` / `app_version` on `register_host` (older
//! Hosts send neither and are never listed). The relay keeps exactly that plus
//! the `device_id` it already had — nothing else — and only for as long as the
//! connection lives. Only a client holding the host token may read the list.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::room::ConnId;

/// Longest `device_id` / `alias` / `app_version` the relay stores (bytes).
pub const MAX_FIELD_LEN: usize = 64;

/// One online Host, as returned by `list_hosts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPresence {
    pub device_id: String,
    pub alias: String,
    pub app_version: String,
}

/// Online Hosts by connection.
#[derive(Default)]
pub struct PresenceRegistry {
    by_conn: HashMap<ConnId, HostPresence>,
}

impl PresenceRegistry {
    /// Record (or, with `None`, withdraw) what `conn` announces. A Host
    /// re-registers on the same socket when its alias or capabilities change.
    pub fn set(&mut self, conn: ConnId, presence: Option<HostPresence>) {
        match presence.filter(|p| !p.device_id.is_empty()) {
            Some(p) => {
                self.by_conn.insert(conn, p);
            }
            None => {
                self.by_conn.remove(&conn);
            }
        }
    }

    pub fn remove(&mut self, conn: ConnId) {
        self.by_conn.remove(&conn);
    }

    /// Online Hosts except `exclude_device`, one entry per device (its newest
    /// connection wins over a stale socket that has not timed out yet), sorted
    /// by alias then device id.
    pub fn list(&self, exclude_device: Option<&str>) -> Vec<HostPresence> {
        let mut newest: HashMap<&str, (ConnId, &HostPresence)> = HashMap::new();
        for (conn, p) in &self.by_conn {
            if exclude_device.is_some_and(|d| d == p.device_id) {
                continue;
            }
            let slot = newest.entry(p.device_id.as_str()).or_insert((*conn, p));
            if *conn > slot.0 {
                *slot = (*conn, p);
            }
        }
        let mut out: Vec<HostPresence> = newest.into_values().map(|(_, p)| p.clone()).collect();
        out.sort_by(|a, b| {
            a.alias
                .cmp(&b.alias)
                .then_with(|| a.device_id.cmp(&b.device_id))
        });
        out
    }
}

/// Cap a client-supplied field to [`MAX_FIELD_LEN`] bytes on a char boundary
/// and drop control characters, so the list never carries anything surprising.
pub fn clean_field(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_FIELD_LEN {
            break;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(device: &str, alias: &str) -> HostPresence {
        HostPresence {
            device_id: device.into(),
            alias: alias.into(),
            app_version: "1.0.0".into(),
        }
    }

    #[test]
    fn list_excludes_the_caller_and_dedupes_by_device() {
        let mut reg = PresenceRegistry::default();
        reg.set(1, Some(p("a", "old-name")));
        reg.set(2, Some(p("b", "mini")));
        reg.set(3, Some(p("a", "air")));
        let all = reg.list(None);
        assert_eq!(
            all,
            vec![p("a", "air"), p("b", "mini")],
            "newest socket of a wins"
        );
        assert_eq!(reg.list(Some("a")), vec![p("b", "mini")]);
    }

    #[test]
    fn withdrawing_or_disconnecting_unlists() {
        let mut reg = PresenceRegistry::default();
        reg.set(1, Some(p("a", "air")));
        reg.set(2, Some(p("b", "mini")));
        reg.set(1, None);
        reg.remove(2);
        assert!(reg.list(None).is_empty());
        reg.set(3, Some(p("", "no-id")));
        assert!(
            reg.list(None).is_empty(),
            "a host without device id is never listed"
        );
    }

    #[test]
    fn fields_are_capped_and_cleaned() {
        assert_eq!(clean_field("  mac\u{7}-mini \n"), "mac-mini");
        assert_eq!(clean_field(&"x".repeat(100)).len(), MAX_FIELD_LEN);
        let multibyte = "é".repeat(40);
        let c = clean_field(&multibyte);
        assert!(c.len() <= MAX_FIELD_LEN && c.chars().all(|ch| ch == 'é'));
    }
}
