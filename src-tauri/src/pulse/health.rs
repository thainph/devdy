//! Health of watches and projects (BR-08/09/10/12, pure). Mirrored by
//! `sidecar-mcp/lib/pulse-health.mjs`; both sides run `health_vectors.json`.
//!
//! Deadlines: an all-day deadline is stored as 23:59:59 local of its day, so
//! every deadline is compared by `due_at` directly. Overdue = `due_at < now`;
//! due soon = `due_at` within the next 72 h.

use chrono::{DateTime, Duration, FixedOffset};
use serde::{Deserialize, Serialize};

const DUE_SOON: Duration = Duration::hours(72);
const MY_BALL_LATE: Duration = Duration::hours(24);
const CUSTOMER_SLOW: Duration = Duration::days(5);
const DONE_SETTLED: Duration = Duration::days(7);

/// A deadline as stored in a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DueItem {
    #[serde(default)]
    pub title: String,
    pub due_at: String,
    #[serde(default)]
    pub all_day: bool,
}

/// One stored digest of a watch, newest first in `WatchFacts::history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DigestFacts {
    pub status: String,
    pub ball: String,
    pub ball_since: Option<String>,
    #[serde(default)]
    pub deadlines: Vec<DueItem>,
    #[serde(default)]
    pub source_closed: bool,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WatchFacts {
    pub last_error: Option<String>,
    /// Digests, newest first (empty = never summarised).
    #[serde(default)]
    pub history: Vec<DigestFacts>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextDue {
    pub title: String,
    pub due_at: String,
    pub all_day: bool,
}

/// BR-08 for one watch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchHealth {
    /// `red` | `yellow` | `green` | `done` | `pending` (no digest yet).
    pub health: String,
    /// BR-12 "done" group: issue closed, or AI `done` for 7 days in a row.
    pub finished: bool,
    pub overdue: bool,
    pub needs_me: bool,
    pub waiting_customer: bool,
    /// Earliest deadline not yet passed.
    pub next_due: Option<NextDue>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectCounts {
    pub needs_me: usize,
    pub waiting_customer: usize,
    pub overdue: usize,
    pub watching: usize,
}

/// BR-09 / BR-10 for one project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectHealth {
    /// `red` | `yellow` | `green` | `none` (no active watch, or all done).
    pub health: String,
    pub next_milestone: Option<NextDue>,
    pub counts: ProjectCounts,
}

/// `board_unavailable` is a soft error (last known board fields were used):
/// shown, but no reason for 🟡.
fn counts_as_error(code: &str) -> bool {
    code != "board_unavailable"
}

fn parse(s: &str) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(s.trim()).ok()
}

/// The moment a deadline falls due.
pub fn due_end(d: &DueItem) -> Option<DateTime<FixedOffset>> {
    parse(&d.due_at)
}

/// BR-12: AI `done` in every digest since one at least 7 days old.
fn done_settled(history: &[DigestFacts], now: DateTime<FixedOffset>) -> bool {
    let mut oldest_done = None;
    for d in history {
        if d.status != "done" {
            break;
        }
        oldest_done = parse(&d.created_at).or(oldest_done);
    }
    oldest_done.is_some_and(|t| now - t >= DONE_SETTLED)
}

pub fn watch_health(w: &WatchFacts, now: DateTime<FixedOffset>) -> WatchHealth {
    let Some(d) = w.history.first() else {
        return WatchHealth {
            health: if w.last_error.as_deref().is_some_and(counts_as_error) {
                "yellow"
            } else {
                "pending"
            }
            .into(),
            finished: false,
            overdue: false,
            needs_me: false,
            waiting_customer: false,
            next_due: None,
        };
    };
    let done = d.status == "done";
    let ends: Vec<(DateTime<FixedOffset>, &DueItem)> = d
        .deadlines
        .iter()
        .filter_map(|x| due_end(x).map(|e| (e, x)))
        .collect();
    let overdue = !done && ends.iter().any(|(e, _)| *e < now);
    let next = ends
        .iter()
        .filter(|(e, _)| *e >= now)
        .min_by_key(|(e, _)| *e)
        .map(|(_, x)| NextDue {
            title: x.title.clone(),
            due_at: x.due_at.clone(),
            all_day: x.all_day,
        });
    let due_soon = !done && ends.iter().any(|(e, _)| *e >= now && *e - now <= DUE_SOON);
    let since = d.ball_since.as_deref().and_then(parse);
    let held = |min: Duration| since.is_some_and(|s| now - s >= min);
    let my_late = d.ball == "me" && since.is_some_and(|s| now - s > MY_BALL_LATE);
    let health = if done {
        "done"
    } else if d.status == "blocked" || overdue || my_late {
        "red"
    } else if due_soon
        || d.status == "at_risk"
        || (d.ball == "customer" && held(CUSTOMER_SLOW))
        || w.last_error.as_deref().is_some_and(counts_as_error)
    {
        "yellow"
    } else {
        "green"
    };
    WatchHealth {
        health: health.into(),
        finished: done && (d.source_closed || done_settled(&w.history, now)),
        overdue,
        needs_me: !done && d.ball == "me",
        waiting_customer: !done && d.ball == "customer",
        next_due: if done { None } else { next },
    }
}

