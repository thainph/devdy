//! Slack Thread Translate: threads are added by link, fetched from the Slack
//! Web API, stored per message (original + Vietnamese) in the `slack_*`
//! tables and translated by a dedicated warm sidecar.
//!
//! - `link`: link parsing (pure)
//! - `token`: token lookup / workspace matching / scopes
//! - `api`: Slack Web API client
//! - `mrkdwn`: placeholder protection, plain text, prompt text (pure)
//! - `translate`: batching + prompt + output parsing, `run_translation`
//! - `files`: file downloads + avatar cache
//! - `store`: SQL for the commands
//! - `run_links`: thread ↔ session (run) links
//! - `job`: one fetch + translate job
//!
//! A background worker (≤ 2 jobs in parallel, 1 per thread) drains an
//! in-memory queue; the DB is the source of truth, so `startup_recover`
//! re-queues unfinished threads after a restart.

pub mod api;
pub mod files;
pub mod job;
pub mod link;
pub mod mrkdwn;
pub mod run_links;
pub mod store;
pub mod token;
pub mod translate;

use crate::commands::translate::{TranslateState, TurnEvent};
use crate::db::Db;
use api::HttpConfig;
use async_trait::async_trait;
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use store::{AddSlackThreadResult, SlackThreadSummary};
use tauri::{AppHandle, Emitter};
use token::{AuthCache, SecretLookup};
use tokio::sync::{Notify, Semaphore};
use translate::{EngineError, TranslateEngine};

pub const CHANGED_EVENT: &str = "slack-threads://changed";
pub const PROGRESS_EVENT: &str = "slack-threads://progress";
/// Emitted when an OS notification is clicked; payload `{ id, kind: 'slack' }`.
pub const NOTIFICATION_CLICKED_EVENT: &str = "capture-notification-clicked";
/// Emitted when a thread arrives over the Inbox API; payload `{ id }`. The
/// main window opens the Slack threads window on it.
pub const API_RECEIVED_EVENT: &str = "slack-threads://api-received";

const MAX_PARALLEL_JOBS: usize = 2;
/// Warm translate session recycled after this many batches.
const TRANSLATE_RECYCLE_AFTER: u32 = 6;
const BATCH_TIMEOUT: Duration = Duration::from_secs(180);
/// At most 4 progress events per second per thread (phase changes and the
/// final event always go out).
const PROGRESS_MIN_GAP: Duration = Duration::from_millis(250);
/// `fetch_error_code`s retried automatically at startup.
const TRANSIENT_CODES: [&str; 5] = [
    "no_token",
    "no_token_for_workspace",
    "invalid_auth",
    "network",
    "rate_limited",
];

/// Longest reply accepted by `post_reply` (Slack truncates `text` at 40k).
pub const MAX_REPLY_CHARS: usize = 8_000;

/// `chat.postMessage` failures as coded errors.
fn post_error(e: api::SlackError) -> CodedError {
    use api::SlackError;
    match e {
        SlackError::Api { code, needed } => match code.as_str() {
            "missing_scope" => CodedError::new(
                "post_missing_scope",
                needed.unwrap_or_else(|| "chat:write".into()),
            ),
            "not_in_channel" | "channel_not_found" | "is_archived" | "thread_not_found"
            | "restricted_action" | "msg_too_long" | "rate_limited" => {
                CodedError::new(code.clone(), code)
            }
            _ => token::auth_error(SlackError::Api { code, needed }),
        },
        // The request may have reached Slack: never retried automatically.
        SlackError::Network(d) => CodedError::new("post_network", d),
        other => token::auth_error(other),
    }
}

/// Error carried to the FE / API as `"<code>: <detail>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodedError {
    pub code: String,
    pub detail: String,
}

impl CodedError {
    pub fn new(code: impl Into<String>, detail: impl Into<String>) -> Self {
        CodedError {
            code: code.into(),
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for CodedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.detail)
    }
}

