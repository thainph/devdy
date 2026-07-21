//! Mirroring Codex sessions into Devdy — the Codex counterpart of [`sessions`].
//!
//! `codex app-server` (spawned by Devdy), the `codex` CLI, and the Codex VS Code
//! extension all persist conversations to the SAME rollout store under
//! `~/.codex/sessions/<YYYY>/<MM>/<DD>/rollout-<ts>-<thread-id>.jsonl`. Resuming a
//! thread (including across processes) APPENDS to the same file — verified — so a
//! rollout maps 1:1 to a Devdy run via the thread id Devdy stores as `session_id`.
//!
//! Unlike Claude (whose transcript already uses Devdy's `{type,message}` shape and
//! whose dir name IS the project), Codex needs:
//!   1. a translator from its rollout vocabulary into Devdy stream-json, and
//!   2. reading each file's `session_meta.cwd` to map it to a project (the tree is
//!      organized by date, not project).
//! Everything after parsing reuses the engine-agnostic core in [`sessions`].

use super::sessions::{
    upsert_session_run_core, ParsedTranscript, SyncOutcome, UsageTotals,
};
use crate::db::Db;
use chrono::TimeZone;
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use tauri::State;

/// `~/.codex/sessions`, the root of the rollout store.
pub(crate) fn codex_sessions_root() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = Path::new(&home).join(".codex").join("sessions");
    dir.is_dir().then_some(dir)
}

/// Read just the first line of a rollout to get `(cwd, thread_id)` without
/// parsing the whole conversation — used to map a file to a project.
///
/// Returns `None` for subagent/forked rollouts. Codex's multi-agent mode
/// (`multi_agent_version: v2`, triggered e.g. by slash commands) spawns child
/// threads that each write their OWN rollout file with the SAME `cwd` as the
/// parent. Since Devdy maps a rollout to a project purely by `cwd`, importing
/// these forks would surface one conversation as several duplicate sessions
/// after a restart. Devdy only ever spawns top-level threads (`thread/start`,
/// `thread_source: "user"`), so skipping subagent/fork rollouts is safe.
pub(crate) fn rollout_meta(file: &Path) -> Option<(String, String)> {
    // Only the first non-empty line is needed, so stream it rather than reading
    // the whole rollout (which can be large) into memory.
    let reader = BufReader::new(fs::File::open(file).ok()?);
    let first = reader
        .lines()
        .map_while(Result::ok)
        .find(|l| !l.trim().is_empty())?;
    let v: Value = serde_json::from_str(&first).ok()?;
    if v.get("type").and_then(|x| x.as_str()) != Some("session_meta") {
        return None;
    }
    let p = v.get("payload")?;
    // Skip subagent spawns / forks — they belong to their parent thread, not a
    // standalone session.
    if p.get("thread_source").and_then(|x| x.as_str()) == Some("subagent")
        || p.get("parent_thread_id").is_some()
        || p.get("forked_from_id").is_some()
    {
        return None;
    }
    let cwd = p.get("cwd").and_then(|x| x.as_str())?.to_string();
    let id = p.get("id").and_then(|x| x.as_str())?.to_string();
    Some((cwd, id))
}

/// Walk the date-organized rollout tree and return every `rollout-*.jsonl` path.
fn all_rollout_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                && path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("rollout-"))
            {
                out.push(path);
            }
        }
    }
    out
}

/// Locate the rollout file for a thread id. The id is the filename suffix
/// (`rollout-<ts>-<id>.jsonl`), so match on that rather than reading every file.
pub(crate) fn codex_session_file(session_id: &str) -> Option<PathBuf> {
    let root = codex_sessions_root()?;
    let suffix = format!("-{}.jsonl", session_id);
    all_rollout_files(&root)
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
}

/// Map a Codex `function_call` to a Devdy tool_use `(name, input)`. Shell execs
/// render as `Bash`; everything else (MCP tools, etc.) keeps its own name.
fn map_function_call(name: &str, args: &Value) -> (String, Value) {
    match name {
        "exec_command" | "shell" | "local_shell" => {
            let command = args
                .get("cmd")
                .or_else(|| args.get("command"))
                .cloned()
                .unwrap_or_else(|| args.clone());
            (
                "Bash".to_string(),
                serde_json::json!({ "command": command }),
            )
        }
        "apply_patch" | "edit" | "edit_file" => ("Edit".to_string(), args.clone()),
        other => (other.to_string(), args.clone()),
    }
}

