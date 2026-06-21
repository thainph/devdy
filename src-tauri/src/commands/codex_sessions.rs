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
    Ok(changed)
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
    fn rollout_meta_reads_first_line_only() {
        let f = temp_file("meta", ROLLOUT);
        let (cwd, id) = rollout_meta(&f).expect("meta");
        assert_eq!(cwd, "/proj");
        assert_eq!(id, "thread-1");
        let _ = fs::remove_file(&f);
    }
}
