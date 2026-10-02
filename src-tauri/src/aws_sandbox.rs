//! Per-project AWS sandbox config files.
//!
//! A run child must never see the user's real `~/.aws`. Instead every run points
//! `AWS_CONFIG_FILE` / `AWS_SHARED_CREDENTIALS_FILE` at a tiny sandbox holding
//! only a region (and an empty credentials file); real credentials are injected
//! per-child by the `aws` shim via the broker.
//!
//! These files are deterministic per project (a project is linked to at most one
//! AWS account), so they are written ONCE when the project's AWS account is
//! configured — NOT on every run. Runs only read them. A project with no linked
//! account shares a single static `_empty` sandbox. This keeps the sandbox count
//! at O(projects) instead of O(runs) and removes the concurrent-write race two
//! parallel runs of the same project would otherwise have.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use sqlx::Row;

use crate::db::Db;

const AWS_PROJECTS_DIR: &str = "aws-projects";
const EMPTY_DIR: &str = "_empty";

/// The per-project sandbox directory: `<app_data>/aws-projects/<project_id>`.
pub fn project_aws_dir(app_data_dir: &Path, project_id: &str) -> PathBuf {
    app_data_dir.join(AWS_PROJECTS_DIR).join(project_id)
}

/// The shared sandbox for projects with no linked AWS account. It only strips
/// the inherited real config; it never carries a region or credentials.
pub fn empty_aws_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(AWS_PROJECTS_DIR).join(EMPTY_DIR)
}

/// Write `contents` to `path` atomically (temp file in the same dir + rename) so
/// a concurrent reader (an `aws`/SDK child) never observes a truncated file.
fn atomic_write(path: &Path, contents: &str) -> std::io::Result<()> {
    let dir = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    std::fs::create_dir_all(dir)?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config");
    let tmp = dir.join(format!(".{file_name}.tmp.{}.{nanos}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

/// Ensure the shared `_empty` sandbox exists. Idempotent; call at startup.
pub fn ensure_empty_sandbox(app_data_dir: &Path) {
    let dir = empty_aws_dir(app_data_dir);
    let _ = atomic_write(&dir.join("config"), "[default]\n");
    let _ = atomic_write(&dir.join("credentials"), "");
}

/// Materialize (or refresh) the per-project sandbox from the project's linked
/// AWS account. When the project has no resolvable account the sandbox is
/// removed. Call this from the commands that link/update AWS config — never from
/// a run.
pub async fn write_project_aws_config(db: &Db, app_data_dir: &Path, project_id: &str) {
    let meta = match crate::runs::broker::token::resolve_aws_runtime_metadata(db, project_id).await
    {
        Ok(Some(meta)) => meta,
        _ => {
            remove_project_aws_config(app_data_dir, project_id);
            return;
        }
    };
    let dir = project_aws_dir(app_data_dir, project_id);
    let _ = atomic_write(
        &dir.join("config"),
        &format!("[default]\nregion = {}\n", meta.region),
    );
    let _ = atomic_write(&dir.join("credentials"), "");
}

/// Remove a project's sandbox (on unlink / project deletion).
pub fn remove_project_aws_config(app_data_dir: &Path, project_id: &str) {
    let _ = std::fs::remove_dir_all(project_aws_dir(app_data_dir, project_id));
}

/// Startup reconciliation: ensure the shared sandbox, (re)write the sandbox for
/// every project that already has a linked account (so projects configured
/// before this scheme still work without a run ever writing), and sweep away the
/// legacy per-run `aws-runs` directory from the old implementation.
pub async fn sync_all(db: &Db, app_data_dir: &Path) {
    ensure_empty_sandbox(app_data_dir);

    if let Ok(rows) = sqlx::query("SELECT id FROM projects WHERE aws_account_id IS NOT NULL")
        .fetch_all(db)
        .await
    {
        for row in rows {
            let id: String = row.get("id");
            write_project_aws_config(db, app_data_dir, &id).await;
        }
    }

    // Legacy scheme wrote one dir per run; none of them are referenced anymore.
    let _ = std::fs::remove_dir_all(app_data_dir.join("aws-runs"));
}
