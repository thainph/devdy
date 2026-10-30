//! Source content fed to the Focus AI prompts (spec §3.6, pure; from Pulse).
//!
//! - Slack: every message by `seq`, one per line
//!   `[<ISO time>] <author>(<me|other>): <text_vi ?? text_raw>`.
//! - GitHub: title, state, labels, assignees, milestone (+ due), board
//!   Start/Deadline/Status, body, then comments in time order.
//!
//! Both are capped at 60,000 characters: the first block (parent message /
//! issue header + body) is kept, then as many of the newest blocks as fit;
//! `truncated` records the cut. GitHub's `hash` is the sha256 of the text;
//! Slack's is over the original messages (`ts|user_id|subtype|text_raw` per
//! line), so a re-translation alone never changes it.

use crate::slack_threads::mrkdwn::{self, Links, Names};
use sha2::{Digest as _, Sha256};

pub const MAX_CONTENT_CHARS: usize = 60_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Content {
    pub text: String,
    pub truncated: bool,
    /// sha256 hex of `text`.
    pub hash: String,
}

/// One stored Slack message (`slack_messages`), in `seq` order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlackMsg {
    pub ts: String,
    pub user_id: Option<String>,
    pub bot_id: Option<String>,
    pub subtype: Option<String>,
    /// Display name (resolved by the caller), else the raw id / bot name.
    pub author: String,
    pub text_raw: String,
    pub text_vi: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueComment {
    pub author: String,
    /// RFC3339.
    pub created_at: String,
    pub body: String,
}

/// Board fields of the issue on the project's GitHub Project board.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BoardFields {
    /// `YYYY-MM-DD`.
    pub start: Option<String>,
    /// `YYYY-MM-DD`.
    pub deadline: Option<String>,
    pub status: Option<String>,
}