fn rank(h: &str) -> u8 {
    match h {
        "red" => 0,
        "yellow" => 1,
        "green" => 2,
        _ => 3,
    }
}

pub fn project_health(watches: &[WatchHealth]) -> ProjectHealth {
    let has = |h: &str| watches.iter().any(|w| w.health == h);
    let health = if has("red") {
        "red"
    } else if has("yellow") {
        "yellow"
    } else if watches.iter().any(|w| w.health != "done") {
        "green"
    } else {
        "none"
    };
    let next_milestone = watches
        .iter()
        .filter_map(|w| w.next_due.as_ref())
        .filter_map(|n| {
            let end = due_end(&DueItem {
                title: n.title.clone(),
                due_at: n.due_at.clone(),
                all_day: n.all_day,
            })?;
            Some((end, n))
        })
        .min_by_key(|(e, _)| *e)
        .map(|(_, n)| n.clone());
    ProjectHealth {
        health: health.into(),
        next_milestone,
        counts: ProjectCounts {
            needs_me: watches.iter().filter(|w| w.needs_me).count(),
            waiting_customer: watches.iter().filter(|w| w.waiting_customer).count(),
            overdue: watches.iter().filter(|w| w.overdue).count(),
            watching: watches.len(),
        },
    }
}

fn next_end(n: &Option<NextDue>) -> Option<DateTime<FixedOffset>> {
    n.as_ref().and_then(|n| {
        due_end(&DueItem {
            title: String::new(),
            due_at: n.due_at.clone(),
            all_day: n.all_day,
        })
    })
}

/// Name sort key shared with `pulse-health.mjs`: NFD, combining marks
/// (U+0300–U+036F) removed, `đ` → `d`, lowercase; compared by code point.
pub fn name_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    name.nfd()
        .filter(|c| !('\u{300}'..='\u{36f}').contains(c))
        .map(|c| match c {
            'đ' => 'd',
            'Đ' => 'D',
            c => c,
        })
        .collect::<String>()
        .to_lowercase()
}

/// Order of project cards: health, nearest milestone (none last), name
/// (`name_key`), then id. `a` / `b` are `(health, name, id)`.
pub fn project_order(
    a: (&ProjectHealth, &str, &str),
    b: (&ProjectHealth, &str, &str),
) -> std::cmp::Ordering {
    rank(&a.0.health)
        .cmp(&rank(&b.0.health))
        .then_with(
            || match (next_end(&a.0.next_milestone), next_end(&b.0.next_milestone)) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            },
        )
        .then_with(|| name_key(a.1).cmp(&name_key(b.1)))
        .then_with(|| a.2.cmp(b.2))
}