impl From<CodedError> for String {
    fn from(e: CodedError) -> Self {
        e.to_string()
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SlackThreadProgress {
    pub id: String,
    pub status: String,
    /// queued | auth | messages | users | files | translate | done
    pub phase: String,
    pub done: usize,
    pub total: Option<usize>,
    pub error_code: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ChangedPayload<'a> {
    id: &'a str,
    action: &'a str,
}

/// `changed` for every thread (empty id) from code without a `SlackCtx` —
/// e.g. a run delete dropped session links. Listeners reload the list and
/// the open thread.
pub fn emit_all_changed(app: &AppHandle) {
    let _ = app.emit(
        CHANGED_EVENT,
        ChangedPayload {
            id: "",
            action: "updated",
        },
    );
}

/// Outbound notifications (Tauri events / OS notification); a no-op double in
/// tests.
pub trait SlackEvents: Send + Sync {
    fn changed(&self, id: &str, action: &str);
    fn progress(&self, p: &SlackThreadProgress);
    /// A thread was added / re-requested over the Inbox API.
    fn api_thread_received(&self, id: &str);
    /// An Inbox-API job ended (`ready` or `error`).
    fn api_job_finished(&self, id: &str, title: &str, ready: bool);
}

/// Real events: Tauri emits + clickable OS notification.
pub struct AppEvents {
    pub app: AppHandle,
}

#[derive(Clone, Serialize)]
struct ApiReceived<'a> {
    id: &'a str,
}

#[derive(Clone, Serialize)]
struct NotificationClick {
    id: String,
    kind: &'static str,
}

impl SlackEvents for AppEvents {
    fn changed(&self, id: &str, action: &str) {
        let _ = self.app.emit(CHANGED_EVENT, ChangedPayload { id, action });
    }

    fn progress(&self, p: &SlackThreadProgress) {
        let _ = self.app.emit(PROGRESS_EVENT, p);
    }

    fn api_thread_received(&self, id: &str) {
        let _ = self.app.emit(API_RECEIVED_EVENT, ApiReceived { id });
    }

    fn api_job_finished(&self, id: &str, title: &str, ready: bool) {
        let body = if ready {
            format!("Đã dịch xong thread Slack: {title}")
        } else {
            format!("Không lấy được thread Slack: {title}")
        };
        let id = id.to_string();
        crate::commands::notifications::show_clickable(
            self.app.clone(),
            "Devdy".to_string(),
            body,
            "slack thread notification",
            move |app| {
                let _ = app.emit(
                    NOTIFICATION_CLICKED_EVENT,
                    NotificationClick { id, kind: "slack" },
                );
            },
        );
    }
}

/// The dedicated warm translation sidecar for Slack threads (separate from the
/// popover / `/v1/translate` one so long jobs never block them).
#[derive(Clone)]
pub struct SlackTranslateState(pub TranslateState);

impl Default for SlackTranslateState {
    fn default() -> Self {
        SlackTranslateState(
            TranslateState::default()
                .with_recycle_after(TRANSLATE_RECYCLE_AFTER)
                .silent()
                .tracked_as(crate::procs::ProcKind::SlackTranslate),
        )
    }
}

/// Engine errors reach the DB / API in English (§8.3): known UI messages are
/// mapped; anything else that is not plain ASCII (another localized message)
/// becomes a generic English one.
fn english_engine_error(msg: &str) -> String {
    use crate::commands::translate as t;
    let ascii_or = |s: &str, fallback: &str| {
        if s.is_ascii() {
            s.to_string()
        } else {
            fallback.to_string()
        }
    };
    match msg {
        t::ERR_SESSION_ENDED => "translation ended unexpectedly".into(),
        t::ERR_EMPTY_RESULT => "empty translation".into(),
        t::ERR_FAILED => "translation failed".into(),
        t::ERR_NOTHING_TO_TRANSLATE => "nothing to translate".into(),
        _ => match msg.strip_prefix(t::ERR_SEND_PREFIX) {
            Some(d) => format!("failed to send the request: {}", ascii_or(d, "write error")),
            None => ascii_or(msg, "translation engine error"),
        },
    }
}

/// `TranslateEngine` over `SlackTranslateState` (`translate::submit_prompt`).
pub struct SidecarEngine {
    pub app: AppHandle,
    pub db: Db,
    pub state: SlackTranslateState,
}

#[async_trait]
impl TranslateEngine for SidecarEngine {
    async fn style_instruction(&self) -> String {
        let style = crate::commands::translate::load_style(&self.db)
            .await
            .unwrap_or_default();
        crate::commands::translate::style_instruction(&style).to_string()
    }