/// Translate a Codex rollout into a Devdy [`ParsedTranscript`].
///
/// Text turns come from the clean `event_msg` user/agent messages; tool calls
/// from `response_item` `function_call`/`function_call_output`. Codex `reasoning`
/// is encrypted (no plaintext) so it is omitted. Token usage is the last
/// cumulative `token_count` (mapped like the live sidecar's `mapCodexUsage`).
pub(crate) fn parse_codex_rollout(file: &Path, project_path: &str) -> Option<ParsedTranscript> {
    let raw = fs::read_to_string(file).ok()?;

    let mut log = String::new();
    let mut first_user: Option<String> = None;
    let mut model: Option<String> = None;
    let mut turn_count = 0i64;
    let mut usage = UsageTotals::default();
    let mut cwd_matches = false;

    let mut push = |obj: Value| {
        log.push_str(&serde_json::to_string(&obj).unwrap_or_default());
        log.push('\n');
    };

    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let payload = v.get("payload");
        // Capture cwd (project match) and a model id wherever they first appear.
        if let Some(p) = payload {
            if !cwd_matches {
                if let Some(c) = p.get("cwd").and_then(|x| x.as_str()) {
                    cwd_matches = c == project_path;
                }
            }
            if model.is_none() {
                if let Some(m) = p.get("model").and_then(|x| x.as_str()) {
                    model = Some(m.to_string());
                }
            }
        }

        match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "event_msg" => {
                let Some(p) = payload else { continue };
                match p.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "user_message" => {
                        if let Some(text) = p.get("message").and_then(|x| x.as_str()) {
                            let text = text.trim();
                            if !text.is_empty() {
                                if first_user.is_none() {
                                    first_user = Some(text.to_string());
                                }
                                push(serde_json::json!({
                                    "type": "user",
                                    "message": { "role": "user", "content": text },
                                }));
                            }
                        }
                    }
                    "agent_message" => {
                        if let Some(text) = p.get("message").and_then(|x| x.as_str()) {
                            let text = text.trim();
                            if !text.is_empty() {
                                turn_count += 1;
                                push(serde_json::json!({
                                    "type": "assistant",
                                    "message": { "role": "assistant",
                                        "content": [{ "type": "text", "text": text }] },
                                }));
                            }
                        }
                    }
                    "token_count" => {
                        // Cumulative session total; the last one wins.
                        if let Some(info) = p.get("info") {
                            let tot = info.get("total_token_usage").unwrap_or(info);
                            let g = |k: &str| tot.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
                            let cached = g("cached_input_tokens");
                            let input_total = g("input_tokens");
                            usage = UsageTotals {
                                input: (input_total - cached).max(0),
                                output: g("output_tokens") + g("reasoning_output_tokens"),
                                cache_creation: 0,
                                cache_read: cached,
                            };
                        }
                    }
                    _ => {}
                }
            }
            "response_item" => {
                let Some(p) = payload else { continue };
                match p.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                    "function_call" => {
                        let name = p.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
                        let call_id = p.get("call_id").and_then(|x| x.as_str()).unwrap_or("");
                        // `arguments` is a JSON-encoded string.
                        let args: Value = p
                            .get("arguments")
                            .and_then(|x| x.as_str())
                            .and_then(|s| serde_json::from_str(s).ok())
                            .unwrap_or(Value::Null);
                        let (tool_name, input) = map_function_call(name, &args);
                        push(serde_json::json!({
                            "type": "assistant",
                            "message": { "role": "assistant", "content": [
                                { "type": "tool_use", "id": call_id, "name": tool_name, "input": input }
                            ] },
                        }));
                    }
                    "function_call_output" => {
                        let call_id = p.get("call_id").and_then(|x| x.as_str()).unwrap_or("");
                        let output = p
                            .get("output")
                            .and_then(|x| x.as_str())
                            .map(str::to_string)
                            .unwrap_or_else(|| p.get("output").map(|v| v.to_string()).unwrap_or_default());
                        // Best-effort error flag from the shell exit footer.
                        let is_error = output.contains("exited with code")
                            && !output.contains("exited with code 0");
                        push(serde_json::json!({
                            "type": "user",
                            "message": { "role": "user", "content": [
                                { "type": "tool_result", "tool_use_id": call_id,
                                  "content": output, "is_error": is_error }
                            ] },
                        }));
                    }
                    // message (duplicates event_msg text) and reasoning (encrypted)
                    // are intentionally skipped.
                    _ => {}
                }
            }
            _ => {}
        }
    }

    if !cwd_matches || log.is_empty() {
        return None;
    }

    let title = super::sessions::title_from_first_message(first_user.as_deref());

    Some(ParsedTranscript {
        log,
        title,
        model,
        turn_count,
        usage,
    })
}

