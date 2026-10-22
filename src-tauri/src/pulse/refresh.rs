//! Refresh of one watch (§3.3): fetch the source, build the BR-05 content,
//! skip the AI when the hash did not change, else summarise (BR-06, one retry)
//! and apply the hard facts (BR-07). A watch still `pending` is being resolved:
//! it becomes `ready` once its source could be fetched, `error` otherwise.
//! Failures go to `last_error`; the previous digest is kept.

use super::ball::ball_mine_since;
use super::content::{github_content, slack_content, Content, IssueData};
use super::digest::{
    apply_hard_facts, build_prompt, keep_quoted, parse_digest, retry_prompt, DigestDraft,
    HardFacts, PromptCtx,
};
use super::source::{github_ref, github_url, Source, SourceType};
use super::store::{self, Finish, Watch};
use super::PulseState;
use crate::slack_threads::{mrkdwn, CodedError};
use chrono::{DateTime, FixedOffset};

/// What a fetch produced.
struct Fetched {
    content: Content,
    facts: HardFacts,
    title: String,
    /// The user as the prompt should name them.
    me: String,
    slack_thread_id: Option<String>,
    /// The board query failed this time (GitHub): its fields are unknown.
    board_unavailable: bool,
    /// The issue was transferred: the watch now has this ref / version.
    moved: Option<(Source, String)>,
}

fn now_local() -> DateTime<FixedOffset> {
    chrono::Local::now().fixed_offset()
}

/// `<o>/<r>#<n>` → parts.
fn split_github_ref(r: &str) -> Option<(&str, &str, u64)> {
    let (repo_part, n) = r.rsplit_once('#')?;
    let (owner, repo) = repo_part.split_once('/')?;
    Some((owner, repo, n.parse().ok()?))
}

impl PulseState {
    /// Refresh (or resolve) one watch by id; logs instead of failing.
    pub(super) async fn refresh_watch(&self, id: &str) {
        // Re-read: it may have been unwatched / moved while queued.
        let w = match store::get(self.db(), id).await {
            Ok(Some(w)) if w.active => w,
            Ok(_) => return,
            Err(e) => {
                eprintln!("[pulse] refresh {id}: {e}");
                return;
            }
        };
        if let Err(e) = self.refresh_loaded(w).await {
            eprintln!("[pulse] refresh {id}: {e}");
        }
    }

