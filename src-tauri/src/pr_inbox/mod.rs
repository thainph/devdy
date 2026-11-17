//! PR Inbox cache + background poller.
//!
//! Every fetch of the review-requested PRs (`commands::github::
//! fetch_review_requested_prs`) goes through `refresh`, which keeps the last
//! result in memory and broadcasts it (`UPDATED_EVENT`) to every window — so
//! the PR Inbox opens on the cached list at once and refreshes behind it.
//!
//! A poller re-fetches every `pr_inbox.poll_minutes` (default 5, "0" = off)
//! and shows a native notification for PRs not seen before. The seen set is
//! persisted (`pr_inbox.seen`) so PRs that arrived while the app was closed
//! still notify; the very first run only records the current ones.

use crate::commands::github::{fetch_review_requested_prs, PrInboxItem, PrInboxResult};
use crate::db::Db;
use serde::Serialize;
use std::collections::BTreeSet;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};

/// Broadcast to every window with a `CachedPrInbox` after each fetch.
pub const UPDATED_EVENT: &str = "pr-inbox-updated";
/// Sent to the main window to open the PR Inbox window (notification click
/// while it is not open; the window is created from JS, lib/prInboxWindow).
pub const OPEN_WINDOW_EVENT: &str = "pr-inbox-open-window";
/// Label of the PR Inbox window (lib/prInboxWindow `PR_INBOX_WINDOW_LABEL`).
const WINDOW_LABEL: &str = "pr-inbox";

pub const POLL_MINUTES_KEY: &str = "pr_inbox.poll_minutes";
const SEEN_KEY: &str = "pr_inbox.seen";
pub const DEFAULT_POLL_MINUTES: u64 = 5;
/// Follow-up prompt sent when the inbox re-reviews a PR in its existing session.
/// Instruction a PR review run starts with (custom text / saved prompt id).
pub const REVIEW_PROMPT_KEY: &str = "pr_inbox.review_prompt";
pub const REVIEW_PROMPT_ID_KEY: &str = "pr_inbox.review_prompt_id";
/// Used while the user hasn't set their own review prompt.
const DEFAULT_REVIEW_PROMPT: &str = "Hãy review pull request này theo các skill đã được cấu hình. Viết kết quả bằng tiếng Việt.";
pub const REREVIEW_PROMPT_KEY: &str = "pr_inbox.rereview_prompt";
/// Id of a Settings → Saved prompts entry to use as the re-review prompt. Takes
/// precedence over [`REREVIEW_PROMPT_KEY`] while that entry still exists.
pub const REREVIEW_PROMPT_ID_KEY: &str = "pr_inbox.rereview_prompt_id";
/// Used while the user hasn't saved their own re-review prompt.
const DEFAULT_REREVIEW_PROMPT: &str = "PR đã được cập nhật kể từ lần review trước. File task (cùng file input như trước) vừa được tải lại với diff, comment và review mới nhất — hãy đọc lại file đó.\n\nReview các thay đổi kể từ lần review trước: kiểm tra từng vấn đề đã nêu trước đó đã được xử lý chưa, và chỉ ra các vấn đề mới phát sinh trong lần cập nhật này. Làm theo đúng quy trình review và định dạng output như lần review trước. Viết kết quả bằng tiếng Việt.";
/// Let startup settle (network, keychain) before the first poll.
const FIRST_TICK: Duration = Duration::from_secs(10);
/// Sleep while polling is off; a settings change wakes the poller earlier.
const IDLE_TICK: Duration = Duration::from_secs(3600);
/// Upper bound on one whole inbox fetch (all accounts, all pages).
const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

/// The last fetched inbox, as sent to the UI.
#[derive(Debug, Serialize, Clone)]
pub struct CachedPrInbox {
    #[serde(flatten)]
    pub result: PrInboxResult,
    pub fetched_at: String,
}

struct Cached {
    inbox: CachedPrInbox,
    at: Instant,
}

#[derive(Default)]
struct InboxState {
    cache: std::sync::Mutex<Option<Cached>>,
    /// Held for the whole fetch: one refresh at a time.
    refresh_lock: tokio::sync::Mutex<()>,
    /// Wakes the poller after its interval changed.
    wake: tokio::sync::Notify,
}

fn state() -> &'static InboxState {
    static S: std::sync::OnceLock<InboxState> = std::sync::OnceLock::new();
    S.get_or_init(InboxState::default)
}

