//! GitHub Mentions: comments on GitHub that `@` the user directly and wait for
//! their answer, gathered across every linked GitHub account. Team mentions
//! and plain replies on threads the user takes part in are left out (noise).
//!
//! Source is the Notifications API over the last few days (setting
//! `LOOKBACK_KEY`, default 3) with `participating=true` (read ones included, so
//! a mention the user only glanced at on github.com stays listed). For each
//! thread the comments are read to find the last one `@`-ing the user (the
//! "trigger") and whether the user commented / reviewed after it ("answered").
//!
//! "Check with AI" opens a session on the issue / PR (in the Devdy project
//! that tracks the repo), like the PR Inbox's review; the link is remembered
//! (`RUNS_KEY`) so a second click opens that session.
//!
//! Like the PR Inbox (`pr_inbox`) the last result is cached in memory and
//! broadcast (`UPDATED_EVENT`) after each fetch, and a poller re-fetches every
//! `github_mentions.poll_minutes` (default 5, "0" = off).

mod logic;

use crate::db::Db;
use crate::{github, secrets};
use futures_util::stream::{self, StreamExt};
use logic::{mentions_login, pick_trigger, Comment, Trigger};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, State};

pub const UPDATED_EVENT: &str = "github-mentions-updated";
const POLL_MINUTES_KEY: &str = "github_mentions.poll_minutes";
const DISMISSED_KEY: &str = "github_mentions.dismissed";
/// Mention key → the "Check with AI" session started for it.
const RUNS_KEY: &str = "github_mentions.runs";
const MAX_RUN_LINKS: usize = 500;
const DEFAULT_POLL_MINUTES: u64 = 5;
const FIRST_TICK: Duration = Duration::from_secs(20);
const IDLE_TICK: Duration = Duration::from_secs(3600);
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);
/// Notifications older than this are not looked at.
/// How far back notifications are looked at (setting `LOOKBACK_KEY`).
const LOOKBACK_KEY: &str = "github_mentions.lookback_days";
const DEFAULT_LOOKBACK_DAYS: i64 = 3;
pub const LOOKBACK_CHOICES: &[i64] = &[1, 3, 7, 14, 30];
/// Per-thread results, persisted so a restart only re-reads changed threads.
/// (v2: only direct mentions; older entries also held replies / team tags.)
const THREADS_KEY: &str = "github_mentions.threads.v2";
/// Answered mentions stay listed (collapsed) this long after the answer.
const ANSWERED_KEEP_DAYS: i64 = 7;
/// Threads whose comments are read at once, per account.
const THREAD_CONCURRENCY: usize = 16;
const MAX_THREADS: usize = 150;
/// Excerpt length sent to the UI.
const EXCERPT_CHARS: usize = 600;
const MAX_DISMISSED: usize = 500;
/// Instruction a "Check with AI" session starts with, configured like the PR
/// review prompt: custom text, or a Settings → Prompt Templates entry (id key,
/// wins while it exists). The mention itself is appended by the UI.
const CHECK_PROMPT_KEY: &str = "github_mentions.check_prompt";
const CHECK_PROMPT_ID_KEY: &str = "github_mentions.check_prompt_id";
/// Used while the user hasn't set their own check prompt.
const DEFAULT_CHECK_PROMPT: &str = "Có người tag tôi trên GitHub, chi tiết ở cuối tin nhắn này. Đọc file task (nội dung issue / PR, comment, và diff nếu có) cùng code liên quan trong repo để hiểu người đó đang hỏi hoặc yêu cầu gì. Sau đó trả lời bằng tiếng Việt, gồm:\n1. Tóm tắt ngắn yêu cầu.\n2. Câu trả lời hoặc hướng xử lý, kèm dẫn chứng (file:dòng).\n3. Bản nháp comment để tôi đăng lên GitHub, viết cùng ngôn ngữ với thread.\n\nKhông tự đăng comment hay thay đổi gì trên GitHub.";
/// "Not interested": threads / repos never listed again, whatever happens on
/// them (unlike `DISMISSED_KEY`, which a new comment undoes).
const MUTED_KEY: &str = "github_mentions.muted";
const MAX_MUTED: usize = 1000;