/// Codex upsert when the rollout path is already known (the watcher has it) —
/// avoids re-walking the whole `~/.codex/sessions` tree to locate the file.
pub(crate) async fn upsert_codex_session_file(
    db: &sqlx::SqlitePool,
    project_id: &str,
    project_name: &str,
    project_path: &str,
    session_id: &str,
    file: &Path,
    defer_when_busy: bool,
) -> Result<SyncOutcome, String> {
    upsert_session_run_core(
        db,
        project_id,
        project_name,
        project_path,
        session_id,
        "codex",
        file,
        parse_codex_rollout,
        defer_when_busy,
    )
    .await
}

/// Reconcile every Codex rollout whose `cwd` matches a project into Devdy runs.
/// Scans the rollout tree once, reading only each file's first line to match the
/// project, then upserts the matching threads. Returns how many runs changed.
#[tauri::command]
pub async fn reconcile_codex_sessions(
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

    let Some(root) = codex_sessions_root() else {
        return Ok(0);
    };

    let mut changed = 0i64;
    for file in all_rollout_files(&root) {
        let Some((cwd, thread_id)) = rollout_meta(&file) else {
            continue;
        };
        if cwd != project_path {
            continue;
        }
        match upsert_codex_session_file(
            db.inner(),
            &project_id,
            &project_name,
            &project_path,
            &thread_id,
            &file,
            true,
        )
        .await
        {
            Ok(SyncOutcome::Imported(_)) | Ok(SyncOutcome::Updated(_)) => changed += 1,
            _ => {}
        }
    }

    // Populate the Codex plan-usage snapshot from the newest rollout so the
    // budget badge / settings panel show a % even before the first live run.
    if let Some(rl) = latest_codex_rate_limits_any(&root) {
        persist_codex_plan_usage(db.inner(), &rl).await;
    }
    Ok(changed)
}

// ── Codex plan-usage (rate-limit) capture ─────────────────────────────────
//
// Codex exposes the account's plan rate-limits (the "% used" behind `/status`)
// via the app-server `account/rateLimits/read` RPC and writes the same data into
// each rollout's `token_count` events. Both shapes are normalized here into the
// SAME `plan_usage` snapshot the Claude path stores, under the settings key
// `plan_usage_codex`, so the badge / settings / budget logic are shared.

/// One RateLimitWindow field, tolerating both the app-server camelCase
/// (`usedPercent`, `windowDurationMins`, `resetsAt`) and the rollout snake_case
/// (`used_percent`, `window_minutes`, `resets_at`).
fn cx_num(w: &Value, camel: &str, snake: &str) -> Option<f64> {
    w.get(camel).and_then(|v| v.as_f64()).or_else(|| w.get(snake).and_then(|v| v.as_f64()))
}

/// Build a normalized plan-usage window `{ utilization, resets_at, status, status_at }`
/// from a codex RateLimitWindow, converting the unix-seconds reset to RFC3339.
fn cx_window(w: &Value) -> Value {
    let util = cx_num(w, "usedPercent", "used_percent");
    let resets_at = cx_num(w, "resetsAt", "resets_at")
        .and_then(|ts| chrono::Utc.timestamp_opt(ts as i64, 0).single())
        .map(|dt| dt.to_rfc3339());
    serde_json::json!({
        "utilization": util,
        "resets_at": resets_at,
        "status": null,
        "status_at": null,
    })
}