fn cached() -> Option<CachedPrInbox> {
    state().cache.lock().ok()?.as_ref().map(|c| c.inbox.clone())
}

/// When the cache was filled, if it was.
fn cached_at() -> Option<Instant> {
    state().cache.lock().ok()?.as_ref().map(|c| c.at)
}

/// Fetch the inbox, refresh the cache and broadcast it. `notify` shows a
/// native notification for PRs not seen before (the poller); a refresh the
/// user asked for only records them as seen — the list is on their screen.
///
/// Single flight: a call made while another fetch runs waits for it and
/// returns its result instead of fetching again.
pub async fn refresh(app: &AppHandle, db: &Db, notify: bool) -> Result<PrInboxResult, String> {
    let st = state();
    let called = Instant::now();
    let _guard = match st.refresh_lock.try_lock() {
        Ok(guard) => guard,
        Err(_) => {
            let guard = st.refresh_lock.lock().await;
            // The fetch we waited on succeeded after this call began: reuse it.
            if cached_at().is_some_and(|at| at >= called) {
                if let Some(c) = cached() {
                    return Ok(c.result);
                }
            }
            guard
        }
    };

    // Backstop over the per-request HTTP timeouts: a hung fetch would hold the
    // lock forever, leaving every later refresh spinning and the poller silent.
    let result = tokio::time::timeout(FETCH_TIMEOUT, fetch_review_requested_prs(db))
        .await
        .map_err(|_| "Timed out fetching review requests from GitHub".to_string())??;
    let inbox = CachedPrInbox { result: result.clone(), fetched_at: chrono::Utc::now().to_rfc3339() };
    if let Ok(mut cache) = st.cache.lock() {
        *cache = Some(Cached { inbox: inbox.clone(), at: Instant::now() });
    }
    let _ = app.emit(UPDATED_EVENT, &inbox);

    match take_new(db, &result).await {
        Ok(new) if notify => notify_new(app, db, new).await,
        Ok(_) => {}
        Err(e) => tracing::warn!(event = "pr_inbox_seen_failed", error = %e),
    }
    Ok(result)
}

/// `owner/repo#number`, lowercased: one PR across accounts and fetches.
pub fn pr_key(item: &PrInboxItem) -> String {
    format!("{}/{}#{}", item.owner.to_lowercase(), item.repo.to_lowercase(), item.number)
}

/// The cached PR with this `pr_key` (case-insensitive), if the inbox lists it.
pub fn cached_item(key: &str) -> Option<PrInboxItem> {
    let key = key.trim().to_lowercase();
    cached()?.result.items.into_iter().find(|i| pr_key(i) == key)
}

/// Whether a fetch is running right now (the user's, a window's or the poller's).
pub fn is_refreshing() -> bool {
    state().refresh_lock.try_lock().is_err()
}

/// How old the cached inbox is, if there is one.
pub fn cache_age() -> Option<Duration> {
    cached_at().map(|at| at.elapsed())
}

/// Point the cached PR at the review run just started (or resumed) for it
/// and tell every window, so no surface offers to start a second one before
/// the next fetch. A no-op when the PR is no longer cached.
pub fn link_run(app: &AppHandle, key: &str, run_id: &str) {
    let key = key.trim().to_lowercase();
    let inbox = {
        let Ok(mut cache) = state().cache.lock() else { return };
        let Some(c) = cache.as_mut() else { return };
        let Some(item) = c.inbox.result.items.iter_mut().find(|i| pr_key(i) == key) else {
            return;
        };
        item.existing_run_id = Some(run_id.to_string());
        item.existing_run_status = Some("running".to_string());
        item.updated_since_review = false;
        c.inbox.clone()
    };
    let _ = app.emit(UPDATED_EVENT, &inbox);
}

/// What the notifier remembers between fetches (`pr_inbox.seen`).
#[derive(Debug, Default, Serialize, serde::Deserialize, PartialEq)]
struct Seen {
    /// Accounts fetched successfully last time.
    accounts: BTreeSet<String>,
    /// `pr_key`s of the PRs seen.
    prs: BTreeSet<String>,
}

