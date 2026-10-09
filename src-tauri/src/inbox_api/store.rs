//! SQLite + filesystem side of the capture inbox (Slack threads + web pages):
//! upsert a parsed capture (with its attachment files), list/read/update/
//! delete/reorder. Shared by the HTTP API and the Tauri commands; takes plain
//! `&Db` + `app_data` so it is testable against an in-memory DB and a temp dir.

use super::ingest::{guess_mime, safe_file_name, CaptureKind, IngestError, ParsedCapture};
use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub id: String,
    pub kind: CaptureKind,
    pub title: String,
    pub content: String,
    /// True once the user renamed the capture (or `X-Devdy-Title` was sent);
    /// re-exports then keep the title.
    pub title_custom: bool,
    pub project_id: Option<String>,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    pub exported_at: Option<String>,
    pub attachment_count: i64,
    // slack
    pub workspace: Option<String>,
    pub channel: Option<String>,
    pub thread_url: Option<String>,
    pub thread_ts: Option<String>,
    // web
    pub source_url: Option<String>,
    pub site_name: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
    pub description: Option<String>,
    pub selection: bool,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CaptureAttachment {
    pub id: String,
    pub capture_id: String,
    /// Original relative path inside the zip.
    pub name: String,
    /// ABSOLUTE path on disk (the DB stores it relative to `<app_data>`).
    pub file_path: String,
    pub size: i64,
    pub mime: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDetail {
    #[serde(flatten)]
    pub capture: Capture,
    pub attachments: Vec<CaptureAttachment>,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IngestStatus {
    Created,
    Updated,
}

impl IngestStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            IngestStatus::Created => "created",
            IngestStatus::Updated => "updated",
        }
    }
}

/// Result of an upsert, returned by the API and `import_capture_file`.
#[derive(Debug, Serialize, Clone)]
pub struct IngestResult {
    pub id: String,
    pub status: IngestStatus,
    /// Final title (used for the OS notification; not part of the API body).
    #[serde(skip)]
    pub title: String,
    #[serde(skip)]
    pub kind: CaptureKind,
}

/// Request-level overrides (`X-Devdy-Project-Id` / `X-Devdy-Title` /
/// `X-Devdy-Mode: new` headers).
#[derive(Debug, Default, Clone)]
pub struct Overrides {
    pub project_id: Option<String>,
    pub title: Option<String>,
    /// Always create a new row (web only; `X-Devdy-Mode: new`).
    pub force_new: bool,
}

const CAPTURE_COLUMNS: &str = "t.id, t.kind, t.title, t.content, t.title_custom, t.project_id, \
     t.position, t.created_at, t.updated_at, t.exported_at, \
     t.workspace, t.channel, t.thread_url, t.thread_ts, \
     t.source_url, t.site_name, t.author, t.published_at, t.description, t.selection, \
     (SELECT COUNT(*) FROM capture_attachments a WHERE a.capture_id = t.id) AS attachment_count";

fn row_kind(row: &sqlx::sqlite::SqliteRow) -> CaptureKind {
    CaptureKind::parse(&row.get::<String, _>("kind")).unwrap_or_default()
}

