//! One fetch + translate job for a thread (SRS §6, §7.3, §8):
//! `pending → fetching → translating → ready | error`.
//!
//! Every DB write checks `rows_affected`: a thread deleted mid-way stops the
//! job (and its dir is removed again in case a download re-created it).

use super::api::{AuthInfo, SlackClient, SlackError};
use super::files::{self, parse_remote_file, RemoteFile};
use super::link::{canonical_url, parse_thread_link, ThreadLink};
use super::mrkdwn::{self, Names, TextMessage};
use super::store::{self, SlackReaction, DEFAULT_TITLE};
use super::translate::{run_translation, MsgInput, MsgOutcome, MsgResult, Stop, TranslateSink};
use super::{token, CodedError, SlackCtx, SlackThreadProgress};
use async_trait::async_trait;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use uuid::Uuid;

/// Max messages kept per thread (`truncated` warning beyond).
pub const MAX_MESSAGES: usize = 1000;
const TITLE_MAX_CHARS: usize = 120;
const USER_CACHE_MAX_AGE_DAYS: i64 = 7;

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobEnd {
    /// The row is gone (deleted, or merged into another row).
    Deleted,
    /// This row was a duplicate and was merged into `0`, which should run.
    MergedInto(String),
    Finished {
        ready: bool,
        title: String,
    },
}

enum Fail {
    Deleted,
    Merged(String),
    Error(CodedError),
}

impl From<CodedError> for Fail {
    fn from(e: CodedError) -> Self {
        Fail::Error(e)
    }
}

impl From<sqlx::Error> for Fail {
    fn from(e: sqlx::Error) -> Self {
        Fail::Error(store::internal(e))
    }
}

fn coded(code: &str, detail: impl Into<String>) -> Fail {
    Fail::Error(CodedError::new(code, detail))
}

/// Errors any Slack call can surface as fatal: auth, rate limit, network.
fn common_fatal(e: &SlackError) -> Option<CodedError> {
    match e {
        SlackError::Api { code, .. }
            if matches!(
                code.as_str(),
                "invalid_auth"
                    | "not_authed"
                    | "token_revoked"
                    | "token_expired"
                    | "account_inactive"
            ) =>
        {
            Some(CodedError::new("invalid_auth", code.clone()))
        }
        SlackError::RateLimited => Some(CodedError::new("rate_limited", "rate limited by Slack")),
        SlackError::Network(d) => Some(CodedError::new("network", d.clone())),
        _ => None,
    }
}

fn replies_error(e: SlackError) -> CodedError {
    if let Some(c) = common_fatal(&e) {
        return c;
    }
    match e {
        SlackError::Api { code, needed } => match code.as_str() {
            "missing_scope" => {
                let needed = needed.unwrap_or_default();
                if needed.contains("im:") || needed.contains("mpim:") {
                    CodedError::new("dm_unsupported", super::link::DM_UNSUPPORTED_MESSAGE)
                } else {
                    CodedError::new("missing_scope", needed)
                }
            }
            "thread_not_found" | "message_not_found" => CodedError::new("thread_not_found", code),
            "not_in_channel" => CodedError::new("not_in_channel", code),
            "channel_not_found" => CodedError::new("channel_not_found", code),
            _ => CodedError::new("slack_error", code),
        },
        other => CodedError::new("slack_error", other.to_string()),
    }
}

/// A message as returned by `conversations.replies`, reduced to what we keep.
#[derive(Debug, Clone)]
struct RemoteMessage {
    ts: String,
    user_id: Option<String>,
    bot_id: Option<String>,
    author_name: Option<String>,
    author_icon_url: Option<String>,
    subtype: Option<String>,
    text: String,
    edited_ts: Option<String>,
    reactions: Vec<SlackReaction>,
    files: Vec<RemoteFile>,
    /// `user_profile` (display name, real name, image_72) when present.
    profile: Option<(Option<String>, Option<String>, Option<String>)>,
}

fn str_at(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn parse_message(v: &Value) -> Option<RemoteMessage> {
    let ts = str_at(v, "/ts")?;
    let subtype = str_at(v, "/subtype");
    if matches!(
        subtype.as_deref(),
        Some("channel_join") | Some("channel_leave")
    ) {
        return None;
    }
    let reactions = v
        .get("reactions")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    Some(SlackReaction {
                        name: str_at(r, "/name")?,
                        count: r.get("count").and_then(Value::as_i64).unwrap_or(1),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let files = v
        .get("files")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|f| parse_remote_file(f, &ts)).collect())
        .unwrap_or_default();
    let profile = v.get("user_profile").map(|p| {
        (
            str_at(p, "/display_name"),
            str_at(p, "/real_name"),
            str_at(p, "/image_72"),
        )
    });
    Some(RemoteMessage {
        user_id: str_at(v, "/user"),
        bot_id: str_at(v, "/bot_id"),
        author_name: str_at(v, "/username").or_else(|| str_at(v, "/bot_profile/name")),
        author_icon_url: str_at(v, "/bot_profile/icons/image_72")
            .or_else(|| str_at(v, "/icons/image_72")),
        subtype,
        text: v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        edited_ts: str_at(v, "/edited/ts"),
        reactions,
        files,
        profile,
        ts,
    })
}

/// Numeric sort key of a Slack ts.
fn ts_key(ts: &str) -> (u64, u64) {
    let (a, b) = ts.split_once('.').unwrap_or((ts, "0"));
    (a.parse().unwrap_or(0), b.parse().unwrap_or(0))
}

fn sha256_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `(ts, user_id, bot_id, author_name, text_raw, text_vi)`
type MessageTextRow = (
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
);

struct Job<'a> {
    ctx: &'a SlackCtx,
    id: String,
    status: &'static str,
    phase: &'static str,
    warnings: BTreeSet<&'static str>,
}