/// The PRs of `result` not in `prev`, and what to remember next. A PR is new
/// only when one of its reviewer accounts was fetched last time too, so the
/// first fetch — of the app, or of an account just added — stays quiet.
fn diff_seen(prev: Option<Seen>, result: &PrInboxResult) -> (Vec<PrInboxItem>, Seen) {
    let new = match &prev {
        Some(seen) => result
            .items
            .iter()
            .filter(|i| !seen.prs.contains(&pr_key(i)))
            .filter(|i| i.reviewer_accounts.iter().any(|a| seen.accounts.contains(&a.account_id)))
            .cloned()
            .collect(),
        None => Vec::new(),
    };
    let mut next = Seen {
        accounts: result.fetched_accounts.iter().cloned().collect(),
        // Keep only the PRs still open, so one re-requested later notifies
        // again — unless an account failed: its PRs are just missing here.
        prs: result.items.iter().map(pr_key).collect(),
    };
    if !result.errors.is_empty() {
        next.prs.extend(prev.into_iter().flat_map(|p| p.prs));
    }
    (new, next)
}

/// The PRs of `result` not seen before (see `diff_seen`); records the rest.
async fn take_new(db: &Db, result: &PrInboxResult) -> Result<Vec<PrInboxItem>, String> {
    let prev: Option<Seen> =
        sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
            .bind(SEEN_KEY)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?
            .and_then(|raw| serde_json::from_str(&raw).ok());
    let (new, next) = diff_seen(prev, result);
    let json = serde_json::to_string(&next).map_err(|e| e.to_string())?;
    crate::commands::settings::set_setting(db, SEEN_KEY, &json).await?;
    Ok(new)
}

async fn language(db: &Db) -> String {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = 'language'")
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| "en".into())
}

/// Title + body of the notification for `new` (non-empty) in `lang`.
fn compose(new: &[PrInboxItem], lang: &str) -> (String, String) {
    let vi = lang == "vi";
    let line = |i: &PrInboxItem| format!("{}/{}#{}: {}", i.owner, i.repo, i.number, i.title);
    if let [only] = new {
        let title = if vi { "PR mới cần review" } else { "New PR to review" };
        let by = if vi { "Tác giả" } else { "By" };
        return (title.to_string(), format!("{}\n{by} {}", line(only), only.author_login));
    }
    const SHOWN: usize = 3;
    let title = if vi {
        format!("{} PR mới cần review", new.len())
    } else {
        format!("{} new PRs to review", new.len())
    };
    let mut lines: Vec<String> = new.iter().take(SHOWN).map(line).collect();
    if new.len() > SHOWN {
        let more = new.len() - SHOWN;
        lines.push(if vi { format!("+{more} PR khác") } else { format!("+{more} more") });
    }
    (title, lines.join("\n"))
}

/// One notification for the new PRs; bot-authored ones (dependabot, renovate…)
/// stay quiet — they still show in the inbox.
async fn notify_new(app: &AppHandle, db: &Db, new: Vec<PrInboxItem>) {
    let new: Vec<PrInboxItem> = new.into_iter().filter(|i| !i.author_is_bot).collect();
    if new.is_empty() {
        return;
    }
    let (title, body) = compose(&new, &language(db).await);
    crate::commands::notifications::show_clickable_opts(
        app.clone(),
        title,
        body,
        "PR inbox notification",
        false,
        open_window,
    );
}

/// Raise the PR Inbox window, or have the main window open it.
pub fn open_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window(WINDOW_LABEL) {
        let _ = win.unminimize();
        let _ = win.show();
        let _ = win.set_focus();
        return;
    }
    if let Some(main) = app.get_webview_window("main") {
        let _ = main.unminimize();
        let _ = main.show();
        let _ = main.set_focus();
    }
    let _ = app.emit_to("main", OPEN_WINDOW_EVENT, ());
}

pub async fn poll_minutes(db: &Db) -> u64 {
    sqlx::query_scalar::<_, String>("SELECT value FROM settings WHERE key = ?")
        .bind(POLL_MINUTES_KEY)
        .fetch_optional(db)
        .await
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_POLL_MINUTES)
}

/// Start the poller (app setup). A fetch made by a window counts as a poll:
/// the next one comes a full interval after it.
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
                        if let Err(e) = refresh(&app, &db, true).await {
                            tracing::warn!(event = "pr_inbox_poll_failed", error = %e);
                        }
                        interval
                    }
                }
            };
            tokio::select! {
                _ = tokio::time::sleep(wait) => {}
                _ = state().wake.notified() => {}
            }
        }
    });
}

