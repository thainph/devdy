//! Project Pulse (docs/project-pulse-spec.md): Slack threads / GitHub issues
//! watched per project, summarised by AI into a progress overview.
//!
//! - `source`: link normalisation (pure, BR-02)
//! - `content`: source text to hash / summarise, 60k cap (pure, BR-05)
//! - `ball`: Slack "ball in my court" rule (pure, BR-07)
//! - `digest`: AI prompt, answer validation, hard facts (pure, BR-06/07)
//! - `github`: issue + board fetch with the project's GitHub account
//! - `refresh`: fetch → hash → summarise pipeline of one watch
//! - `store`: SQL for the watch rules, overview, digests and request queue
//!
//! The MCP sidecar only writes SQLite (BR-11): new watches land as
//! `resolve_status = 'pending'` plus a `pulse_requests` row. A background
//! poller picks them up every 3 s while a session is running or the Pulse
//! screen is open (commands kick it directly). Resolving a watch is its first
//! refresh; at most 3 refreshes run at once and a watch already refreshing is
//! never started twice (a second request waits for the running one).

pub mod ball;
pub mod content;
pub mod digest;
pub mod github;
pub mod health;
pub mod refresh;
pub mod source;
pub mod store;

#[cfg(test)]
mod tests;

use crate::db::Db;
use crate::slack_threads::{link, CodedError, SlackThreadsState};
use async_trait::async_trait;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use store::Watch;
use tauri::{AppHandle, Emitter};
use tokio::sync::{watch as done_signal, Notify, Semaphore};

pub const CHANGED_EVENT: &str = "pulse:changed";
pub const PROGRESS_EVENT: &str = "pulse:refresh-progress";
const POLL_INTERVAL: Duration = Duration::from_secs(3);
/// Sources refreshed at once (§3.3).
const MAX_PARALLEL_REFRESH: usize = 3;
/// Longest wait for a Slack fetch + translate job.
const SLACK_JOB_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const SLACK_JOB_POLL: Duration = Duration::from_millis(500);

/// A Slack thread captured in Devdy for a watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedThread {
    pub id: String,
    pub title: Option<String>,
    /// Capture failed (e.g. `no_token`): the watch goes to `error`.
    pub error_code: Option<String>,
}

/// Slack side of a watch; a fake in tests.
#[async_trait]
pub trait SlackCapture: Send + Sync {
    /// Thread of `url`, captured (fetch + translate queued) when missing and
    /// moved to `project_id` when it belongs elsewhere (BR-03).
    async fn ensure_thread(
        &self,
        url: &str,
        project_id: &str,
    ) -> Result<CapturedThread, CodedError>;

    /// Fetch + translate the thread again (the existing job; a job already
    /// queued / running is just awaited) and wait until it ends. `Err` carries
    /// the thread's `fetch_error_code`.
    async fn refresh_and_wait(&self, thread_id: &str) -> Result<(), CodedError>;
}

#[async_trait]
impl SlackCapture for SlackThreadsState {
    async fn ensure_thread(
        &self,
        url: &str,
        project_id: &str,
    ) -> Result<CapturedThread, CodedError> {
        use crate::slack_threads::store as slack_store;
        let l = link::parse_thread_link(url)?;
        let db = &self.ctx().db;
        let existing = slack_store::find_by_key(db, &l.channel_id, &l.thread_ts, None)
            .await
            .map_err(slack_store::internal)?;
        let id = match existing {
            Some(id) => id,
            None => {
                self.add_thread(url, Some(project_id), None, false)
                    .await?
                    .id
            }
        };
        let s = slack_store::get_summary(db, &id).await?;
        if s.project_id.as_deref() != Some(project_id) {
            slack_store::set_project(db, &id, Some(project_id)).await?;
            self.ctx().emit_changed(&id, "updated");
        }
        let error_code = (s.fetch_status == "error").then(|| {
            s.fetch_error_code
                .clone()
                .unwrap_or_else(|| "slack_fetch_failed".to_string())
        });
        Ok(CapturedThread {
            id,
            title: Some(s.title).filter(|t| !t.trim().is_empty()),
            error_code,
        })
    }