/// Order of watch rows in a card: red, yellow, green / pending, done; then the
/// nearest deadline.
pub fn watch_order(a: &WatchHealth, b: &WatchHealth) -> std::cmp::Ordering {
    let r = |w: &WatchHealth| match w.health.as_str() {
        "red" => 0,
        "yellow" => 1,
        "green" | "pending" => 2,
        _ => 3,
    };
    r(a).cmp(&r(b))
        .then_with(|| match (next_end(&a.next_due), next_end(&b.next_due)) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Vectors {
        health: Vec<Case>,
        sort: Vec<SortCase>,
    }

    #[derive(Deserialize)]
    struct SortCase {
        name: String,
        projects: Vec<SortProject>,
        expect: Vec<String>,
    }

    #[derive(Deserialize)]
    struct SortProject {
        id: String,
        name: String,
        health: String,
        next: Option<String>,
    }

    /// Card order shared with `sidecar-mcp/test-pulse.mjs`.
    #[test]
    fn shared_sort_vectors() {
        let v: Vectors = serde_json::from_str(include_str!("health_vectors.json")).unwrap();
        assert!(v.sort.len() >= 3);
        for c in v.sort {
            let mut ps: Vec<(ProjectHealth, &str, &str)> = c
                .projects
                .iter()
                .map(|p| {
                    let h = ProjectHealth {
                        health: p.health.clone(),
                        next_milestone: p.next.as_ref().map(|d| NextDue {
                            title: String::new(),
                            due_at: d.clone(),
                            all_day: false,
                        }),
                        counts: ProjectCounts::default(),
                    };
                    (h, p.name.as_str(), p.id.as_str())
                })
                .collect();
            ps.sort_by(|a, b| project_order((&a.0, a.1, a.2), (&b.0, b.1, b.2)));
            let ids: Vec<&str> = ps.iter().map(|p| p.2).collect();
            assert_eq!(ids, c.expect, "{}", c.name);
        }
    }

    #[derive(Deserialize)]
    struct Case {
        name: String,
        now: String,
        watches: Vec<WatchFacts>,
        expect: Expect,
    }

    #[derive(Deserialize)]
    struct Expect {
        watches: Vec<ExpectWatch>,
        project: String,
        next: Option<String>,
        counts: [usize; 4],
    }

    #[derive(Deserialize)]
    struct ExpectWatch {
        health: String,
        finished: bool,
    }

    /// Shared with `sidecar-mcp/test-pulse.mjs`.
    #[test]
    fn shared_vectors() {
        let v: Vectors = serde_json::from_str(include_str!("health_vectors.json")).unwrap();
        let cases = v.health;
        assert!(cases.len() >= 15);
        for c in cases {
            let now = parse(&c.now).unwrap();
            let hs: Vec<WatchHealth> = c.watches.iter().map(|w| watch_health(w, now)).collect();
            for (i, (h, e)) in hs.iter().zip(&c.expect.watches).enumerate() {
                assert_eq!(
                    (h.health.as_str(), h.finished),
                    (e.health.as_str(), e.finished),
                    "{} watch {i}",
                    c.name
                );
            }
            let p = project_health(&hs);
            assert_eq!(p.health, c.expect.project, "{}", c.name);
            assert_eq!(
                p.next_milestone.map(|n| n.title),
                c.expect.next,
                "{}",
                c.name
            );
            let k = &p.counts;
            assert_eq!(
                [k.needs_me, k.waiting_customer, k.overdue, k.watching],
                c.expect.counts,
                "{}",
                c.name
            );
        }
    }

    fn ph(health: &str, next: Option<&str>) -> ProjectHealth {
        ProjectHealth {
            health: health.into(),
            next_milestone: next.map(|d| NextDue {
                title: "x".into(),
                due_at: d.into(),
                all_day: false,
            }),
            counts: ProjectCounts::default(),
        }
    }

    #[test]
    fn project_cards_sort_by_health_then_milestone_then_name() {
        let mut v = vec![
            (ph("green", None), "zeta"),
            (ph("none", None), "alpha"),
            (ph("red", Some("2026-11-01T00:00:00+07:00")), "beta"),
            (ph("green", Some("2026-10-25T00:00:00+07:00")), "omega"),
            (ph("yellow", None), "gamma"),
            (ph("red", Some("2026-10-30T00:00:00+07:00")), "Delta"),
            (ph("green", None), "Eta"),
        ];
        v.sort_by(|a, b| project_order((&a.0, a.1, a.1), (&b.0, b.1, b.1)));
        let names: Vec<&str> = v.iter().map(|x| x.1).collect();
        assert_eq!(
            names,
            vec!["Delta", "beta", "gamma", "omega", "Eta", "zeta", "alpha"]
        );
    }

    #[test]
    fn watch_rows_sort_red_first_done_last() {
        let w = |h: &str, due: Option<&str>| WatchHealth {
            health: h.into(),
            finished: false,
            overdue: false,
            needs_me: false,
            waiting_customer: false,
            next_due: due.map(|d| NextDue {
                title: d.into(),
                due_at: d.into(),
                all_day: false,
            }),
        };
        let mut v = vec![
            w("done", None),
            w("green", None),
            w("pending", Some("2026-10-24T00:00:00Z")),
            w("yellow", None),
            w("red", Some("2026-10-30T00:00:00Z")),
            w("red", Some("2026-10-23T00:00:00Z")),
        ];
        v.sort_by(watch_order);
        let order: Vec<(String, Option<String>)> = v
            .iter()
            .map(|x| {
                (
                    x.health.clone(),
                    x.next_due.as_ref().map(|n| n.title.clone()),
                )
            })
            .collect();
        assert_eq!(
            order,
            vec![
                ("red".into(), Some("2026-10-23T00:00:00Z".into())),
                ("red".into(), Some("2026-10-30T00:00:00Z".into())),
                ("yellow".into(), None),
                ("pending".into(), Some("2026-10-24T00:00:00Z".into())),
                ("green".into(), None),
                ("done".into(), None),
            ]
        );
    }
}
