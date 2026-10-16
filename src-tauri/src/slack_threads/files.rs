//! Slack file downloads (bearer token, Slack hosts only) and the shared,
//! token-less avatar cache.

use super::api::{SlackClient, SlackError};
use super::store::{self, avatar_rel_path};
use super::SlackCtx;
use crate::inbox_api::ingest::safe_file_name;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

pub const MAX_FILE_BYTES: i64 = 100 * 1024 * 1024;
pub const MAX_THREAD_BYTES: i64 = 500 * 1024 * 1024;
const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;
const PARALLEL_DOWNLOADS: usize = 3;

/// File metadata from a message's `files[]`.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteFile {
    pub slack_file_id: String,
    pub message_ts: String,
    pub name: String,
    pub title: Option<String>,
    pub mime: Option<String>,
    pub filetype: Option<String>,
    pub size: i64,
    pub remote_updated: Option<i64>,
    pub permalink: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub mode: Option<String>,
    /// `url_private_download`, else `url_private`.
    pub url: Option<String>,
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
        .map(str::to_string)
}

pub fn parse_remote_file(v: &Value, message_ts: &str) -> Option<RemoteFile> {
    let id = s(v, "id")?;
    let name = s(v, "name")
        .or_else(|| s(v, "title"))
        .unwrap_or_else(|| id.clone());
    Some(RemoteFile {
        message_ts: message_ts.to_string(),
        title: s(v, "title"),
        mime: s(v, "mimetype"),
        filetype: s(v, "filetype"),
        size: v.get("size").and_then(Value::as_i64).unwrap_or(0),
        remote_updated: v
            .get("updated")
            .and_then(Value::as_i64)
            .filter(|x| *x > 0)
            .or_else(|| v.get("created").and_then(Value::as_i64)),
        permalink: s(v, "permalink"),
        width: v.get("original_w").and_then(Value::as_i64),
        height: v.get("original_h").and_then(Value::as_i64),
        mode: s(v, "mode"),
        url: s(v, "url_private_download").or_else(|| s(v, "url_private")),
        slack_file_id: id,
        name,
    })
}

/// Relative dir of one Slack file: `slack-threads/<id>/files/<slack_file_id>`;
/// `None` unless both ids are safe to join into a path.
pub fn file_dir_rel(thread_id: &str, slack_file_id: &str) -> Option<String> {
    (store::is_safe_id(thread_id) && is_safe_file_id(slack_file_id)).then(|| {
        format!(
            "{}/{}/files/{}",
            store::THREADS_DIR,
            thread_id,
            slack_file_id
        )
    })
}