    async fn refresh_and_wait(&self, thread_id: &str) -> Result<(), CodedError> {
        use crate::slack_threads::store as slack_store;
        if !self.is_queued(thread_id) {
            self.refresh(thread_id).await?;
        }
        let deadline = tokio::time::Instant::now() + SLACK_JOB_TIMEOUT;
        loop {
            tokio::time::sleep(SLACK_JOB_POLL).await;
            if self.is_queued(thread_id) {
                if tokio::time::Instant::now() >= deadline {
                    return Err(CodedError::new("slack_fetch_failed", "Slack job timed out"));
                }
                continue;
            }
            let s = slack_store::get_summary(&self.ctx().db, thread_id).await?;
            return match s.fetch_status.as_str() {
                "error" => Err(CodedError::new(
                    s.fetch_error_code
                        .unwrap_or_else(|| "slack_fetch_failed".to_string()),
                    s.fetch_error.unwrap_or_default(),
                )),
                _ => Ok(()),
            };
        }
    }
}

/// The AI that writes digests; a fake in tests.
#[async_trait]
pub trait DigestEngine: Send + Sync {
    /// One prompt → the raw answer.
    async fn complete(&self, prompt: String) -> Result<String, String>;
    /// `engine:model`, stored with the digest.
    async fn model(&self) -> Option<String>;
    /// Stop the engine's warm session (app exit).
    async fn shutdown(&self) {}
}

/// The warm sidecar engine Slack translation uses (`translate::submit_prompt`),
/// on its own instance so digests never queue behind thread translations.
pub struct SidecarDigestEngine(pub crate::slack_threads::SidecarEngine);

#[async_trait]
impl DigestEngine for SidecarDigestEngine {
    async fn complete(&self, prompt: String) -> Result<String, String> {
        use crate::slack_threads::translate::{EngineError, TranslateEngine};
        self.0.translate(prompt).await.map_err(|e| match e {
            EngineError::Unavailable(d) | EngineError::Failed(d) => d,
        })
    }

    async fn model(&self) -> Option<String> {
        crate::commands::translate::load_engine_model(&self.0.db)
            .await
            .ok()
    }

    async fn shutdown(&self) {
        self.0.state.0.shutdown().await;
    }
}

pub trait PulseEvents: Send + Sync {
    fn changed(&self, watch_ids: &[String]);
    fn progress(&self, done: usize, total: usize);
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangedPayload<'a> {
    watch_ids: &'a [String],
}

#[derive(Clone, Serialize)]
struct ProgressPayload {
    done: usize,
    total: usize,
}

pub struct AppEvents {
    pub app: AppHandle,
}

impl PulseEvents for AppEvents {
    fn changed(&self, watch_ids: &[String]) {
        let _ = self.app.emit(CHANGED_EVENT, ChangedPayload { watch_ids });
    }

    fn progress(&self, done: usize, total: usize) {
        let _ = self
            .app
            .emit(PROGRESS_EVENT, ProgressPayload { done, total });
    }
}

/// Everything the pipeline talks to (fakes in tests).
pub struct Services {
    pub slack: Arc<dyn SlackCapture>,
    pub github: Arc<dyn github::GithubFetch>,
    pub engine: Arc<dyn DigestEngine>,
    pub events: Arc<dyn PulseEvents>,
}

struct Inner {
    db: Db,
    svc: Services,
    kick: Notify,
    screen_open: AtomicBool,
    /// One `process_pending` at a time.
    busy: tokio::sync::Mutex<()>,
    /// `MAX(pulse_watches.updated_at)` seen by the last pass (outer None =
    /// not read yet): a change nobody emitted (an MCP unwatch) still reaches
    /// the UI.
    last_updated: Mutex<Option<Option<String>>>,
    slots: Arc<Semaphore>,
    /// Watch id → "finished" signal of its running refresh.
    inflight: Mutex<HashMap<String, done_signal::Receiver<bool>>>,
    /// Refresh requests (MCP) being served.
    serving: Mutex<HashSet<String>>,
    /// `(done, total)` of the refreshes since the queue was last empty.
    progress: Mutex<(usize, usize)>,
}

#[derive(Clone)]
pub struct PulseState {
    inner: Arc<Inner>,
}

