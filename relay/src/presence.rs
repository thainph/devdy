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

/// What one registered Host connection announced.
struct HostEntry {
    device_id: String,
    /// Random per app launch (newer Hosts); see `Envelope::instance_id`.
    instance_id: Option<String>,
    /// `Some` when the Host opted into `list_hosts`.
    listed: Option<HostPresence>,
}

/// Outcome of [`PresenceRegistry::register`].
#[derive(Debug, PartialEq, Eq)]
pub enum Registration {
    /// Registered. `alias_conflict`: another listed device uses the same alias.
    Accepted { alias_conflict: Option<String> },
    /// Another live connection of a different app instance has this device id:
    /// refused, nothing recorded.
    DeviceIdConflict,
}

/// Registered Hosts by connection (listed or not).
#[derive(Default)]
pub struct PresenceRegistry {
    by_conn: HashMap<ConnId, HostEntry>,
}

/// Aliases compare trimmed and case-insensitively.
fn same_alias(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    !a.is_empty() && a.to_lowercase() == b.to_lowercase()
}

impl PresenceRegistry {
    /// Record what `conn` announces on `register_host` (a Host re-registers on
    /// the same socket when its alias or capabilities change).
    ///
    /// Another connection with the same `device_id` is refused only when both
    /// carry an `instance_id` and they differ: the same instance is the same
    /// app reconnecting before its old socket timed out, and a Host without an
    /// instance id (older app) keeps the old "newest wins" behavior.
    pub fn register(
        &mut self,
        conn: ConnId,
        device_id: &str,
        instance_id: Option<&str>,
        presence: Option<HostPresence>,
    ) -> Registration {
        let instance_id = instance_id.filter(|i| !i.is_empty());
        if !device_id.is_empty() {
            let clash = self.by_conn.iter().any(|(c, e)| {
                *c != conn
                    && e.device_id == device_id
                    && matches!((&e.instance_id, instance_id), (Some(a), Some(b)) if a != b)
            });
            if clash {
                return Registration::DeviceIdConflict;
            }
        }
        let listed = presence.filter(|p| !p.device_id.is_empty());
        let alias_conflict = listed.as_ref().and_then(|p| {
            self.by_conn
                .iter()
                .filter(|(c, _)| **c != conn)
                .filter_map(|(_, e)| e.listed.as_ref())
                .any(|other| other.device_id != p.device_id && same_alias(&other.alias, &p.alias))
                .then(|| p.alias.clone())
        });
        self.by_conn.insert(
            conn,
            HostEntry {
                device_id: device_id.to_string(),
                instance_id: instance_id.map(str::to_string),
                listed,
            },
        );
        Registration::Accepted { alias_conflict }
    }

    pub fn remove(&mut self, conn: ConnId) {
        self.by_conn.remove(&conn);
    }

    /// Listed Hosts except `exclude_device`, one entry per device (its newest
    /// connection wins over a stale socket that has not timed out yet), sorted
    /// by alias then device id.
    pub fn list(&self, exclude_device: Option<&str>) -> Vec<HostPresence> {
        let mut newest: HashMap<&str, (ConnId, &HostPresence)> = HashMap::new();
        for (conn, p) in self.by_conn.iter().filter_map(|(c, e)| Some((c, e.listed.as_ref()?))) {
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

    /// Register `p` (listed) on `conn` with no instance id; must be accepted.
    fn set(reg: &mut PresenceRegistry, conn: ConnId, p: HostPresence) {
        let device = p.device_id.clone();
        assert!(matches!(reg.register(conn, &device, None, Some(p)), Registration::Accepted { .. }));
    }

    #[test]
    fn list_excludes_the_caller_and_dedupes_by_device() {
        let mut reg = PresenceRegistry::default();
        set(&mut reg, 1, p("a", "old-name"));
        set(&mut reg, 2, p("b", "mini"));
        set(&mut reg, 3, p("a", "air"));
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
        set(&mut reg, 1, p("a", "air"));
        set(&mut reg, 2, p("b", "mini"));
        reg.register(1, "a", None, None);
        reg.remove(2);
        assert!(reg.list(None).is_empty());
        reg.register(3, "", None, Some(p("", "no-id")));
        assert!(
            reg.list(None).is_empty(),
            "a host without device id is never listed"
        );
    }

    #[test]
    fn another_instance_on_a_live_device_id_is_refused() {
        let mut reg = PresenceRegistry::default();
        let ok = Registration::Accepted { alias_conflict: None };
        assert_eq!(reg.register(1, "dev", Some("run-1"), Some(p("dev", "air"))), ok);
        // Another machine with a copied device id.
        assert_eq!(
            reg.register(2, "dev", Some("run-2"), Some(p("dev", "air"))),
            Registration::DeviceIdConflict
        );
        assert_eq!(reg.list(None), vec![p("dev", "air")], "the newcomer is not listed");
        // The same app reconnecting while its old socket is half-open.
        assert_eq!(reg.register(3, "dev", Some("run-1"), Some(p("dev", "air"))), ok);
        // Older Hosts (no instance id) on either side: newest wins, as before.
        assert_eq!(reg.register(4, "dev", None, None), ok);
        let mut old = PresenceRegistry::default();
        assert_eq!(old.register(1, "dev", None, None), ok);
        assert_eq!(old.register(2, "dev", Some("run-2"), None), ok);
        // Once the first connection is gone, another instance may register.
        reg.remove(1);
        reg.remove(3);
        assert_eq!(reg.register(5, "dev", Some("run-2"), None), ok);
        // Re-registering on the same socket is never a conflict.
        assert_eq!(reg.register(5, "dev", Some("run-2"), Some(p("dev", "x"))), ok);
    }

    #[test]
    fn a_listed_alias_used_by_another_device_is_reported() {
        let mut reg = PresenceRegistry::default();
        set(&mut reg, 1, p("a", "Mac Mini"));
        assert_eq!(
            reg.register(2, "b", None, Some(p("b", " mac mini "))),
            Registration::Accepted { alias_conflict: Some(" mac mini ".into()) },
            "still accepted, with a warning"
        );
        assert_eq!(reg.list(None).len(), 2);
        // Same device (stale socket), unlisted hosts and empty aliases don't count.
        assert_eq!(
            reg.register(3, "a", None, Some(p("a", "mac mini"))),
            Registration::Accepted { alias_conflict: Some("mac mini".into()) },
            "b still has it"
        );
        reg.remove(2);
        assert_eq!(
            reg.register(4, "a", None, Some(p("a", "Mac Mini"))),
            Registration::Accepted { alias_conflict: None }
        );
        reg.register(5, "c", None, None);
        assert_eq!(
            reg.register(6, "d", None, Some(p("d", ""))),
            Registration::Accepted { alias_conflict: None }
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