    async fn translate(&self, prompt: String) -> Result<String, EngineError> {
        use crate::commands::translate::submit_prompt;
        let (app, db, state) = (self.app.clone(), self.db.clone(), self.state.0.clone());
        // Detached so the turn is never cancelled between enqueue and write.
        let (turn, rx) =
            tokio::spawn(async move { submit_prompt(&app, &db, &state, prompt, true).await })
                .await
                .map_err(|e| EngineError::Unavailable(e.to_string()))?
                .map_err(|e| EngineError::Unavailable(english_engine_error(&e)))?;
        let mut rx = rx.ok_or_else(|| EngineError::Unavailable("no result channel".into()))?;
        let wait = async {
            loop {
                match rx.recv().await {
                    Some(TurnEvent::Chunk(_)) => continue,
                    Some(TurnEvent::Done(text)) => return Ok(text),
                    Some(TurnEvent::Error(e)) => {
                        return Err(EngineError::Failed(english_engine_error(&e)))
                    }
                    None => {
                        return Err(EngineError::Failed("translation ended unexpectedly".into()))
                    }
                }
            }
        };
        match tokio::time::timeout(BATCH_TIMEOUT, wait).await {
            Ok(r) => r,
            Err(_) => {
                self.state.0.abandon_turn(turn).await;
                Err(EngineError::Failed("timeout".into()))
            }
        }
    }
}

/// Everything a job needs (cheap to clone).
#[derive(Clone)]
pub struct SlackCtx {
    pub db: Db,
    pub app_data: PathBuf,
    pub events: Arc<dyn SlackEvents>,
    pub lookup: SecretLookup,
    pub http: Arc<HttpConfig>,
    pub token_http: reqwest::Client,
    pub public_http: reqwest::Client,
    pub engine: Arc<dyn TranslateEngine>,
    pub auth_cache: AuthCache,
    last_progress: Arc<Mutex<ProgressMemo>>,
}

/// Per thread: time, phase and status of the last emitted progress event.
type ProgressMemo = HashMap<String, (Instant, String, String)>;

impl SlackCtx {
    pub fn new(
        db: Db,
        app_data: PathBuf,
        events: Arc<dyn SlackEvents>,
        lookup: SecretLookup,
        http: HttpConfig,
        engine: Arc<dyn TranslateEngine>,
    ) -> Self {
        let http = Arc::new(http);
        SlackCtx {
            token_http: http.token_client(),
            public_http: http.public_client(),
            db,
            app_data,
            events,
            lookup,
            http,
            engine,
            auth_cache: AuthCache::default(),
            last_progress: Arc::default(),
        }
    }

    pub fn emit_changed(&self, id: &str, action: &str) {
        self.events.changed(id, action);
    }

    /// Throttled progress (≤ 4/s/thread); phase/status changes, the `done`
    /// phase and errors are never dropped.
    pub fn emit_progress(&self, p: SlackThreadProgress) {
        let terminal = p.phase == "done" || p.status == "error";
        let complete = p.total.is_some_and(|t| p.done >= t);
        if let Ok(mut last) = self.last_progress.lock() {
            let now = Instant::now();
            let send = match last.get(&p.id) {
                Some((at, phase, status)) => {
                    terminal
                        || complete
                        || *phase != p.phase
                        || *status != p.status
                        || now.duration_since(*at) >= PROGRESS_MIN_GAP
                }
                None => true,
            };
            if !send {
                return;
            }
            if terminal {
                last.remove(&p.id);
            } else {
                last.insert(p.id.clone(), (now, p.phase.clone(), p.status.clone()));
            }
        }
        self.events.progress(&p);
    }
}

#[derive(Default)]
struct Queue {
    pending: VecDeque<String>,
    /// Queued / running ids that came from the Inbox API (notify at the end).
    from_api: HashSet<String>,
    running: HashSet<String>,
    /// Re-run requested while running (value: requested via the API).
    rerun: HashMap<String, bool>,
}

/// Job queue + worker + the operations shared by commands and the Inbox API.
#[derive(Clone)]
pub struct SlackThreadsState {
    ctx: SlackCtx,
    queue: Arc<Mutex<Queue>>,
    notify: Arc<Notify>,
}

impl SlackThreadsState {
    pub fn new(ctx: SlackCtx) -> Self {
        SlackThreadsState {
            ctx,
            queue: Arc::default(),
            notify: Arc::new(Notify::new()),
        }
    }

    /// The real app wiring.
    pub fn for_app(
        app: &AppHandle,
        db: Db,
        app_data: PathBuf,
        translate: SlackTranslateState,
    ) -> Self {
        let engine = Arc::new(SidecarEngine {
            app: app.clone(),
            db: db.clone(),
            state: translate,
        });
        SlackThreadsState::new(SlackCtx::new(
            db,
            app_data,
            Arc::new(AppEvents { app: app.clone() }),
            token::keychain_lookup(),
            HttpConfig::default(),
            engine,
        ))
    }

    pub fn ctx(&self) -> &SlackCtx {
        &self.ctx
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.queue.lock().is_ok_and(|q| q.running.contains(id))
    }