impl PulseState {
    pub fn new(db: Db, svc: Services) -> Self {
        PulseState {
            inner: Arc::new(Inner {
                db,
                svc,
                kick: Notify::new(),
                screen_open: AtomicBool::new(false),
                busy: tokio::sync::Mutex::new(()),
                last_updated: Mutex::new(None),
                slots: Arc::new(Semaphore::new(MAX_PARALLEL_REFRESH)),
                inflight: Mutex::default(),
                serving: Mutex::default(),
                progress: Mutex::default(),
            }),
        }
    }

    pub fn for_app(app: &AppHandle, db: Db, slack: SlackThreadsState) -> Self {
        let engine = crate::slack_threads::SidecarEngine {
            app: app.clone(),
            db: db.clone(),
            state: crate::slack_threads::SlackTranslateState(
                crate::commands::translate::TranslateState::default()
                    .with_recycle_after(DIGEST_RECYCLE_AFTER)
                    .silent()
                    .tracked_as(crate::procs::ProcKind::PulseDigest),
            ),
        };
        Self::new(
            db.clone(),
            Services {
                slack: Arc::new(slack),
                github: Arc::new(github::OctoGithub { db }),
                engine: Arc::new(SidecarDigestEngine(engine)),
                events: Arc::new(AppEvents { app: app.clone() }),
            },
        )
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    /// Kill the digest engine's warm session (app exit).
    pub async fn shutdown(&self) {
        self.inner.svc.engine.shutdown().await;
    }

    pub fn emit_changed(&self, watch_ids: &[String]) {
        self.inner.svc.events.changed(watch_ids);
    }

    /// Process the queue now (after a command added / moved a watch).
    pub fn kick(&self) {
        self.inner.kick.notify_one();
    }

    /// The Pulse screen keeps the poller running while it is open.
    pub fn set_screen_open(&self, open: bool) {
        self.inner.screen_open.store(open, Ordering::Relaxed);
        if open {
            self.kick();
        }
    }

    async fn should_poll(&self) -> bool {
        self.inner.screen_open.load(Ordering::Relaxed)
            || store::any_run_running(&self.inner.db)
                .await
                .unwrap_or(false)
    }

    /// Background loop: a pass on every kick, and every 3 s while the BR-11
    /// gate is open. Rows left over from before a restart are picked up by
    /// the first pass after the gate opens.
    pub fn start_poller(&self) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            loop {
                let kicked = tokio::select! {
                    _ = this.inner.kick.notified() => true,
                    _ = tokio::time::sleep(POLL_INTERVAL) => false,
                };
                this.tick(kicked).await;
            }
        });
    }

    /// One poller step: process the queue when kicked or when a session is
    /// running / the Pulse screen is open. Returns whether it processed.
    pub async fn tick(&self, kicked: bool) -> bool {
        if !kicked && !self.should_poll().await {
            return false;
        }
        self.process_pending().await;
        true
    }

    /// Start refreshing `watches` (those already refreshing are not started
    /// again). Returns how many were newly started and one "finished" signal
    /// per watch.
    pub fn schedule(&self, watches: Vec<Watch>) -> (usize, Vec<done_signal::Receiver<bool>>) {
        let mut waits = Vec::new();
        let mut started = Vec::new();
        {
            let mut inflight = self.inner.inflight.lock().unwrap();
            for w in watches {
                if let Some(rx) = inflight.get(&w.id) {
                    waits.push(rx.clone());
                    continue;
                }
                let (tx, rx) = done_signal::channel(false);
                inflight.insert(w.id.clone(), rx.clone());
                waits.push(rx);
                started.push((w, tx));
            }
        }
        let n = started.len();
        if n > 0 {
            let (done, total) = {
                let mut p = self.inner.progress.lock().unwrap();
                p.1 += n;
                *p
            };
            self.inner.svc.events.progress(done, total);
        }
        for (w, tx) in started {
            let this = self.clone();
            tokio::spawn(async move {
                let permit = this.inner.slots.clone().acquire_owned().await;
                this.refresh_watch(&w.id).await;
                drop(permit);
                this.inner.inflight.lock().unwrap().remove(&w.id);
                let _ = tx.send(true);
                let (done, total) = {
                    let mut p = this.inner.progress.lock().unwrap();
                    p.0 += 1;
                    let now = *p;
                    if p.0 >= p.1 {
                        *p = (0, 0);
                    }
                    now
                };
                this.inner.svc.events.progress(done, total);
                this.emit_changed(std::slice::from_ref(&w.id));
            });
        }
        (n, waits)
    }

    /// Wait for the given "finished" signals.
    pub async fn wait_all(waits: Vec<done_signal::Receiver<bool>>) {
        for mut rx in waits {
            let _ = rx.wait_for(|done| *done).await;
        }
    }

    /// Refresh every active watch in scope (both None = all) and wait for it.
    pub async fn refresh_scope(
        &self,
        project_id: Option<&str>,
        watch_id: Option<&str>,
    ) -> Result<usize, CodedError> {
        let watches = store::scope_watches(&self.inner.db, project_id, watch_id).await?;
        let n = watches.len();
        let (_, waits) = self.schedule(watches);
        Self::wait_all(waits).await;
        Ok(n)
    }

    /// Nothing refreshing and no request being served.
    #[cfg(test)]
    pub async fn idle(&self) {
        loop {
            let busy = !self.inner.inflight.lock().unwrap().is_empty()
                || !self.inner.serving.lock().unwrap().is_empty();
            if !busy {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    /// Start resolving pending watches (their first refresh) and serve open
    /// requests: a `refresh` request gets `done_at` only once every watch in
    /// its scope finished refreshing; a `resolve` request once its watch is
    /// no longer pending. Returns the ids of the watches started.
    pub async fn process_pending(&self) -> Vec<String> {
        let _guard = self.inner.busy.lock().await;
        let db = &self.inner.db;
        let mut started: Vec<String> = Vec::new();
        match store::pending_watches(db).await {
            Ok(watches) => {
                let fresh: Vec<Watch> = {
                    let inflight = self.inner.inflight.lock().unwrap();
                    watches
                        .into_iter()
                        .filter(|w| !inflight.contains_key(&w.id))
                        .collect()
                };
                started.extend(fresh.iter().map(|w| w.id.clone()));
                self.schedule(fresh);
            }
            Err(e) => eprintln!("[pulse] pending watches: {e}"),
        }
        match store::open_requests(db).await {
            Ok(reqs) => {
                for r in reqs {
                    self.serve_request(r).await;
                }
            }
            Err(e) => eprintln!("[pulse] open requests: {e}"),
        }
        let external = match store::max_updated_at(db).await {
            Ok(max) => {
                let prev = self.inner.last_updated.lock().unwrap().replace(max.clone());
                prev.is_some_and(|p| p != max)
            }
            Err(e) => {
                eprintln!("[pulse] max updated_at: {e}");
                false
            }
        };
        if external {
            self.emit_changed(&[]);
        }
        started
    }

    async fn serve_request(&self, r: store::PulseRequest) {
        let db = &self.inner.db;
        if r.kind == "resolve" {
            let pending = match r.watch_id.as_deref() {
                Some(id) => store::get(db, id)
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|w| w.active && w.resolve_status == "pending"),
                None => false,
            };
            if !pending {
                if let Err(e) = store::mark_request_done(db, &r.id).await {
                    eprintln!("[pulse] request {} failed: {e}", r.id);
                }
            }
            return;
        }
        if !self.inner.serving.lock().unwrap().insert(r.id.clone()) {
            return;
        }
        let this = self.clone();
        tokio::spawn(async move {
            let db = &this.inner.db;
            if let Err(e) = this
                .refresh_scope(r.project_id.as_deref(), r.watch_id.as_deref())
                .await
            {
                eprintln!("[pulse] refresh request {} failed: {e}", r.id);
            }
            // Per-source failures are on the watches (`last_error`).
            if let Err(e) = store::mark_request_done(db, &r.id).await {
                eprintln!("[pulse] request {} failed: {e}", r.id);
            }
            this.inner.serving.lock().unwrap().remove(&r.id);
        });
    }
}

/// A fresh engine session per digest: one source's content must never leak
/// into another's summary through a shared conversation.
const DIGEST_RECYCLE_AFTER: u32 = 1;