/// The last fetched inbox, or None before the first fetch.
#[tauri::command]
pub fn get_cached_pr_inbox() -> Option<CachedPrInbox> {
    cached()
}

#[tauri::command]
pub async fn get_pr_inbox_poll_minutes(db: tauri::State<'_, Db>) -> Result<u64, String> {
    Ok(poll_minutes(db.inner()).await)
}

/// The re-review follow-up prompt: the user's saved one, else the default.
#[tauri::command]
pub async fn get_pr_inbox_rereview_prompt(db: tauri::State<'_, Db>) -> Result<String, String> {
    rereview_prompt(db.inner()).await
}

/// [`get_pr_inbox_rereview_prompt`] for callers without a Tauri `State`.
pub async fn rereview_prompt(db: &Db) -> Result<String, String> {
    resolve_prompt(db, PromptKind::Rereview).await
}

/// The instruction a PR review run starts with (before the "PR details are in"
/// line), used whenever the run has no per-run prompt of its own.
pub async fn review_prompt(db: &Db) -> Result<String, String> {
    resolve_prompt(db, PromptKind::Review).await
}

/// The two configurable PR review prompts: the first review, and the follow-up
/// "Re-review" sends to the same session.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PromptKind {
    Review,
    Rereview,
}

impl PromptKind {
    /// (custom text key, saved prompt id key, built-in default)
    fn keys(self) -> (&'static str, &'static str, &'static str) {
        match self {
            PromptKind::Review => (REVIEW_PROMPT_KEY, REVIEW_PROMPT_ID_KEY, DEFAULT_REVIEW_PROMPT),
            PromptKind::Rereview => (REREVIEW_PROMPT_KEY, REREVIEW_PROMPT_ID_KEY, DEFAULT_REREVIEW_PROMPT),
        }
    }
}

/// The linked saved prompt (if it still exists and isn't blank), else the
/// custom text, else the built-in default.
async fn resolve_prompt(db: &Db, kind: PromptKind) -> Result<String, String> {
    let (text_key, id_key, default) = kind.keys();
    if let Some(id) = setting(db, id_key).await? {
        let library = setting(db, "saved_prompts").await?.unwrap_or_default();
        let body = crate::remote::forwarder::parse_saved_prompts(&library)
            .into_iter()
            .find(|p| p.id == id)
            .map(|p| p.body)
            .filter(|b| !b.trim().is_empty());
        if let Some(body) = body {
            return Ok(body);
        }
    }
    Ok(setting(db, text_key).await?.unwrap_or_else(|| default.to_string()))
}

async fn setting(db: &Db, key: &str) -> Result<Option<String>, String> {
    let v: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(key)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(v.filter(|v| !v.trim().is_empty()))
}

async fn delete_setting(db: &Db, key: &str) -> Result<(), String> {
    sqlx::query("DELETE FROM settings WHERE key = ?")
        .bind(key)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// What a prompt editor shows: the linked saved prompt (if any) and the custom
/// text (the default when none is saved).
#[derive(Debug, Serialize)]
pub struct PrPromptConfig {
    pub saved_prompt_id: Option<String>,
    pub prompt: String,
}

#[tauri::command]
pub async fn get_pr_inbox_prompt_config(db: tauri::State<'_, Db>, kind: PromptKind) -> Result<PrPromptConfig, String> {
    let (text_key, id_key, default) = kind.keys();
    Ok(PrPromptConfig {
        saved_prompt_id: setting(db.inner(), id_key).await?,
        prompt: setting(db.inner(), text_key).await?.unwrap_or_else(|| default.to_string()),
    })
}

/// Link a prompt to a saved prompt (Settings → Prompt Templates), or unlink it
/// (`None`) so the custom text / default applies again.
#[tauri::command]
pub async fn set_pr_inbox_prompt_id(
    db: tauri::State<'_, Db>,
    kind: PromptKind,
    saved_prompt_id: Option<String>,
) -> Result<(), String> {
    let (_, id_key, _) = kind.keys();
    match saved_prompt_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => crate::commands::settings::set_setting(db.inner(), id_key, &id).await,
        None => delete_setting(db.inner(), id_key).await,
    }
}

