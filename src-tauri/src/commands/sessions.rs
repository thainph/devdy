//! Mirroring externally-created Claude sessions into Devdy.
//!
//! Devdy spawns the Claude Agent SDK, which reads/writes its conversation
//! transcripts to the SAME store the `claude` CLI / VS Code extension use:
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`. That's why sessions
//! started in Devdy show up in the CLI. This module does the reverse: it
//! discovers the transcripts living there for a project's working dir and mirrors
//! each as a Devdy `session` run — automatically, via `reconcile_claude_sessions`
//! (on project open) and the live `session_watcher` — so the user can read their
//! history and continue them via the normal resume path (`resume_run`). There is
//! no manual import step; the Codex counterpart works the same way.
//!
//! The transcript's `user` / `assistant` entries already carry the
//! `{ type, message }` shape Devdy's stream-json renderer expects, so mirroring
//! is mostly: filter to those lines, re-emit them into a `.devdy/runs/<id>.log`,
//! and create a run row pointing at the captured `session_id`.

use crate::db::Db;
use crate::runs::sidecar::insert_usage_from_result;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use tauri::State;
use uuid::Uuid;

/// Encode an absolute path the way Claude Code names its per-project session
/// directory: every non-alphanumeric character becomes `-`
/// (e.g. `/Users/x/Tools/devdy` → `-Users-x-Tools-devdy`).
pub(crate) fn encode_project_dir(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude_sessions_dir(project_path: &str) -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = Path::new(&home)
        .join(".claude")
        .join("projects")
        .join(encode_project_dir(project_path));
    dir.is_dir().then_some(dir)
}

/// Accumulated token usage parsed out of a transcript's assistant turns.
#[derive(Default)]
pub(crate) struct UsageTotals {
    pub input: i64,
    pub output: i64,
    pub cache_creation: i64,
    pub cache_read: i64,
}

/// Everything `import`/sync needs out of a native Claude transcript in one pass.
pub(crate) struct ParsedTranscript {
    /// Devdy stream-json log: one `{ type, message }` line per user/assistant
    /// turn (sidechains skipped), which `parseStreamLog` renders directly.
    pub log: String,
    /// Resolved display title (ai-title, else first user message).
    pub title: String,
    pub model: Option<String>,
    /// Number of assistant turns — also the multiplier for usage.
    pub turn_count: i64,
    pub usage: UsageTotals,
}

/// Parse a native Claude transcript into the shape the live session-sync needs.
/// Returns `None` when the file holds no
/// conversation for `project_path` (e.g. an encoded-dir hash collision).
///
/// The SDK / `claude` CLI / VS Code extension all write the SAME transcript, so
/// this is the single reader Devdy uses to mirror externally-created or
/// externally-continued sessions back into its own run/log/usage model.
pub(crate) fn parse_transcript(file: &Path, project_path: &str) -> Option<ParsedTranscript> {
    let raw = fs::read_to_string(file).ok()?;

    let mut log = String::new();
    let mut title: Option<String> = None;
    let mut first_message: Option<String> = None;
    let mut model: Option<String> = None;
    let mut turn_count = 0i64;
    let mut usage = UsageTotals::default();
    let mut cwd_matches = false;

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !cwd_matches {
            if let Some(c) = v.get("cwd").and_then(|x| x.as_str()) {
                cwd_matches = c == project_path;
            }
        }
        match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "ai-title" => {
                if let Some(s) = v.get("aiTitle").and_then(|x| x.as_str()) {
                    title = Some(s.to_string());
                }
            }
            t @ ("user" | "assistant") => {
                if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
                    continue;
                }
                let Some(msg) = v.get("message") else { continue };
                if t == "user" && first_message.is_none() {
                    if let Some(tx) = message_text(msg) {
                        if !tx.trim().is_empty() {
                            first_message = Some(tx);
                        }
                    }
                }
                if t == "assistant" {
                    turn_count += 1;
                    if model.is_none() {
                        if let Some(m) = msg.get("model").and_then(|x| x.as_str()) {
                            model = Some(m.to_string());
                        }
                    }
                    if let Some(u) = msg.get("usage") {
                        let g = |k: &str| u.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                        usage.input += g("input_tokens");
                        usage.output += g("output_tokens");
                        usage.cache_creation += g("cache_creation_input_tokens");
                        usage.cache_read += g("cache_read_input_tokens");
                    }
                }
                let out = serde_json::json!({ "type": t, "message": msg });
                log.push_str(&serde_json::to_string(&out).unwrap_or_default());
                log.push('\n');
            }
            _ => {}
        }
    }

    if !cwd_matches || log.is_empty() {
        return None;
    }

    let title = match title.filter(|t| !t.trim().is_empty()) {
        Some(t) => truncate(&t, 100),
        None => title_from_first_message(first_message.as_deref()),
    };

    Some(ParsedTranscript {
        log,
        title,
        model,
        turn_count,
        usage,
    })
}

/// Resolve a run title from the first user message: trimmed, truncated, with a
/// generic fallback. Shared with the Codex parser.
pub(crate) fn title_from_first_message(first: Option<&str>) -> String {
    let t = truncate(first.unwrap_or("").trim(), 100);
    if t.is_empty() {
        "Imported session".to_string()
    } else {
        t
    }
}

/// Pull the first text block (or plain-string content) out of a transcript
/// message. Returns None for messages with no human-readable text (e.g. a
/// user turn carrying only a `tool_result`).
fn message_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        for b in arr {
            if b.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|v| v.as_str()) {
                    return Some(t.to_string());
                }
            }
        }
    }
    None
}

fn truncate(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn file_modified_rfc3339(path: &Path) -> String {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| chrono::DateTime::<chrono::Utc>::from(t).to_rfc3339())
        .unwrap_or_default()
}

/// Byte length of a file, used as the "how far have we mirrored this transcript"
/// marker. Transcripts are append-only, so a changed size means new content.
fn file_size(path: &Path) -> Option<i64> {
    fs::metadata(path).ok().map(|m| m.len() as i64)
}

/// Result of mirroring one transcript into Devdy's run/log/usage model.
pub(crate) enum SyncOutcome {
    /// A new `session` run row was created for an externally-started session.
    Imported(String),
    /// An existing run's on-disk log was refreshed from the transcript.
    Updated(String),
    /// Nothing to do — no conversation, or a live run owns this session, or a
    /// brand-new session was deferred to avoid racing a still-initializing run.
    Skipped,
}

/// Re-derive a run's token-usage ledger from its (full) transcript, replacing
/// any existing rows for the run. Idempotent: re-running on a grown transcript
/// always lands on the current total without double-counting. Cost is estimated
/// (the transcript carries no SDK `total_cost_usd`); `created_at` should be the
/// transcript mtime so the row sorts with the session.
///
/// For a run that originated in Devdy this *replaces* the live drain's accurate
/// per-turn cost rows with one estimated lump — the tradeoff for counting turns
/// that were added outside Devdy, whose real cost the transcript never recorded.
async fn sync_run_usage(
    db: &sqlx::SqlitePool,
    run_id: &str,
    project_id: &str,
    project_name: &str,
    engine: &str,
    parsed: &ParsedTranscript,
    created_at: &str,
) {
    let total = parsed.usage.input
        + parsed.usage.output
        + parsed.usage.cache_creation
        + parsed.usage.cache_read;
    // Nothing reliable to record. Leave any existing usage rows untouched rather
    // than wiping them for an empty / usage-less rebuild (a transcript can have
    // turns but no token counts) — otherwise a rebuild could strand the run with
    // no usage at all if the synthetic insert is skipped.
    if parsed.turn_count == 0 || total == 0 {
        return;
    }
    // Safe to clear now: the synthetic carries real tokens, so the insert below
    // is guaranteed to succeed (it only skips when total == 0 && cost == 0).
    let _ = sqlx::query("DELETE FROM run_usage WHERE run_id = ?")
        .bind(run_id)
        .execute(db)
        .await;
    let synthetic = serde_json::json!({
        "type": "result",
        "usage": {
            "input_tokens": parsed.usage.input,
            "output_tokens": parsed.usage.output,
            "cache_creation_input_tokens": parsed.usage.cache_creation,
            "cache_read_input_tokens": parsed.usage.cache_read,
        },
        "num_turns": parsed.turn_count,
    });
    let _ = insert_usage_from_result(
        db,
        run_id,
        project_id,
        project_name,
        engine,
        parsed.model.as_deref(),
        &synthetic,
        created_at,
    )
    .await;
}

/// Mirror a single session transcript into Devdy. Engine-agnostic core shared by
/// Claude (`upsert_claude_session`) and Codex (`codex_sessions`). The caller
/// resolves the engine's transcript `file` (it must exist) and supplies the
/// engine's `parse` fn. This reconciles the shared transcript store with runs:
/// - existing run for the session → refresh its rendered log on disk;
/// - no run yet → import it as a `session` run (history + token usage);
/// - the session is owned by a *running* Devdy run → leave it to the live
///   stream; and if any run in the project is still `running` we defer importing
///   *new* sessions, since that run may just not have captured its `session_id`
///   yet (avoids creating a duplicate of a live run).
pub(crate) async fn upsert_session_run_core(
    db: &sqlx::SqlitePool,
    project_id: &str,
    project_name: &str,
    project_path: &str,
    session_id: &str,
    engine: &str,
    file: &Path,
    parse: impl Fn(&Path, &str) -> Option<ParsedTranscript>,
    // When true (the automatic watcher/reconcile paths), defer importing a
    // brand-new session while any run in the project is still `running`, since
    // that run may simply not have captured its `session_id` yet. `get_run_log`
    // passes false — it always targets an existing run, never a new import.
    defer_when_busy: bool,
) -> Result<SyncOutcome, String> {
    use sqlx::Row;

    let runs_dir = Path::new(project_path).join(".devdy").join("runs");

    // Existing run for this session?
    if let Some(row) =
        sqlx::query("SELECT id, status, transcript_synced_size FROM runs WHERE project_id = ? AND session_id = ?")
            .bind(project_id)
            .bind(session_id)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?
    {
        let id: String = row.get("id");
        let status: String = row.get("status");
        let synced_size: Option<i64> = row.get("transcript_synced_size");
        // A live run streams its own updates; never touch its log from here.
        if status == "running" {
            return Ok(SyncOutcome::Skipped);
        }
        let conv_path = runs_dir.join(format!("{}.log", id));
        let cur_size = file_size(file);
        let newer = match synced_size {
            // We know exactly how many bytes we last mirrored. Since transcripts
            // are append-only, rebuild iff the size changed — exact, so a short
            // final tail written just after a previous sync is never missed.
            Some(s) => cur_size.is_some_and(|c| c != s),
            // Never mirrored from a transcript before — e.g. a Devdy-native run
            // whose richer log (result/cost summary, stderr) the drain wrote
            // directly. Keep the original guard so the engine's own finishing
            // write doesn't clobber it; a real external edit advances the
            // transcript past the margin, and from then on we track size exactly.
            None => {
                let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
                match (mtime(file), mtime(&conv_path)) {
                    (Some(n), Some(c)) => n > c + std::time::Duration::from_secs(5),
                    (Some(_), None) => true,
                    _ => false,
                }
            }
        };
        if !newer {
            return Ok(SyncOutcome::Skipped);
        }
        let Some(parsed) = parse(file, project_path) else {
            return Ok(SyncOutcome::Skipped);
        };
        fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
        fs::write(&conv_path, &parsed.log).map_err(|e| e.to_string())?;
        // Re-tally usage from the now-larger transcript so stats track the turns
        // added outside Devdy. created_at = transcript mtime.
        let created_at = file_modified_rfc3339(file);
        let created_at = if created_at.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            created_at
        };
        sync_run_usage(db, &id, project_id, project_name, engine, &parsed, &created_at).await;
        // Remember where the transcript lives and how far we've mirrored it.
        let _ = sqlx::query("UPDATE runs SET transcript_path = ?, transcript_synced_size = ? WHERE id = ?")
            .bind(file.to_string_lossy().as_ref())
            .bind(cur_size)
            .bind(&id)
            .execute(db)
            .await;
        return Ok(SyncOutcome::Updated(id));
    }

    // New session. Defer if a run is mid-flight (its session_id may be pending)
    // so the watcher doesn't import a duplicate of a still-initializing run.
    if defer_when_busy {
        let running: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs WHERE project_id = ? AND status = 'running'",
        )
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
        if running > 0 {
            return Ok(SyncOutcome::Skipped);
        }
    }

    let Some(parsed) = parse(file, project_path) else {
        return Ok(SyncOutcome::Skipped);
    };

    let new_id = Uuid::new_v4().to_string();
    let log_path = runs_dir.join(format!("{}.log", new_id));
    let output_path = log_path.to_string_lossy().to_string();
    let synced_size = file_size(file);

    // created_at = session mtime so it sorts naturally among runs; mark done so
    // the UI offers resume right away.
    let created_at = {
        let m = file_modified_rfc3339(file);
        if m.is_empty() {
            chrono::Utc::now().to_rfc3339()
        } else {
            m
        }
    };
    let finished_at = chrono::Utc::now().to_rfc3339();

    // `OR IGNORE` makes this idempotent against the unique session index: if the
    // watcher and the on-open reconcile race, the loser is silently rejected
    // (rows_affected == 0) and we treat it as a no-op — the winner already
    // imported the session.
    let res = sqlx::query(
        "INSERT OR IGNORE INTO runs (id, project_id, type, status, engine, session_id, output_path, transcript_path, transcript_synced_size, started_at, finished_at, created_at, title)
         VALUES (?, ?, 'session', 'done', ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&new_id)
    .bind(project_id)
    .bind(engine)
    .bind(session_id)
    .bind(&output_path)
    .bind(file.to_string_lossy().as_ref())
    .bind(synced_size)
    .bind(&created_at)
    .bind(&finished_at)
    .bind(&created_at)
    .bind(&parsed.title)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    if res.rows_affected() == 0 {
        return Ok(SyncOutcome::Skipped);
    }

    // Persist the converted history where get_run_log / resume_run expect it.
    fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
    fs::write(&log_path, &parsed.log).map_err(|e| e.to_string())?;

    // Record the session's accumulated token usage so the imported history shows
    // in stats. Cost is estimated (the transcript has no SDK total_cost_usd).
    sync_run_usage(db, &new_id, project_id, project_name, engine, &parsed, &created_at).await;

    Ok(SyncOutcome::Imported(new_id))
}

/// Claude wrapper around [`upsert_session_run_core`]: resolves the native
/// `<session-id>.jsonl` transcript and uses the Claude parser.
pub(crate) async fn upsert_claude_session(
    db: &sqlx::SqlitePool,
    project_id: &str,
    project_name: &str,
    project_path: &str,
    session_id: &str,
    defer_when_busy: bool,
) -> Result<SyncOutcome, String> {
    let Some(dir) = claude_sessions_dir(project_path) else {
        return Ok(SyncOutcome::Skipped);
    };
    let file = dir.join(format!("{}.jsonl", session_id));
    if !file.is_file() {
        return Ok(SyncOutcome::Skipped);
    }
    upsert_session_run_core(
        db,
        project_id,
        project_name,
        project_path,
        session_id,
        "claude",
        &file,
        parse_transcript,
        defer_when_busy,
    )
    .await
}

/// Reconcile every Claude session stored for a project's working dir into Devdy
/// runs — importing externally-created ones and refreshing existing ones. Called
/// on project open (and by the live watcher) so the run list mirrors the shared
/// transcript store. Returns the number of runs imported or refreshed.
#[tauri::command]
pub async fn reconcile_claude_sessions(
    db: State<'_, Db>,
    project_id: String,
) -> Result<i64, String> {
    use sqlx::Row;

    let row = sqlx::query("SELECT name, path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let project_name: String = row.get("name");
    let project_path: String = row.get("path");

    let Some(dir) = claude_sessions_dir(&project_path) else {
        return Ok(0);
    };

    let mut changed = 0i64;
    for entry in fs::read_dir(&dir).map_err(|e| e.to_string())? {
        let path = entry.map_err(|e| e.to_string())?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        match upsert_claude_session(
            db.inner(),
            &project_id,
            &project_name,
            &project_path,
            session_id,
            true,
        )
        .await
        {
            Ok(SyncOutcome::Imported(_)) | Ok(SyncOutcome::Updated(_)) => changed += 1,
            _ => {}
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("devdy_claude_test_{}_{}.jsonl", name, std::process::id()));
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn parse_transcript_extracts_turns_title_usage() {
        let content = concat!(
            r#"{"type":"user","cwd":"/proj","message":{"role":"user","content":"Hello there"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","model":"claude-x","content":[{"type":"text","text":"Hi"}],"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2}}}"#,
            "\n",
        );
        let f = temp_file("basic", content);
        let p = parse_transcript(&f, "/proj").expect("should parse");
        assert_eq!(p.turn_count, 1);
        assert_eq!(p.model.as_deref(), Some("claude-x"));
        assert_eq!(p.usage.input, 10);
        assert_eq!(p.usage.output, 5);
        assert_eq!(p.usage.cache_read, 2);
        assert!(p.title.contains("Hello"));
        assert_eq!(p.log.lines().count(), 2);
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn parse_transcript_rejects_wrong_cwd() {
        let content =
            "{\"type\":\"user\",\"cwd\":\"/other\",\"message\":{\"role\":\"user\",\"content\":\"hi\"}}\n";
        let f = temp_file("cwd", content);
        assert!(parse_transcript(&f, "/proj").is_none());
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn parse_transcript_prefers_ai_title_and_skips_sidechain() {
        let content = concat!(
            r#"{"type":"ai-title","aiTitle":"My Title"}"#,
            "\n",
            r#"{"type":"user","cwd":"/proj","isSidechain":true,"message":{"role":"user","content":"sidechain"}}"#,
            "\n",
            r#"{"type":"user","cwd":"/proj","message":{"role":"user","content":"real"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#,
            "\n",
        );
        let f = temp_file("title", content);
        let p = parse_transcript(&f, "/proj").unwrap();
        assert_eq!(p.title, "My Title");
        // The sidechain user line is skipped: only the real user + assistant logged.
        assert_eq!(p.log.lines().count(), 2);
        let _ = fs::remove_file(&f);
    }
}
