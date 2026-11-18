//! Pure parts of GitHub Mentions: which comment is waiting for the user's
//! answer and whether they answered it.

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
}
