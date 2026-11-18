//! GitHub Mentions: places on GitHub where someone is waiting for the user's
//! answer — a direct `@login`, a team mention, or a reply on an issue / PR the
//! user takes part in — gathered across every linked GitHub account.
//!
//! Source is the Notifications API (`participating=true`, read ones included so
//! a mention the user only glanced at on github.com stays listed). For each
//! thread the comments are read to find the one waiting for an answer (the
//! "trigger") and whether the user commented / reviewed after it ("answered").
//!
//! Like the PR Inbox (`pr_inbox`) the last result is cached in memory and
//! broadcast (`UPDATED_EVENT`) after each fetch, and a poller re-fetches every
//! `github_mentions.poll_minutes` (default 5, "0" = off).

mod logic;

use crate::commands::translate::{submit_prompt, TranslateState, TurnEvent};
use crate::db::Db;
use crate::{github, secrets};
use futures_util::stream::{self, StreamExt};
use logic::{mentions_login, mentions_team, pick_trigger, Comment, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

pub const UPDATED_EVENT: &str = "github-mentions-updated";
const POLL_MINUTES_KEY: &str = "github_mentions.poll_minutes";
const DISMISSED_KEY: &str = "github_mentions.dismissed";
const DEFAULT_POLL_MINUTES: u64 = 5;
const FIRST_TICK: Duration = Duration::from_secs(20);
const IDLE_TICK: Duration = Duration::from_secs(3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
const DRAFT_TIMEOUT: Duration = Duration::from_secs(120);
/// Notifications older than this are not looked at.
const LOOKBACK_DAYS: i64 = 30;
/// Answered mentions stay listed (collapsed) this long after the answer.
const ANSWERED_KEEP_DAYS: i64 = 7;
/// Threads whose comments are read at once, per account.
const THREAD_CONCURRENCY: usize = 6;
const MAX_THREADS: usize = 150;
/// Excerpt length sent to the UI.
const EXCERPT_CHARS: usize = 600;
const MAX_REPLY_CHARS: usize = 20_000;
const MAX_DISMISSED: usize = 500;

/// Notification reasons that can wait for an answer. Review requests have the
/// PR Inbox; assignments, CI and state changes ask nothing of the user.
const REASONS: &[&str] = &["mention", "team_mention", "comment", "author"];

#[derive(Debug, Serialize, Clone)]
pub struct MentionComment {
    /// GitHub comment id; 0 when the trigger is the issue / PR body.
    pub id: u64,
    pub author_login: String,
    pub author_avatar_url: String,
    pub body: String,
    pub html_url: String,
    pub created_at: String,
    /// A PR review (diff) comment: replies go into its review thread.
    pub review_comment: bool,
    pub path: Option<String>,
    /// First comment of the review thread (replies must target it).
    #[serde(skip)]
    pub thread_root_id: Option<u64>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MentionItem {
    /// `{account_id}:{thread_id}` — stable across fetches.
    pub key: String,
    pub thread_id: String,
    pub account_id: String,
    pub account_label: String,
    pub account_login: String,
    /// mention | team_mention | comment | author
    pub reason: String,
    pub unread: bool,
    /// PullRequest | Issue | Discussion
    pub kind: String,
    pub owner: String,
    pub repo: String,
    pub number: Option<u64>,
    pub title: String,
    pub html_url: String,
    pub updated_at: String,
    pub trigger: Option<MentionComment>,
    /// Comments by others after the trigger.
    pub newer_comments: u32,
    pub answered: bool,
    pub answered_at: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
pub struct MentionAccountError {
    pub account_id: String,
    pub account_label: String,
    pub message: String,
}

#[derive(Debug, Serialize, Clone, Default)]
pub struct MentionsResult {
    pub items: Vec<MentionItem>,
    pub errors: Vec<MentionAccountError>,
}

#[derive(Debug, Serialize, Clone)]
pub struct CachedMentions {
    #[serde(flatten)]
    pub result: MentionsResult,
    pub fetched_at: String,
}

// ---------------------------------------------------------------------------
// GitHub payloads (only the fields used)

#[derive(Deserialize)]
struct Notification {
    id: String,
    unread: bool,
    reason: String,
    updated_at: String,
    subject: Subject,
    repository: NRepo,
}

#[derive(Deserialize)]
struct Subject {
    title: String,
    url: Option<String>,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct NRepo {
    name: String,
    owner: NOwner,
    html_url: String,
}

#[derive(Deserialize)]
struct NOwner {
    login: String,
}

#[derive(Deserialize, Clone)]
struct GhUser {
    login: String,
    #[serde(default)]
    avatar_url: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Deserialize)]
struct GhComment {
    id: u64,
    user: Option<GhUser>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    created_at: String,
    #[serde(default)]
    path: Option<String>,
    /// Set on a reply inside a review thread: the thread's first comment.
    #[serde(default)]
    in_reply_to_id: Option<u64>,
}

#[derive(Deserialize)]
struct GhReview {
    id: u64,
    user: Option<GhUser>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(Deserialize)]
struct GhIssue {
    user: Option<GhUser>,
    #[serde(default)]
    body: Option<String>,
    html_url: String,
    created_at: String,
}

// ---------------------------------------------------------------------------
// Fetch

struct Account {
    id: String,
    label: String,
    login: String,
}

async fn accounts(db: &Db) -> Result<Vec<Account>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, label, username FROM github_accounts WHERE username IS NOT NULL AND username != '' ORDER BY created_at ASC",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| Account { id: r.get("id"), label: r.get("label"), login: r.get("username") })
        .filter(|a| secrets::has_account_pat(&a.id))
        .collect())
}

/// `owner/repo/{issues|pulls}/N` API url → number.
fn number_from_url(url: &str) -> Option<u64> {
    url.rsplit('/').next()?.parse().ok()
}

fn is_bot(u: &GhUser) -> bool {
    u.kind == "Bot" || u.login.ends_with("[bot]")
}

fn excerpt(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= EXCERPT_CHARS {
        return s.to_string();
    }
    let mut out: String = s.chars().take(EXCERPT_CHARS).collect();
    out.push('…');
    out
}

async fn get_all<T: serde::de::DeserializeOwned>(
    client: &octocrab::Octocrab,
    route: &str,
    params: &[(&str, &str)],
) -> Result<Vec<T>, String> {
    let page = client
        .get::<octocrab::Page<T>, _, _>(route, Some(params))
        .await
        .map_err(|e| e.to_string())?;
    client.all_pages(page).await.map_err(|e| e.to_string())
}

/// Every comment on the thread since `since` (issue comments, and for PRs the
/// review comments and submitted reviews), oldest first.
async fn thread_comments(
    client: &octocrab::Octocrab,
    owner: &str,
    repo: &str,
    number: u64,
    is_pr: bool,
    since: &str,
) -> Result<Vec<(Comment, Option<MentionComment>)>, String> {
    let base = format!("/repos/{owner}/{repo}");
    let (issue_route, review_route, reviews_route) = (
        format!("{base}/issues/{number}/comments"),
        format!("{base}/pulls/{number}/comments"),
        format!("{base}/pulls/{number}/reviews"),
    );
    let since_params = [("since", since), ("per_page", "100")];
    let page_params = [("per_page", "100")];
    let issue = get_all::<GhComment>(client, &issue_route, &since_params);
    let (issue, review, reviews) = if is_pr {
        let review = get_all::<GhComment>(client, &review_route, &since_params);
        let reviews = get_all::<GhReview>(client, &reviews_route, &page_params);
        let (a, b, c) = tokio::join!(issue, review, reviews);
        (a?, b?, c?)
    } else {
        (issue.await?, Vec::new(), Vec::new())
    };

    let mut out = Vec::new();
    for (list, review_comment) in [(issue, false), (review, true)] {
        for c in list {
            let Some(user) = c.user else { continue };
            let body = c.body.unwrap_or_default();
            out.push((
                Comment {
                    author: user.login.clone(),
                    bot: is_bot(&user),
                    body: body.clone(),
                    created_at: c.created_at.clone(),
                },
                Some(MentionComment {
                    id: c.id,
                    author_login: user.login,
                    author_avatar_url: user.avatar_url,
                    body: excerpt(&body),
                    html_url: c.html_url,
                    created_at: c.created_at,
                    review_comment,
                    path: c.path,
                    thread_root_id: c.in_reply_to_id,
                }),
            ));
        }
    }
    // Submitted reviews: the user's own (often empty) one answers; a review
    // summary by someone else can carry the mention (answered in the PR thread).
    for r in reviews {
        let (Some(user), Some(at)) = (r.user, r.submitted_at) else { continue };
        if at.as_str() < since {
            continue;
        }
        let body = r.body.unwrap_or_default();
        out.push((
            Comment { author: user.login.clone(), bot: is_bot(&user), body: body.clone(), created_at: at.clone() },
            Some(MentionComment {
                id: r.id,
                author_login: user.login,
                author_avatar_url: user.avatar_url,
                body: excerpt(&body),
                html_url: r.html_url,
                created_at: at,
                review_comment: false,
                path: None,
                thread_root_id: None,
            }),
        ));
    }
    out.sort_by(|a, b| a.0.created_at.cmp(&b.0.created_at));
    Ok(out)
}

async fn build_item(
    client: &octocrab::Octocrab,
    acct: &Account,
    n: Notification,
    since: &str,
) -> Result<Option<MentionItem>, String> {
    if !REASONS.contains(&n.reason.as_str()) {
        return Ok(None);
    }
    let owner = n.repository.owner.login.clone();
    let repo = n.repository.name.clone();
    let is_discussion = n.subject.kind == "Discussion";
    // A discussion's number is not an issue number: never reply / draft there.
    let number = n.subject.url.as_deref().and_then(number_from_url).filter(|_| !is_discussion);
    let is_pr = n.subject.kind == "PullRequest";
    let base_item = |html_url: String| MentionItem {
        key: format!("{}:{}", acct.id, n.id),
        thread_id: n.id.clone(),
        account_id: acct.id.clone(),
        account_label: acct.label.clone(),
        account_login: acct.login.clone(),
        reason: n.reason.clone(),
        unread: n.unread,
        kind: n.subject.kind.clone(),
        owner: owner.clone(),
        repo: repo.clone(),
        number,
        title: n.subject.title.clone(),
        html_url,
        updated_at: n.updated_at.clone(),
        trigger: None,
        newer_comments: 0,
        answered: false,
        answered_at: None,
    };

    // Discussions (GraphQL only) and other subjects: no comment lookup — listed
    // while unread, so the user can open them on GitHub.
    if is_discussion {
        let url = format!("{}/discussions", n.repository.html_url);
        return Ok(n.unread.then(|| base_item(url)));
    }
    let (Some(number), true) = (number, n.subject.kind == "Issue" || is_pr) else {
        return Ok(None);
    };
    let kind_path = if is_pr { "pull" } else { "issues" };
    let html_url = format!("{}/{kind_path}/{number}", n.repository.html_url);

    let comments = thread_comments(client, &owner, &repo, number, is_pr, since).await?;
    let plain: Vec<Comment> = comments.iter().map(|(c, _)| c.clone()).collect();
    let mut found = pick_trigger(&plain, &acct.login, &n.reason);

    // A mention in the issue / PR description itself (no comment carries it).
    let mut body_trigger: Option<MentionComment> = None;
    if found.is_none() && (n.reason == "mention" || n.reason == "team_mention") {
        let route = format!("/repos/{owner}/{repo}/issues/{number}");
        let issue = client.get::<GhIssue, _, ()>(&route, None).await.map_err(|e| e.to_string())?;
        let body = issue.body.clone().unwrap_or_default();
        let mentioned = if n.reason == "mention" { mentions_login(&body, &acct.login) } else { mentions_team(&body) };
        if mentioned {
            if let Some(user) = issue.user.filter(|u| !u.login.eq_ignore_ascii_case(&acct.login)) {
                let mine_after = plain
                    .iter()
                    .filter(|c| c.author.eq_ignore_ascii_case(&acct.login) && c.created_at > issue.created_at)
                    .map(|c| c.created_at.clone())
                    .next_back();
                found = Some(Trigger {
                    index: usize::MAX,
                    newer_comments: plain.iter().filter(|c| !c.author.eq_ignore_ascii_case(&acct.login)).count() as u32,
                    answered_at: mine_after,
                });
                body_trigger = Some(MentionComment {
                    id: 0,
                    author_login: user.login,
                    author_avatar_url: user.avatar_url,
                    body: excerpt(&body),
                    html_url: issue.html_url,
                    created_at: issue.created_at,
                    review_comment: false,
                    path: None,
                    thread_root_id: None,
                });
            }
        }
    }

    let Some(t) = found else { return Ok(None) };
    let trigger = if t.index == usize::MAX { body_trigger } else { comments[t.index].1.clone() };
    let answered = t.answered_at.is_some();
    let mut item = base_item(html_url);
    item.trigger = trigger;
    item.newer_comments = t.newer_comments;
    item.answered = answered;
    item.answered_at = t.answered_at;
    Ok(Some(item))
}

/// Reading a thread's comments costs 1–3 requests, and an account can have a
/// few hundred participating threads: a thread is only re-read when GitHub
/// reports activity on it (`updated_at` moved). The read flag still comes from
/// the fresh notification, and an answer that aged out drops off here.
async fn memo_item(
    client: &octocrab::Octocrab,
    acct: &Account,
    n: Notification,
    since: &str,
    answered_cutoff: &str,
) -> Option<MentionItem> {
    let memo_key = format!("{}:{}", acct.id, n.id);
    let hit = state()
        .threads
        .lock()
        .ok()
        .and_then(|m| m.get(&memo_key).filter(|(at, _)| *at == n.updated_at).map(|(_, item)| item.clone()));
    let item = match hit {
        Some(item) => {
            let unread = n.unread;
            item.map(|mut i| {
                i.unread = unread;
                i
            })
        }
        None => {
            let updated_at = n.updated_at.clone();
            let thread = format!("{}/{}", n.repository.owner.login, n.repository.name);
            match build_item(client, acct, n, since).await {
                Ok(item) => {
                    if let Ok(mut m) = state().threads.lock() {
                        m.insert(memo_key, (updated_at, item.clone()));
                    }
                    item
                }
                // Not remembered: the next fetch reads the thread again.
                Err(e) => {
                    tracing::warn!(event = "github_mentions_thread_failed", repo = %thread, error = %e);
                    None
                }
            }
        }
    };
    item.filter(|i| !i.answered_at.as_deref().is_some_and(|a| a < answered_cutoff))
}

async fn fetch_account(acct: &Account, dismissed: &HashSet<String>) -> Result<Vec<MentionItem>, String> {
    let client = github::client_for_account(&acct.id).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now();
    let since = (now - chrono::Duration::days(LOOKBACK_DAYS)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let answered_cutoff = (now - chrono::Duration::days(ANSWERED_KEEP_DAYS)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    let mut last_err = None;
    let mut notifications: Option<Vec<Notification>> = None;
    for attempt in 0u32..3 {
        match get_all::<Notification>(
            &client,
            "/notifications",
            &[("participating", "true"), ("all", "true"), ("since", &since), ("per_page", "50")],
        )
        .await
        {
            Ok(n) => {
                notifications = Some(n);
                break;
            }
            Err(e) => {
                last_err = Some(e);
                if attempt < 2 {
                    tokio::time::sleep(Duration::from_millis(300 * (attempt as u64 + 1))).await;
                }
            }
        }
    }
    let Some(mut notifications) = notifications else {
        return Err(last_err.unwrap_or_else(|| "GitHub notifications failed".into()));
    };
    notifications.retain(|n| REASONS.contains(&n.reason.as_str()));
    notifications.truncate(MAX_THREADS);
    // Forget threads of this account that left the window.
    let live: HashSet<String> = notifications.iter().map(|n| format!("{}:{}", acct.id, n.id)).collect();
    let prefix = format!("{}:", acct.id);
    if let Ok(mut m) = state().threads.lock() {
        m.retain(|k, _| !k.starts_with(&prefix) || live.contains(k));
    }

    let items: Vec<MentionItem> = stream::iter(notifications)
        .map(|n| memo_item(&client, acct, n, &since, &answered_cutoff))
        .buffer_unordered(THREAD_CONCURRENCY)
        .filter_map(|i| async move { i })
        .collect()
        .await;
    Ok(items.into_iter().filter(|i| !dismissed.contains(&dismiss_key(i))).collect())
}

/// What "Ignore" remembers: the thread plus the comment waiting for an answer,
/// so a later mention on the same thread shows up again.
fn dismiss_key(item: &MentionItem) -> String {
    let trigger = item.trigger.as_ref().map(|t| t.id.to_string()).unwrap_or_else(|| item.updated_at.clone());
    format!("{}:{}", item.key, trigger)
}

async fn fetch_all(db: &Db) -> Result<MentionsResult, String> {
    let accounts = accounts(db).await?;
    let dismissed: HashSet<String> = load_dismissed(db).await.into_iter().collect();
    let results = futures_util::future::join_all(accounts.iter().map(|a| fetch_account(a, &dismissed))).await;

    let mut out = MentionsResult::default();
    let mut seen = HashSet::new();
    for (acct, res) in accounts.iter().zip(results) {
        match res {
            Ok(items) => {
                for item in items {
                    // The same thread reaching two accounts: keep the first.
                    let thread = format!("{}/{}#{:?}:{:?}", item.owner, item.repo, item.number, item.trigger.as_ref().map(|t| t.id));
                    if seen.insert(thread.to_lowercase()) {
                        out.items.push(item);
                    }
                }
            }
            Err(message) => out.errors.push(MentionAccountError {
                account_id: acct.id.clone(),
                account_label: acct.label.clone(),
                message,
            }),
        }
    }
    let at = |i: &MentionItem| i.trigger.as_ref().map(|t| t.created_at.clone()).unwrap_or_else(|| i.updated_at.clone());
    out.items.sort_by(|a, b| a.answered.cmp(&b.answered).then_with(|| at(b).cmp(&at(a))));
    Ok(out)
}

// ---------------------------------------------------------------------------
// Settings-backed state

async fn setting(db: &Db, key: &str) -> Option<String> {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
}

async fn set_setting(db: &Db, key: &str, value: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(db)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

async fn load_dismissed(db: &Db) -> Vec<String> {
    setting(db, DISMISSED_KEY).await.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

async fn poll_minutes(db: &Db) -> u64 {
    setting(db, POLL_MINUTES_KEY).await.and_then(|v| v.trim().parse().ok()).unwrap_or(DEFAULT_POLL_MINUTES)
}

// ---------------------------------------------------------------------------
// Cache + poller (same shape as pr_inbox)

struct Cached {
    value: CachedMentions,
    at: Instant,
}

#[derive(Default)]
struct MentionsState {
    cache: std::sync::Mutex<Option<Cached>>,
    refresh_lock: tokio::sync::Mutex<()>,
    /// `{account_id}:{thread_id}` → (notification `updated_at`, built item).
    threads: std::sync::Mutex<HashMap<String, (String, Option<MentionItem>)>>,
}

fn state() -> &'static MentionsState {
    static S: std::sync::OnceLock<MentionsState> = std::sync::OnceLock::new();
    S.get_or_init(MentionsState::default)
}

fn cached() -> Option<CachedMentions> {
    state().cache.lock().ok()?.as_ref().map(|c| c.value.clone())
}

fn cached_at() -> Option<Instant> {
    state().cache.lock().ok()?.as_ref().map(|c| c.at)
}

fn store(app: &AppHandle, result: MentionsResult) {
    let value = CachedMentions { result, fetched_at: chrono::Utc::now().to_rfc3339() };
    if let Ok(mut cache) = state().cache.lock() {
        *cache = Some(Cached { value: value.clone(), at: Instant::now() });
    }
    let _ = app.emit(UPDATED_EVENT, &value);
}

/// Edit the cached list in place (after a reply / dismiss) and broadcast it,
/// so the UI updates without waiting for a full re-fetch.
fn patch_cache(app: &AppHandle, f: impl FnOnce(&mut Vec<MentionItem>)) {
    let Some(mut value) = cached() else { return };
    f(&mut value.result.items);
    store(app, value.result);
}

/// The thread changed on our side (a reply): read it again on the next fetch.
fn forget_thread(key: &str) {
    if let Ok(mut m) = state().threads.lock() {
        m.remove(key);
    }
}

/// Single flight: a call made while another fetch runs waits for it and
/// returns its result instead of fetching again.
pub async fn refresh(app: &AppHandle, db: &Db) -> Result<MentionsResult, String> {
    let st = state();
    let called = Instant::now();
    let _guard = match st.refresh_lock.try_lock() {
        Ok(g) => g,
        Err(_) => {
            let g = st.refresh_lock.lock().await;
            if cached_at().is_some_and(|at| at >= called) {
                if let Some(c) = cached() {
                    return Ok(c.result);
                }
            }
            g
        }
    };
    let result = tokio::time::timeout(FETCH_TIMEOUT, fetch_all(db))
        .await
        .map_err(|_| "Timed out fetching GitHub notifications".to_string())??;
    store(app, result.clone());
    Ok(result)
}

pub fn start(app: AppHandle, db: Db) {
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(FIRST_TICK).await;
        loop {
            let minutes = poll_minutes(&db).await;
            let wait = if minutes == 0 {
                IDLE_TICK
            } else {
                let interval = Duration::from_secs(minutes * 60);
                match cached_at().map(|at| at.elapsed()) {
                    Some(age) if age < interval => interval - age,
                    _ => {
                        if let Err(e) = refresh(&app, &db).await {
                            tracing::warn!(event = "github_mentions_poll_failed", error = %e);
                        }
                        interval
                    }
                }
            };
            tokio::time::sleep(wait).await;
        }
    });
}

// ---------------------------------------------------------------------------
// Commands

#[tauri::command]
pub fn get_cached_github_mentions() -> Option<CachedMentions> {
    cached()
}

#[tauri::command]
pub async fn list_github_mentions(app: AppHandle, db: State<'_, Db>) -> Result<MentionsResult, String> {
    refresh(&app, &db).await
}

fn cached_item(key: &str) -> Result<MentionItem, String> {
    cached()
        .and_then(|c| c.result.items.into_iter().find(|i| i.key == key))
        .ok_or_else(|| "This mention is no longer listed. Refresh and try again.".to_string())
}

async fn mark_thread_read(client: &octocrab::Octocrab, thread_id: &str) -> Result<(), String> {
    let res = client
        ._patch(format!("/notifications/threads/{thread_id}"), None::<&()>)
        .await
        .map_err(|e| e.to_string())?;
    octocrab::map_github_error(res).await.map(|_| ()).map_err(|e| e.to_string())
}

/// Mark the thread read on GitHub (the unread stripe goes away).
#[tauri::command]
pub async fn mark_github_mention_read(app: AppHandle, key: String) -> Result<(), String> {
    let item = cached_item(&key)?;
    let client = github::client_for_account(&item.account_id).map_err(|e| e.to_string())?;
    mark_thread_read(&client, &item.thread_id).await?;
    patch_cache(&app, |items| {
        if let Some(i) = items.iter_mut().find(|i| i.key == key) {
            i.unread = false;
        }
    });
    Ok(())
}

/// Hide the mention until someone writes on the thread again: remembered
/// locally, and marked read on GitHub.
#[tauri::command]
pub async fn dismiss_github_mention(app: AppHandle, db: State<'_, Db>, key: String) -> Result<(), String> {
    let item = cached_item(&key)?;
    let mut list = load_dismissed(&db).await;
    let dk = dismiss_key(&item);
    if !list.contains(&dk) {
        list.push(dk);
    }
    if list.len() > MAX_DISMISSED {
        list.drain(..list.len() - MAX_DISMISSED);
    }
    set_setting(&db, DISMISSED_KEY, &serde_json::to_string(&list).unwrap_or_default()).await?;
    if item.unread {
        if let Ok(client) = github::client_for_account(&item.account_id) {
            if let Err(e) = mark_thread_read(&client, &item.thread_id).await {
                tracing::warn!(event = "github_mentions_mark_read_failed", error = %e);
            }
        }
    }
    patch_cache(&app, |items| items.retain(|i| i.key != key));
    Ok(())
}

#[derive(Serialize)]
pub struct PostedReply {
    pub html_url: String,
}

/// Post a reply as the account the mention reached: into the review thread
/// for a diff comment, otherwise as an issue / PR comment.
#[tauri::command]
pub async fn reply_github_mention(app: AppHandle, key: String, body: String) -> Result<PostedReply, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("The reply is empty.".into());
    }
    if body.chars().count() > MAX_REPLY_CHARS {
        return Err(format!("The reply is longer than {MAX_REPLY_CHARS} characters."));
    }
    let item = cached_item(&key)?;
    let number = item.number.ok_or("Replying is only supported on issues and pull requests.")?;
    let client = github::client_for_account(&item.account_id).map_err(|e| e.to_string())?;

    #[derive(Deserialize)]
    struct Posted {
        html_url: String,
        created_at: String,
    }
    let payload = serde_json::json!({ "body": body });
    let review_id = item.trigger.as_ref().filter(|t| t.review_comment).map(|t| t.thread_root_id.unwrap_or(t.id));
    let route = match review_id {
        Some(id) => format!("/repos/{}/{}/pulls/{number}/comments/{id}/replies", item.owner, item.repo),
        None => format!("/repos/{}/{}/issues/{number}/comments", item.owner, item.repo),
    };
    let posted: Posted = client.post(route, Some(&payload)).await.map_err(|e| e.to_string())?;

    if let Err(e) = mark_thread_read(&client, &item.thread_id).await {
        tracing::warn!(event = "github_mentions_mark_read_failed", error = %e);
    }
    forget_thread(&key);
    patch_cache(&app, |items| {
        if let Some(i) = items.iter_mut().find(|i| i.key == key) {
            i.answered = true;
            i.unread = false;
            i.answered_at = Some(posted.created_at.clone());
        }
    });
    Ok(PostedReply { html_url: posted.html_url })
}

/// Draft a reply with the warm translate sidecar: the issue / PR text and the
/// recent comments as context, answering the trigger comment.
#[tauri::command]
pub async fn draft_github_mention_reply(
    app: AppHandle,
    db: State<'_, Db>,
    translate: State<'_, TranslateState>,
    key: String,
) -> Result<String, String> {
    let item = cached_item(&key)?;
    let number = item.number.ok_or("Drafting is only supported on issues and pull requests.")?;
    let client = github::client_for_account(&item.account_id).map_err(|e| e.to_string())?;
    let route = format!("/repos/{}/{}/issues/{number}", item.owner, item.repo);
    let issue: GhIssue = client.get::<GhIssue, _, ()>(&route, None).await.map_err(|e| e.to_string())?;
    let since = (chrono::Utc::now() - chrono::Duration::days(365)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let comments = thread_comments(&client, &item.owner, &item.repo, number, item.kind == "PullRequest", &since)
        .await
        .unwrap_or_default();

    let prompt = logic::draft_prompt(&logic::DraftInput {
        login: &item.account_login,
        kind: &item.kind,
        repo: &format!("{}/{}", item.owner, item.repo),
        number,
        title: &item.title,
        description: issue.body.as_deref().unwrap_or(""),
        description_author: issue.user.as_ref().map(|u| u.login.as_str()).unwrap_or(""),
        comments: &comments
            .iter()
            .filter(|(c, _)| !c.body.trim().is_empty())
            .map(|(c, m)| (c.author.as_str(), c.body.as_str(), m.as_ref().and_then(|m| m.path.as_deref())))
            .collect::<Vec<_>>(),
        trigger: item.trigger.as_ref().map(|t| (t.author_login.as_str(), t.body.as_str())),
    });

    let (turn, rx) = submit_prompt(&app, &db, &translate, prompt, true).await?;
    let mut rx = rx.ok_or("no result channel")?;
    let wait = async {
        loop {
            match rx.recv().await {
                Some(TurnEvent::Chunk(_)) => continue,
                Some(TurnEvent::Done(t)) => return Ok(t),
                Some(TurnEvent::Error(e)) => return Err(e),
                None => return Err("The draft ended unexpectedly.".to_string()),
            }
        }
    };
    match tokio::time::timeout(DRAFT_TIMEOUT, wait).await {
        Ok(r) => r.map(|t| t.trim().to_string()),
        Err(_) => {
            translate.abandon_turn(turn).await;
            Err("Drafting took too long. Try again.".into())
        }
    }
}