    /// A job for `id` is queued or running.
    pub fn is_queued(&self, id: &str) -> bool {
        self.queue
            .lock()
            .is_ok_and(|q| q.running.contains(id) || q.pending.iter().any(|x| x == id))
    }

    /// Queue a job, or flag one re-run if a job for `id` is in flight.
    pub fn enqueue(&self, id: &str, from_api: bool) {
        if let Ok(mut q) = self.queue.lock() {
            if q.running.contains(id) {
                let api = q.rerun.get(id).copied().unwrap_or(false) || from_api;
                q.rerun.insert(id.to_string(), api);
            } else if !q.pending.iter().any(|x| x == id) {
                q.pending.push_back(id.to_string());
            }
            if from_api {
                q.from_api.insert(id.to_string());
            }
        }
        self.notify.notify_one();
    }

    fn pop(&self) -> Option<(String, bool)> {
        let mut q = self.queue.lock().ok()?;
        let id = q.pending.pop_front()?;
        q.running.insert(id.clone());
        let api = q.from_api.remove(&id);
        Some((id, api))
    }

    /// Spawn the worker loop (call once, inside the async runtime).
    pub fn start_worker(&self) {
        let st = self.clone();
        tokio::spawn(async move {
            let slots = Arc::new(Semaphore::new(MAX_PARALLEL_JOBS));
            loop {
                let Ok(permit) = slots.clone().acquire_owned().await else {
                    return;
                };
                let (id, from_api) = loop {
                    if let Some(next) = st.pop() {
                        break next;
                    }
                    st.notify.notified().await;
                };
                let st = st.clone();
                tokio::spawn(async move {
                    st.run_one(&id, from_api).await;
                    drop(permit);
                });
            }
        });
    }

    /// Run one job and handle its follow-ups (notification, merge, re-run).
    pub async fn run_one(&self, id: &str, from_api: bool) {
        let end = job::run_job(&self.ctx, id).await;
        match &end {
            job::JobEnd::Finished { ready, title } if from_api => {
                self.ctx.events.api_job_finished(id, title, *ready);
            }
            _ => {}
        }
        let rerun = self.queue.lock().ok().and_then(|mut q| {
            q.running.remove(id);
            q.rerun.remove(id)
        });
        if let job::JobEnd::MergedInto(other) = &end {
            if self.is_running(other) {
                // A job is already on the kept row: only flag one re-run (§6.1).
                self.enqueue(other, from_api);
            } else if let Ok(n) =
                store::set_requested_status(&self.ctx.db, other, "pending", None).await
            {
                // n == 0: the kept row vanished too, nothing to run.
                if n > 0 {
                    self.ctx.emit_changed(other, "updated");
                    self.enqueue(other, from_api);
                }
            }
        }
        if let (Some(api), job::JobEnd::Finished { .. }) = (rerun, &end) {
            if let Ok(n) = store::set_requested_status(&self.ctx.db, id, "pending", None).await {
                if n > 0 {
                    self.ctx.emit_changed(id, "updated");
                    self.enqueue(id, api);
                }
            }
        }
    }

    /// After a restart: unfinished rows back to `pending`, legacy / transient
    /// errors retried, merged-away legacy dirs removed, pending rows queued.
    pub async fn startup_recover(&self) {
        let db = &self.ctx.db;
        let _ = sqlx::query(
            "UPDATE slack_threads SET fetch_status = 'pending' \
             WHERE fetch_status IN ('fetching', 'translating')",
        )
        .execute(db)
        .await;
        let codes = TRANSIENT_CODES.map(|c| format!("'{c}'")).join(", ");
        let _ = sqlx::query(&format!(
            "UPDATE slack_threads SET fetch_status = 'pending' WHERE fetch_status = 'error' \
             AND (legacy = 1 OR fetch_error_code IN ({codes}))"
        ))
        .execute(db)
        .await;
        let orphans: Vec<String> = sqlx::query_scalar("SELECT id FROM slack_legacy_orphans")
            .fetch_all(db)
            .await
            .unwrap_or_default();
        for id in orphans {
            store::remove_thread_dir(&self.ctx.app_data, &id);
            let _ = sqlx::query("DELETE FROM slack_legacy_orphans WHERE id = ?")
                .bind(&id)
                .execute(db)
                .await;
        }
        let pending: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM slack_threads WHERE fetch_status = 'pending' \
             ORDER BY position ASC, updated_at DESC",
        )
        .fetch_all(db)
        .await
        .unwrap_or_default();
        for id in pending {
            self.enqueue(&id, false);
        }
    }

