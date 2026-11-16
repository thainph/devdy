//! Remote Control PR inbox: the PRs waiting for the user's review (the desktop
//! PR Inbox) and starting / repeating a review from the phone.
//!
//! Only reached for a Controller that advertised
//! [`FEATURE_PR`](crate::remote::protocol::FEATURE_PR) — the gate lives in
//! [`crate::remote::handler::handle`]. The list is the Host's inbox cache
//! (`crate::pr_inbox`), and the review actions take the same steps as the
//! desktop's Review / Re-review buttons (`views/PrInboxView.vue`).
//!
//! The phone names a PR by its `owner/repo#number` key only. Project, repo and
//! account are looked up in the cache, so a Controller can only review a PR the
//! inbox actually lists, as the account that was asked to review it.
//!
//! Failures only ever put a short code on the wire; every outcome is audited
//! exactly once.

use crate::commands::github::PrInboxItem;
use crate::pr_inbox::{self, CachedPrInbox};
use crate::remote::audit;
use crate::remote::command::RejectReason;
use crate::remote::focus::error_code;
use crate::remote::handler::{accept, command_ack, command_ack_for, reject, send_payload, HandlerCtx};
use crate::remote::protocol::{CmdPayload, PrInboxInfo, StreamPayload};
use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::{AppHandle, Listener, Manager};

/// A cached inbox older than this is re-fetched when the phone opens it (the
/// desktop's `CACHE_FRESH_MS`).
const STALE_AFTER: Duration = Duration::from_secs(60);
/// How long the live inbox waits for a burst of updates to settle.
const LIVE_DEBOUNCE: Duration = Duration::from_millis(300);

// ---- Wire shape ----------------------------------------------------------------

/// One cached PR as the phone sees it (no account / repo ids).
pub fn to_wire(item: &PrInboxItem) -> PrInboxInfo {
    PrInboxInfo {
        key: pr_inbox::pr_key(item),
        owner: item.owner.clone(),
        repo: item.repo.clone(),
        number: item.number,
        title: item.title.clone(),
        html_url: item.html_url.clone(),
        author_login: item.author_login.clone(),
        author_avatar_url: item.author_avatar_url.clone(),
        author_is_bot: item.author_is_bot,
        draft: item.draft,
        labels: item.labels.clone(),
        updated_at: item.updated_at.clone(),
        mapped: item.mapped,
        project_id: item.project_id.clone(),
        project_name: item.project_name.clone(),
        account_label: item.account_label.clone(),
        existing_run_id: item.existing_run_id.clone(),
        existing_run_status: item.existing_run_status.clone(),
        updated_since_review: item.updated_since_review,
    }
}

/// The `pr_inbox` frame for `cached` (None before the first fetch).
pub fn inbox_payload(cached: Option<CachedPrInbox>, refreshing: bool, error: Option<String>) -> StreamPayload {
    let (items, failed_accounts, fetched_at) = match cached {
        Some(c) => (
            c.result.items.iter().map(to_wire).collect(),
            c.result.errors.iter().map(|e| e.account_label.clone()).collect(),
            Some(c.fetched_at),
        ),
        None => (Vec::new(), Vec::new(), None),
    };
    StreamPayload::PrInbox { items, fetched_at, refreshing, failed_accounts, error }
}

async fn send_inbox(ctx: &HandlerCtx, refreshing: bool, error: Option<&str>) {
    let payload = inbox_payload(pr_inbox::get_cached_pr_inbox(), refreshing, error.map(str::to_string));
    send_payload(ctx, payload).await;
}

/// Whether the phone should get a fresh fetch behind the cached list.
pub fn needs_refresh(force: bool, age: Option<Duration>) -> bool {
    force || age.is_none_or(|a| a > STALE_AFTER)
}

// ---- Live inbox ----------------------------------------------------------------

/// The inbox-update listener of one session and the task that turns it into
/// pushes. Dropped with the session's [`HandlerCtx`].
pub struct LivePrInbox {
    app: AppHandle,
    listener: tauri::EventId,
    task: tokio::task::AbortHandle,
}

impl Drop for LivePrInbox {
    fn drop(&mut self) {
        self.app.unlisten(self.listener);
        self.task.abort();
    }
}

