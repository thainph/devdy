//! Pure parts of GitHub Mentions: which comment is waiting for the user's
//! answer, whether they answered it, and the draft-reply prompt.

#[derive(Debug, Clone)]
pub struct Comment {
    pub author: String,
    pub bot: bool,
    pub body: String,
    /// RFC 3339 (`…Z`) as GitHub returns it, so strings compare in time order.
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Trigger {
    /// Index into the comments; `usize::MAX` for the issue / PR description.
    pub index: usize,
    /// Comments by others after the trigger.
    pub newer_comments: u32,
    /// The user's last comment / review after the trigger.
    pub answered_at: Option<String>,
}

/// `@login` as a whole handle (not inside an email or a longer handle).
pub fn mentions_login(body: &str, login: &str) -> bool {
    let body = body.to_lowercase();
    let needle = format!("@{}", login.to_lowercase());
    let handle_char = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    body.match_indices(&needle).any(|(i, _)| {
        let before = body[..i].chars().next_back();
        let after = body[i + needle.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !after.is_some_and(|c| handle_char(c) || c == '/')
    })
}

/// `@org/team`.
pub fn mentions_team(body: &str) -> bool {
    body.split('@').skip(1).any(|rest| {
        let handle: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/')).collect();
        let mut parts = handle.split('/');
        matches!((parts.next(), parts.next()), (Some(a), Some(b)) if !a.is_empty() && !b.is_empty())
    })
}

/// The comment waiting for an answer, for a notification `reason`:
/// - `mention`: the last comment by someone else that `@`s the user;
/// - `team_mention`: the last one that `@`s a team;
/// - `comment` / `author`: the last comment by someone else.
///
/// Bots and empty bodies never count. `None` when nothing matches (for
/// mentions the caller then looks at the description).
pub fn pick_trigger(comments: &[Comment], login: &str, reason: &str) -> Option<Trigger> {
    let mine = |c: &Comment| c.author.eq_ignore_ascii_case(login);
    let other = |c: &Comment| !mine(c) && !c.bot && !c.body.trim().is_empty();
    let index = comments.iter().rposition(|c| {
        other(c)
            && match reason {
                "mention" => mentions_login(&c.body, login),
                "team_mention" => mentions_team(&c.body),
                _ => true,
            }
    })?;
    let at = &comments[index].created_at;
    let after = || comments[index + 1..].iter().filter(|c| &c.created_at >= at);
    Some(Trigger {
        index,
        newer_comments: after().filter(|c| other(c)).count() as u32,
        answered_at: after().filter(|c| mine(c)).map(|c| c.created_at.clone()).next_back(),
    })
}

pub struct DraftInput<'a> {
    pub login: &'a str,
    pub kind: &'a str,
    pub repo: &'a str,
    pub number: u64,
    pub title: &'a str,
    pub description: &'a str,
    pub description_author: &'a str,
    /// (author, body, file path for diff comments), oldest first.
    pub comments: &'a [(&'a str, &'a str, Option<&'a str>)],
    /// (author, excerpt) of the comment to answer.
    pub trigger: Option<(&'a str, &'a str)>,
}

const MAX_DESCRIPTION: usize = 4000;
const MAX_COMMENT: usize = 1500;
const MAX_COMMENTS: usize = 20;

fn clip(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str(" […]");
    out
}