fn row_to_capture(row: &sqlx::sqlite::SqliteRow) -> Capture {
    Capture {
        id: row.get("id"),
        kind: row_kind(row),
        title: row.get("title"),
        content: row.get("content"),
        title_custom: row.get::<i64, _>("title_custom") != 0,
        project_id: row.get("project_id"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        exported_at: row.get("exported_at"),
        attachment_count: row.get("attachment_count"),
        workspace: row.get("workspace"),
        channel: row.get("channel"),
        thread_url: row.get("thread_url"),
        thread_ts: row.get("thread_ts"),
        source_url: row.get("source_url"),
        site_name: row.get("site_name"),
        author: row.get("author"),
        published_at: row.get("published_at"),
        description: row.get("description"),
        selection: row.get::<i64, _>("selection") != 0,
    }
}

/// Capture ids are UUIDs we generate; refuse anything else before it is joined
/// into a filesystem path.
fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `<app_data>/<kind dir>/<id>` (e.g. `slack-threads/<id>`, `web-pages/<id>`).
fn capture_dir(app_data: &Path, kind: CaptureKind, id: &str) -> PathBuf {
    app_data.join(kind.dir_name()).join(id)
}

fn internal(e: impl std::fmt::Display) -> IngestError {
    IngestError::Internal(e.to_string())
}

fn not_found(id: &str) -> String {
    format!("capture not found: {}", id)
}

/// Find the row a payload should update. Returns `(id, title, title_custom)`.
///
/// - slack: same non-empty `thread_url`, or else same (`channel`, `thread_ts`).
/// - web: same `url_key` among full-page (`selection = 0`) web rows; when
///   several share the key (possible after `X-Devdy-Mode: new`), the most
///   recently updated one is the target. A
///   selection capture, `X-Devdy-Mode: new`, or a missing URL never dedups.
async fn find_existing(
    db: &Db,
    t: &ParsedCapture,
    force_new: bool,
) -> Result<Option<(String, String, i64)>, sqlx::Error> {
    match t.kind {
        CaptureKind::Slack => {
            if let Some(url) = t.thread_url.as_deref() {
                return sqlx::query_as(
                    "SELECT id, title, title_custom FROM captures \
                     WHERE kind = 'slack' AND thread_url = ? ORDER BY created_at ASC LIMIT 1",
                )
                .bind(url)
                .fetch_optional(db)
                .await;
            }
            if let (Some(ch), Some(ts)) = (t.channel.as_deref(), t.thread_ts.as_deref()) {
                return sqlx::query_as(
                    "SELECT id, title, title_custom FROM captures \
                     WHERE kind = 'slack' AND channel = ? AND thread_ts = ? \
                     ORDER BY created_at ASC LIMIT 1",
                )
                .bind(ch)
                .bind(ts)
                .fetch_optional(db)
                .await;
            }
            Ok(None)
        }
        CaptureKind::Web => {
            if force_new || t.selection {
                return Ok(None);
            }
            let Some(key) = t.url_key.as_deref() else {
                return Ok(None);
            };
            sqlx::query_as(
                "SELECT id, title, title_custom FROM captures \
                 WHERE kind = 'web' AND selection = 0 AND url_key = ? \
                 ORDER BY updated_at DESC, created_at DESC, id DESC LIMIT 1",
            )
            .bind(key)
            .fetch_optional(db)
            .await
        }
    }
}

/// Insert or update a parsed capture and replace its attachment files.
///
/// Files are first written to a fresh staging dir and swapped in for the
/// capture's `files/` dir right before the DB transaction commits; a failure
/// at any step leaves the previous state.
pub async fn ingest(
    db: &Db,
    app_data: &Path,
    mut parsed: ParsedCapture,
    overrides: Overrides,
) -> Result<IngestResult, IngestError> {
    // Serialize upserts so two concurrent posts of the same capture can't both
    // miss the dedup lookup and insert duplicates.
    static INGEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = INGEST_LOCK.lock().await;

    let kind = parsed.kind;
    // An explicit X-Devdy-Title counts as a user-chosen title.
    let explicit_title = overrides
        .title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(title) = &explicit_title {
        parsed.title = title.clone();
    }
    let project_override = overrides
        .project_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if let Some(pid) = project_override.as_deref() {
        let exists: Option<String> = sqlx::query_scalar("SELECT id FROM projects WHERE id = ?")
            .bind(pid)
            .fetch_optional(db)
            .await
            .map_err(internal)?;
        if exists.is_none() {
            return Err(IngestError::BadRequest(format!(
                "unknown project id: {}",
                pid
            )));
        }
    }

    let existing = find_existing(db, &parsed, overrides.force_new)
        .await
        .map_err(internal)?;
    let mut title_custom = explicit_title.is_some();
    let (id, status) = match existing {
        Some((id, current_title, current_custom)) => {
            // A renamed capture keeps its title across re-exports unless the
            // request explicitly sets one.
            if current_custom != 0 && explicit_title.is_none() {
                parsed.title = current_title;
                title_custom = true;
            }
            (id, IngestStatus::Updated)
        }
        None => (Uuid::new_v4().to_string(), IngestStatus::Created),
    };
    let now = chrono::Utc::now().to_rfc3339();

    // Stage attachment files.
    let dir = capture_dir(app_data, kind, &id);
    let staging = dir.join(format!("files.tmp-{}", Uuid::new_v4()));
    let mut taken = HashSet::new();
    let mut staged = Vec::with_capacity(parsed.attachments.len());
    if !parsed.attachments.is_empty() {
        std::fs::create_dir_all(&staging).map_err(internal)?;
    }
    for att in &parsed.attachments {
        let file_name = safe_file_name(&att.name, &mut taken);
        if let Err(e) = std::fs::write(staging.join(&file_name), &att.data) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(internal(e));
        }
        staged.push((att, file_name));
    }

    let p = &parsed;
    let result = async {
        let mut tx = db.begin().await?;
        match status {
            IngestStatus::Created => {
                // New rows go on top of their own kind's list.
                let min_pos: Option<i64> =
                    sqlx::query_scalar("SELECT MIN(position) FROM captures WHERE kind = ?")
                        .bind(kind.as_str())
                        .fetch_one(&mut *tx)
                        .await?;
                sqlx::query(
                    "INSERT INTO captures (id, kind, title, content, title_custom, project_id, \
                     position, created_at, updated_at, exported_at, \
                     workspace, channel, thread_url, thread_ts, \
                     source_url, url_key, site_name, author, published_at, description, selection) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(kind.as_str())
                .bind(&p.title)
                .bind(&p.content)
                .bind(title_custom as i64)
                .bind(&project_override)
                .bind(min_pos.unwrap_or(0) - 1)
                .bind(&now)
                .bind(&now)
                .bind(&p.exported_at)
                .bind(&p.workspace)
                .bind(&p.channel)
                .bind(&p.thread_url)
                .bind(&p.thread_ts)
                .bind(&p.source_url)
                .bind(&p.url_key)
                .bind(&p.site_name)
                .bind(&p.author)
                .bind(&p.published_at)
                .bind(&p.description)
                .bind(p.selection as i64)
                .execute(&mut *tx)
                .await?;
            }
            IngestStatus::Updated => {
                // Keep id/position; keep project_id unless the header overrides it.
                sqlx::query(
                    "UPDATE captures SET title = ?, content = ?, title_custom = ?, \
                     project_id = COALESCE(?, project_id), updated_at = ?, exported_at = ?, \
                     workspace = ?, channel = ?, thread_url = ?, thread_ts = ?, \
                     source_url = ?, url_key = ?, site_name = ?, author = ?, published_at = ?, \
                     description = ?, selection = ? WHERE id = ?",
                )
                .bind(&p.title)
                .bind(&p.content)
                .bind(title_custom as i64)
                .bind(&project_override)
                .bind(&now)
                .bind(&p.exported_at)
                .bind(&p.workspace)
                .bind(&p.channel)
                .bind(&p.thread_url)
                .bind(&p.thread_ts)
                .bind(&p.source_url)
                .bind(&p.url_key)
                .bind(&p.site_name)
                .bind(&p.author)
                .bind(&p.published_at)
                .bind(&p.description)
                .bind(p.selection as i64)
                .bind(&id)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM capture_attachments WHERE capture_id = ?")
                    .bind(&id)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for (att, file_name) in &staged {
            let rel = format!("{}/{}/files/{}", kind.dir_name(), id, file_name);
            sqlx::query(
                "INSERT INTO capture_attachments (id, capture_id, name, file_path, size, mime, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(Uuid::new_v4().to_string())
            .bind(&id)
            .bind(&att.name)
            .bind(rel)
            .bind(att.data.len() as i64)
            .bind(guess_mime(&att.name))
            .bind(&now)
            .execute(&mut *tx)
            .await?;
        }
        Ok::<_, sqlx::Error>(tx)
    }
    .await;

    let tx = match result {
        Ok(tx) => tx,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(internal(e));
        }
    };

    // Swap the staged files in BEFORE committing, so committed attachment rows
    // never point at missing files. The previous `files/` dir is parked as a
    // backup and restored if the swap or the commit fails.
    let files_dir = dir.join("files");
    let backup = dir.join(format!("files.old-{}", Uuid::new_v4()));
    let had_old = files_dir.exists();
    let restore = |remove_new: bool| {
        if remove_new {
            let _ = std::fs::remove_dir_all(&files_dir);
        }
        if had_old {
            let _ = std::fs::rename(&backup, &files_dir);
        }
        let _ = std::fs::remove_dir_all(&staging);
    };
    if had_old {
        if let Err(e) = std::fs::rename(&files_dir, &backup) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(internal(e));
        }
    }
    if !staged.is_empty() {
        if let Err(e) = std::fs::rename(&staging, &files_dir) {
            restore(false);
            return Err(internal(e));
        }
    }
    if let Err(e) = tx.commit().await {
        restore(!staged.is_empty());
        return Err(internal(e));
    }
    if had_old {
        if let Err(e) = std::fs::remove_dir_all(&backup) {
            tracing::warn!(event = "capture_old_files_remove_failed", id = %id, error = %e);
        }
    }

    Ok(IngestResult {
        id,
        status,
        title: parsed.title,
        kind,
    })
}

/// List captures (top-priority first), optionally filtered by kind and project.
pub async fn list(
    db: &Db,
    kind: Option<CaptureKind>,
    project_id: Option<&str>,
) -> Result<Vec<Capture>, sqlx::Error> {
    let mut sql = format!("SELECT {} FROM captures t WHERE 1 = 1", CAPTURE_COLUMNS);
    if kind.is_some() {
        sql.push_str(" AND t.kind = ?");
    }
    if project_id.is_some() {
        sql.push_str(" AND t.project_id = ?");
    }
    sql.push_str(" ORDER BY t.position ASC, t.updated_at DESC");
    let mut q = sqlx::query(&sql);
    if let Some(k) = kind {
        q = q.bind(k.as_str());
    }
    if let Some(pid) = project_id {
        q = q.bind(pid);
    }
    let rows = q.fetch_all(db).await?;
    Ok(rows.iter().map(row_to_capture).collect())
}

pub async fn get_capture(db: &Db, id: &str) -> Result<Capture, String> {
    sqlx::query(&format!(
        "SELECT {} FROM captures t WHERE t.id = ?",
        CAPTURE_COLUMNS
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?
    .map(|r| row_to_capture(&r))
    .ok_or_else(|| not_found(id))
}

pub async fn get_detail(db: &Db, app_data: &Path, id: &str) -> Result<CaptureDetail, String> {
    let capture = get_capture(db, id).await?;
    let rows = sqlx::query(
        "SELECT id, capture_id, name, file_path, size, mime, created_at \
         FROM capture_attachments WHERE capture_id = ? ORDER BY name ASC",
    )
    .bind(id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let attachments = rows
        .iter()
        .map(|row| {
            let rel: String = row.get("file_path");
            CaptureAttachment {
                id: row.get("id"),
                capture_id: row.get("capture_id"),
                name: row.get("name"),
                file_path: app_data.join(rel).to_string_lossy().to_string(),
                size: row.get("size"),
                mime: row.get("mime"),
                created_at: row.get("created_at"),
            }
        })
        .collect();
    Ok(CaptureDetail {
        capture,
        attachments,
    })
}

/// Update title and/or content (fields left `None` are unchanged). A title that
/// differs from the current one marks the capture as user-renamed.
pub async fn update(
    db: &Db,
    id: &str,
    title: Option<String>,
    content: Option<String>,
) -> Result<Capture, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let res = sqlx::query(
        "UPDATE captures SET \
         title_custom = CASE WHEN ?1 IS NOT NULL AND ?1 <> title THEN 1 ELSE title_custom END, \
         title = COALESCE(?1, title), content = COALESCE(?2, content), \
         updated_at = ?3 WHERE id = ?4",
    )
    .bind(title.map(|t| t.trim().to_string()))
    .bind(content)
    .bind(&now)
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    if res.rows_affected() == 0 {
        return Err(not_found(id));
    }
    get_capture(db, id).await
}

/// Max length (in chars) of a user-chosen title.
pub const MAX_TITLE_CHARS: usize = 200;

/// Rename a capture and mark the title as user-chosen, so re-exports keep it.
pub async fn rename(db: &Db, id: &str, title: &str) -> Result<Capture, String> {
    let title = title.trim();
    if title.is_empty() {
        return Err("title is required".into());
    }
    if title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!(
            "title must be at most {} characters",
            MAX_TITLE_CHARS
        ));
    }
    let now = chrono::Utc::now().to_rfc3339();
    let res =
        sqlx::query("UPDATE captures SET title = ?, title_custom = 1, updated_at = ? WHERE id = ?")
            .bind(title)
            .bind(&now)
            .bind(id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    if res.rows_affected() == 0 {
        return Err(not_found(id));
    }
    get_capture(db, id).await
}

/// Kind of each existing id (unknown ids are skipped), in input order.
pub async fn kinds_of(db: &Db, ids: &[String]) -> Result<Vec<(String, CaptureKind)>, String> {
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM captures WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
        if let Some(k) = kind {
            out.push((id.clone(), CaptureKind::parse(&k).unwrap_or_default()));
        }
    }
    Ok(out)
}

/// Link or unlink (`None`) a capture from a project. Returns its kind.
pub async fn set_project(
    db: &Db,
    id: &str,
    project_id: Option<String>,
) -> Result<CaptureKind, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let res = sqlx::query("UPDATE captures SET project_id = ?, updated_at = ? WHERE id = ?")
        .bind(project_id.filter(|p| !p.trim().is_empty()))
        .bind(&now)
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    if res.rows_affected() == 0 {
        return Err(not_found(id));
    }
    Ok(get_capture(db, id).await?.kind)
}

/// Delete captures (attachment rows explicitly, then the capture rows) in one
/// transaction, then remove each capture's directory for its kind. Returns the
/// `(id, kind)` of every row that existed.
pub async fn delete(
    db: &Db,
    app_data: &Path,
    ids: &[String],
) -> Result<Vec<(String, CaptureKind)>, String> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let existing = kinds_of(db, ids).await?;
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;
    for id in ids {
        sqlx::query("DELETE FROM capture_attachments WHERE capture_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM captures WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    for (id, kind) in existing.iter().filter(|(id, _)| is_safe_id(id)) {
        let dir = capture_dir(app_data, *kind, id);
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::warn!(event = "capture_dir_remove_failed", id = %id, error = %e);
            }
        }
    }
    Ok(existing)
}