/// Push the inbox to a `pr` session after every fetch (poller, desktop, phone)
/// and every review linked to a PR. The task only holds a weak reference: the
/// listener goes away with the session.
pub fn start_live_pr_inbox(ctx: &Arc<HandlerCtx>) {
    if !ctx.pr {
        return;
    }
    let (tx, mut rx) = tokio::sync::mpsc::channel::<()>(1);
    let listener = ctx.app.listen_any(pr_inbox::UPDATED_EVENT, move |_| {
        // Full means a push is already due.
        let _ = tx.try_send(());
    });
    let weak = Arc::downgrade(ctx);
    let task = tokio::spawn(async move {
        while rx.recv().await.is_some() {
            tokio::time::sleep(LIVE_DEBOUNCE).await;
            while rx.try_recv().is_ok() {}
            let Some(ctx) = weak.upgrade() else { break };
            // An update means a fetch just ended (or a review was linked).
            send_inbox(&ctx, false, None).await;
        }
    });
    let _ = ctx.live_pr.set(LivePrInbox { app: ctx.app.clone(), listener, task: task.abort_handle() });
}

// ---- Dispatch ------------------------------------------------------------------

enum Failure {
    /// Refused before touching anything (bad input, unknown PR).
    Rejected(RejectReason),
    /// The shared logic refused or failed: the short code only.
    Failed(String),
}

impl From<RejectReason> for Failure {
    fn from(r: RejectReason) -> Self {
        Failure::Rejected(r)
    }
}

/// Log the full error locally, keep only its code.
fn failed(e: String) -> Failure {
    tracing::debug!(event = "remote_pr_action_failed", error = %e);
    Failure::Failed(error_code(&e).to_string())
}

/// What an action answers with: the review run it started / resumed / found.
type PrResult = Result<Option<String>, Failure>;

/// Run one PR action (already allow-listed, rate-limited and flag-gated).
///
/// The review actions fetch from GitHub and launch a session, which takes
/// seconds: they run on their own task so the room keeps reading frames, and
/// ack when done.
pub async fn handle(ctx: &Arc<HandlerCtx>, cmd: &CmdPayload) {
    match cmd.action.as_str() {
        "pr_inbox_list" => finish(ctx, cmd, list(ctx, false).await).await,
        "pr_inbox_refresh" => finish(ctx, cmd, list(ctx, true).await).await,
        "pr_review_start" | "pr_review_again" => {
            let ctx = ctx.clone();
            let cmd = cmd.clone();
            tokio::spawn(async move {
                let result = if cmd.action == "pr_review_start" {
                    start_review(&ctx, &cmd).await
                } else {
                    review_again(&ctx, &cmd).await
                };
                finish(&ctx, &cmd, result).await;
            });
        }
        _ => finish(ctx, cmd, Err(Failure::Rejected(RejectReason::NotAllowed))).await,
    }
}

async fn finish(ctx: &HandlerCtx, cmd: &CmdPayload, result: PrResult) {
    let action = cmd.action.as_str();
    match result {
        Ok(run_id) => {
            accept(ctx, action, run_id.as_deref(), None).await;
            command_ack_for(ctx, cmd, true, None, run_id).await;
        }
        Err(Failure::Rejected(reason)) => {
            reject(ctx, action, None, reason).await;
            command_ack(ctx, cmd, false, Some(reason.as_str())).await;
        }
        Err(Failure::Failed(code)) => {
            audit::audit(&ctx.db, None, Some(&ctx.room_id), None, action, audit::RESULT_REJECTED, Some(&code))
                .await;
            command_ack(ctx, cmd, false, Some(&code)).await;
        }
    }
}

/// Send the cached inbox at once; fetch behind it when asked to or when it is
/// missing / older than a minute (the fetch's result is pushed live).
async fn list(ctx: &Arc<HandlerCtx>, force: bool) -> PrResult {
    let refresh = needs_refresh(force, pr_inbox::cache_age());
    if refresh {
        let app = ctx.app.clone();
        let db = ctx.db.clone();
        let weak = Arc::downgrade(ctx);
        tokio::spawn(async move {
            if let Err(e) = pr_inbox::refresh(&app, &db, false).await {
                tracing::debug!(event = "remote_pr_inbox_refresh_failed", error = %e);
                if let Some(ctx) = weak.upgrade() {
                    send_inbox(&ctx, false, Some("refresh_failed")).await;
                }
            }
        });
    }
    send_inbox(ctx, refresh || pr_inbox::is_refreshing(), None).await;
    Ok(None)
}

/// The PR a review action names, from the inbox cache.
fn cached_pr(cmd: &CmdPayload) -> Result<(String, PrInboxItem), Failure> {
    let key = cmd.item_id.as_deref().map(str::trim).filter(|k| !k.is_empty()).ok_or(RejectReason::Malformed)?;
    let item = pr_inbox::cached_item(key).ok_or(RejectReason::NotFound)?;
    Ok((pr_inbox::pr_key(&item), item))
}