/// Remove a file's on-disk dir (`files/<slack_file_id>/`), if any.
pub fn remove_file_dir(app_data: &Path, thread_id: &str, slack_file_id: &str) {
    if let Some(rel) = file_dir_rel(thread_id, slack_file_id) {
        let dir = app_data.join(rel);
        if dir.exists() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

/// Slack file ids are `F…`; anything else is not joined into a path.
pub fn is_safe_file_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Plan {
    Keep,
    Download,
    Mark {
        status: &'static str,
        error: Option<String>,
    },
}

#[derive(Debug)]
enum DlError {
    /// 403/404 or another failure (`files_partial`).
    Failed(String),
    /// An HTML page instead of the file (`files_scope`).
    NoAccess,
    TooLarge,
    /// The thread was deleted meanwhile; nothing was kept on disk.
    Cancelled,
}

/// Lets a download know whether its thread still exists.
#[derive(Clone)]
struct Guard {
    db: crate::db::Db,
    thread_id: String,
    cancelled: Arc<AtomicBool>,
}

impl Guard {
    /// False once the thread is gone (checked in the DB, then remembered).
    async fn alive(&self) -> bool {
        if self.cancelled.load(Ordering::SeqCst) {
            return false;
        }
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads WHERE id = ?")
            .bind(&self.thread_id)
            .fetch_one(&self.db)
            .await
            .unwrap_or(1);
        if n == 0 {
            self.cancelled.store(true, Ordering::SeqCst);
        }
        n > 0
    }
}

/// Download one file into `dir/<name>` (via `.part`), replacing anything else
/// in that dir. Returns the bytes written. The body is read chunk by chunk with
/// an idle timeout (no overall 30 s limit), and nothing is written once the
/// thread is gone.
async fn download_one(
    client: &SlackClient,
    url: &str,
    dir: &Path,
    file_name: &str,
    html_expected: bool,
    guard: &Guard,
) -> Result<i64, DlError> {
    let mut resp = match client.get_download(url).await {
        Ok(r) => r,
        Err(SlackError::RateLimited) => return Err(DlError::Failed("rate limited".into())),
        Err(e) => return Err(DlError::Failed(e.to_string())),
    };
    let status = resp.status();
    if !status.is_success() {
        return Err(DlError::Failed(format!("HTTP {}", status.as_u16())));
    }
    let is_html = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("text/html"));
    if is_html && !html_expected {
        return Err(DlError::NoAccess);
    }
    if !guard.alive().await {
        return Err(DlError::Cancelled);
    }
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| DlError::Failed(e.to_string()))?;
    let part = dir.join(format!("{file_name}.part"));
    let result = async {
        let mut f = tokio::fs::File::create(&part)
            .await
            .map_err(|e| DlError::Failed(e.to_string()))?;
        let mut written: i64 = 0;
        let idle = client.download_idle_timeout();
        loop {
            if guard.cancelled.load(Ordering::SeqCst) {
                return Err(DlError::Cancelled);
            }
            let chunk = tokio::time::timeout(idle, resp.chunk())
                .await
                .map_err(|_| DlError::Failed("download stalled".into()))?
                .map_err(|_| DlError::Failed("download interrupted".into()))?;
            let Some(chunk) = chunk else { break };
            written += chunk.len() as i64;
            if written > MAX_FILE_BYTES {
                return Err(DlError::TooLarge);
            }
            f.write_all(&chunk)
                .await
                .map_err(|e| DlError::Failed(e.to_string()))?;
        }
        f.flush()
            .await
            .map_err(|e| DlError::Failed(e.to_string()))?;
        Ok(written)
    }
    .await;
    let written = match result {
        Ok(n) => n,
        Err(e) => {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(e);
        }
    };
    if !guard.alive().await {
        let _ = tokio::fs::remove_file(&part).await;
        return Err(DlError::Cancelled);
    }
    let final_path = dir.join(file_name);
    tokio::fs::rename(&part, &final_path)
        .await
        .map_err(|e| DlError::Failed(e.to_string()))?;
    // Drop older versions with another name.
    if let Ok(mut rd) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            if entry.file_name().to_string_lossy() != file_name {
                let _ = tokio::fs::remove_file(entry.path()).await;
            }
        }
    }
    Ok(written)
}

/// Outcome of a file sync.
#[derive(Debug, Default)]
pub struct FileSync {
    pub warnings: Vec<&'static str>,
    /// The thread disappeared mid-way.
    pub deleted: bool,
}