/// Persist a new order: each id's position becomes its index (same as notes).
/// The FE sends the ids of one kind's list.
pub async fn reorder(db: &Db, ids: &[String]) -> Result<(), String> {
    let mut tx = db.begin().await.map_err(|e| e.to_string())?;
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE captures SET position = ? WHERE id = ?")
            .bind(index as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::ingest::{parse_markdown, parse_zip, tests::make_zip, ParsedCapture};
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn setup(label: &str) -> (Db, PathBuf) {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'proj', '/tmp/p1', '2026-01-01')")
            .execute(&pool)
            .await
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "devdy-slack-{}-{}-{}",
            label,
            std::process::id(),
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        (pool, dir)
    }

    const MD_URL: &str = "---\nchannel: \"#dev\"\nthread_url: \"https://x.slack.com/archives/C1/p1\"\n---\n# First\nbody";

    #[tokio::test]
    async fn create_then_update_by_thread_url_replaces_attachments() {
        let (db, app_data) = setup("upsert").await;
        let z1 = make_zip(&[
            ("t.md", MD_URL.as_bytes()),
            ("files/a.png", b"png-bytes"),
            ("other/a.png", b"second a"),
        ]);
        let r1 = ingest(
            &db,
            &app_data,
            parse_zip(CaptureKind::Slack, &z1).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(r1.status, IngestStatus::Created);
        let d1 = get_detail(&db, &app_data, &r1.id).await.unwrap();
        assert_eq!(d1.capture.attachment_count, 2);
        assert_eq!(d1.capture.title, "First");
        for att in &d1.attachments {
            assert!(Path::new(&att.file_path).is_absolute());
            assert!(Path::new(&att.file_path).exists(), "{}", att.file_path);
        }
        assert_eq!(d1.attachments[0].mime.as_deref(), Some("image/png"));

        // Link to a project, reposition, then re-post the same thread_url.
        set_project(&db, &r1.id, Some("p1".into())).await.unwrap();
        reorder(&db, &[r1.id.clone()]).await.unwrap();
        let z2 = make_zip(&[
            ("t.md", MD_URL.replace("First", "Second").as_bytes()),
            ("log.txt", b"l"),
        ]);
        let r2 = ingest(
            &db,
            &app_data,
            parse_zip(CaptureKind::Slack, &z2).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(r2.status, IngestStatus::Updated);
        assert_eq!(r2.id, r1.id);

        let all = list(&db, None, None).await.unwrap();
        assert_eq!(all.len(), 1);
        let d2 = get_detail(&db, &app_data, &r1.id).await.unwrap();
        assert_eq!(d2.capture.title, "Second");
        assert_eq!(d2.capture.project_id.as_deref(), Some("p1"));
        assert_eq!(d2.capture.position, 0);
        assert_eq!(d2.attachments.len(), 1);
        assert_eq!(d2.attachments[0].name, "log.txt");
        let files: Vec<_> =
            std::fs::read_dir(app_data.join("slack-threads").join(&r1.id).join("files"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect();
        assert_eq!(files, vec!["log.txt"]);
        // No stale staging dirs left behind.
        assert_eq!(
            std::fs::read_dir(app_data.join("slack-threads").join(&r1.id))
                .unwrap()
                .count(),
            1
        );

        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn dedup_by_channel_and_ts_and_new_rows_go_on_top() {
        let (db, app_data) = setup("dedup").await;
        let md = "---\nchannel: general\nthread_ts: \"1.2\"\n---\nhello";
        let a = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, md.as_bytes()).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let b = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, md.as_bytes()).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(a.id, b.id);
        assert_eq!(b.status, IngestStatus::Updated);
        // No dedup keys → always a new row, placed above existing ones.
        let c = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"plain").unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let d = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"plain").unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_ne!(c.id, d.id);
        let ids: Vec<_> = list(&db, None, None)
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![d.id, c.id, a.id]);
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn overrides_and_unknown_project() {
        let (db, app_data) = setup("overrides").await;
        let r = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"# body title").unwrap(),
            Overrides {
                project_id: Some("p1".into()),
                title: Some("Header title".into()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let t = list(&db, None, Some("p1")).await.unwrap();
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].id, r.id);
        assert_eq!(t[0].title, "Header title");
        let err = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"x").unwrap(),
            Overrides {
                project_id: Some("nope".into()),
                title: None,
                ..Default::default()
            },
        )
        .await
        .unwrap_err();
        assert!(matches!(err, IngestError::BadRequest(_)));
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn renamed_title_survives_reexport_unless_header_overrides() {
        let (db, app_data) = setup("rename").await;
        let md = || parse_markdown(CaptureKind::Slack, MD_URL.as_bytes()).unwrap();
        let r = ingest(&db, &app_data, md(), Overrides::default())
            .await
            .unwrap();
        assert!(!get_capture(&db, &r.id).await.unwrap().title_custom);

        // Empty / too long renames are rejected and change nothing.
        assert_eq!(
            rename(&db, &r.id, "   ").await.unwrap_err(),
            "title is required"
        );
        assert!(rename(&db, &r.id, &"x".repeat(MAX_TITLE_CHARS + 1))
            .await
            .is_err());
        assert!(rename(&db, "missing", "x").await.is_err());
        assert!(!get_capture(&db, &r.id).await.unwrap().title_custom);

        let t = rename(&db, &r.id, "  My name  ").await.unwrap();
        assert_eq!(t.title, "My name");
        assert!(t.title_custom);

        // Re-export of the same thread_url keeps the user's title.
        let again = ingest(&db, &app_data, md(), Overrides::default())
            .await
            .unwrap();
        assert_eq!(again.status, IngestStatus::Updated);
        assert_eq!(again.title, "My name");
        let t = get_capture(&db, &r.id).await.unwrap();
        assert_eq!(t.title, "My name");
        assert!(t.title_custom);

        // An explicit X-Devdy-Title wins and stays custom.
        let o = Overrides {
            project_id: None,
            title: Some("From header".into()),
            ..Default::default()
        };
        ingest(&db, &app_data, md(), o).await.unwrap();
        let t = get_capture(&db, &r.id).await.unwrap();
        assert_eq!(t.title, "From header");
        assert!(t.title_custom);

        // New row created with a header title is custom from the start.
        let o = Overrides {
            project_id: None,
            title: Some("Header new".into()),
            ..Default::default()
        };
        let n = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"other").unwrap(),
            o,
        )
        .await
        .unwrap();
        assert!(get_capture(&db, &n.id).await.unwrap().title_custom);
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn update_sets_custom_flag_only_when_title_changes() {
        let (db, app_data) = setup("updflag").await;
        let r = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, b"# Same").unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let t = update(&db, &r.id, Some("Same".into()), Some("new body".into()))
            .await
            .unwrap();
        assert!(!t.title_custom);
        let t = update(&db, &r.id, None, Some("x".into())).await.unwrap();
        assert!(!t.title_custom);
        let t = update(&db, &r.id, Some("Changed".into()), None)
            .await
            .unwrap();
        assert!(t.title_custom);
        assert_eq!(t.title, "Changed");
        let _ = std::fs::remove_dir_all(&app_data);
    }

    /// 0040 applied on a DB that already holds 0038/0039 data (incl.
    /// attachments) through the real sqlx runner, exactly like a dev DB.
    #[tokio::test]
    async fn migration_0040_keeps_existing_slack_data_and_fk() {
        use sqlx::migrate::Migrator;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let full = sqlx::migrate!("./migrations");
        let upto_0039 = Migrator {
            migrations: std::borrow::Cow::Owned(
                full.migrations
                    .iter()
                    .filter(|m| m.version <= 39)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: full.locking,
            no_tx: full.no_tx,
        };
        upto_0039.run(&pool).await.unwrap();
        sqlx::query(
            "INSERT INTO slack_threads (id, title, content, channel, thread_url, position, \
             created_at, updated_at, title_custom) \
             VALUES ('s1', 'Old', 'body', '#dev', 'https://x/p1', -3, '2026-10-08', '2026-10-08', 1)",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO slack_thread_attachments (id, thread_id, name, file_path, size, mime, created_at) \
             VALUES ('a1', 's1', 'a.png', 'slack-threads/s1/files/a.png', 3, 'image/png', '2026-10-08')",
        )
        .execute(&pool)
        .await
        .unwrap();

        full.run(&pool).await.unwrap();

        let c = get_capture(&pool, "s1").await.unwrap();
        assert_eq!(c.kind, CaptureKind::Slack);
        assert_eq!(c.title, "Old");
        assert!(c.title_custom);
        assert_eq!(c.channel.as_deref(), Some("#dev"));
        assert_eq!(c.position, -3);
        assert_eq!(c.attachment_count, 1);
        assert!(!c.selection);
        let d = get_detail(&pool, Path::new("/data"), "s1").await.unwrap();
        assert_eq!(d.attachments[0].capture_id, "s1");
        assert_eq!(
            d.attachments[0].file_path,
            "/data/slack-threads/s1/files/a.png"
        );

        // The attachments FK now references `captures`.
        let fk_table: String = sqlx::query_scalar(
            "SELECT \"table\" FROM pragma_foreign_key_list('capture_attachments')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(fk_table, "captures");
        let sql: String =
            sqlx::query_scalar("SELECT sql FROM sqlite_master WHERE name = 'capture_attachments'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(
            sql.contains("REFERENCES \"captures\"") || sql.contains("REFERENCES captures"),
            "{}",
            sql
        );
        assert!(sql.contains("capture_id"), "{}", sql);
        let old: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('slack_threads', 'slack_thread_attachments')",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(old, 0);
    }

    fn web_md(url: &str, extra: &str) -> ParsedCapture {
        let md = format!("---\nurl: \"{}\"\n{}---\n# Page\nbody", url, extra);
        parse_markdown(CaptureKind::Web, md.as_bytes()).unwrap()
    }

    #[tokio::test]
    async fn web_dedup_matrix() {
        let (db, app_data) = setup("webdedup").await;
        let url = "https://v2.tauri.app/security/capabilities/";
        let full = ingest(&db, &app_data, web_md(url, ""), Overrides::default())
            .await
            .unwrap();
        assert_eq!(full.status, IngestStatus::Created);
        assert_eq!(full.kind, CaptureKind::Web);

        // Rename, then re-post with tracking noise → same row, title kept.
        rename(&db, &full.id, "Mine").await.unwrap();
        let noisy = format!(
            "{}?utm_source=x#frag",
            url.to_uppercase()
                .replace("/SECURITY/CAPABILITIES/", "/security/capabilities/")
        );
        let again = ingest(&db, &app_data, web_md(&noisy, ""), Overrides::default())
            .await
            .unwrap();
        assert_eq!(again.status, IngestStatus::Updated);
        assert_eq!(again.id, full.id);
        let c = get_capture(&db, &full.id).await.unwrap();
        assert_eq!(c.title, "Mine");
        assert_eq!(c.source_url.as_deref(), Some(noisy.as_str()));

        // selection: true twice → two new rows; full-page row untouched.
        let before = get_capture(&db, &full.id).await.unwrap();
        let s1 = ingest(
            &db,
            &app_data,
            web_md(url, "selection: true\n"),
            Overrides::default(),
        )
        .await
        .unwrap();
        let s2 = ingest(
            &db,
            &app_data,
            web_md(url, "selection: true\n"),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(s1.status, IngestStatus::Created);
        assert_eq!(s2.status, IngestStatus::Created);
        assert_ne!(s1.id, s2.id);
        assert!(get_capture(&db, &s1.id).await.unwrap().selection);
        let after = get_capture(&db, &full.id).await.unwrap();
        assert_eq!(after.updated_at, before.updated_at);
        assert_eq!(after.content, before.content);

        // A later full-page post still updates the full-page row, never a selection row.
        let again = ingest(&db, &app_data, web_md(url, ""), Overrides::default())
            .await
            .unwrap();
        assert_eq!(again.id, full.id);

        // X-Devdy-Mode: new → new row.
        let forced = ingest(
            &db,
            &app_data,
            web_md(url, ""),
            Overrides {
                force_new: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(forced.status, IngestStatus::Created);
        assert_ne!(forced.id, full.id);

        // No url → always create.
        let n1 = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Web, b"# a").unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let n2 = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Web, b"# a").unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_ne!(n1.id, n2.id);

        // Different query param → different page.
        let other = ingest(
            &db,
            &app_data,
            web_md(&format!("{}?page=2", url), ""),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(other.status, IngestStatus::Created);

        // Kinds never dedup against each other: a slack thread with the same
        // URL as thread_url is a separate row, and lists are per kind.
        let slack_md = format!("---\nthread_url: \"{}\"\n---\nx", url);
        let sl = ingest(
            &db,
            &app_data,
            parse_markdown(CaptureKind::Slack, slack_md.as_bytes()).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        assert_eq!(sl.status, IngestStatus::Created);
        assert_eq!(
            list(&db, Some(CaptureKind::Slack), None)
                .await
                .unwrap()
                .len(),
            1
        );
        let webs = list(&db, Some(CaptureKind::Web), None).await.unwrap();
        assert_eq!(webs.len(), 7);
        // Per-kind positions: the slack row sits at -1 like a fresh list.
        assert_eq!(get_capture(&db, &sl.id).await.unwrap().position, -1);
        assert_eq!(webs[0].id, other.id);
        assert_eq!(list(&db, None, None).await.unwrap().len(), 8);
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn web_dedup_targets_most_recently_updated_full_page_row() {
        let (db, app_data) = setup("weblatest").await;
        let url = "https://example.com/doc";
        let forced = || Overrides {
            force_new: true,
            ..Default::default()
        };
        let a = ingest(&db, &app_data, web_md(url, ""), forced()).await.unwrap();
        let b = ingest(&db, &app_data, web_md(url, ""), forced()).await.unwrap();
        let c = ingest(&db, &app_data, web_md(url, ""), forced()).await.unwrap();
        // Make the oldest row the most recently updated one.
        sqlx::query("UPDATE captures SET updated_at = '2099-01-01T00:00:00Z' WHERE id = ?")
            .bind(&a.id)
            .execute(&db)
            .await
            .unwrap();
        let r = ingest(&db, &app_data, web_md(url, ""), Overrides::default()).await.unwrap();
        assert_eq!(r.status, IngestStatus::Updated);
        assert_eq!(r.id, a.id);
        // Selection rows never win, even if updated later.
        let s = ingest(&db, &app_data, web_md(url, "selection: true\n"), Overrides::default())
            .await
            .unwrap();
        sqlx::query("UPDATE captures SET updated_at = '2100-01-01T00:00:00Z' WHERE id IN (?, ?)")
            .bind(&s.id)
            .bind(&c.id)
            .execute(&db)
            .await
            .unwrap();
        let r = ingest(&db, &app_data, web_md(url, ""), Overrides::default()).await.unwrap();
        assert_eq!(r.id, c.id);
        assert_ne!(r.id, b.id);
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn web_files_live_under_web_pages_and_delete_removes_them() {
        let (db, app_data) = setup("webfiles").await;
        let md = "---\nurl: https://example.com/a\n---\n# A";
        let z = make_zip(&[("page.md", md.as_bytes()), ("images/x.png", b"png")]);
        let r = ingest(
            &db,
            &app_data,
            parse_zip(CaptureKind::Web, &z).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let d = get_detail(&db, &app_data, &r.id).await.unwrap();
        let expected = app_data
            .join("web-pages")
            .join(&r.id)
            .join("files")
            .join("x.png");
        assert_eq!(d.attachments[0].file_path, expected.to_string_lossy());
        assert!(expected.exists());
        assert!(!app_data.join("slack-threads").join(&r.id).exists());
        let deleted = delete(&db, &app_data, &[r.id.clone(), "missing".into()])
            .await
            .unwrap();
        assert_eq!(deleted, vec![(r.id.clone(), CaptureKind::Web)]);
        assert!(!app_data.join("web-pages").join(&r.id).exists());
        let _ = std::fs::remove_dir_all(&app_data);
    }

    #[tokio::test]
    async fn update_and_delete_remove_rows_and_files() {
        let (db, app_data) = setup("delete").await;
        let z = make_zip(&[("t.md", b"# T"), ("a.png", b"x")]);
        let r = ingest(
            &db,
            &app_data,
            parse_zip(CaptureKind::Slack, &z).unwrap(),
            Overrides::default(),
        )
        .await
        .unwrap();
        let updated = update(&db, &r.id, Some(" New ".into()), None)
            .await
            .unwrap();
        assert_eq!(updated.title, "New");
        assert_eq!(updated.content, "# T");
        assert!(update(&db, "missing", None, Some("x".into()))
            .await
            .is_err());

        let dir = app_data.join("slack-threads").join(&r.id);
        assert!(dir.exists());
        delete(&db, &app_data, &[r.id.clone()]).await.unwrap();
        assert!(!dir.exists());
        assert!(list(&db, None, None).await.unwrap().is_empty());
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM capture_attachments")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_dir_all(&app_data);
    }
}