    /// Every write after the fetch is conditional on `w` still being the
    /// current version of the watch (`updated_at`), so a refresh overtaken by
    /// a move / unwatch / re-watch writes nothing.
    async fn refresh_loaded(&self, mut w: Watch) -> Result<(), CodedError> {
        let db = self.db();
        let w = &mut w;
        let resolving = w.resolve_status == "pending";
        let fetched = match w.source_type.as_str() {
            "slack" => self.fetch_slack(w).await,
            _ => self.fetch_github(w).await,
        };
        let f = match fetched {
            Ok(f) => f,
            Err(e) => {
                tracing::info!(event = "pulse_fetch_failed", watch = %w.id, code = %e.code);
                return store::finish_refresh(
                    db,
                    w,
                    Finish {
                        error: Some(&e.code),
                        resolve_status: resolving.then_some("error"),
                        resolving,
                        ..Default::default()
                    },
                )
                .await
                .map(|_| ());
            }
        };
        if let Some((src, version)) = &f.moved {
            w.source_ref = src.source_ref.clone();
            w.source_url = src.source_url.clone();
            w.updated_at = version.clone();
        }
        let w: &Watch = w;
        let done = |error: Option<&'static str>| Finish {
            title: Some(&f.title),
            slack_thread_id: f.slack_thread_id.as_deref(),
            checked: true,
            error,
            resolve_status: Some("ready"),
            resolving,
        };
        // A failed board query reused the last known board fields (so it is
        // no content change by itself) and is reported as a soft error.
        let soft = f.board_unavailable.then_some("board_unavailable");
        // BR-05: unchanged content → keep the digest, no AI call.
        let last_hash = store::latest_digest_hash(db, &w.id).await?;
        if last_hash.as_deref() == Some(&f.content.hash) {
            return store::finish_refresh(db, w, done(soft)).await.map(|_| ());
        }
        let now = now_local();
        let ctx = PromptCtx {
            source_type: &w.source_type,
            title: &f.title,
            me: &f.me,
            now,
            content: &f.content.text,
            truncated: f.content.truncated,
        };
        match self.summarize(&ctx, *now.offset()).await {
            Ok(mut d) => {
                keep_quoted(&mut d, &f.content.text);
                apply_hard_facts(&mut d, &f.facts, *now.offset());
                let model = self.inner.svc.engine.model().await;
                let stored = store::insert_digest(
                    db,
                    w,
                    &f.content.hash,
                    &d,
                    f.content.truncated,
                    f.facts.issue_closed,
                    model.as_deref(),
                )
                .await?;
                if stored.is_none() {
                    tracing::info!(event = "pulse_refresh_stale", watch = %w.id);
                    return Ok(());
                }
                store::finish_refresh(db, w, done(soft)).await.map(|_| ())
            }
            Err(e) => {
                tracing::info!(event = "pulse_summarize_failed", watch = %w.id, error = %e);
                store::finish_refresh(db, w, done(Some("summarize_failed")))
                    .await
                    .map(|_| ())
            }
        }
    }

    /// BR-06: one attempt + one retry carrying the validation error.
    async fn summarize(&self, ctx: &PromptCtx<'_>, tz: FixedOffset) -> Result<DigestDraft, String> {
        let engine = &self.inner.svc.engine;
        let first = engine.complete(build_prompt(ctx)).await?;
        let err = match parse_digest(&first, tz) {
            Ok(d) => return Ok(d),
            Err(e) => e,
        };
        let second = engine.complete(retry_prompt(ctx, &err)).await?;
        parse_digest(&second, tz)
    }

    async fn fetch_slack(&self, w: &Watch) -> Result<Fetched, CodedError> {
        let db = self.db();
        let slack = &self.inner.svc.slack;
        // §9: a thread deleted from Devdy is captured again from the link.
        let thread_id = match w.slack_thread_id.as_deref() {
            Some(id) if store::slack_thread_exists(db, id).await? => id.to_string(),
            _ => slack.ensure_thread(&w.source_url, &w.project_id).await?.id,
        };
        slack.refresh_and_wait(&thread_id).await?;
        let t = store::load_slack_thread(db, &thread_id)
            .await?
            .ok_or_else(|| CodedError::new("slack_fetch_failed", "thread disappeared"))?;
        let content = slack_content(&t.msgs, t.me.as_deref(), &t.names);
        let ball =
            t.me.as_deref()
                .and_then(|me| ball_mine_since(&t.msgs, me))
                .and_then(|ts| mrkdwn::ts_to_iso(&ts));
        let me = match (&t.me_name, &t.me) {
            (Some(name), _) => format!("{name} (their lines are marked (me))"),
            (None, Some(id)) => format!("Slack user {id} (their lines are marked (me))"),
            (None, None) => "unknown — no line is marked (me)".to_string(),
        };
        Ok(Fetched {
            content,
            facts: HardFacts {
                ball_mine_since: ball,
                ..Default::default()
            },
            title: t.title,
            me,
            slack_thread_id: Some(thread_id),
            board_unavailable: false,
            moved: None,
        })
    }

    async fn fetch_github(&self, w: &Watch) -> Result<Fetched, CodedError> {
        let gh = &self.inner.svc.github;
        let (owner, repo, number) = split_github_ref(&w.source_ref)
            .ok_or_else(|| CodedError::new("invalid_source", "bad GitHub source ref"))?;
        let mut issue: IssueData = gh.fetch_issue(&w.project_id, owner, repo, number).await?;
        // D3: a failed board query reuses the last known board fields.
        if issue.board_unavailable {
            issue.board = store::last_board(self.db(), &w.id).await?;
        } else {
            store::save_board(self.db(), &w.id, issue.board.as_ref()).await?;
        }
        if issue.is_pull_request {
            return Err(CodedError::new(
                "invalid_source",
                "this number is a pull request, not an issue",
            ));
        }
        // §9: follow a transferred issue to its new repo / number.
        let new_ref = github_ref(&issue.owner, &issue.repo, issue.number);
        let mut moved = None;
        if new_ref != w.source_ref {
            let src = Source {
                source_type: SourceType::GithubIssue,
                source_ref: new_ref,
                source_url: github_url(&issue.owner, &issue.repo, issue.number),
            };
            match store::update_source(self.db(), w, &src).await? {
                Some(version) => moved = Some((src, version)),
                None => {
                    tracing::warn!(
                        event = "pulse_duplicate_source",
                        watch = %w.id,
                        to = %src.source_ref
                    );
                    return Err(CodedError::new(
                        "duplicate_source",
                        format!(
                            "the issue moved to {}, which another watch has",
                            src.source_ref
                        ),
                    ));
                }
            }
        }
        let me = gh
            .my_login(&w.project_id)
            .await
            .map(|l| format!("GitHub user {l}"))
            .unwrap_or_else(|| "the owner of this Devdy project (GitHub login unknown)".into());
        Ok(Fetched {
            content: github_content(&issue),
            facts: HardFacts {
                issue_closed: issue.is_closed(),
                board_deadline: issue.hard_deadline(),
                ..Default::default()
            },
            title: issue.title.clone(),
            me,
            slack_thread_id: None,
            board_unavailable: issue.board_unavailable,
            moved,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_ref_parts() {
        assert_eq!(split_github_ref("org/app#12"), Some(("org", "app", 12)));
        assert_eq!(split_github_ref("org/app"), None);
        assert_eq!(split_github_ref("orgapp#1"), None);
    }
}