/// Notification reasons whose threads may hold a direct `@login`: a mention on
/// a thread the user already follows can arrive as `comment` / `author`, so
/// those are read too, but only a comment that `@`s the user is listed. Team
/// mentions, review requests (PR Inbox), assignments, CI and state changes are
/// not.
const REASONS: &[&str] = &["mention", "comment", "author"];

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MentionComment {
    /// GitHub comment id; 0 when the trigger is the issue / PR body.
    pub id: u64,
    pub author_login: String,
    pub author_avatar_url: String,
    pub body: String,
    pub html_url: String,
    pub created_at: String,
    /// A PR review (diff) comment.
    pub review_comment: bool,
    pub path: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MentionItem {
    /// `{account_id}:{thread_id}` — stable across fetches.
    pub key: String,
    pub thread_id: String,
    pub account_id: String,
    pub account_label: String,
    pub account_login: String,
    /// Always `mention` (only direct mentions are listed).
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
    /// The Devdy project / repo tracking this GitHub repo, if any.
    pub project_id: Option<String>,
    pub repo_id: Option<String>,
    pub project_name: Option<String>,
    /// The "Check with AI" session started for this mention.
    pub existing_run_id: Option<String>,
    pub existing_run_status: Option<String>,
}

/// A muted thread (`scope` "thread", `target` = mention key) or repository
/// (`scope` "repo", `target` = lowercase `owner/repo`, every account).
#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct MentionMute {
    pub scope: String,
    pub target: String,
    /// Shown in the muted list: `owner/repo#N title` or `owner/repo`.
    pub label: String,
    pub muted_at: String,
}

#[derive(Default)]
struct MuteSet {
    threads: HashSet<String>,
    repos: HashSet<String>,
}

impl MuteSet {
    fn from(list: &[MentionMute]) -> Self {
        let mut s = MuteSet::default();
        for m in list {
            match m.scope.as_str() {
                "thread" => s.threads.insert(m.target.clone()),
                _ => s.repos.insert(m.target.clone()),
            };
        }
        s
    }

    fn mutes(&self, thread_key: &str, owner: &str, repo: &str) -> bool {
        self.threads.contains(thread_key) || self.repos.contains(&repo_target(owner, repo))
    }
}