pub fn draft_prompt(d: &DraftInput) -> String {
    let kind = if d.kind == "PullRequest" { "pull request" } else { "issue" };
    let mut p = format!(
        "You are drafting a GitHub comment for the user @{login}, who was asked something on {repo}#{number} ({kind}).\n\
         Write the reply @{login} would post. Rules:\n\
         - Answer the latest comment addressed to @{login} (marked below). Be direct and concise; GitHub Markdown is fine.\n\
         - Use the same language the thread uses (e.g. Vietnamese, English, Japanese).\n\
         - Do not invent facts about the code or decisions. When the answer is not in the thread, say what you will check or ask a precise question.\n\
         - Output ONLY the comment body: no preamble, no quotes around it, no signature.\n\n\
         Title: {title}\n\nDescription by @{desc_author}:\n{description}\n",
        login = d.login,
        repo = d.repo,
        number = d.number,
        title = d.title,
        desc_author = if d.description_author.is_empty() { "unknown" } else { d.description_author },
        description = clip(d.description, MAX_DESCRIPTION),
    );
    let skip = d.comments.len().saturating_sub(MAX_COMMENTS);
    if !d.comments.is_empty() {
        p.push_str("\nRecent comments (oldest first):\n");
        for (author, body, path) in &d.comments[skip..] {
            match path {
                Some(path) => p.push_str(&format!("\n@{author} (on {path}):\n{}\n", clip(body, MAX_COMMENT))),
                None => p.push_str(&format!("\n@{author}:\n{}\n", clip(body, MAX_COMMENT))),
            }
        }
    }
    if let Some((author, body)) = d.trigger {
        p.push_str(&format!("\n>>> Comment to answer, by @{author}:\n{}\n", clip(body, MAX_COMMENT)));
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(author: &str, body: &str, at: &str) -> Comment {
        Comment { author: author.into(), bot: false, body: body.into(), created_at: at.into() }
    }

    #[test]
    fn mention_matches_whole_handle_only() {
        assert!(mentions_login("hi @ThaiNPH can you check", "thainph"));
        assert!(mentions_login("@thainph.", "thainph"));
        assert!(!mentions_login("@thainph-sora please", "thainph"));
        assert!(!mentions_login("mail thai@thainph.dev", "thainph"));
        assert!(!mentions_login("@thainph/team", "thainph"));
    }

    #[test]
    fn team_mention_needs_org_and_team() {
        assert!(mentions_team("ping @sora-tech/backend"));
        assert!(!mentions_team("ping @thainph"));
        assert!(!mentions_team("a@b/"));
    }

    #[test]
    fn mention_trigger_and_answer() {
        let cs = vec![
            c("lam", "@thainph look at this?", "2026-11-10T01:00:00Z"),
            c("an", "+1", "2026-11-10T02:00:00Z"),
            c("thainph", "done", "2026-11-10T03:00:00Z"),
        ];
        let t = pick_trigger(&cs, "thainph", "mention").unwrap();
        assert_eq!(t.index, 0);
        assert_eq!(t.newer_comments, 1);
        assert_eq!(t.answered_at.as_deref(), Some("2026-11-10T03:00:00Z"));
    }

    #[test]
    fn new_mention_after_answer_is_pending() {
        let cs = vec![
            c("lam", "@thainph first", "2026-11-10T01:00:00Z"),
            c("thainph", "ok", "2026-11-10T02:00:00Z"),
            c("lam", "@thainph again", "2026-11-10T03:00:00Z"),
        ];
        let t = pick_trigger(&cs, "thainph", "mention").unwrap();
        assert_eq!(t.index, 2);
        assert_eq!(t.answered_at, None);
    }

    #[test]
    fn comment_reason_skips_bots_and_self() {
        let mut bot = c("ci[bot]", "build failed", "2026-11-10T03:00:00Z");
        bot.bot = true;
        let cs = vec![c("hoa", "fixed, please recheck", "2026-11-10T01:00:00Z"), c("thainph", "", "2026-11-10T02:00:00Z"), bot];
        let t = pick_trigger(&cs, "thainph", "comment").unwrap();
        assert_eq!(t.index, 0);
        // An empty-bodied review by the user still answers.
        assert_eq!(t.answered_at.as_deref(), Some("2026-11-10T02:00:00Z"));
        assert!(pick_trigger(&[c("thainph", "mine", "2026-11-10T01:00:00Z")], "thainph", "author").is_none());
    }

    #[test]
    fn mention_without_matching_comment_is_none() {
        let cs = vec![c("lam", "no handle here", "2026-11-10T01:00:00Z")];
        assert!(pick_trigger(&cs, "thainph", "mention").is_none());
    }

    #[test]
    fn draft_prompt_marks_trigger_and_clips() {
        let long = "x".repeat(5000);
        let comments = [("lam", "question?", None), ("an", "on diff", Some("src/a.rs"))];
        let p = draft_prompt(&DraftInput {
            login: "thainph",
            kind: "PullRequest",
            repo: "o/r",
            number: 7,
            title: "T",
            description: &long,
            description_author: "lam",
            comments: &comments,
            trigger: Some(("lam", "question?")),
        });
        assert!(p.contains("o/r#7 (pull request)"));
        assert!(p.contains("@an (on src/a.rs)"));
        assert!(p.contains(">>> Comment to answer, by @lam"));
        assert!(p.contains(" […]"));
    }
}