/// Bring `slack_files` of a thread in line with `remote` (rows already
/// upserted): download what is missing/changed (3 at a time), mark what can't
/// be downloaded. `files_ok = false` when the token lacks `files:read`.
pub async fn sync_files(
    ctx: &SlackCtx,
    client: &SlackClient,
    thread_id: &str,
    remote: &[RemoteFile],
    files_ok: bool,
    progress: &(dyn Fn(usize, usize) + Send + Sync),
) -> Result<FileSync, sqlx::Error> {
    let mut out = FileSync::default();
    let warn = |w: &'static str, out: &mut FileSync| {
        if !out.warnings.contains(&w) {
            out.warnings.push(w);
        }
    };
    if !files_ok && !remote.is_empty() {
        warn("files_scope", &mut out);
    }

    // Decide per file, in message order (the 500 MB budget is cumulative).
    let mut budget: i64 = 0;
    let mut plans: Vec<(&RemoteFile, Plan)> = Vec::new();
    for f in remote {
        let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
            "SELECT download_status, remote_updated, file_path FROM slack_files \
             WHERE thread_id = ? AND slack_file_id = ?",
        )
        .bind(thread_id)
        .bind(&f.slack_file_id)
        .fetch_optional(&ctx.db)
        .await?;
        let unavailable_mode = matches!(
            f.mode.as_deref(),
            Some("tombstone") | Some("hidden_by_limit") | Some("external")
        );
        let plan = if unavailable_mode
            || f.url.is_none()
            || !files_ok
            || file_dir_rel(thread_id, &f.slack_file_id).is_none()
        {
            Plan::Mark {
                status: "unavailable",
                error: None,
            }
        } else if f.size > MAX_FILE_BYTES || budget + f.size > MAX_THREAD_BYTES {
            Plan::Mark {
                status: "too_large",
                error: None,
            }
        } else {
            budget += f.size;
            let up_to_date = row.as_ref().is_some_and(|(status, updated, path)| {
                status == "done"
                    && *updated == f.remote_updated
                    && path.as_ref().is_some_and(|p| {
                        std::fs::metadata(ctx.app_data.join(p))
                            .is_ok_and(|m| m.is_file() && m.len() as i64 == f.size)
                    })
            });
            if up_to_date {
                Plan::Keep
            } else if !ctx.http.token_allowed(f.url.as_deref().unwrap_or("")) {
                Plan::Mark {
                    status: "unavailable",
                    error: Some("file host is not a Slack host".into()),
                }
            } else {
                Plan::Download
            }
        };
        plans.push((f, plan));
    }

    for (f, plan) in &plans {
        if let Plan::Mark { status, error } = plan {
            let r = sqlx::query(
                "UPDATE slack_files SET download_status = ?, download_error = ?, file_path = NULL \
                 WHERE thread_id = ? AND slack_file_id = ?",
            )
            .bind(status)
            .bind(error)
            .bind(thread_id)
            .bind(&f.slack_file_id)
            .execute(&ctx.db)
            .await?;
            if r.rows_affected() == 0 && !thread_exists(ctx, thread_id).await? {
                out.deleted = true;
                return Ok(out);
            }
            // A copy downloaded earlier is no longer valid.
            remove_file_dir(&ctx.app_data, thread_id, &f.slack_file_id);
        }
    }

    let todo: Vec<&RemoteFile> = plans
        .iter()
        .filter(|(_, p)| *p == Plan::Download)
        .map(|(f, _)| *f)
        .collect();
    let total = todo.len();
    progress(0, total);
    let sem = Arc::new(tokio::sync::Semaphore::new(PARALLEL_DOWNLOADS));
    let guard = Guard {
        db: ctx.db.clone(),
        thread_id: thread_id.to_string(),
        cancelled: Arc::new(AtomicBool::new(false)),
    };
    let mut set = tokio::task::JoinSet::new();
    for f in todo {
        let (client, sem, f, guard) = (client.clone(), sem.clone(), f.clone(), guard.clone());
        // Planned downloads always have a safe dir (checked above).
        let Some(rel_dir) = file_dir_rel(thread_id, &f.slack_file_id) else {
            continue;
        };
        let dir: PathBuf = ctx.app_data.join(&rel_dir);
        set.spawn(async move {
            let _permit = sem.acquire_owned().await;
            if guard.cancelled.load(Ordering::SeqCst) {
                return (f, String::new(), Err(DlError::Cancelled));
            }
            let name = safe_file_name(&f.name, &mut HashSet::new());
            let html = f.filetype.as_deref() == Some("html")
                || f.mime
                    .as_deref()
                    .is_some_and(|m| m.starts_with("text/html"));
            let url = f.url.clone().unwrap_or_default();
            let r = download_one(&client, &url, &dir, &name, html, &guard).await;
            (f, format!("{rel_dir}/{name}"), r)
        });
    }
    let mut done = 0;
    while let Some(joined) = set.join_next().await {
        let Ok((f, rel, r)) = joined else { continue };
        let (status, error, path): (&str, Option<String>, Option<String>) = match r {
            Ok(_) => ("done", None, Some(rel)),
            Err(DlError::NoAccess) => {
                warn("files_scope", &mut out);
                (
                    "error",
                    Some("no access to file (files:read?)".into()),
                    None,
                )
            }
            Err(DlError::TooLarge) => ("too_large", None, None),
            Err(DlError::Cancelled) => {
                guard.cancelled.store(true, Ordering::SeqCst);
                continue;
            }
            Err(DlError::Failed(e)) => {
                warn("files_partial", &mut out);
                ("error", Some(e), None)
            }
        };
        let res = sqlx::query(
            "UPDATE slack_files SET download_status = ?, download_error = ?, file_path = ? \
             WHERE thread_id = ? AND slack_file_id = ?",
        )
        .bind(status)
        .bind(error)
        .bind(path)
        .bind(thread_id)
        .bind(&f.slack_file_id)
        .execute(&ctx.db)
        .await?;
        if res.rows_affected() == 0 && !thread_exists(ctx, thread_id).await? {
            guard.cancelled.store(true, Ordering::SeqCst);
            break;
        }
        done += 1;
        progress(done, total);
    }
    if guard.cancelled.load(Ordering::SeqCst) {
        // Let every download task stop before the caller removes the thread
        // dir, so none can re-create it afterwards.
        set.abort_all();
        while set.join_next().await.is_some() {}
        out.deleted = true;
    }
    Ok(out)
}