fn repo_target(owner: &str, repo: &str) -> String {
    format!("{owner}/{repo}").to_lowercase()
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
    // A discussion's number is not an issue number: no AI check there.
    let number = n.subject.url.as_deref().and_then(number_from_url).filter(|_| !is_discussion);
    let is_pr = n.subject.kind == "PullRequest";
    let base_item = |html_url: String| MentionItem {
        key: format!("{}:{}", acct.id, n.id),
        thread_id: n.id.clone(),
        account_id: acct.id.clone(),
        account_label: acct.label.clone(),
        account_login: acct.login.clone(),
        reason: "mention".into(),
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
        project_id: None,
        repo_id: None,
        project_name: None,
        existing_run_id: None,
        existing_run_status: None,
    };

    // Discussions (GraphQL only): no comment lookup — listed while unread when
    // GitHub says the user was mentioned, so they can open it on GitHub.
    if is_discussion {
        let url = format!("{}/discussions", n.repository.html_url);
        return Ok((n.unread && n.reason == "mention").then(|| base_item(url)));
    }
    let (Some(number), true) = (number, n.subject.kind == "Issue" || is_pr) else {
        return Ok(None);
    };
    let kind_path = if is_pr { "pull" } else { "issues" };
    let html_url = format!("{}/{kind_path}/{number}", n.repository.html_url);

    let comments = thread_comments(client, &owner, &repo, number, is_pr, since).await?;
    let plain: Vec<Comment> = comments.iter().map(|(c, _)| c.clone()).collect();
    let mut found = pick_trigger(&plain, &acct.login);

    // A mention in the issue / PR description itself (no comment carries it).
    let mut body_trigger: Option<MentionComment> = None;
    if found.is_none() && n.reason == "mention" {
        let route = format!("/repos/{owner}/{repo}/issues/{number}");
        let issue = client.get::<GhIssue, _, ()>(&route, None).await.map_err(|e| e.to_string())?;
        let body = issue.body.clone().unwrap_or_default();
        if mentions_login(&body, &acct.login) {
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

async fn fetch_account(
    acct: &Account,
    dismissed: &HashSet<String>,
    muted: &MuteSet,
    lookback_days: i64,
) -> Result<Vec<MentionItem>, String> {
    let client = github::client_for_account(&acct.id).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now();
    let since = (now - chrono::Duration::days(lookback_days)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let answered_cutoff = (now - chrono::Duration::days(ANSWERED_KEEP_DAYS.min(lookback_days))).to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

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
    // Muted threads / repos are dropped before their comments are read.
    notifications.retain(|n| {
        REASONS.contains(&n.reason.as_str())
            && !muted.mutes(&format!("{}:{}", acct.id, n.id), &n.repository.owner.login, &n.repository.name)
    });
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
    let muted = MuteSet::from(&load_muted(db).await);
    restore_threads(db).await;
    let lookback = lookback_days(db).await;
    let results = futures_util::future::join_all(accounts.iter().map(|a| fetch_account(a, &dismissed, &muted, lookback))).await;

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
    save_threads(db).await;
    enrich(db, &mut out.items).await;
    let at = |i: &MentionItem| i.trigger.as_ref().map(|t| t.created_at.clone()).unwrap_or_else(|| i.updated_at.clone());
    out.items.sort_by(|a, b| a.answered.cmp(&b.answered).then_with(|| at(b).cmp(&at(a))));
    Ok(out)
}

/// Project mapping (repo tracked in Devdy) and the linked AI-check session.
async fn enrich(db: &Db, items: &mut [MentionItem]) {
    use sqlx::Row;
    let mut repos: HashMap<(String, String), (String, String, String)> = HashMap::new();
    if let Ok(rows) = sqlx::query(
        "SELECT r.id AS repo_id, r.project_id AS project_id, p.name AS project_name,
                r.github_owner AS owner, r.github_repo AS repo
         FROM repos r JOIN projects p ON p.id = r.project_id
         WHERE r.github_owner IS NOT NULL AND r.github_repo IS NOT NULL
         ORDER BY r.created_at ASC",
    )
    .fetch_all(db)
    .await
    {
        for row in rows {
            let key = (row.get::<String, _>("owner").to_lowercase(), row.get::<String, _>("repo").to_lowercase());
            repos.entry(key).or_insert((row.get("repo_id"), row.get("project_id"), row.get("project_name")));
        }
    }
    let links = load_run_links(db).await;
    for item in items.iter_mut() {
        item.project_id = None;
        item.repo_id = None;
        item.project_name = None;
        item.existing_run_id = None;
        item.existing_run_status = None;
        if let Some((repo_id, project_id, name)) = repos.get(&(item.owner.to_lowercase(), item.repo.to_lowercase())) {
            item.repo_id = Some(repo_id.clone());
            item.project_id = Some(project_id.clone());
            item.project_name = Some(name.clone());
        }
        let Some(run_id) = links.get(&item.key) else { continue };
        // A deleted session no longer counts.
        let status = sqlx::query_scalar::<_, String>("SELECT status FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
        if let Some(status) = status {
            item.existing_run_id = Some(run_id.clone());
            item.existing_run_status = Some(status);
        }
    }
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

async fn load_muted(db: &Db) -> Vec<MentionMute> {
    setting(db, MUTED_KEY).await.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

async fn save_muted(db: &Db, list: &[MentionMute]) -> Result<(), String> {
    set_setting(db, MUTED_KEY, &serde_json::to_string(list).unwrap_or_default()).await
}

/// Kept in insertion order so the oldest links drop first past the cap.
async fn load_run_links(db: &Db) -> HashMap<String, String> {
    load_run_link_list(db).await.into_iter().collect()
}

async fn load_run_link_list(db: &Db) -> Vec<(String, String)> {
    setting(db, RUNS_KEY).await.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default()
}

type ThreadMemo = HashMap<String, (String, Option<MentionItem>)>;

/// Load the per-thread results saved by a previous app run (first fetch only).
async fn restore_threads(db: &Db) {
    use std::sync::atomic::Ordering;
    if state().threads_restored.swap(true, Ordering::SeqCst) {
        return;
    }
    let Some(saved) = setting(db, THREADS_KEY).await.and_then(|v| serde_json::from_str::<ThreadMemo>(&v).ok()) else {
        return;
    };
    if let Ok(mut m) = state().threads.lock() {
        for (k, v) in saved {
            m.entry(k).or_insert(v);
        }
    }
}

async fn save_threads(db: &Db) {
    let json = match state().threads.lock() {
        Ok(m) => serde_json::to_string(&*m).unwrap_or_default(),
        Err(_) => return,
    };
    if let Err(e) = set_setting(db, THREADS_KEY, &json).await {
        tracing::warn!(event = "github_mentions_save_threads_failed", error = %e);
    }
}

async fn lookback_days(db: &Db) -> i64 {
    setting(db, LOOKBACK_KEY)
        .await
        .and_then(|v| v.trim().parse().ok())
        .filter(|d| LOOKBACK_CHOICES.contains(d))
        .unwrap_or(DEFAULT_LOOKBACK_DAYS)
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
    /// The persisted per-thread results were loaded (once per app run).
    threads_restored: std::sync::atomic::AtomicBool,
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

/// Edit the cached list in place (after a dismiss / linking a session) and broadcast it,
/// so the UI updates without waiting for a full re-fetch.
fn patch_cache(app: &AppHandle, f: impl FnOnce(&mut Vec<MentionItem>)) {
    let Some(mut value) = cached() else { return };
    f(&mut value.result.items);
    store(app, value.result);
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
    let started = Instant::now();
    let result = tokio::time::timeout(FETCH_TIMEOUT, fetch_all(db))
        .await
        .map_err(|_| "Timed out fetching GitHub notifications".to_string())??;
    tracing::info!(
        event = "github_mentions_fetched",
        items = result.items.len(),
        errors = result.errors.len(),
        ms = started.elapsed().as_millis() as u64,
    );
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
pub async fn get_github_mentions_lookback_days(db: State<'_, Db>) -> Result<i64, String> {
    Ok(lookback_days(&db).await)
}

/// Change the window and forget the per-thread results: they were read with
/// the old window's `since`. The caller refreshes.
#[tauri::command]
pub async fn set_github_mentions_lookback_days(db: State<'_, Db>, days: i64) -> Result<(), String> {
    if !LOOKBACK_CHOICES.contains(&days) {
        return Err(format!("Unsupported window: {days} days"));
    }
    set_setting(&db, LOOKBACK_KEY, &days.to_string()).await?;
    if let Ok(mut m) = state().threads.lock() {
        m.clear();
    }
    if let Ok(mut c) = state().cache.lock() {
        *c = None;
    }
    save_threads(&db).await;
    Ok(())
}

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

/// "Not interested": never list this thread (`scope` "thread") or anything
/// from its repository (`scope` "repo") again, until unmuted. Marked read on
/// GitHub too; the GitHub subscription itself is left alone.
#[tauri::command]
pub async fn mute_github_mention(app: AppHandle, db: State<'_, Db>, key: String, scope: String) -> Result<(), String> {
    let item = cached_item(&key)?;
    let repo_label = format!("{}/{}", item.owner, item.repo);
    let mute = match scope.as_str() {
        "thread" => MentionMute {
            scope,
            target: item.key.clone(),
            label: match item.number {
                Some(n) => format!("{repo_label}#{n} {}", item.title),
                None => format!("{repo_label} · {}", item.title),
            },
            muted_at: chrono::Utc::now().to_rfc3339(),
        },
        "repo" => MentionMute {
            scope,
            target: repo_target(&item.owner, &item.repo),
            label: repo_label,
            muted_at: chrono::Utc::now().to_rfc3339(),
        },
        other => return Err(format!("Unknown mute scope: {other}")),
    };
    let mut list = load_muted(&db).await;
    list.retain(|m| !(m.scope == mute.scope && m.target == mute.target));
    list.push(mute.clone());
    if list.len() > MAX_MUTED {
        list.drain(..list.len() - MAX_MUTED);
    }
    save_muted(&db, &list).await?;

    let set = MuteSet::from(std::slice::from_ref(&mute));
    let hit: Vec<MentionItem> = cached()
        .map(|c| c.result.items.into_iter().filter(|i| set.mutes(&i.key, &i.owner, &i.repo)).collect())
        .unwrap_or_default();
    for i in hit.iter().filter(|i| i.unread) {
        if let Ok(client) = github::client_for_account(&i.account_id) {
            if let Err(e) = mark_thread_read(&client, &i.thread_id).await {
                tracing::warn!(event = "github_mentions_mark_read_failed", error = %e);
            }
        }
    }
    patch_cache(&app, |items| items.retain(|i| !set.mutes(&i.key, &i.owner, &i.repo)));
    Ok(())
}

/// Muted threads / repositories, newest first.
#[tauri::command]
pub async fn list_github_mention_mutes(db: State<'_, Db>) -> Result<Vec<MentionMute>, String> {
    let mut list = load_muted(&db).await;
    list.reverse();
    Ok(list)
}

/// Unmute; the thread / repo comes back on the next fetch (the caller refreshes).
#[tauri::command]
pub async fn unmute_github_mention(db: State<'_, Db>, scope: String, target: String) -> Result<(), String> {
    let mut list = load_muted(&db).await;
    list.retain(|m| !(m.scope == scope && m.target == target));
    save_muted(&db, &list).await
}

/// Remember the "Check with AI" session started for a mention, so the card
/// shows it and a second click opens it instead of starting another.
#[tauri::command]
pub async fn link_github_mention_run(app: AppHandle, db: State<'_, Db>, key: String, run_id: String) -> Result<(), String> {
    let mut list = load_run_link_list(&db).await;
    list.retain(|(k, _)| k != &key);
    list.push((key.clone(), run_id.clone()));
    if list.len() > MAX_RUN_LINKS {
        list.drain(..list.len() - MAX_RUN_LINKS);
    }
    set_setting(&db, RUNS_KEY, &serde_json::to_string(&list).unwrap_or_default()).await?;
    patch_cache(&app, |items| {
        if let Some(i) = items.iter_mut().find(|i| i.key == key) {
            i.existing_run_id = Some(run_id);
            i.existing_run_status = Some("running".into());
        }
    });
    Ok(())
}

/// The instruction a "Check with AI" session starts with: the linked saved
/// prompt, else the custom text, else the default (Vietnamese, like the PR
/// review prompt).
#[tauri::command]
pub async fn get_github_mentions_check_prompt(db: State<'_, Db>) -> Result<String, String> {
    crate::pr_inbox::resolve_configured_prompt(db.inner(), CHECK_PROMPT_KEY, CHECK_PROMPT_ID_KEY, DEFAULT_CHECK_PROMPT).await
}

/// What the prompt editor shows (same shape as the PR review prompts).
#[tauri::command]
pub async fn get_github_mentions_prompt_config(db: State<'_, Db>) -> Result<crate::pr_inbox::PrPromptConfig, String> {
    crate::pr_inbox::prompt_config(db.inner(), CHECK_PROMPT_KEY, CHECK_PROMPT_ID_KEY, DEFAULT_CHECK_PROMPT).await
}

/// Save the custom check prompt; blank (or the default) restores the default.
#[tauri::command]
pub async fn set_github_mentions_prompt(db: State<'_, Db>, prompt: String) -> Result<(), String> {
    crate::pr_inbox::save_prompt_text(db.inner(), CHECK_PROMPT_KEY, DEFAULT_CHECK_PROMPT, &prompt).await
}

/// Link the check prompt to a saved prompt, or unlink it (`None`).
#[tauri::command]
pub async fn set_github_mentions_prompt_id(db: State<'_, Db>, saved_prompt_id: Option<String>) -> Result<(), String> {
    crate::pr_inbox::save_prompt_id(db.inner(), CHECK_PROMPT_ID_KEY, saved_prompt_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mute(scope: &str, target: &str) -> MentionMute {
        MentionMute { scope: scope.into(), target: target.into(), label: String::new(), muted_at: String::new() }
    }

    #[test]
    fn mute_set_matches_thread_or_repo() {
        let set = MuteSet::from(&[mute("thread", "acc:42"), mute("repo", "sora-tech/noise")]);
        assert!(set.mutes("acc:42", "any", "repo"));
        assert!(set.mutes("acc:7", "Sora-Tech", "Noise"));
        assert!(!set.mutes("acc:7", "sora-tech", "app"));
        assert!(!set.mutes("other:42", "sora-tech", "app"));
    }
}
