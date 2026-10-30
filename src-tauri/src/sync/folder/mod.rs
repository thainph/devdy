//! Project folder sync (design: .workspace/docs/03-design/on-demand-sync.md
//! O4–O8, §4): make the receiver's project folder identical to the source's.
//!
//! - Source: [`handler::FolderSource`] serves `folder_manifest` (walk the
//!   folder, describe every git repo + non-git file, bundle the commits the
//!   receiver lacks) and `folder_fetch` (upload contents by sha256, in ranges).
//! - Receiver: [`apply::preview`] (no writes) and [`apply::apply`]: backup →
//!   git (bundle fetch, refs, checkout, index patch) → files (receiver-driven
//!   batches, sha-verified atomic writes) → cleanup (receiver-only files).
//!
//! Safety: every path stays under the mapped root (`paths`), excluded paths
//! are never touched, git runs without hooks or network (`git`), file
//! contents are never logged, and failures after the backup name it.

pub mod apply;
pub mod backup;
// The source side is only reached once the engine registers
// `commands::sync_folder::folder_handler`.
// TODO(integrator): drop these allows after wiring it.
#[allow(dead_code)]
pub mod git;
#[allow(dead_code)]
pub mod handler;
#[allow(dead_code)]
pub mod manifest;
pub mod paths;
pub mod plan;
pub mod scan;
pub mod transfer;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Request kinds served on the source.
pub const KIND_MANIFEST: &str = "folder_manifest";
pub const KIND_FETCH: &str = "folder_fetch";
/// Reply kinds.
pub const REPLY_MANIFEST: &str = "folder_manifest_ready";
pub const REPLY_CHUNK: &str = "folder_chunk";
pub const REPLY_FETCH_DONE: &str = "folder_fetch_done";

/// Progress event (`FolderProgress`).
pub const EVENT: &str = "sync://folder";

/// Most content bytes requested by one `folder_fetch` (keeps the relay's
/// per-group blob cap out of reach).
pub const BATCH_BYTES: u64 = 256 * 1024 * 1024;
/// Backups kept per repo / project folder.
pub const KEEP_BACKUPS: usize = 5;
pub const DEFAULT_MAX_FILE_MB: u64 = 500;
/// Default excludes (O6). TODO(integrator): same list as `prefs::DEFAULT_EXCLUDES`.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    "Pods",
    ".DS_Store",
];
/// Always excluded, whatever the settings.
pub const ALWAYS_EXCLUDES: &[&str] = &[".git", ".devdy/"];

/// Receiver-side settings of one project's folder sync.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FolderOptions {
    pub project_id: String,
    /// Where the project lives on this machine (created when missing).
    pub local_root: PathBuf,
    /// Extra gitignore-style patterns.
    pub excludes: Vec<String>,
    /// Apply [`DEFAULT_EXCLUDES`] too.
    pub default_excludes: bool,
    pub max_file_mb: u64,
}

impl FolderOptions {
    /// Exclude patterns sent to the source (without [`ALWAYS_EXCLUDES`],
    /// which both sides add themselves).
    pub fn patterns(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.default_excludes {
            out.extend(DEFAULT_EXCLUDES.iter().map(|s| s.to_string()));
        }
        out.extend(self.excludes.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty() && !s.starts_with('#')));
        out
    }

    pub fn max_file_bytes(&self) -> u64 {
        self.max_file_mb.saturating_mul(1024 * 1024)
    }
}

/// Payload of [`EVENT`].
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderProgress {
    pub project_id: String,
    /// `manifest` | `backup` | `git` | `files` | `cleanup` | `done` | `error` | `skipped`.
    pub phase: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Receives progress (the command layer emits it as [`EVENT`]).
pub type ProgressSink = Arc<dyn Fn(&FolderProgress) + Send + Sync>;

/// Cooperative cancellation, checked between steps, files and chunks.
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> Result<(), String> {
        if self.is_cancelled() {
            Err("cancelled".into())
        } else {
            Ok(())
        }
    }
}

/// Run blocking filesystem / git work off the async runtime.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("folder sync task failed: {e}"))?
}