/// Convert a codex RateLimitSnapshot (`{ primary, secondary, planType, ... }`)
/// into the shared plan-usage snapshot. Windows are routed by their duration:
/// ≤ 1 day → the rolling 5h window, else the weekly window (codex reports them
/// as `primary`/`secondary` in no fixed order). Returns `None` when neither
/// window carries a usable `%`.
pub(crate) fn codex_rate_limits_to_snapshot(rl: &Value) -> Option<Value> {
    // Accept either the bare snapshot or the `{ rateLimits: {...} }` wrapper.
    let rl = rl.get("rateLimits").unwrap_or(rl);
    let mut five_hour = serde_json::json!({ "utilization": null, "resets_at": null, "status": null, "status_at": null });
    let mut seven_day = five_hour.clone();
    let mut have = false;

    for key in ["primary", "secondary"] {
        let Some(w) = rl.get(key).filter(|v| v.is_object()) else { continue };
        if cx_num(w, "usedPercent", "used_percent").is_none() {
            continue;
        }
        let mins = cx_num(w, "windowDurationMins", "window_minutes").unwrap_or(0.0);
        if mins > 0.0 && mins <= 1440.0 {
            five_hour = cx_window(w);
        } else {
            seven_day = cx_window(w);
        }
        have = true;
    }
    if !have {
        return None;
    }

    let plan_type = rl
        .get("planType")
        .or_else(|| rl.get("plan_type"))
        .and_then(|v| v.as_str());
    Some(serde_json::json!({
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "subscription_type": plan_type,
        "rate_limits_available": true,
        "windows": {
            "five_hour": five_hour,
            "seven_day": seven_day,
            "seven_day_opus": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
            "seven_day_sonnet": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
        },
    }))
}

/// Scan a rollout for the LAST `token_count` event that carries `rate_limits`.
pub(crate) fn latest_codex_rate_limits(file: &Path) -> Option<Value> {
    let f = fs::File::open(file).ok()?;
    let mut latest: Option<Value> = None;
    for line in BufReader::new(f).lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() || !line.contains("rate_limits") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        let p = v.get("payload").unwrap_or(&v);
        if p.get("type").and_then(|t| t.as_str()) != Some("token_count") {
            continue;
        }
        if let Some(rl) = p.get("rate_limits").filter(|x| x.is_object()) {
            latest = Some(rl.clone());
        }
    }
    latest
}

/// Newest rollout across the whole store that has usable rate-limits — used at
/// reconcile time to seed the snapshot regardless of which project it came from.
fn latest_codex_rate_limits_any(root: &Path) -> Option<Value> {
    let mut files = all_rollout_files(root);
    files.sort();
    for file in files.into_iter().rev() {
        if let Some(rl) = latest_codex_rate_limits(&file) {
            if codex_rate_limits_to_snapshot(&rl).is_some() {
                return Some(rl);
            }
        }
    }
    None
}

