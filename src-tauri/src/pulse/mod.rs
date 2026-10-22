//! Project Pulse (docs/project-pulse-spec.md): Slack threads / GitHub issues
//! watched per project, summarised by AI into a progress overview.
//!
//! - `source`: link normalisation (pure, BR-02)
//! - `store`: SQL for the watch rules, overview and request queue
//!
//! The MCP sidecar only writes SQLite (BR-11): new watches land as
//! `resolve_status = 'pending'` plus a `pulse_requests` row. A background
//! poller picks them up every 3 s while a session is running or the Pulse
//! screen is open (commands kick it directly), resolves each source and emits
//! `pulse:changed`.

pub mod source;
pub mod store;

#[cfg(test)]
mod tests;

use crate::db::Db;
use crate::slack_threads::{link, CodedError, SlackThreadsState};
use async_trait::async_trait;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use store::Watch;
use tauri::{AppHandle, Emitter};
use tokio::sync::Notify;

pub const CHANGED_EVENT: &str = "pulse:changed";
const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// A Slack thread captured in Devdy for a watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapturedThread {
    pub id: String,
    pub title: Option<String>,
    /// Capture failed (e.g. `no_token`): the watch goes to `error`.
    pub error_code: Option<String>,
}

/// Slack side of resolving a watch; a fake in tests.
#[async_trait]
pub trait SlackCapture: Send + Sync {
    /// Thread of `url`, captured (fetch + translate queued) when missing and
    /// moved to `project_id` when it belongs elsewhere (BR-03).
    async fn ensure_thread(
        &self,
        url: &str,
        project_id: &str,
    ) -> Result<CapturedThread, CodedError>;
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
}

pub trait PulseEvents: Send + Sync {
    fn changed(&self, watch_ids: &[String]);
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangedPayload<'a> {
    watch_ids: &'a [String],
}

pub struct AppEvents {
    pub app: AppHandle,
}

impl PulseEvents for AppEvents {
    fn changed(&self, watch_ids: &[String]) {
        let _ = self.app.emit(CHANGED_EVENT, ChangedPayload { watch_ids });
    }
}

struct Inner {
    db: Db,
    slack: Arc<dyn SlackCapture>,
    events: Arc<dyn PulseEvents>,
    kick: Notify,
    screen_open: AtomicBool,
    /// One `process_pending` at a time.
    busy: tokio::sync::Mutex<()>,
    /// `MAX(pulse_watches.updated_at)` seen by the last pass (outer None =
    /// not read yet): a change nobody emitted (an MCP unwatch) still reaches
    /// the UI.
    last_updated: std::sync::Mutex<Option<Option<String>>>,
}

#[derive(Clone)]
pub struct PulseState {
    inner: Arc<Inner>,
}

impl PulseState {
    pub fn new(db: Db, slack: Arc<dyn SlackCapture>, events: Arc<dyn PulseEvents>) -> Self {
        PulseState {
            inner: Arc::new(Inner {
                db,
                slack,
                events,
                kick: Notify::new(),
                screen_open: AtomicBool::new(false),
                busy: tokio::sync::Mutex::new(()),
                last_updated: std::sync::Mutex::new(None),
            }),
        }
    }

    pub fn for_app(app: &AppHandle, db: Db, slack: SlackThreadsState) -> Self {
        Self::new(
            db,
            Arc::new(slack),
            Arc::new(AppEvents { app: app.clone() }),
        )
    }

    pub fn db(&self) -> &Db {
        &self.inner.db
    }

    pub fn emit_changed(&self, watch_ids: &[String]) {
        self.inner.events.changed(watch_ids);
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

    /// Background loop: one pass at startup, then on every kick and every 3 s
    /// while a session is running or the Pulse screen is open.
    pub fn start_poller(&self) {
        let this = self.clone();
        tauri::async_runtime::spawn(async move {
            this.process_pending().await;
            loop {
                let kicked = tokio::select! {
                    _ = this.inner.kick.notified() => true,
                    _ = tokio::time::sleep(POLL_INTERVAL) => false,
                };
                if kicked || this.should_poll().await {
                    this.process_pending().await;
                }
            }
        });
    }

    /// Resolve pending watches, then close open requests. Returns the ids of
    /// the watches that changed (also emitted as `pulse:changed`); other
    /// writes to `pulse_watches` since the last pass emit `pulse:changed`
    /// with no ids (reload everything).
    pub async fn process_pending(&self) -> Vec<String> {
        let _guard = self.inner.busy.lock().await;
        let db = &self.inner.db;
        let mut changed: Vec<String> = Vec::new();
        match store::pending_watches(db).await {
            Ok(watches) => {
                for w in watches {
                    match self.resolve(&w).await {
                        Ok(true) => changed.push(w.id.clone()),
                        Ok(false) => {}
                        Err(e) => eprintln!("[pulse] resolve {} failed: {e}", w.id),
                    }
                }
            }
            Err(e) => eprintln!("[pulse] pending watches: {e}"),
        }
        // `resolve` requests are covered by the pending watches above. Refresh
        // (fetch + summarise) arrives with the refresh pipeline; until then a
        // refresh request is acknowledged as is.
        match store::open_requests(db).await {
            Ok(reqs) => {
                for r in reqs {
                    if let Err(e) = store::mark_request_done(db, &r.id).await {
                        eprintln!("[pulse] request {} failed: {e}", r.id);
                    }
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
        if !changed.is_empty() {
            self.emit_changed(&changed);
        } else if external {
            self.emit_changed(&[]);
        }
        changed
    }

    /// Slack: make sure the thread is captured and linked; GitHub: ready (the
    /// issue itself is checked by the refresh pipeline).
    async fn resolve(&self, w: &Watch) -> Result<bool, CodedError> {
        let db = &self.inner.db;
        match w.source_type.as_str() {
            "slack" => match self
                .inner
                .slack
                .ensure_thread(&w.source_url, &w.project_id)
                .await
            {
                Ok(t) => {
                    store::set_resolved(
                        db,
                        w,
                        Some(&t.id),
                        t.title.as_deref(),
                        t.error_code.as_deref(),
                    )
                    .await
                }
                Err(e) => store::set_resolved(db, w, None, None, Some(&e.code)).await,
            },
            _ => store::set_resolved(db, w, None, None, None).await,
        }
    }
}
