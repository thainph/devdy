//! SQL over the `slack_*` tables: add / re-import (dedup by channel + thread
//! ts), list, view, rename, project, reorder, delete, composer prompt text.
//! Session links live in `run_links`.
//! Takes plain `&Db` + `app_data` so it is testable against an in-memory DB.

use super::link::{parse_thread_link, ThreadLink};
use super::mrkdwn::{self, Names, TextMessage};
use super::CodedError;
use crate::db::Db;
use serde::Serialize;
use sqlx::sqlite::SqliteRow;
use sqlx::Row;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use uuid::Uuid;

pub const DEFAULT_TITLE: &str = "Slack thread";
pub const MAX_TITLE_CHARS: usize = 200;
const PREVIEW_MAX_CHARS: usize = 140;
/// Root of per-thread dirs under `<app_data>`.
pub const THREADS_DIR: &str = "slack-threads";

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackThreadSummary {
    pub id: String,
    pub title: String,
    pub title_custom: bool,
    pub project_id: Option<String>,
    pub position: i64,
    pub created_at: String,
    pub updated_at: String,
    pub workspace: Option<String>,
    pub workspace_domain: Option<String>,
    pub team_id: Option<String>,
    pub channel: Option<String>,
    pub channel_id: Option<String>,
    pub thread_url: Option<String>,
    pub thread_ts: Option<String>,
    pub fetch_status: String,
    pub fetch_error_code: Option<String>,
    pub fetch_error: Option<String>,
    pub fetched_at: Option<String>,
    pub reply_count: i64,
    pub last_reply_ts: Option<String>,
    pub last_reply_at: Option<String>,
    pub new_count: i64,
    pub warnings: Vec<String>,
    pub legacy: bool,
    pub file_count: i64,
    pub preview: String,
    /// Linked sessions whose run still exists.
    pub linked_run_count: i64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackUser {
    pub user_id: String,
    pub display_name: String,
    pub real_name: Option<String>,
    pub avatar_path: Option<String>,
    pub is_bot: bool,
}

#[derive(Debug, Serialize, serde::Deserialize, Clone, PartialEq, Eq)]
pub struct SlackReaction {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackFile {
    pub id: String,
    pub slack_file_id: Option<String>,
    pub message_ts: Option<String>,
    pub name: String,
    pub title: Option<String>,
    pub mime: Option<String>,
    pub filetype: Option<String>,
    pub size: i64,
    pub file_path: Option<String>,
    pub download_status: String,
    pub download_error: Option<String>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub permalink: Option<String>,
    pub is_image: bool,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackMessage {
    pub ts: String,
    pub seq: i64,
    pub user_id: Option<String>,
    pub bot_id: Option<String>,
    pub author_name: String,
    pub avatar_path: Option<String>,
    pub subtype: Option<String>,
    pub text_raw: String,
    pub text_vi: Option<String>,
    pub translate_status: String,
    pub translate_error: Option<String>,
    pub edited_ts: Option<String>,
    pub posted_at: String,
    pub reactions: Vec<SlackReaction>,
    pub file_ids: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackThreadView {
    #[serde(flatten)]
    pub summary: SlackThreadSummary,
    pub messages: Vec<SlackMessage>,
    pub users: BTreeMap<String, SlackUser>,
    pub channels: BTreeMap<String, String>,
    pub files: Vec<SlackFile>,
    pub legacy_content: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AddSlackThreadResult {
    pub id: String,
    /// `created` | `existing`
    pub status: &'static str,
    pub fetch_status: String,
    /// Set when the row was put straight into `error` (e.g. `no_token`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
pub struct SlackPromptText {
    pub text: String,
    pub truncated: bool,
    pub chars: usize,
}

pub fn internal(e: impl std::fmt::Display) -> CodedError {
    CodedError::new("internal", e.to_string())
}

pub fn not_found(id: &str) -> CodedError {
    CodedError::new("not_found", format!("Slack thread not found: {id}"))
}

/// Thread ids are UUIDs (or legacy capture UUIDs); refuse anything else before
/// it is joined into a filesystem path.
pub fn is_safe_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// `<app_data>/slack-threads/<id>`.
pub fn thread_dir(app_data: &Path, id: &str) -> PathBuf {
    app_data.join(THREADS_DIR).join(id)
}

/// Remove a thread's dir (safe ids only); missing dir is fine.
pub fn remove_thread_dir(app_data: &Path, id: &str) {
    if !is_safe_id(id) {
        return;
    }
    let dir = thread_dir(app_data, id);
    if dir.exists() {
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            tracing::warn!(event = "slack_thread_dir_remove_failed", id = %id, error = %e);
        }
    }
}

/// Trimmed title of 1–200 chars.
pub fn validate_title(title: &str) -> Result<String, CodedError> {
    let t = title.trim();
    if t.is_empty() || t.chars().count() > MAX_TITLE_CHARS {
        return Err(CodedError::new(
            "invalid_title",
            format!("title must be 1-{MAX_TITLE_CHARS} characters"),
        ));
    }
    Ok(t.to_string())
}

/// `Ok(())` when `project_id` names an existing project.
pub async fn check_project(db: &Db, project_id: &str) -> Result<(), CodedError> {
    let exists: Option<String> = sqlx::query_scalar("SELECT id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(internal)?;
    match exists {
        Some(_) => Ok(()),
        None => Err(CodedError::new(
            "invalid_project",
            format!("unknown project id: {project_id}"),
        )),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Row with the same `(channel_id, thread_ts)`, or a legacy row without a key
/// whose `thread_url` parses to it.
pub async fn find_by_key(
    db: &Db,
    channel_id: &str,
    thread_ts: &str,
    exclude: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    let direct: Option<String> = sqlx::query_scalar(
        "SELECT id FROM slack_threads WHERE channel_id = ? AND thread_ts = ? AND id <> ?",
    )
    .bind(channel_id)
    .bind(thread_ts)
    .bind(exclude.unwrap_or(""))
    .fetch_optional(db)
    .await?;
    if direct.is_some() {
        return Ok(direct);
    }
    let legacy: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT id, thread_url FROM slack_threads WHERE channel_id IS NULL AND id <> ? \
         ORDER BY updated_at DESC",
    )
    .bind(exclude.unwrap_or(""))
    .fetch_all(db)
    .await?;
    Ok(legacy.into_iter().find_map(|(id, url)| {
        let l = parse_thread_link(url.as_deref()?).ok()?;
        (l.channel_id == channel_id && l.thread_ts == thread_ts).then_some(id)
    }))
}

/// What `add_or_requeue` decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddOutcome {
    pub result: AddSlackThreadResult,
    /// Queue a job (or a re-run when one is in flight).
    pub enqueue: bool,
}

/// Add a thread by link or re-import an existing one (SRS §12): an existing
/// row keeps id/position, keeps a custom title (else takes `title`), gets
/// `project_id` only when it has none, and goes back to `pending` — unless a
/// job is running for it (`is_running`), then the caller flags a re-run.
/// Without any token the row goes straight to `error/no_token`.
pub async fn add_or_requeue(
    db: &Db,
    link: &ThreadLink,
    project_id: Option<&str>,
    title: Option<&str>,
    has_token: bool,
    is_running: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Result<AddOutcome, CodedError> {
    static ADD_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _guard = ADD_LOCK.lock().await;
    let ts = now();
    let existing = find_by_key(db, &link.channel_id, &link.thread_ts, None)
        .await
        .map_err(internal)?;
    let (status, error_code) = if has_token {
        ("pending", None)
    } else {
        ("error", Some("no_token".to_string()))
    };
    match existing {
        Some(id) => {
            let running = is_running(&id);
            sqlx::query(
                "UPDATE slack_threads SET \
                 title = CASE WHEN title_custom = 0 AND ?1 IS NOT NULL THEN ?1 ELSE title END, \
                 title_custom = CASE WHEN title_custom = 0 AND ?1 IS NOT NULL THEN 1 ELSE title_custom END, \
                 project_id = COALESCE(project_id, ?2), updated_at = ?3 WHERE id = ?4",
            )
            .bind(title)
            .bind(project_id)
            .bind(&ts)
            .bind(&id)
            .execute(db)
            .await
            .map_err(internal)?;
            if !running {
                set_requested_status(db, &id, status, error_code.as_deref())
                    .await
                    .map_err(internal)?;
            }
            let fetch_status: String =
                sqlx::query_scalar("SELECT fetch_status FROM slack_threads WHERE id = ?")
                    .bind(&id)
                    .fetch_one(db)
                    .await
                    .map_err(internal)?;
            Ok(AddOutcome {
                enqueue: running || has_token,
                result: AddSlackThreadResult {
                    id,
                    status: "existing",
                    fetch_status,
                    error_code: if running { None } else { error_code },
                },
            })
        }
        None => {
            let id = Uuid::new_v4().to_string();
            let min_pos: Option<i64> =
                sqlx::query_scalar("SELECT MIN(position) FROM slack_threads")
                    .fetch_one(db)
                    .await
                    .map_err(internal)?;
            sqlx::query(
                "INSERT INTO slack_threads (id, title, title_custom, project_id, position, \
                 created_at, updated_at, workspace_domain, channel_id, thread_ts, thread_url, \
                 fetch_status, fetch_error_code, fetch_error) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(title.unwrap_or(DEFAULT_TITLE))
            .bind(title.is_some() as i64)
            .bind(project_id)
            .bind(min_pos.unwrap_or(0) - 1)
            .bind(&ts)
            .bind(&ts)
            .bind(&link.host)
            .bind(&link.channel_id)
            .bind(&link.thread_ts)
            .bind(&link.canonical_url)
            .bind(status)
            .bind(&error_code)
            .bind(error_code.as_ref().map(|_| "no Slack token configured"))
            .execute(db)
            .await
            .map_err(internal)?;
            Ok(AddOutcome {
                enqueue: has_token,
                result: AddSlackThreadResult {
                    id,
                    status: "created",
                    fetch_status: status.to_string(),
                    error_code,
                },
            })
        }
    }
}

/// `pending` (errors cleared) or `error/<code>`.
pub async fn set_requested_status(
    db: &Db,
    id: &str,
    status: &str,
    error_code: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let detail = error_code.map(|c| match c {
        "no_token" => "no Slack token configured".to_string(),
        other => other.to_string(),
    });
    Ok(sqlx::query(
        "UPDATE slack_threads SET fetch_status = ?, fetch_error_code = ?, fetch_error = ? \
         WHERE id = ?",
    )
    .bind(status)
    .bind(error_code)
    .bind(detail)
    .bind(id)
    .execute(db)
    .await?
    .rows_affected())
}

const SUMMARY_SQL: &str = "SELECT t.*, \
     (SELECT COUNT(*) FROM slack_files f WHERE f.thread_id = t.id) AS file_count, \
     (SELECT COUNT(*) FROM slack_thread_runs l JOIN runs r ON r.id = l.run_id \
        WHERE l.thread_id = t.id) AS linked_run_count, \
     (SELECT COUNT(*) FROM slack_messages m WHERE m.thread_id = t.id \
        AND t.last_seen_ts IS NOT NULL AND m.ts > t.last_seen_ts) AS new_count, \
     (SELECT COALESCE(m.text_vi, m.text_raw) FROM slack_messages m \
        WHERE m.thread_id = t.id ORDER BY m.seq ASC LIMIT 1) AS parent_text \
     FROM slack_threads t";

/// Mention names per team: user display names + channel names.
pub async fn load_names(db: &Db) -> Result<HashMap<String, Names>, sqlx::Error> {
    let mut out: HashMap<String, Names> = HashMap::new();
    let users: Vec<(String, String, Option<String>, Option<String>)> =
        sqlx::query_as("SELECT team_id, user_id, display_name, real_name FROM slack_users")
            .fetch_all(db)
            .await?;
    for (team, user, display, real) in users {
        let name = display.or(real).unwrap_or_else(|| user.clone());
        out.entry(team).or_default().users.insert(user, name);
    }
    let channels: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT team_id, channel_id, name FROM slack_channels WHERE name IS NOT NULL",
    )
    .fetch_all(db)
    .await?;
    for (team, ch, name) in channels {
        if let Some(name) = name {
            out.entry(team).or_default().channels.insert(ch, name);
        }
    }
    Ok(out)
}

fn names_for<'a>(
    all: &'a HashMap<String, Names>,
    team: Option<&str>,
) -> std::borrow::Cow<'a, Names> {
    match team.and_then(|t| all.get(t)) {
        Some(n) => std::borrow::Cow::Borrowed(n),
        None => std::borrow::Cow::Owned(Names::default()),
    }
}

fn row_to_summary(row: &SqliteRow, names: &HashMap<String, Names>) -> SlackThreadSummary {
    let team_id: Option<String> = row.get("team_id");
    let last_reply_ts: Option<String> = row.get("last_reply_ts");
    let parent: Option<String> = row.get("parent_text");
    let legacy_content: Option<String> = row.get("legacy_content");
    let preview = match parent {
        Some(p) => {
            let n = names_for(names, team_id.as_deref());
            mrkdwn::first_line(
                &mrkdwn::to_plain(&p, &n, mrkdwn::Links::Label),
                PREVIEW_MAX_CHARS,
            )
        }
        None => legacy_content
            .as_deref()
            .map(|c| {
                let lines = c.lines().map(|l| l.trim().trim_start_matches('#'));
                mrkdwn::first_line(&lines.collect::<Vec<_>>().join("\n"), PREVIEW_MAX_CHARS)
            })
            .unwrap_or_default(),
    };
    let warnings: String = row.get("warnings");
    SlackThreadSummary {
        id: row.get("id"),
        title: row.get("title"),
        title_custom: row.get::<i64, _>("title_custom") != 0,
        project_id: row.get("project_id"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        workspace: row.get("workspace"),
        workspace_domain: row.get("workspace_domain"),
        team_id,
        channel: row.get("channel"),
        channel_id: row.get("channel_id"),
        thread_url: row.get("thread_url"),
        thread_ts: row.get("thread_ts"),
        fetch_status: row.get("fetch_status"),
        fetch_error_code: row.get("fetch_error_code"),
        fetch_error: row.get("fetch_error"),
        fetched_at: row.get("fetched_at"),
        reply_count: row.get("reply_count"),
        last_reply_at: last_reply_ts.as_deref().and_then(mrkdwn::ts_to_iso),
        last_reply_ts,
        new_count: row.get("new_count"),
        warnings: serde_json::from_str(&warnings).unwrap_or_default(),
        legacy: row.get::<i64, _>("legacy") != 0,
        file_count: row.get("file_count"),
        preview,
        linked_run_count: row.get("linked_run_count"),
    }
}

/// Unicode-aware, case-insensitive substring match (SQLite `LIKE`/`lower` only
/// fold ASCII, which would miss "TRIỂN" vs "triển").
pub fn matches_query(fields: &[Option<&str>], query: &str) -> bool {
    let q = query.trim().to_lowercase();
    q.is_empty()
        || fields
            .iter()
            .flatten()
            .any(|f| f.to_lowercase().contains(&q))
}

/// Threads ordered by `position ASC, updated_at DESC`, optionally filtered by
/// project and a free-text query (title, channel, workspace, transcript,
/// legacy markdown).
pub async fn list(
    db: &Db,
    project_id: Option<&str>,
    query: Option<&str>,
) -> Result<Vec<SlackThreadSummary>, CodedError> {
    let mut sql = format!("{SUMMARY_SQL} WHERE 1 = 1");
    if project_id.is_some() {
        sql.push_str(" AND t.project_id = ?");
    }
    sql.push_str(" ORDER BY t.position ASC, t.updated_at DESC");
    let mut q = sqlx::query(&sql);
    if let Some(pid) = project_id {
        q = q.bind(pid);
    }
    let rows = q.fetch_all(db).await.map_err(internal)?;
    let names = load_names(db).await.map_err(internal)?;
    let query = query.map(str::trim).filter(|q| !q.is_empty());
    Ok(rows
        .iter()
        .filter(|row| match query {
            None => true,
            Some(q) => matches_query(
                &[
                    row.get::<Option<&str>, _>("title"),
                    row.get::<Option<&str>, _>("channel"),
                    row.get::<Option<&str>, _>("workspace"),
                    row.get::<Option<&str>, _>("search_text"),
                    row.get::<Option<&str>, _>("legacy_content"),
                ],
                q,
            ),
        })
        .map(|row| row_to_summary(row, &names))
        .collect())
}

pub async fn get_summary(db: &Db, id: &str) -> Result<SlackThreadSummary, CodedError> {
    let row = sqlx::query(&format!("{SUMMARY_SQL} WHERE t.id = ?"))
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(internal)?
        .ok_or_else(|| not_found(id))?;
    let names = load_names(db).await.map_err(internal)?;
    Ok(row_to_summary(&row, &names))
}

/// `slack-cache/avatars/<team>/<user>-<sha256(url)[0..8]>.<ext>` (relative).
pub fn avatar_rel_path(team_id: &str, user_id: &str, url: &str) -> Option<String> {
    use sha2::{Digest, Sha256};
    if !is_safe_id(team_id) || !is_safe_id(user_id) {
        return None;
    }
    let hash = hex::encode(Sha256::digest(url.as_bytes()));
    let ext = reqwest::Url::parse(url)
        .ok()
        .and_then(|u| {
            u.path()
                .rsplit('.')
                .next()
                .map(|e| e.to_ascii_lowercase())
                .filter(|e| matches!(e.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp"))
        })
        .unwrap_or_else(|| "png".to_string());
    Some(format!(
        "slack-cache/avatars/{}/{}-{}.{}",
        team_id,
        user_id,
        &hash[..8],
        ext
    ))
}

fn abs(app_data: &Path, rel: Option<String>) -> Option<String> {
    rel.map(|r| app_data.join(r).to_string_lossy().to_string())
}

fn row_to_file(app_data: &Path, row: &SqliteRow) -> SlackFile {
    let status: String = row.get("download_status");
    let mime: Option<String> = row.get("mime");
    let done = status == "done";
    let path: Option<String> = row.get("file_path");
    SlackFile {
        id: row.get("id"),
        slack_file_id: row.get("slack_file_id"),
        message_ts: row.get("message_ts"),
        name: row.get("name"),
        title: row.get("title"),
        is_image: done && mime.as_deref().is_some_and(|m| m.starts_with("image/")),
        mime,
        filetype: row.get("filetype"),
        size: row.get("size"),
        file_path: if done { abs(app_data, path) } else { None },
        download_status: status,
        download_error: row.get("download_error"),
        width: row.get("width"),
        height: row.get("height"),
        permalink: row.get("permalink"),
    }
}

/// Files of a thread: Slack files in message order, then legacy files.
pub async fn files_of(db: &Db, app_data: &Path, id: &str) -> Result<Vec<SlackFile>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT * FROM slack_files WHERE thread_id = ? \
         ORDER BY (slack_file_id IS NULL), message_ts, created_at, name",
    )
    .bind(id)
    .fetch_all(db)
    .await?;
    Ok(rows.iter().map(|r| row_to_file(app_data, r)).collect())
}

/// `(user_id, display_name, real_name, avatar_path, is_bot)`
type UserRow = (String, Option<String>, Option<String>, Option<String>, i64);

/// Full view of a thread for the Slack window.
pub async fn get_view(db: &Db, app_data: &Path, id: &str) -> Result<SlackThreadView, CodedError> {
    let summary = get_summary(db, id).await?;
    let team = summary.team_id.clone().unwrap_or_default();
    let legacy_content: Option<String> = if summary.legacy {
        sqlx::query_scalar("SELECT legacy_content FROM slack_threads WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(internal)?
    } else {
        None
    };
    let rows = sqlx::query("SELECT * FROM slack_messages WHERE thread_id = ? ORDER BY seq ASC")
        .bind(id)
        .fetch_all(db)
        .await
        .map_err(internal)?;

    // Users referenced by messages and mentions.
    let user_rows: Vec<UserRow> = sqlx::query_as(
        "SELECT user_id, display_name, real_name, avatar_path, is_bot FROM slack_users \
             WHERE team_id = ?",
    )
    .bind(&team)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let known: HashMap<String, SlackUser> = user_rows
        .into_iter()
        .map(|(uid, display, real, avatar, bot)| {
            let display_name = display
                .clone()
                .or_else(|| real.clone())
                .unwrap_or_else(|| uid.clone());
            (
                uid.clone(),
                SlackUser {
                    user_id: uid,
                    display_name,
                    real_name: real,
                    avatar_path: abs(app_data, avatar),
                    is_bot: bot != 0,
                },
            )
        })
        .collect();
    let mut users: BTreeMap<String, SlackUser> = BTreeMap::new();
    let mut want_user = |uid: &str| {
        if !users.contains_key(uid) {
            let u = known.get(uid).cloned().unwrap_or_else(|| SlackUser {
                user_id: uid.to_string(),
                display_name: uid.to_string(),
                real_name: None,
                avatar_path: None,
                is_bot: false,
            });
            users.insert(uid.to_string(), u);
        }
    };

    let mut messages = Vec::with_capacity(rows.len());
    for row in &rows {
        let user_id: Option<String> = row.get("user_id");
        let bot_id: Option<String> = row.get("bot_id");
        let text_raw: String = row.get("text_raw");
        let text_vi: Option<String> = row.get("text_vi");
        if let Some(u) = &user_id {
            want_user(u);
        }
        for u in mrkdwn::mentioned_users(&text_raw).into_iter().chain(
            text_vi
                .as_deref()
                .map(mrkdwn::mentioned_users)
                .unwrap_or_default(),
        ) {
            want_user(&u);
        }
        let author_name: Option<String> = row.get("author_name");
        let icon: Option<String> = row.get("author_icon_url");
        let user = user_id.as_deref().and_then(|u| known.get(u));
        let bot_avatar = match (&bot_id, &icon, user) {
            (Some(b), Some(url), None) => avatar_rel_path(&team, b, url)
                .map(|rel| app_data.join(rel))
                .filter(|p| p.exists())
                .map(|p| p.to_string_lossy().to_string()),
            _ => None,
        };
        let ts: String = row.get("ts");
        let reactions: String = row.get("reactions");
        let file_ids: String = row.get("file_ids");
        messages.push(SlackMessage {
            posted_at: mrkdwn::ts_to_iso(&ts).unwrap_or_default(),
            author_name: user
                .map(|u| u.display_name.clone())
                .or(author_name)
                .or_else(|| user_id.clone())
                .or_else(|| bot_id.clone())
                .unwrap_or_else(|| "Unknown".to_string()),
            avatar_path: user.and_then(|u| u.avatar_path.clone()).or(bot_avatar),
            ts,
            seq: row.get("seq"),
            user_id,
            bot_id,
            subtype: row.get("subtype"),
            text_raw,
            text_vi,
            translate_status: row.get("translate_status"),
            translate_error: row.get("translate_error"),
            edited_ts: row.get("edited_ts"),
            reactions: serde_json::from_str(&reactions).unwrap_or_default(),
            file_ids: serde_json::from_str(&file_ids).unwrap_or_default(),
        });
    }

    let channel_rows: Vec<(String, Option<String>)> =
        sqlx::query_as("SELECT channel_id, name FROM slack_channels WHERE team_id = ?")
            .bind(&team)
            .fetch_all(db)
            .await
            .map_err(internal)?;
    let mut channels: BTreeMap<String, String> = channel_rows
        .into_iter()
        .filter_map(|(id, name)| Some((id, name?)))
        .collect();
    if let (Some(cid), Some(name)) = (&summary.channel_id, &summary.channel) {
        channels.insert(cid.clone(), name.clone());
    }
    let files = files_of(db, app_data, id).await.map_err(internal)?;
    Ok(SlackThreadView {
        summary,
        messages,
        users,
        channels,
        files,
        legacy_content,
    })
}

/// `last_seen_ts = last_reply_ts` (no `updated_at` bump).
pub async fn mark_seen(db: &Db, id: &str) -> Result<(), CodedError> {
    let r = sqlx::query("UPDATE slack_threads SET last_seen_ts = last_reply_ts WHERE id = ?")
        .bind(id)
        .execute(db)
        .await
        .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(not_found(id));
    }
    Ok(())
}

pub async fn rename(db: &Db, id: &str, title: &str) -> Result<SlackThreadSummary, CodedError> {
    let title = validate_title(title)?;
    let r = sqlx::query(
        "UPDATE slack_threads SET title = ?, title_custom = 1, updated_at = ? WHERE id = ?",
    )
    .bind(&title)
    .bind(now())
    .bind(id)
    .execute(db)
    .await
    .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(not_found(id));
    }
    get_summary(db, id).await
}

pub async fn set_project(db: &Db, id: &str, project_id: Option<&str>) -> Result<(), CodedError> {
    let project_id = project_id.map(str::trim).filter(|p| !p.is_empty());
    if let Some(pid) = project_id {
        check_project(db, pid).await?;
    }
    let r = sqlx::query("UPDATE slack_threads SET project_id = ?, updated_at = ? WHERE id = ?")
        .bind(project_id)
        .bind(now())
        .bind(id)
        .execute(db)
        .await
        .map_err(internal)?;
    if r.rows_affected() == 0 {
        return Err(not_found(id));
    }
    Ok(())
}

/// Each id's position becomes its index (same as notes).
pub async fn reorder(db: &Db, ids: &[String]) -> Result<(), CodedError> {
    let mut tx = db.begin().await.map_err(internal)?;
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE slack_threads SET position = ? WHERE id = ?")
            .bind(index as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
    }
    tx.commit().await.map_err(internal)
}

/// Delete threads: children (incl. session links) explicitly, then the rows
/// (one transaction), then each thread dir. The avatar cache is shared and
/// kept. Returns the ids that existed.
pub async fn delete(db: &Db, app_data: &Path, ids: &[String]) -> Result<Vec<String>, CodedError> {
    let mut existed = Vec::new();
    let mut tx = db.begin().await.map_err(internal)?;
    for id in ids {
        sqlx::query("DELETE FROM slack_messages WHERE thread_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        sqlx::query("DELETE FROM slack_files WHERE thread_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        sqlx::query("DELETE FROM slack_thread_runs WHERE thread_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        let r = sqlx::query("DELETE FROM slack_threads WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?;
        if r.rows_affected() > 0 {
            existed.push(id.clone());
        }
    }
    tx.commit().await.map_err(internal)?;
    for id in ids {
        remove_thread_dir(app_data, id);
    }
    Ok(existed)
}

/// Composer transcript of a thread (SRS §13.9).
pub async fn prompt_text(
    db: &Db,
    app_data: &Path,
    id: &str,
    include_original: bool,
) -> Result<SlackPromptText, CodedError> {
    let view = get_view(db, app_data, id).await?;
    let names = Names {
        users: view
            .users
            .iter()
            .map(|(k, u)| (k.clone(), u.display_name.clone()))
            .collect(),
        channels: view.channels.clone().into_iter().collect(),
    };
    let file_line = |f: &SlackFile| match &f.file_path {
        Some(p) => format!("{} — {}", f.name, p),
        None => format!(
            "{} — not downloaded: {}",
            f.name,
            f.permalink.as_deref().unwrap_or("")
        ),
    };
    let by_id: HashMap<&str, &SlackFile> = view.files.iter().map(|f| (f.id.as_str(), f)).collect();
    let msgs: Vec<TextMessage> = view
        .messages
        .iter()
        .map(|m| TextMessage {
            author: &m.author_name,
            ts: &m.ts,
            text_raw: &m.text_raw,
            text_vi: m.text_vi.as_deref(),
            files: m
                .file_ids
                .iter()
                .filter_map(|fid| by_id.get(fid.as_str()).map(|f| file_line(f)))
                .collect(),
        })
        .collect();
    let legacy_files: Vec<String> = view
        .files
        .iter()
        .filter(|f| f.slack_file_id.is_none())
        .map(file_line)
        .collect();
    let s = &view.summary;
    let header = mrkdwn::PromptHeader {
        id: &s.id,
        title: &s.title,
        channel: s.channel.as_deref().or(s.channel_id.as_deref()),
        workspace: s.workspace.as_deref().or(s.workspace_domain.as_deref()),
        thread_url: s.thread_url.as_deref(),
        fetch_status: &s.fetch_status,
        legacy_content: view.legacy_content.as_deref(),
    };
    let (text, truncated) = mrkdwn::prompt_text(
        &header,
        &msgs,
        &legacy_files,
        &names,
        include_original,
        mrkdwn::PROMPT_TEXT_MAX_CHARS,
    );
    let chars = text.chars().count();
    Ok(SlackPromptText {
        text,
        truncated,
        chars,
    })
}