    /// Add (or re-import) a thread by link. `title` only comes from the API.
    pub async fn add_thread(
        &self,
        url: &str,
        project_id: Option<&str>,
        title: Option<&str>,
        from_api: bool,
    ) -> Result<AddSlackThreadResult, CodedError> {
        let link = link::parse_thread_link(url)?;
        let title = title.map(store::validate_title).transpose()?;
        let project_id = project_id.map(str::trim).filter(|p| !p.is_empty());
        if let Some(pid) = project_id {
            store::check_project(&self.ctx.db, pid).await?;
        }
        let has_token = !token::sources(&self.ctx.db, &self.ctx.lookup)
            .await
            .map_err(store::internal)?
            .is_empty();
        let is_running = |id: &str| self.is_running(id);
        let out = store::add_or_requeue(
            &self.ctx.db,
            &link,
            project_id,
            title.as_deref(),
            has_token,
            &is_running,
        )
        .await?;
        if out.enqueue {
            self.enqueue(&out.result.id, from_api);
        }
        let action = if out.result.status == "created" {
            "created"
        } else {
            "updated"
        };
        self.ctx.emit_changed(&out.result.id, action);
        if from_api {
            self.ctx.events.api_thread_received(&out.result.id);
        }
        Ok(out.result)
    }

    /// Manual refresh: `pending` + queue (or one re-run if in flight).
    pub async fn refresh(&self, id: &str) -> Result<SlackThreadSummary, CodedError> {
        let summary = store::get_summary(&self.ctx.db, id).await?;
        if summary.legacy
            && summary
                .thread_url
                .as_deref()
                .is_none_or(|u| link::parse_thread_link(u).is_err())
        {
            return Err(CodedError::new(
                "legacy_no_url",
                "legacy snapshot has no parsable Slack thread_url",
            ));
        }
        if !self.is_running(id) {
            store::set_requested_status(&self.ctx.db, id, "pending", None)
                .await
                .map_err(store::internal)?;
        }
        self.enqueue(id, false);
        self.ctx.emit_changed(id, "updated");
        store::get_summary(&self.ctx.db, id).await
    }

    /// Re-translate the whole thread: every message back to `pending`, then a
    /// normal refresh (a job in flight gets one re-run).
    pub async fn retranslate(&self, id: &str) -> Result<SlackThreadSummary, CodedError> {
        store::get_summary(&self.ctx.db, id).await?;
        store::reset_translations(&self.ctx.db, id)
            .await
            .map_err(store::internal)?;
        self.refresh(id).await
    }

    /// Post `text` as a reply in the thread (as the token's user), then refresh
    /// the thread so the new message shows up.
    pub async fn post_reply(&self, id: &str, text: &str) -> Result<SlackThreadSummary, CodedError> {
        let text = text.trim();
        if text.is_empty() {
            return Err(CodedError::new("empty_reply", "reply text is empty"));
        }
        if text.chars().count() > MAX_REPLY_CHARS {
            return Err(CodedError::new(
                "reply_too_long",
                format!("reply exceeds {MAX_REPLY_CHARS} characters"),
            ));
        }
        let summary = store::get_summary(&self.ctx.db, id).await?;
        let link = summary
            .thread_url
            .as_deref()
            .and_then(|u| link::parse_thread_link(u).ok())
            .ok_or_else(|| {
                CodedError::new("invalid_link", "thread has no parsable Slack thread_url")
            })?;
        let found = token::sources(&self.ctx.db, &self.ctx.lookup)
            .await
            .map_err(store::internal)?;
        let (client, _) = token::resolve(
            &found,
            &link.host,
            &self.ctx.http,
            &self.ctx.token_http,
            &self.ctx.auth_cache,
        )
        .await?;
        // A fetched thread knows its root ts even when the link was a reply.
        let thread_ts = summary.thread_ts.as_deref().unwrap_or(&link.thread_ts);
        client
            .post_thread_reply(&link.channel_id, thread_ts, text)
            .await
            .map_err(post_error)?;
        self.refresh(id).await
    }

    pub async fn delete(&self, ids: &[String]) -> Result<(), CodedError> {
        if let Ok(mut q) = self.queue.lock() {
            q.pending.retain(|x| !ids.contains(x));
            for id in ids {
                q.rerun.remove(id);
                q.from_api.remove(id);
            }
        }
        let deleted = store::delete(&self.ctx.db, &self.ctx.app_data, ids).await?;
        for id in &deleted {
            self.ctx.emit_changed(id, "deleted");
        }
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests;