/// Run the job for `id` and record its end state.
pub async fn run_job(ctx: &SlackCtx, id: &str) -> JobEnd {
    let mut job = Job {
        ctx,
        id: id.to_string(),
        status: "pending",
        phase: "queued",
        warnings: BTreeSet::new(),
    };
    match job.run().await {
        Ok(end) => end,
        Err(Fail::Deleted) => {
            store::remove_thread_dir(&ctx.app_data, id);
            JobEnd::Deleted
        }
        Err(Fail::Merged(other)) => JobEnd::MergedInto(other),
        Err(Fail::Error(e)) => job.fail(e).await,
    }
}

impl Job<'_> {
    fn db(&self) -> &crate::db::Db {
        &self.ctx.db
    }

    fn progress(&self, done: usize, total: Option<usize>) {
        self.ctx.emit_progress(SlackThreadProgress {
            id: self.id.clone(),
            status: self.status.to_string(),
            phase: self.phase.to_string(),
            done,
            total,
            error_code: None,
            error: None,
        });
    }

    fn set_phase(&mut self, phase: &'static str) {
        self.phase = phase;
        self.progress(0, None);
    }

    fn warn(&mut self, w: &'static str) {
        self.warnings.insert(w);
    }

    fn warnings_json(&self) -> String {
        serde_json::to_string(&self.warnings.iter().collect::<Vec<_>>())
            .unwrap_or_else(|_| "[]".into())
    }

    /// Persist a status transition and notify.
    async fn set_status(&mut self, status: &'static str, phase: &'static str) -> Result<(), Fail> {
        let r = sqlx::query("UPDATE slack_threads SET fetch_status = ? WHERE id = ?")
            .bind(status)
            .bind(&self.id)
            .execute(self.db())
            .await?;
        if r.rows_affected() == 0 {
            return Err(Fail::Deleted);
        }
        self.status = status;
        self.phase = phase;
        self.ctx.emit_changed(&self.id, "updated");
        self.progress(0, None);
        Ok(())
    }

    async fn title(&self) -> String {
        sqlx::query_scalar("SELECT title FROM slack_threads WHERE id = ?")
            .bind(&self.id)
            .fetch_optional(self.db())
            .await
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_TITLE.to_string())
    }

    /// Record a fatal error (data kept as is).
    async fn fail(&mut self, e: CodedError) -> JobEnd {
        let r = sqlx::query(
            "UPDATE slack_threads SET fetch_status = 'error', fetch_error_code = ?, \
             fetch_error = ?, warnings = ? WHERE id = ?",
        )
        .bind(&e.code)
        .bind(&e.detail)
        .bind(self.warnings_json())
        .bind(&self.id)
        .execute(self.db())
        .await;
        if !matches!(r, Ok(ref r) if r.rows_affected() > 0) {
            store::remove_thread_dir(&self.ctx.app_data, &self.id);
            return JobEnd::Deleted;
        }
        tracing::info!(event = "slack_thread_job_failed", id = %self.id, code = %e.code);
        self.status = "error";
        self.ctx.emit_changed(&self.id, "updated");
        self.ctx.emit_progress(SlackThreadProgress {
            id: self.id.clone(),
            status: "error".into(),
            phase: self.phase.to_string(),
            done: 0,
            total: None,
            error_code: Some(e.code.clone()),
            error: Some(e.detail.clone()),
        });
        JobEnd::Finished {
            ready: false,
            title: self.title().await,
        }
    }

    /// Make this row own `link`'s key. If another row already has it, the
    /// most recently updated row wins (SRS §12) and the other one is deleted.
    async fn ensure_key(&mut self, link: &ThreadLink) -> Result<(), Fail> {
        let current: Option<(Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT channel_id, thread_ts, thread_url FROM slack_threads WHERE id = ?",
        )
        .bind(&self.id)
        .fetch_optional(self.db())
        .await?;
        let Some((ch, ts, url)) = current else {
            return Err(Fail::Deleted);
        };
        if ch.as_deref() == Some(link.channel_id.as_str())
            && ts.as_deref() == Some(link.thread_ts.as_str())
            && url.as_deref() == Some(link.canonical_url.as_str())
        {
            return Ok(());
        }
        if let Some(other) =
            store::find_by_key(self.db(), &link.channel_id, &link.thread_ts, Some(&self.id)).await?
        {
            let mine = updated_at(self.db(), &self.id).await?;
            let theirs = updated_at(self.db(), &other).await?;
            if keep_other(&mine, &self.id, &theirs, &other) {
                super::run_links::move_links(self.db(), &self.id, &other).await?;
                store::delete(
                    self.db(),
                    &self.ctx.app_data,
                    std::slice::from_ref(&self.id),
                )
                .await?;
                self.ctx.emit_changed(&self.id, "deleted");
                tracing::info!(event = "slack_thread_merged", from = %self.id, into = %other);
                return Err(Fail::Merged(other));
            }
            super::run_links::move_links(self.db(), &other, &self.id).await?;
            store::delete(self.db(), &self.ctx.app_data, std::slice::from_ref(&other)).await?;
            self.ctx.emit_changed(&other, "deleted");
            tracing::info!(event = "slack_thread_merged", from = %other, into = %self.id);
        }
        let r = sqlx::query(
            "UPDATE slack_threads SET channel_id = ?, thread_ts = ?, thread_url = ?, \
             workspace_domain = ? WHERE id = ?",
        )
        .bind(&link.channel_id)
        .bind(&link.thread_ts)
        .bind(&link.canonical_url)
        .bind(&link.host)
        .bind(&self.id)
        .execute(self.db())
        .await?;
        if r.rows_affected() == 0 {
            return Err(Fail::Deleted);
        }
        Ok(())
    }

    async fn run(&mut self) -> Result<JobEnd, Fail> {
        let row: Option<(Option<String>, i64)> =
            sqlx::query_as("SELECT thread_url, legacy FROM slack_threads WHERE id = ?")
                .bind(&self.id)
                .fetch_optional(self.db())
                .await?;
        let (url, legacy) = row.ok_or(Fail::Deleted)?;
        let legacy = legacy != 0;
        self.set_status("fetching", "queued").await?;

        let mut link = match url.as_deref().map(parse_thread_link) {
            Some(Ok(l)) => l,
            _ if legacy => {
                return Err(coded(
                    "legacy_no_url",
                    "legacy snapshot has no parsable Slack thread_url",
                ))
            }
            Some(Err(e)) => return Err(Fail::Error(e.into())),
            None => return Err(coded("invalid_link", super::link::INVALID_LINK_MESSAGE)),
        };
        self.ensure_key(&link).await?;

        // 1. Token + auth.test.
        self.set_phase("auth");
        let found = token::sources(self.db(), &self.ctx.lookup).await?;
        if found.is_empty() {
            return Err(coded("no_token", "no Slack token configured"));
        }
        let (client, auth) = token::resolve(
            &found,
            &link.host,
            &self.ctx.http,
            &self.ctx.token_http,
            &self.ctx.auth_cache,
        )
        .await?;

        // 2. Channel name + DM guard.
        let channel_name = self.channel_info(&client, &auth, &link.channel_id).await?;

        // 3. Messages (3b: the link pointed at a reply).
        self.set_phase("messages");
        let (mut raw, mut truncated) = client
            .conversations_replies_all(&link.channel_id, &link.thread_ts, MAX_MESSAGES)
            .await
            .map_err(replies_error)?;
        let root = raw
            .first()
            .and_then(|m| m.get("thread_ts"))
            .and_then(Value::as_str)
            .map(str::to_string);
        if let Some(root) = root.filter(|r| *r != link.thread_ts && super::link::is_slack_ts(r)) {
            link = ThreadLink {
                canonical_url: canonical_url(&link.host, &link.channel_id, &root),
                thread_ts: root,
                ..link
            };
            self.ensure_key(&link).await?;
            (raw, truncated) = client
                .conversations_replies_all(&link.channel_id, &link.thread_ts, MAX_MESSAGES)
                .await
                .map_err(replies_error)?;
        }
        if truncated {
            self.warn("truncated");
        }
        let mut msgs: Vec<RemoteMessage> = raw.iter().filter_map(parse_message).collect();
        msgs.sort_by_key(|m| ts_key(&m.ts));
        let remote_files = self
            .upsert_messages(&msgs, &auth, &link, channel_name.as_deref(), legacy)
            .await?;
        self.progress(msgs.len(), Some(msgs.len()));

        // 4/5/7. Users, mentioned channels, avatars.
        self.set_phase("users");
        self.sync_users(&client, &auth, &msgs).await?;

        // 6. Files.
        self.set_phase("files");
        let files_ok = auth.has_scope("files:read") != Some(false);
        let ctx = self.ctx;
        let (id, status) = (self.id.clone(), self.status);
        let report = move |done: usize, total: usize| {
            ctx.emit_progress(SlackThreadProgress {
                id: id.clone(),
                status: status.to_string(),
                phase: "files".into(),
                done,
                total: Some(total),
                error_code: None,
                error: None,
            });
        };
        let sync = files::sync_files(
            self.ctx,
            &client,
            &self.id,
            &remote_files,
            files_ok,
            &report,
        )
        .await?;
        if sync.deleted {
            return Err(Fail::Deleted);
        }
        for w in sync.warnings {
            self.warn(w);
        }
        self.ctx.emit_changed(&self.id, "updated");

        // Translation.
        self.set_status("translating", "translate").await?;
        let inputs: Vec<MsgInput> = sqlx::query_as::<_, (String, i64, String)>(
            "SELECT ts, seq, text_raw FROM slack_messages WHERE thread_id = ? \
             AND translate_status IN ('pending', 'error') ORDER BY seq ASC",
        )
        .bind(&self.id)
        .fetch_all(self.db())
        .await?
        .into_iter()
        .map(|(ts, seq, text_raw)| MsgInput { ts, seq, text_raw })
        .collect();
        let mut sink = DbSink { job: self };
        let summary = run_translation(self.ctx.engine.as_ref(), &mut sink, &inputs)
            .await
            .map_err(|Stop| Fail::Deleted)?;
        if summary.all_failed {
            // Originals stay searchable and the title is still derived.
            self.refresh_derived().await?;
            let detail: Option<String> = sqlx::query_scalar(
                "SELECT translate_error FROM slack_messages WHERE thread_id = ? \
                 AND translate_status = 'error' AND translate_error IS NOT NULL LIMIT 1",
            )
            .bind(&self.id)
            .fetch_optional(self.db())
            .await?
            .flatten();
            return Err(coded(
                "translate_failed",
                detail.unwrap_or_else(|| "translation failed".into()),
            ));
        }
        if summary.failed > 0 {
            self.warn("translate_partial");
        }
        self.finish().await
    }

    async fn channel_info(
        &mut self,
        client: &SlackClient,
        auth: &AuthInfo,
        channel_id: &str,
    ) -> Result<Option<String>, Fail> {
        if auth.lacks_all(&["channels:read", "groups:read"]) {
            self.warn("channel_name_unavailable");
            return Ok(None);
        }
        match client.conversations_info(channel_id).await {
            Ok(info) => {
                if info.is_im || info.is_mpim {
                    return Err(coded("dm_unsupported", super::link::DM_UNSUPPORTED_MESSAGE));
                }
                save_channel(
                    self.ctx,
                    &auth.team_id,
                    channel_id,
                    info.name.as_deref(),
                    info.kind(),
                )
                .await?;
                Ok(info.name)
            }
            Err(e) => {
                if let Some(fatal) = common_fatal(&e) {
                    return Err(Fail::Error(fatal));
                }
                match e {
                    SlackError::Api { code, .. } if code == "channel_not_found" => {
                        Err(coded("channel_not_found", code))
                    }
                    _ => {
                        self.warn("channel_name_unavailable");
                        Ok(None)
                    }
                }
            }
        }
    }

    /// Write messages, file rows and thread metadata in one transaction.
    /// Returns the thread's remote files (unique, message order).
    async fn upsert_messages(
        &mut self,
        msgs: &[RemoteMessage],
        auth: &AuthInfo,
        link: &ThreadLink,
        channel_name: Option<&str>,
        legacy: bool,
    ) -> Result<Vec<RemoteFile>, Fail> {
        let now = chrono::Utc::now().to_rfc3339();
        let mut tx = self.db().begin().await?;

        // Files first: messages reference slack_files.id.
        let mut remote_files: Vec<RemoteFile> = Vec::new();
        let mut seen = HashSet::new();
        for f in msgs.iter().flat_map(|m| &m.files) {
            if seen.insert(f.slack_file_id.clone()) {
                remote_files.push(f.clone());
            }
        }
        let existing_files: Vec<(String, String, Option<i64>)> = sqlx::query_as(
            "SELECT id, slack_file_id, remote_updated FROM slack_files \
             WHERE thread_id = ? AND slack_file_id IS NOT NULL",
        )
        .bind(&self.id)
        .fetch_all(&mut *tx)
        .await?;
        let existing_by_sid: HashMap<&str, (&str, Option<i64>)> = existing_files
            .iter()
            .map(|(id, sid, up)| (sid.as_str(), (id.as_str(), *up)))
            .collect();
        let mut file_row_ids: HashMap<String, String> = HashMap::new();
        for f in &remote_files {
            match existing_by_sid.get(f.slack_file_id.as_str()) {
                Some((row_id, updated)) => {
                    sqlx::query(
                        "UPDATE slack_files SET message_ts = ?, name = ?, title = ?, mime = ?, \
                         filetype = ?, size = ?, permalink = ?, width = ?, height = ?, \
                         download_status = CASE WHEN ? THEN 'pending' ELSE download_status END, \
                         remote_updated = ? WHERE id = ?",
                    )
                    .bind(&f.message_ts)
                    .bind(&f.name)
                    .bind(&f.title)
                    .bind(&f.mime)
                    .bind(&f.filetype)
                    .bind(f.size)
                    .bind(&f.permalink)
                    .bind(f.width)
                    .bind(f.height)
                    .bind(*updated != f.remote_updated)
                    .bind(f.remote_updated)
                    .bind(row_id)
                    .execute(&mut *tx)
                    .await?;
                    file_row_ids.insert(f.slack_file_id.clone(), row_id.to_string());
                }
                None => {
                    let row_id = Uuid::new_v4().to_string();
                    sqlx::query(
                        "INSERT INTO slack_files (id, thread_id, slack_file_id, message_ts, name, \
                         title, mime, filetype, size, remote_updated, permalink, width, height, \
                         download_status, created_at) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?)",
                    )
                    .bind(&row_id)
                    .bind(&self.id)
                    .bind(&f.slack_file_id)
                    .bind(&f.message_ts)
                    .bind(&f.name)
                    .bind(&f.title)
                    .bind(&f.mime)
                    .bind(&f.filetype)
                    .bind(f.size)
                    .bind(f.remote_updated)
                    .bind(&f.permalink)
                    .bind(f.width)
                    .bind(f.height)
                    .bind(&now)
                    .execute(&mut *tx)
                    .await?;
                    file_row_ids.insert(f.slack_file_id.clone(), row_id);
                }
            }
        }
        let gone_files: Vec<String> = existing_files
            .iter()
            .filter(|(_, sid, _)| !seen.contains(sid))
            .map(|(_, sid, _)| sid.clone())
            .collect();
        for sid in &gone_files {
            sqlx::query("DELETE FROM slack_files WHERE thread_id = ? AND slack_file_id = ?")
                .bind(&self.id)
                .bind(sid)
                .execute(&mut *tx)
                .await?;
        }

        // Messages.
        let existing_msgs: Vec<(String, String)> =
            sqlx::query_as("SELECT ts, src_hash FROM slack_messages WHERE thread_id = ?")
                .bind(&self.id)
                .fetch_all(&mut *tx)
                .await?;
        let existing_hash: HashMap<&str, &str> = existing_msgs
            .iter()
            .map(|(ts, h)| (ts.as_str(), h.as_str()))
            .collect();
        let keep: HashSet<&str> = msgs.iter().map(|m| m.ts.as_str()).collect();
        for (ts, _) in &existing_msgs {
            if !keep.contains(ts.as_str()) {
                sqlx::query("DELETE FROM slack_messages WHERE thread_id = ? AND ts = ?")
                    .bind(&self.id)
                    .bind(ts)
                    .execute(&mut *tx)
                    .await?;
            }
        }
        for (seq, m) in msgs.iter().enumerate() {
            let hash = sha256_hex(&m.text);
            let reactions = serde_json::to_string(&m.reactions).unwrap_or_else(|_| "[]".into());
            let mut fids: Vec<&str> = Vec::new();
            for f in &m.files {
                if let Some(id) = file_row_ids.get(&f.slack_file_id) {
                    if !fids.contains(&id.as_str()) {
                        fids.push(id);
                    }
                }
            }
            let file_ids = serde_json::to_string(&fids).unwrap_or_else(|_| "[]".into());
            let tombstone = m.subtype.as_deref() == Some("tombstone");
            match existing_hash.get(m.ts.as_str()) {
                Some(old) if *old == hash && !tombstone => {
                    sqlx::query(
                        "UPDATE slack_messages SET seq = ?, user_id = ?, bot_id = ?, author_name = ?, \
                         author_icon_url = ?, subtype = ?, edited_ts = ?, reactions = ?, file_ids = ?, \
                         translate_status = CASE WHEN translate_status = 'error' THEN 'pending' \
                           ELSE translate_status END \
                         WHERE thread_id = ? AND ts = ?",
                    )
                    .bind(seq as i64)
                    .bind(&m.user_id)
                    .bind(&m.bot_id)
                    .bind(&m.author_name)
                    .bind(&m.author_icon_url)
                    .bind(&m.subtype)
                    .bind(&m.edited_ts)
                    .bind(&reactions)
                    .bind(&file_ids)
                    .bind(&self.id)
                    .bind(&m.ts)
                    .execute(&mut *tx)
                    .await?;
                }
                _ => {
                    // New or edited text → (re)translate; tombstones are kept as is.
                    let (vi, status) = if tombstone {
                        (Some(m.text.as_str()), "skipped")
                    } else {
                        (None, "pending")
                    };
                    sqlx::query(
                        "INSERT INTO slack_messages (thread_id, ts, seq, user_id, bot_id, author_name, \
                         author_icon_url, subtype, text_raw, text_vi, src_hash, translate_status, \
                         translate_error, edited_ts, reactions, file_ids) \
                         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?) \
                         ON CONFLICT(thread_id, ts) DO UPDATE SET seq = excluded.seq, \
                         user_id = excluded.user_id, bot_id = excluded.bot_id, \
                         author_name = excluded.author_name, author_icon_url = excluded.author_icon_url, \
                         subtype = excluded.subtype, text_raw = excluded.text_raw, \
                         text_vi = excluded.text_vi, src_hash = excluded.src_hash, \
                         translate_status = excluded.translate_status, translate_error = NULL, \
                         edited_ts = excluded.edited_ts, reactions = excluded.reactions, \
                         file_ids = excluded.file_ids",
                    )
                    .bind(&self.id)
                    .bind(&m.ts)
                    .bind(seq as i64)
                    .bind(&m.user_id)
                    .bind(&m.bot_id)
                    .bind(&m.author_name)
                    .bind(&m.author_icon_url)
                    .bind(&m.subtype)
                    .bind(&m.text)
                    .bind(vi)
                    .bind(&hash)
                    .bind(status)
                    .bind(&m.edited_ts)
                    .bind(&reactions)
                    .bind(&file_ids)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        if legacy {
            sqlx::query("DELETE FROM slack_files WHERE thread_id = ? AND slack_file_id IS NULL")
                .bind(&self.id)
                .execute(&mut *tx)
                .await?;
        }
        let last_ts = msgs.last().map(|m| m.ts.clone());
        let r = sqlx::query(
            "UPDATE slack_threads SET team_id = ?, workspace = ?, workspace_domain = ?, \
             channel_id = ?, channel = COALESCE(?, channel, channel_id), thread_ts = ?, \
             thread_url = ?, reply_count = ?, last_reply_ts = ?, legacy = 0, \
             legacy_content = NULL WHERE id = ?",
        )
        .bind(&auth.team_id)
        .bind(&auth.team)
        .bind(&link.host)
        .bind(&link.channel_id)
        .bind(channel_name)
        .bind(&link.thread_ts)
        .bind(&link.canonical_url)
        .bind(msgs.len().saturating_sub(1) as i64)
        .bind(&last_ts)
        .bind(&self.id)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            return Err(Fail::Deleted);
        }
        tx.commit().await?;

        // After commit: files of removed Slack files, and the old zip files
        // that sat directly in files/ (sub dirs are Slack files).
        for sid in &gone_files {
            files::remove_file_dir(&self.ctx.app_data, &self.id, sid);
        }
        let files_dir = store::thread_dir(&self.ctx.app_data, &self.id).join("files");
        if legacy && store::is_safe_id(&self.id) {
            if let Ok(rd) = std::fs::read_dir(&files_dir) {
                for entry in rd.flatten() {
                    if entry.file_type().is_ok_and(|t| t.is_file()) {
                        let _ = std::fs::remove_file(entry.path());
                    }
                }
            }
        }
        self.ctx.emit_changed(&self.id, "updated");
        Ok(remote_files)
    }

    /// `users.info` for authors + mentions (cached 7 days), avatars, bot icons
    /// and unlabeled `<#C…>` channel names.
    async fn sync_users(
        &mut self,
        client: &SlackClient,
        auth: &AuthInfo,
        msgs: &[RemoteMessage],
    ) -> Result<(), Fail> {
        let team = auth.team_id.as_str();
        let now = chrono::Utc::now();
        let mut ids: Vec<String> = Vec::new();
        for m in msgs {
            for u in m
                .user_id
                .iter()
                .cloned()
                .chain(mrkdwn::mentioned_users(&m.text))
            {
                if !ids.contains(&u) {
                    ids.push(u);
                }
            }
        }
        let cached: HashMap<String, String> = sqlx::query_as::<_, (String, String)>(
            "SELECT user_id, fetched_at FROM slack_users WHERE team_id = ?",
        )
        .bind(team)
        .fetch_all(self.db())
        .await?
        .into_iter()
        .collect();
        let fresh = |uid: &str| {
            cached
                .get(uid)
                .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
                .is_some_and(|t| {
                    (now - t.with_timezone(&chrono::Utc)).num_days() < USER_CACHE_MAX_AGE_DAYS
                })
        };
        let users_ok = auth.has_scope("users:read") != Some(false);
        if !users_ok {
            self.warn("users_scope");
        }
        // Profiles embedded in messages fill the gaps when users.info is not
        // available (or not needed yet).
        if !users_ok {
            for m in msgs {
                if let (Some(uid), Some((display, real, image))) = (&m.user_id, &m.profile) {
                    if !fresh(uid) {
                        upsert_user(
                            self.ctx,
                            team,
                            uid,
                            display.as_deref().or(real.as_deref()),
                            real.as_deref(),
                            image.as_deref(),
                            false,
                        )
                        .await?;
                    }
                }
            }
        } else {
            let todo: Vec<&String> = ids.iter().filter(|u| !fresh(u)).collect();
            let total = todo.len();
            for (i, uid) in todo.into_iter().enumerate() {
                match client.users_info(uid).await {
                    Ok(u) => {
                        let display = u
                            .display_name
                            .clone()
                            .or(u.real_name.clone())
                            .or(u.name.clone());
                        upsert_user(
                            self.ctx,
                            team,
                            uid,
                            display.as_deref(),
                            u.real_name.as_deref(),
                            u.avatar_url.as_deref(),
                            u.is_bot,
                        )
                        .await?;
                    }
                    Err(SlackError::Api { code, .. }) if code == "missing_scope" => {
                        self.warn("users_scope");
                        break;
                    }
                    Err(SlackError::Api { code, .. }) if code == "user_not_found" => {
                        upsert_user(self.ctx, team, uid, Some(uid), None, None, false).await?;
                    }
                    Err(e) => {
                        tracing::warn!(event = "slack_users_info_failed", error = %e);
                    }
                }
                self.progress(i + 1, Some(total));
            }
        }

        // Avatars (no token).
        let avatars: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
            "SELECT user_id, avatar_url, avatar_path FROM slack_users WHERE team_id = ?",
        )
        .bind(team)
        .fetch_all(self.db())
        .await?;
        for (uid, url, path) in avatars.into_iter().filter(|(u, _, _)| ids.contains(u)) {
            let Some(url) = url else { continue };
            let expected = store::avatar_rel_path(team, &uid, &url);
            let present = path.is_some()
                && path == expected
                && path
                    .as_ref()
                    .is_some_and(|p| self.ctx.app_data.join(p).is_file());
            if present {
                continue;
            }
            if let Some(rel) = files::ensure_avatar(self.ctx, team, &uid, &url).await {
                sqlx::query(
                    "UPDATE slack_users SET avatar_path = ? WHERE team_id = ? AND user_id = ?",
                )
                .bind(rel)
                .bind(team)
                .bind(&uid)
                .execute(self.db())
                .await?;
            }
        }
        let mut bots = HashSet::new();
        for m in msgs {
            if let (Some(bot), Some(url), None) = (&m.bot_id, &m.author_icon_url, &m.user_id) {
                if bots.insert((bot.clone(), url.clone())) {
                    files::ensure_avatar(self.ctx, team, bot, url).await;
                }
            }
        }

        // Unlabeled channel mentions.
        if !auth.lacks_all(&["channels:read", "groups:read"]) {
            let known: HashSet<String> = sqlx::query_scalar::<_, String>(
                "SELECT channel_id FROM slack_channels WHERE team_id = ?",
            )
            .bind(team)
            .fetch_all(self.db())
            .await?
            .into_iter()
            .collect();
            let mut wanted: Vec<String> = Vec::new();
            for m in msgs {
                for c in mrkdwn::unlabeled_channels(&m.text) {
                    if !known.contains(&c) && !wanted.contains(&c) {
                        wanted.push(c);
                    }
                }
            }
            for c in wanted {
                if let Ok(info) = client.conversations_info(&c).await {
                    save_channel(self.ctx, team, &c, info.name.as_deref(), info.kind()).await?;
                }
            }
        }
        self.ctx.emit_changed(&self.id, "updated");
        Ok(())
    }

    /// Recompute title / transcript and mark the thread `ready`.
    async fn finish(&mut self) -> Result<JobEnd, Fail> {
        self.refresh_derived().await?;
        let r = sqlx::query(
            "UPDATE slack_threads SET fetch_status = 'ready', fetch_error_code = NULL, \
             fetch_error = NULL, fetched_at = ?, warnings = ? WHERE id = ?",
        )
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(self.warnings_json())
        .bind(&self.id)
        .execute(self.db())
        .await?;
        if r.rows_affected() == 0 {
            return Err(Fail::Deleted);
        }
        self.status = "ready";
        self.phase = "done";
        self.ctx.emit_changed(&self.id, "updated");
        self.progress(0, None);
        Ok(JobEnd::Finished {
            ready: true,
            title: self.title().await,
        })
    }

    /// Recompute the auto title (unless user-chosen) and the search
    /// transcript from the stored messages (translated or not).
    async fn refresh_derived(&mut self) -> Result<(), Fail> {
        let team: Option<String> =
            sqlx::query_scalar("SELECT team_id FROM slack_threads WHERE id = ?")
                .bind(&self.id)
                .fetch_optional(self.db())
                .await?
                .ok_or(Fail::Deleted)?;
        let all_names = store::load_names(self.db()).await?;
        let names = team
            .as_deref()
            .and_then(|t| all_names.get(t))
            .cloned()
            .unwrap_or_else(Names::default);
        let rows: Vec<MessageTextRow> = sqlx::query_as(
            "SELECT ts, user_id, bot_id, author_name, text_raw, text_vi FROM slack_messages \
                 WHERE thread_id = ? ORDER BY seq ASC",
        )
        .bind(&self.id)
        .fetch_all(self.db())
        .await?;
        let authors: Vec<String> = rows
            .iter()
            .map(|(_, uid, bot, name, _, _)| {
                uid.as_deref()
                    .and_then(|u| names.user(u))
                    .map(str::to_string)
                    .or_else(|| name.clone())
                    .or_else(|| uid.clone())
                    .or_else(|| bot.clone())
                    .unwrap_or_else(|| "Unknown".into())
            })
            .collect();
        let texts: Vec<TextMessage> = rows
            .iter()
            .zip(&authors)
            .map(|((ts, _, _, _, raw, vi), author)| TextMessage {
                author,
                ts,
                text_raw: raw,
                text_vi: vi.as_deref(),
                files: Vec::new(),
            })
            .collect();
        let search_text = mrkdwn::transcript(&texts, &names);
        let auto_title = rows
            .first()
            .map(|(_, _, _, _, raw, vi)| {
                mrkdwn::first_line(
                    &mrkdwn::to_plain(vi.as_deref().unwrap_or(raw), &names, mrkdwn::Links::Label),
                    TITLE_MAX_CHARS,
                )
            })
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| DEFAULT_TITLE.to_string());
        let r = sqlx::query(
            "UPDATE slack_threads SET search_text = ?, \
             title = CASE WHEN title_custom = 0 THEN ? ELSE title END WHERE id = ?",
        )
        .bind(&search_text)
        .bind(&auto_title)
        .bind(&self.id)
        .execute(self.db())
        .await?;
        if r.rows_affected() == 0 {
            return Err(Fail::Deleted);
        }
        Ok(())
    }
}