/// What the refresh needs from one GitHub issue.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IssueData {
    /// Owner / repo / number as GitHub reports them now (a transferred issue
    /// comes back under its new repo).
    pub owner: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    /// `open` | `closed`.
    pub state: String,
    pub state_reason: Option<String>,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
    pub milestone: Option<String>,
    /// `YYYY-MM-DD`.
    pub milestone_due: Option<String>,
    pub author: String,
    pub created_at: String,
    pub body: String,
    /// Human comments, oldest first.
    pub comments: Vec<IssueComment>,
    pub is_pull_request: bool,
    pub board: Option<BoardFields>,
    /// The board query failed this time (board fields unknown, not absent).
    pub board_unavailable: bool,
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn take_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Keep `blocks[0]` + the newest blocks that fit in `max` chars (blocks are
/// joined by `\n`); a marker line notes how many were left out.
fn cap(blocks: &[String], max: usize) -> (String, bool) {
    let len = |s: &str| s.chars().count();
    let total: usize = blocks.iter().map(|b| len(b) + 1).sum();
    if total <= max + 1 {
        return (blocks.join("\n"), false);
    }
    let Some((first, rest)) = blocks.split_first() else {
        return (String::new(), false);
    };
    let marker = |n: usize| format!("[… {n} earlier entries omitted …]");
    let first = take_chars(first, max.saturating_sub(len(&marker(rest.len())) + 2));
    let mut budget = max
        .saturating_sub(len(first) + 1)
        .saturating_sub(len(&marker(rest.len())) + 1);
    let mut tail: Vec<&String> = Vec::new();
    for b in rest.iter().rev() {
        let need = len(b) + 1;
        if need > budget {
            break;
        }
        budget -= need;
        tail.push(b);
    }
    tail.reverse();
    let mut out = vec![first.to_string(), marker(rest.len() - tail.len())];
    out.extend(tail.into_iter().cloned());
    (out.join("\n"), true)
}

fn finish(blocks: Vec<String>) -> Content {
    let (text, truncated) = cap(&blocks, MAX_CONTENT_CHARS);
    Content {
        hash: sha256_hex(&text),
        text,
        truncated,
    }
}

pub fn slack_content(msgs: &[SlackMsg], me: Option<&str>, names: &Names) -> Content {
    let blocks = msgs
        .iter()
        .map(|m| {
            let when = mrkdwn::ts_to_iso(&m.ts).unwrap_or_else(|| m.ts.clone());
            let who = if me.is_some() && m.user_id.as_deref() == me {
                "me"
            } else {
                "other"
            };
            let text = m.text_vi.as_deref().unwrap_or(&m.text_raw);
            let text = mrkdwn::to_plain(text, names, Links::Label);
            format!("[{when}] {}({who}): {}", m.author, text.trim())
        })
        .collect();
    let raw: Vec<String> = msgs
        .iter()
        .map(|m| {
            format!(
                "{}|{}|{}|{}",
                m.ts,
                m.user_id.as_deref().unwrap_or(""),
                m.subtype.as_deref().unwrap_or(""),
                m.text_raw
            )
        })
        .collect();
    Content {
        hash: sha256_hex(&raw.join("\n")),
        ..finish(blocks)
    }
}

fn or_none(v: &[String]) -> String {
    if v.is_empty() {
        "(none)".to_string()
    } else {
        v.join(", ")
    }
}

pub fn github_content(i: &IssueData) -> Content {
    let mut head = vec![
        format!("Issue: {}/{}#{}", i.owner, i.repo, i.number),
        format!("Title: {}", i.title),
        match &i.state_reason {
            Some(r) => format!("State: {} ({r})", i.state),
            None => format!("State: {}", i.state),
        },
        format!("Labels: {}", or_none(&i.labels)),
        format!("Assignees: {}", or_none(&i.assignees)),
        match (&i.milestone, &i.milestone_due) {
            (Some(m), Some(d)) => format!("Milestone: {m} (due {d})"),
            (Some(m), None) => format!("Milestone: {m}"),
            (None, _) => "Milestone: (none)".to_string(),
        },
    ];
    if let Some(b) = &i.board {
        let f = |v: &Option<String>| v.clone().unwrap_or_else(|| "-".into());
        head.push(format!(
            "Board: Start={} Deadline={} Status={}",
            f(&b.start),
            f(&b.deadline),
            f(&b.status)
        ));
    }
    head.push(format!("Opened by {} at {}", i.author, i.created_at));
    head.push(String::new());
    head.push(i.body.trim().to_string());
    let mut blocks = vec![head.join("\n")];
    blocks.extend(i.comments.iter().map(|c| {
        format!(
            "--- [{}] {} commented:\n{}",
            c.created_at,
            c.author,
            c.body.trim()
        )
    }));
    finish(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(ts: &str, user: &str, raw: &str, vi: Option<&str>) -> SlackMsg {
        SlackMsg {
            ts: ts.into(),
            user_id: Some(user.into()),
            author: format!("name-{user}"),
            text_raw: raw.into(),
            text_vi: vi.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn slack_lines_mark_me_and_prefer_vietnamese() {
        let mut names = Names::default();
        names.users.insert("U2".into(), "Bob".into());
        let c = slack_content(
            &[
                msg(
                    "1789877283.026359",
                    "U1",
                    "Hello <@U2>",
                    Some("Xin chào <@U2>"),
                ),
                msg("1789877300.000100", "U2", "ok &amp; thanks", None),
            ],
            Some("U1"),
            &names,
        );
        assert_eq!(
            c.text,
            "[2026-09-20T04:08:03.026Z] name-U1(me): Xin chào @Bob\n\
             [2026-09-20T04:08:20.000Z] name-U2(other): ok & thanks"
        );
        assert!(!c.truncated);
        assert_eq!(
            c.hash,
            sha256_hex("1789877283.026359|U1||Hello <@U2>\n1789877300.000100|U2||ok &amp; thanks")
        );
        // Without my id everyone is "other".
        let c2 = slack_content(&[msg("1789877283.026359", "U1", "x", None)], None, &names);
        assert!(c2.text.contains("(other)"));
    }

    #[test]
    fn slack_hash_follows_the_original_messages_only() {
        let names = Names::default();
        let a = slack_content(&[msg("1.000001", "U1", "a", None)], None, &names);
        let b = slack_content(&[msg("1.000001", "U1", "a", None)], None, &names);
        let c = slack_content(&[msg("1.000001", "U1", "b", None)], None, &names);
        assert_eq!(a.hash, b.hash);
        assert_ne!(a.hash, c.hash);
        // A new / changed translation alone keeps the hash (and the digest).
        let vi = slack_content(
            &[msg("1.000001", "U1", "a", Some("bản dịch"))],
            None,
            &names,
        );
        assert_eq!(vi.hash, a.hash);
        assert_ne!(vi.text, a.text);
        // Knowing my id changes the labels, not the hash.
        let me = slack_content(&[msg("1.000001", "U1", "a", None)], Some("U1"), &names);
        assert_eq!(me.hash, a.hash);
        let mut sub = msg("1.000001", "U1", "a", None);
        sub.subtype = Some("thread_broadcast".into());
        assert_ne!(slack_content(&[sub], None, &names).hash, a.hash);
    }

    #[test]
    fn japanese_text_is_capped_by_characters() {
        let names = Names::default();
        let line = "デプロイが失敗しました。ロールバックをお願いします。".repeat(40);
        let msgs: Vec<SlackMsg> = (0..120)
            .map(|i| msg(&format!("{}.000000", 1_700_000_000 + i), "U1", &line, None))
            .collect();
        let c = slack_content(&msgs, None, &names);
        assert!(c.truncated);
        let n = c.text.chars().count();
        assert!(
            n <= MAX_CONTENT_CHARS && n > MAX_CONTENT_CHARS - 2_000,
            "{n}"
        );
        assert!(c.text.contains("earlier entries omitted"));
        assert!(c.text.ends_with(&line));
        // A first block larger than the cap is cut on a character boundary.
        let huge = msg("1.000000", "U1", &"日本語".repeat(30_000), None);
        let c = slack_content(&[huge, msg("2.000000", "U2", "tail", None)], None, &names);
        assert!(c.text.chars().count() <= MAX_CONTENT_CHARS);
    }

    #[test]
    fn cap_keeps_first_and_newest() {
        let blocks: Vec<String> = (0..10).map(|i| format!("{i}{}", "x".repeat(9))).collect();
        let (text, truncated) = cap(&blocks, 60);
        assert!(truncated);
        assert!(text.chars().count() <= 60, "{}", text.chars().count());
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines[0].starts_with('0'));
        assert!(lines[1].contains("omitted"));
        assert!(lines.last().unwrap().starts_with('9'));
        let (text, truncated) = cap(&blocks[..2], 60);
        assert!(!truncated);
        assert_eq!(text, format!("{}\n{}", blocks[0], blocks[1]));
        // A huge first block is cut to fit.
        let (text, truncated) = cap(&["é".repeat(200), "tail".into()], 60);
        assert!(truncated && text.chars().count() <= 60);
    }

    #[test]
    fn slack_content_respects_the_60k_cap() {
        let names = Names::default();
        let msgs: Vec<SlackMsg> = (0..200)
            .map(|i| {
                msg(
                    &format!("{}.000000", 1_700_000_000 + i),
                    "U1",
                    &"y".repeat(500),
                    None,
                )
            })
            .collect();
        let c = slack_content(&msgs, None, &names);
        assert!(c.truncated);
        assert!(c.text.chars().count() <= MAX_CONTENT_CHARS);
        assert!(c.text.starts_with("[2023-11-14T22:13:20.000Z]"));
        assert!(c.text.ends_with(&"y".repeat(500)));
    }

    #[test]
    fn github_content_lists_issue_fields() {
        let mut i = IssueData {
            owner: "org".into(),
            repo: "app".into(),
            number: 12,
            title: "Login fails".into(),
            state: "closed".into(),
            state_reason: Some("completed".into()),
            labels: vec!["bug".into()],
            assignees: vec![],
            milestone: Some("v1".into()),
            milestone_due: Some("2026-10-30".into()),
            author: "alice".into(),
            created_at: "2026-10-01T00:00:00+00:00".into(),
            body: "Steps…".into(),
            comments: vec![IssueComment {
                author: "bob".into(),
                created_at: "2026-10-02T00:00:00+00:00".into(),
                body: "Fixed in #13".into(),
            }],
            ..Default::default()
        };
        let c = github_content(&i);
        assert_eq!(
            c.text,
            "Issue: org/app#12\nTitle: Login fails\nState: closed (completed)\nLabels: bug\n\
             Assignees: (none)\nMilestone: v1 (due 2026-10-30)\nOpened by alice at \
             2026-10-01T00:00:00+00:00\n\nSteps…\n--- [2026-10-02T00:00:00+00:00] bob commented:\n\
             Fixed in #13"
        );
        i.board = Some(BoardFields {
            start: None,
            deadline: Some("2026-11-05".into()),
            status: Some("In progress".into()),
        });
        assert!(github_content(&i)
            .text
            .contains("Board: Start=- Deadline=2026-11-05 Status=In progress"));
    }
}