/// Persist a codex RateLimitSnapshot as the latest `plan_usage_codex` state.
/// No-op (returns false) when the snapshot has no usable window.
pub(crate) async fn persist_codex_plan_usage(db_pool: &sqlx::SqlitePool, rl: &Value) -> bool {
    let Some(snapshot) = codex_rate_limits_to_snapshot(rl) else {
        return false;
    };
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('plan_usage_codex', ?)")
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_file(name: &str, content: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("devdy_codex_test_{}_{}.jsonl", name, std::process::id()));
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    const ROLLOUT: &str = concat!(
        r#"{"type":"session_meta","payload":{"cwd":"/proj","id":"thread-1","model":"gpt-x"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"user_message","message":"Build me a thing"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"agent_message","message":"Sure"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c1","arguments":"{\"command\":\"ls\"}"}}"#,
        "\n",
        r#"{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"file.txt"}}"#,
        "\n",
        r#"{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":100,"cached_input_tokens":20,"output_tokens":30,"reasoning_output_tokens":5}}}}"#,
        "\n",
    );

    #[test]
    fn parse_codex_rollout_translates_turns_tools_usage() {
        let f = temp_file("basic", ROLLOUT);
        let p = parse_codex_rollout(&f, "/proj").expect("should parse");
        assert_eq!(p.turn_count, 1); // one agent_message
        assert_eq!(p.model.as_deref(), Some("gpt-x"));
        assert_eq!(p.usage.input, 80); // 100 input - 20 cached
        assert_eq!(p.usage.cache_read, 20);
        assert_eq!(p.usage.output, 35); // 30 output + 5 reasoning
        assert!(p.title.contains("Build"));
        // user + agent text + tool_use + tool_result = 4 log lines.
        assert_eq!(p.log.lines().count(), 4);
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn parse_codex_rollout_rejects_wrong_cwd() {
        let f = temp_file("cwd", ROLLOUT);
        assert!(parse_codex_rollout(&f, "/somewhere-else").is_none());
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn codex_rate_limits_routes_windows_by_duration() {
        // app-server camelCase shape: primary is the weekly window (10080 min),
        // secondary is the 5h window (300 min) — routed by duration, not name.
        let rl = serde_json::json!({
            "primary": { "usedPercent": 38.0, "windowDurationMins": 10080, "resetsAt": 1785213453i64 },
            "secondary": { "usedPercent": 12.0, "windowDurationMins": 300, "resetsAt": 1782373251i64 },
            "planType": "team",
        });
        let snap = codex_rate_limits_to_snapshot(&rl).expect("snapshot");
        assert_eq!(snap["subscription_type"], "team");
        assert_eq!(snap["rate_limits_available"], true);
        assert_eq!(snap["windows"]["five_hour"]["utilization"], 12.0);
        assert_eq!(snap["windows"]["seven_day"]["utilization"], 38.0);
        assert_eq!(snap["windows"]["seven_day_opus"]["utilization"], Value::Null);
        // resets_at converted from unix seconds to RFC3339.
        assert!(snap["windows"]["seven_day"]["resets_at"].as_str().unwrap().starts_with("20"));
    }

    #[test]
    fn codex_rate_limits_snake_case_and_null_secondary() {
        // Rollout snake_case shape with only a weekly window populated.
        let rl = serde_json::json!({
            "primary": { "used_percent": 77.0, "window_minutes": 10080, "resets_at": 1776707194i64 },
            "secondary": null,
            "plan_type": "free",
        });
        let snap = codex_rate_limits_to_snapshot(&rl).expect("snapshot");
        assert_eq!(snap["subscription_type"], "free");
        assert_eq!(snap["windows"]["seven_day"]["utilization"], 77.0);
        assert_eq!(snap["windows"]["five_hour"]["utilization"], Value::Null);
    }

    #[test]
    fn codex_rate_limits_empty_returns_none() {
        let rl = serde_json::json!({ "primary": null, "secondary": null, "planType": "pro" });
        assert!(codex_rate_limits_to_snapshot(&rl).is_none());
    }

    #[test]
    fn rollout_meta_reads_first_line_only() {
        let f = temp_file("meta", ROLLOUT);
        let (cwd, id) = rollout_meta(&f).expect("meta");
        assert_eq!(cwd, "/proj");
        assert_eq!(id, "thread-1");
        let _ = fs::remove_file(&f);
    }

    #[test]
    fn rollout_meta_skips_subagent_and_fork_threads() {
        // A subagent spawn (multi-agent v2) shares the parent's cwd but must not
        // be imported as its own session.
        let subagent = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/proj","id":"thread-2","thread_source":"subagent","parent_thread_id":"thread-1","forked_from_id":"thread-1"}}"#,
            "\n",
        );
        let f = temp_file("subagent", subagent);
        assert!(rollout_meta(&f).is_none());
        let _ = fs::remove_file(&f);

        // A plain fork (parent_thread_id present, no explicit subagent source).
        let fork = concat!(
            r#"{"type":"session_meta","payload":{"cwd":"/proj","id":"thread-3","parent_thread_id":"thread-1"}}"#,
            "\n",
        );
        let f = temp_file("fork", fork);
        assert!(rollout_meta(&f).is_none());
        let _ = fs::remove_file(&f);
    }
}