/// Merge rule for two rows of the same thread: the most recently updated row
/// is kept; on a tie the smaller id wins, so concurrent jobs on both rows
/// always agree on the survivor.
pub(crate) fn keep_other(mine: &str, my_id: &str, theirs: &str, their_id: &str) -> bool {
    (theirs, std::cmp::Reverse(their_id)) > (mine, std::cmp::Reverse(my_id))
}

async fn updated_at(db: &crate::db::Db, id: &str) -> Result<String, sqlx::Error> {
    Ok(
        sqlx::query_scalar::<_, String>("SELECT updated_at FROM slack_threads WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await?
            .unwrap_or_default(),
    )
}

async fn save_channel(
    ctx: &SlackCtx,
    team: &str,
    channel_id: &str,
    name: Option<&str>,
    kind: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO slack_channels (team_id, channel_id, name, kind, fetched_at) \
         VALUES (?, ?, ?, ?, ?) ON CONFLICT(team_id, channel_id) DO UPDATE SET \
         name = COALESCE(excluded.name, name), kind = excluded.kind, fetched_at = excluded.fetched_at",
    )
    .bind(team)
    .bind(channel_id)
    .bind(name)
    .bind(kind)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&ctx.db)
    .await?;
    Ok(())
}

async fn upsert_user(
    ctx: &SlackCtx,
    team: &str,
    user_id: &str,
    display_name: Option<&str>,
    real_name: Option<&str>,
    avatar_url: Option<&str>,
    is_bot: bool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO slack_users (team_id, user_id, display_name, real_name, avatar_url, \
         avatar_path, is_bot, fetched_at) VALUES (?, ?, ?, ?, ?, NULL, ?, ?) \
         ON CONFLICT(team_id, user_id) DO UPDATE SET display_name = excluded.display_name, \
         real_name = excluded.real_name, \
         avatar_path = CASE WHEN avatar_url IS excluded.avatar_url THEN avatar_path ELSE NULL END, \
         avatar_url = excluded.avatar_url, is_bot = excluded.is_bot, fetched_at = excluded.fetched_at",
    )
    .bind(team)
    .bind(user_id)
    .bind(display_name)
    .bind(real_name)
    .bind(avatar_url)
    .bind(is_bot as i64)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&ctx.db)
    .await?;
    Ok(())
}