/// Save a prompt's custom text. A blank one (or the default itself) clears the
/// setting, so the built-in default applies again.
#[tauri::command]
pub async fn set_pr_inbox_prompt(db: tauri::State<'_, Db>, kind: PromptKind, prompt: String) -> Result<(), String> {
    let (text_key, _, default) = kind.keys();
    let prompt = prompt.trim();
    if prompt.is_empty() || prompt == default {
        return delete_setting(db.inner(), text_key).await;
    }
    crate::commands::settings::set_setting(db.inner(), text_key, prompt).await
}

#[tauri::command]
pub async fn set_pr_inbox_poll_minutes(db: tauri::State<'_, Db>, minutes: u64) -> Result<(), String> {
    crate::commands::settings::set_setting(db.inner(), POLL_MINUTES_KEY, &minutes.to_string()).await?;
    state().wake.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(n: u64, title: &str) -> PrInboxItem {
        PrInboxItem {
            account_id: "a".into(),
            account_label: "A".into(),
            owner: "Org".into(),
            repo: "Repo".into(),
            number: n,
            title: title.into(),
            html_url: String::new(),
            author_login: "alice".into(),
            author_avatar_url: None,
            author_is_bot: false,
            draft: false,
            labels: vec![],
            updated_at: String::new(),
            mapped: false,
            project_id: None,
            repo_id: None,
            project_name: None,
            existing_run_id: None,
            existing_run_status: None,
            reviewer_accounts: vec![crate::commands::github::PrInboxReviewerAccount {
                account_id: "a".into(),
                account_label: "A".into(),
            }],
            requested_reviewers: vec![],
            updated_since_review: false,
            stats: Default::default(),
        }
    }

    #[test]
    fn one_pr_names_it_and_its_author() {
        let (title, body) = compose(&[item(7, "Fix login")], "en");
        assert_eq!(title, "New PR to review");
        assert_eq!(body, "Org/Repo#7: Fix login\nBy alice");
        let (title, _) = compose(&[item(7, "Fix login")], "vi");
        assert_eq!(title, "PR mới cần review");
    }

    #[test]
    fn many_prs_list_the_first_three() {
        let new: Vec<_> = (1..=5).map(|n| item(n, "T")).collect();
        let (title, body) = compose(&new, "en");
        assert_eq!(title, "5 new PRs to review");
        assert_eq!(body.lines().count(), 4);
        assert!(body.ends_with("+2 more"));
    }

    fn result(items: Vec<PrInboxItem>, failed: bool) -> PrInboxResult {
        PrInboxResult {
            items,
            errors: if failed {
                vec![crate::commands::github::PrInboxAccountError {
                    account_id: "b".into(),
                    account_label: "B".into(),
                    message: "boom".into(),
                }]
            } else {
                vec![]
            },
            fetched_accounts: vec!["a".into()],
        }
    }

    fn numbers(items: &[PrInboxItem]) -> Vec<u64> {
        items.iter().map(|i| i.number).collect()
    }

    #[test]
    fn first_fetch_only_seeds() {
        let (new, next) = diff_seen(None, &result(vec![item(1, "x")], false));
        assert!(new.is_empty());
        assert!(next.prs.contains("org/repo#1"));
        assert!(next.accounts.contains("a"));
    }

    #[test]
    fn a_pr_not_seen_before_is_new() {
        let (_, seen) = diff_seen(None, &result(vec![item(1, "x")], false));
        let (new, _) = diff_seen(Some(seen), &result(vec![item(1, "x"), item(2, "y")], false));
        assert_eq!(numbers(&new), vec![2]);
    }

    #[test]
    fn an_account_just_added_stays_quiet() {
        let seen = Seen { accounts: BTreeSet::new(), prs: BTreeSet::new() };
        let (new, next) = diff_seen(Some(seen), &result(vec![item(1, "x")], false));
        assert!(new.is_empty());
        assert!(next.accounts.contains("a"));
    }

    #[test]
    fn a_failed_account_keeps_its_prs_seen() {
        let (_, seen) = diff_seen(None, &result(vec![item(1, "x"), item(2, "y")], false));
        let (_, next) = diff_seen(Some(seen), &result(vec![item(1, "x")], true));
        assert!(next.prs.contains("org/repo#2"));
        // Without a failure, a closed PR is forgotten.
        let (_, next) = diff_seen(Some(next), &result(vec![item(1, "x")], false));
        assert!(!next.prs.contains("org/repo#2"));
    }

    #[test]
    fn keys_ignore_case() {
        assert_eq!(pr_key(&item(3, "x")), "org/repo#3");
    }
}