async fn thread_exists(ctx: &SlackCtx, id: &str) -> Result<bool, sqlx::Error> {
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads WHERE id = ?")
        .bind(id)
        .fetch_one(&ctx.db)
        .await?;
    Ok(n > 0)
}

/// Cache an avatar (no token) under `slack-cache/avatars/…`; returns its
/// relative path, or `None` when it can't be fetched (the UI shows initials).
pub async fn ensure_avatar(
    ctx: &SlackCtx,
    team_id: &str,
    owner_id: &str,
    url: &str,
) -> Option<String> {
    let rel = avatar_rel_path(team_id, owner_id, url)?;
    let path = ctx.app_data.join(&rel);
    if path.is_file() {
        return Some(rel);
    }
    if !ctx.http.public_allowed(url) {
        return None;
    }
    let resp = ctx.public_http.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    if bytes.is_empty() || bytes.len() > MAX_AVATAR_BYTES {
        return None;
    }
    let dir = path.parent()?;
    tokio::fs::create_dir_all(dir).await.ok()?;
    let part = path.with_extension("part");
    tokio::fs::write(&part, &bytes).await.ok()?;
    tokio::fs::rename(&part, &path).await.ok()?;
    Some(rel)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_file_parsing() {
        let v = serde_json::json!({
            "id": "F1", "name": "shot one.png", "title": "Shot", "mimetype": "image/png",
            "filetype": "png", "size": 12, "created": 100, "updated": 0,
            "permalink": "https://malme.slack.com/files/U1/F1/shot",
            "url_private": "https://files.slack.com/files-pri/T1-F1/shot.png",
            "url_private_download": "https://files.slack.com/files-pri/T1-F1/download/shot.png",
            "original_w": 800, "original_h": 600
        });
        let f = parse_remote_file(&v, "1.2").unwrap();
        assert_eq!(f.slack_file_id, "F1");
        assert_eq!(f.remote_updated, Some(100));
        assert_eq!(
            f.url.as_deref(),
            Some("https://files.slack.com/files-pri/T1-F1/download/shot.png")
        );
        assert_eq!((f.width, f.height), (Some(800), Some(600)));
        assert!(parse_remote_file(&serde_json::json!({"name": "x"}), "1").is_none());
        assert!(is_safe_file_id("F0ABC"));
        assert!(!is_safe_file_id("../F1"));
        assert_eq!(
            file_dir_rel("t1", "F1").as_deref(),
            Some("slack-threads/t1/files/F1")
        );
        assert_eq!(file_dir_rel("../t1", "F1"), None);
        assert_eq!(file_dir_rel("t1", "F/1"), None);
    }
}