/// Writes translation results as they arrive.
struct DbSink<'a, 'b> {
    job: &'a Job<'b>,
}

#[async_trait]
impl TranslateSink for DbSink<'_, '_> {
    async fn progress(&mut self, done: usize, total: usize) {
        self.job.progress(done, Some(total));
    }

    async fn apply(&mut self, results: Vec<MsgResult>) -> Result<(), Stop> {
        for r in &results {
            let (vi, status, err) = match &r.outcome {
                MsgOutcome::Done(t) => (Some(t.as_str()), "done", None),
                MsgOutcome::Skipped(t) => (Some(t.as_str()), "skipped", None),
                MsgOutcome::Error(e) => (None, "error", Some(e.as_str())),
            };
            sqlx::query(
                "UPDATE slack_messages SET text_vi = ?, translate_status = ?, translate_error = ? \
                 WHERE thread_id = ? AND ts = ?",
            )
            .bind(vi)
            .bind(status)
            .bind(err)
            .bind(&self.job.id)
            .bind(&r.ts)
            .execute(self.job.db())
            .await
            .map_err(|_| Stop)?;
        }
        let exists: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads WHERE id = ?")
            .bind(&self.job.id)
            .fetch_one(self.job.db())
            .await
            .map_err(|_| Stop)?;
        if exists == 0 {
            return Err(Stop);
        }
        self.job.ctx.emit_changed(&self.job.id, "updated");
        Ok(())
    }
}