/// PRs a review is being started for — one start per PR at a time, whichever
/// phone asked.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static S: std::sync::OnceLock<Mutex<HashSet<String>>> = std::sync::OnceLock::new();
    S.get_or_init(Default::default)
}

/// Holds a PR's slot in [`in_flight`] until dropped.
struct Claim(String);

impl Claim {
    fn take(key: &str) -> Result<Claim, Failure> {
        let mut set = in_flight().lock().map_err(|_| Failure::Failed("failed".into()))?;
        if !set.insert(key.to_string()) {
            return Err(Failure::Failed("busy".into()));
        }
        Ok(Claim(key.to_string()))
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if let Ok(mut set) = in_flight().lock() {
            set.remove(&self.0);
        }
    }
}

async fn run_status(ctx: &HandlerCtx, run_id: &str) -> Result<Option<String>, Failure> {
    sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(&ctx.db)
        .await
        .map_err(|e| failed(e.to_string()))
}

/// The session title the desktop gives an inbox review: `PR #N · @author · title`.
pub fn review_title(item: &PrInboxItem) -> String {
    let author = (!item.author_login.is_empty()).then(|| format!("@{}", item.author_login));
    [Some(format!("PR #{}", item.number)), author, Some(item.title.trim().to_string())]
        .into_iter()
        .flatten()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Desktop "Review": fetch the PR into a review run and launch it as a
/// session with the default review prompt. A PR that already has its review
/// run answers with that run instead of starting a second one.
async fn start_review(ctx: &HandlerCtx, cmd: &CmdPayload) -> PrResult {
    let (key, item) = cached_pr(cmd)?;
    let (Some(project_id), Some(repo_id)) = (item.project_id.clone(), item.repo_id.clone()) else {
        return Err(Failure::Failed("not_mapped".into()));
    };
    if !item.mapped {
        return Err(Failure::Failed("not_mapped".into()));
    }
    if let Some(run_id) = item.existing_run_id.clone() {
        if run_status(ctx, &run_id).await?.is_some() {
            return Ok(Some(run_id));
        }
    }
    let _claim = Claim::take(&key)?;

    let run = crate::commands::github::fetch_pr(
        ctx.app.state::<crate::db::Db>(),
        project_id,
        repo_id,
        item.number,
        None,
        // Review even when no linked issue can be resolved.
        Some(true),
        Some(review_title(&item)),
        // As the account that was asked to review, not the project default.
        Some(item.account_id.clone()),
    )
    .await
    .map_err(failed)?;
    // Empty goal: the default review prompt; default worker cap; the global
    // default permission mode (start_run_inner's fallback).
    crate::conductor::commands::start_conductor(
        ctx.app.clone(),
        ctx.app.state::<crate::db::Db>(),
        ctx.app.state::<crate::runs::RunRegistry>(),
        ctx.app.state::<crate::conductor::ConductorState>(),
        run.id.clone(),
        Some(String::new()),
        None,
        None,
        None,
        None,
        None,
        None,
    )
    .await
    .map_err(failed)?;
    pr_inbox::link_run(&ctx.app, &key, &run.id);
    crate::remote::live_lists::emit_runs_changed(&ctx.app);
    Ok(Some(run.id))
}

/// Desktop "Re-review": re-fetch the review run's task file (latest diff,
/// comments, reviews), resume its session and send the re-review prompt.
async fn review_again(ctx: &HandlerCtx, cmd: &CmdPayload) -> PrResult {
    let (key, item) = cached_pr(cmd)?;
    let Some(run_id) = item.existing_run_id.clone() else {
        return Err(Failure::Failed("not_reviewed".into()));
    };
    match run_status(ctx, &run_id).await?.as_deref() {
        None => return Err(RejectReason::NotFound.into()),
        Some("running" | "fetched") => return Err(Failure::Failed("busy".into())),
        Some(_) => {}
    }
    let _claim = Claim::take(&key)?;

    let prompt = pr_inbox::rereview_prompt(&ctx.db).await.map_err(failed)?;
    crate::commands::github::refetch_run(ctx.app.state::<crate::db::Db>(), run_id.clone())
        .await
        .map_err(failed)?;
    crate::commands::runs::resume_run(
        ctx.app.clone(),
        ctx.app.state::<crate::db::Db>(),
        ctx.app.state::<crate::runs::RunRegistry>(),
        run_id.clone(),
        None,
        None,
        Some(false),
    )
    .await
    .map_err(failed)?;
    let payload = crate::commands::runs::SendUserMessagePayload {
        run_id: run_id.clone(),
        content: prompt,
        images: Vec::new(),
        override_budget: false,
    };
    crate::commands::runs::send_user_message_inner(&ctx.db, &ctx.registry, payload, true)
        .await
        .map_err(failed)?;
    pr_inbox::link_run(&ctx.app, &key, &run_id);
    crate::remote::live_lists::emit_runs_changed(&ctx.app);
    Ok(Some(run_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::github::{PrInboxAccountError, PrInboxResult};
    use serde_json::json;

    fn item() -> PrInboxItem {
        PrInboxItem {
            account_id: "acc-1".into(),
            account_label: "work".into(),
            owner: "Acme".into(),
            repo: "Web".into(),
            number: 42,
            title: "  Fix login ".into(),
            html_url: "https://github.com/Acme/Web/pull/42".into(),
            author_login: "octo".into(),
            author_avatar_url: None,
            author_is_bot: false,
            draft: false,
            labels: vec!["bug".into()],
            updated_at: "2026-11-16T00:00:00Z".into(),
            mapped: true,
            project_id: Some("p1".into()),
            repo_id: Some("r1".into()),
            project_name: Some("Web".into()),
            existing_run_id: None,
            existing_run_status: None,
            reviewer_accounts: Vec::new(),
            requested_reviewers: Vec::new(),
            updated_since_review: false,
            stats: Default::default(),
        }
    }

    #[test]
    fn wire_item_leaves_out_ids_the_phone_does_not_need() {
        let v = serde_json::to_value(to_wire(&item())).unwrap();
        assert_eq!(v["key"], "acme/web#42");
        assert_eq!(v["owner"], "Acme");
        assert_eq!(v["account_label"], "work");
        assert_eq!(v["project_id"], "p1");
        for hidden in ["account_id", "repo_id", "reviewer_accounts", "requested_reviewers"] {
            assert!(v.get(hidden).is_none(), "{hidden} must stay on the Host");
        }
        assert!(v.get("existing_run_id").is_none(), "None is left out");
    }

    #[test]
    fn inbox_payload_shapes() {
        let v = serde_json::to_value(inbox_payload(None, true, None)).unwrap();
        assert_eq!(v, json!({ "kind": "pr_inbox", "items": [], "refreshing": true }));

        let cached = CachedPrInbox {
            result: PrInboxResult {
                items: vec![item()],
                errors: vec![PrInboxAccountError {
                    account_id: "acc-2".into(),
                    account_label: "personal".into(),
                    message: "401 Bad credentials".into(),
                }],
                fetched_accounts: vec!["acc-1".into()],
            },
            fetched_at: "2026-11-16T01:00:00Z".into(),
        };
        let v = serde_json::to_value(inbox_payload(Some(cached), false, Some("refresh_failed".into()))).unwrap();
        assert_eq!(v["items"][0]["number"], 42);
        assert_eq!(v["fetched_at"], "2026-11-16T01:00:00Z");
        assert_eq!(v["failed_accounts"], json!(["personal"]));
        assert_eq!(v["error"], "refresh_failed");
        // A fetch error's text (it may quote a token or a provider answer) never leaves.
        assert!(!v.to_string().contains("Bad credentials"));
    }

    #[test]
    fn refresh_when_asked_missing_or_stale() {
        assert!(needs_refresh(true, Some(Duration::from_secs(1))));
        assert!(needs_refresh(false, None));
        assert!(needs_refresh(false, Some(Duration::from_secs(61))));
        assert!(!needs_refresh(false, Some(Duration::from_secs(30))));
    }

    #[test]
    fn review_title_matches_the_desktop() {
        assert_eq!(review_title(&item()), "PR #42 · @octo · Fix login");
        let mut anon = item();
        anon.author_login = String::new();
        anon.title = " ".into();
        assert_eq!(review_title(&anon), "PR #42");
    }

    #[test]
    fn one_start_per_pr_at_a_time() {
        let first = Claim::take("acme/web#7").ok().expect("free");
        assert!(matches!(Claim::take("acme/web#7"), Err(Failure::Failed(c)) if c == "busy"));
        assert!(Claim::take("acme/web#8").is_ok(), "another PR is independent");
        drop(first);
        assert!(Claim::take("acme/web#7").is_ok(), "released on drop");
    }
}
