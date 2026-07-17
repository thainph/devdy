//! Work Summary — one-shot LLM digest of the work done across a date range.
//!
//! Where `work_digest` only aggregates metadata (durations, tokens, cost),
//! this command asks Claude for a plain-English work report. It reuses the same
//! run selection (date range + optional project multi-select) as
//! `get_work_digest`.
//!
//! Approach: rather than dumping transcript text into the prompt, we hand Claude
//! the LIST OF TRANSCRIPT FILE PATHS and let it open them with the `Read` tool
//! (plus `Grep`/`Glob` to navigate large files). This keeps the prompt tiny and
//! lets the model pull only what it needs. The run is driven through the
//! existing Agent-SDK sidecar with `permissionMode: bypassPermissions` so the
//! `Read`/`Grep`/`Glob` tools auto-approve (no permission modal), and
//! `allowedTools` whitelists exactly those read-only tools so nothing can mutate
//! the repo.
//!
//! A single session often contains several distinct tasks, so the prompt asks
//! the model to break each session into the tasks it actually accomplished and
//! group everything by project.
//!
//! Progress streams to the frontend as `work_summary:*` Tauri events: a synthetic
//! `user` preview, then the raw stream-json SDK messages (so the same StreamLog
//! component the project "AI result" view uses can render the tool activity),
//! then `done` / `error`.

use crate::commands::work_digest::WorkDigestFilter;
use crate::db::Db;
use crate::runs::sidecar::{augment_command_path, detach_process_group, resolve_sidecar};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex as TokioMutex;

/// Cap on how many transcript files we hand to the model in one go.
const MAX_SESSIONS: usize = 80;
/// Cap on each condensed per-run digest file (chars).
const PER_RUN_CHARS: usize = 16_000;

/// One run's transcript file path plus display metadata.
struct RunDoc {
    project_name: String,
    label: String,
    started_at: Option<String>,
    path: String,
}

/// Shared handle to the currently-running summary sidecar so a new request (or
/// an explicit cancel) can tear the previous one down, and a `generation`
/// counter so a stale drain task never emits over a newer run.
#[derive(Default)]
pub struct WorkSummaryState {
    child: Arc<TokioMutex<Option<tokio::process::Child>>>,
    generation: Arc<AtomicU64>,
}

/// Cancel the in-flight summary (if any). Bumps the generation so its drain
/// task stops emitting, then kills the sidecar process.
#[tauri::command]
pub async fn cancel_work_summary(state: State<'_, WorkSummaryState>) -> Result<(), String> {
    state.generation.fetch_add(1, Ordering::SeqCst);
    if let Some(mut child) = state.child.lock().await.take() {
        let _ = child.start_kill();
    }
    Ok(())
}

#[tauri::command]
pub async fn summarize_work_digest(
    app: AppHandle,
    db: State<'_, Db>,
    state: State<'_, WorkSummaryState>,
    filter: WorkDigestFilter,
) -> Result<(), String> {
    // Explicit empty project selection → nothing to summarize.
    if matches!(filter.project_ids.as_ref(), Some(ids) if ids.is_empty()) {
        return Err("No project selected.".into());
    }

    // ── select the same runs get_work_digest would show ──────────────────────
    let mut qb = QueryBuilder::<Sqlite>::new(
        "SELECT r.id, p.name AS project_name, r.type, r.ref_number, r.title, \
         r.transcript_path, r.started_at, r.created_at, p.path AS project_path \
         FROM runs r JOIN projects p ON p.id = r.project_id WHERE 1 = 1",
    );
    if let Some(f) = filter.from.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(COALESCE(r.started_at, r.created_at), 1, 10) >= ")
            .push_bind(f.clone());
    }
    if let Some(t) = filter.to.as_ref().filter(|s| !s.is_empty()) {
        qb.push(" AND substr(COALESCE(r.started_at, r.created_at), 1, 10) <= ")
            .push_bind(t.clone());
    }
    if let Some(ids) = filter.project_ids.as_ref().filter(|v| !v.is_empty()) {
        qb.push(" AND r.project_id IN (");
        let mut sep = qb.separated(", ");
        for id in ids {
            sep.push_bind(id.clone());
        }
        qb.push(")");
    }
    qb.push(" ORDER BY COALESCE(r.started_at, r.created_at) ASC");

    let rows = qb
        .build()
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    // ── condense each run's log into a compact digest file ────────────────────
    // The raw `.devdy/runs/{id}.log` (stream-json) is long and noisy, so we
    // distill it into a small markdown digest (user asks + assistant text +
    // one-line tool actions, no tool_result payloads) and hand Claude those.
    let out_dir = std::env::temp_dir().join("devdy-work-summary");
    let _ = std::fs::remove_dir_all(&out_dir);
    std::fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;

    let mut docs: Vec<RunDoc> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut truncated = false;

    for row in &rows {
        let id: String = row.get("id");
        let project_name: String = row.get("project_name");
        let run_type: String = row.get("type");
        let ref_number: Option<i64> = row.get("ref_number");
        let title: Option<String> = row.get("title");
        let transcript_path: Option<String> = row.get("transcript_path");
        let started_at: Option<String> = row.get("started_at");
        let project_path: String = row.get("project_path");

        if cwd.is_none() && Path::new(&project_path).is_dir() {
            cwd = Some(project_path.clone());
        }

        let Some(src) = resolve_transcript_path(transcript_path.as_deref(), &project_path, &id)
        else {
            continue;
        };
        if docs.len() >= MAX_SESSIONS {
            truncated = true;
            break;
        }
        let label = label_for(&run_type, ref_number, title.as_deref());
        let Some(path) = condense_log(&src, &out_dir, &id, &project_name, &label, PER_RUN_CHARS)
        else {
            continue;
        };
        docs.push(RunDoc {
            project_name,
            label,
            started_at,
            path,
        });
    }

    if docs.is_empty() {
        return Err("No session transcripts found in this range to summarize.".into());
    }

    let Some(cwd) = cwd else {
        return Err("No valid project directory to run the summarizer in.".into());
    };

    // ── build the prompt (file list, not content) ─────────────────────────────
    let document = build_document(&docs, truncated);
    let prompt = format!("{PROMPT_INSTRUCTIONS}\n\n{document}");

    // ── settings needed to spawn the Claude sidecar ───────────────────────────
    let settings_rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut claude_path = String::new();
    let mut claude_model = String::new();
    for row in &settings_rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "claude_path" => claude_path = value,
            "claude_model" => claude_model = value,
            _ => {}
        }
    }

    // ── spawn the sidecar and stream results via events ───────────────────────
    // A new request supersedes any in-flight one: bump the generation (so the
    // old drain stops emitting) and kill the previous child.
    let my_gen = state.generation.fetch_add(1, Ordering::SeqCst) + 1;
    if let Some(mut old) = state.child.lock().await.take() {
        let _ = old.start_kill();
    }

    // Tell the UI what we're about to ask — the readable prompt (instructions +
    // the list of transcript files the model will read).
    let _ = app.emit("work_summary:user", serde_json::json!({ "text": prompt }));

    let (node_bin, sidecar_script) = resolve_sidecar(&app, &node_path, &sidecar_path)?;
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(&cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if claude_path != "claude" && !claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
    }
    cmd.env("DEVDY_USAGE_CAPTURE_MODE", "normal");
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar ({node_bin}): {e}"))?;
    let stdout = child.stdout.take().ok_or("no stdout")?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;

    // One read-only, auto-approved turn: the model may Read/Grep/Glob the
    // transcript files but nothing else, and no permission modal fires.
    let mut options = serde_json::json!({
        "cwd": cwd,
        "permissionMode": "bypassPermissions",
        "allowedTools": ["Read", "Grep", "Glob"],
        "includePartialMessages": false,
    });
    if !claude_model.trim().is_empty() {
        options["model"] = Value::String(claude_model.trim().to_string());
    }
    let first = serde_json::json!({ "type": "prompt", "text": prompt, "options": options });
    stdin
        .write_all(format!("{first}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.ok();
    // Single turn: close input so the query loop finishes after the answer.
    stdin.write_all(b"{\"type\":\"end_input\"}\n").await.ok();
    stdin.flush().await.ok();
    drop(stdin);

    // Store the child so cancel / a superseding request can kill it.
    *state.child.lock().await = Some(child);

    // Drain in the background; the command returns immediately and the frontend
    // follows progress through `work_summary:*` events.
    let app_bg = app.clone();
    let gen_arc = state.generation.clone();
    let child_arc = state.child.clone();
    tokio::spawn(async move {
        drain_summary(app_bg, stdout, gen_arc, my_gen, child_arc, out_dir).await;
    });

    Ok(())
}

/// Read the sidecar's stdout and forward each raw stream-json SDK message to the
/// frontend as `work_summary:event` (so StreamLog renders the tool activity),
/// then `work_summary:done` / `work_summary:error`. A `generation` guard makes a
/// superseded drain stop emitting silently.
async fn drain_summary(
    app: AppHandle,
    stdout: tokio::process::ChildStdout,
    gen_arc: Arc<AtomicU64>,
    my_gen: u64,
    child_arc: Arc<TokioMutex<Option<tokio::process::Child>>>,
    out_dir: std::path::PathBuf,
) {
    let is_current = |g: &AtomicU64| g.load(Ordering::SeqCst) == my_gen;

    let mut sidecar_error: Option<String> = None;
    let mut produced = false;
    let mut lines = BufReader::new(stdout).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        if !is_current(&gen_arc) {
            return; // superseded/cancelled — stop silently.
        }
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            // Control channel: capture a fatal error, swallow the rest.
            Some("_devdy_error") => {
                sidecar_error = v.get("error").and_then(|e| e.as_str()).map(str::to_string);
            }
            Some(t) if t.starts_with("_devdy_") => {}
            // Raw stream-json SDK message → forward for StreamLog to render.
            Some(t) => {
                if t == "assistant" || t == "result" {
                    produced = true;
                }
                let _ = app.emit("work_summary:event", &v);
            }
            None => {}
        }
    }

    // EOF: reap the child and emit a terminal event (if still current).
    if let Some(mut child) = child_arc.lock().await.take() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    if !is_current(&gen_arc) {
        return;
    }
    if produced {
        let _ = app.emit("work_summary:done", serde_json::json!({}));
    } else {
        let msg = sidecar_error
            .unwrap_or_else(|| "The summarizer returned an empty response.".to_string());
        let _ = app.emit("work_summary:error", serde_json::json!({ "error": msg }));
    }
    // The digests were only needed while Claude was reading them — clean up.
    let _ = std::fs::remove_dir_all(&out_dir);
}

/// System-style instructions prepended to the file list.
const PROMPT_INSTRUCTIONS: &str = "\
You are a work-log summarizer. Below is a list of session digest FILES — each is \
a compact markdown digest of one AI coding session (the user's requests, the \
assistant's replies, and one-line notes of the tool actions taken). Use the \
`Read` tool to open each file and review it; use `Grep` to search across them \
when needed. Do not modify anything.

From the transcripts, write a concise work report in ENGLISH describing what was \
accomplished. A single session frequently contains several distinct tasks — \
identify each task separately, do not merge unrelated work into one line.

Output GitHub-flavored Markdown only, with this structure:
- A short one-paragraph overview of the period.
- One `##` heading per project.
- Under each project, a bullet list where each bullet is a concrete task that \
was done (a feature added, a bug fixed, a refactor, a file changed, a decision \
made). Keep each bullet to one factual sentence.

Rules: be factual and specific — prefer naming the files, features, or bugs \
touched. Do not invent work that is not in the transcripts. Only use the Read, \
Grep, and Glob tools. Do not include a preamble like \"Here is the report\" — \
start directly with the overview.";

/// Human-readable label for a run inside the file list.
fn label_for(run_type: &str, ref_number: Option<i64>, title: Option<&str>) -> String {
    let title = title.map(str::trim).filter(|t| !t.is_empty());
    match run_type {
        "analyze_issue" => match (ref_number, title) {
            (Some(n), Some(t)) => format!("Issue #{n} — {t}"),
            (Some(n), None) => format!("Issue #{n}"),
            (None, Some(t)) => t.to_string(),
            (None, None) => "Issue".to_string(),
        },
        "review_pr" => match (ref_number, title) {
            (Some(n), Some(t)) => format!("PR #{n} — {t}"),
            (Some(n), None) => format!("PR #{n}"),
            (None, Some(t)) => t.to_string(),
            (None, None) => "PR".to_string(),
        },
        _ => title.unwrap_or("Session").to_string(),
    }
}

/// Assemble the per-project file list (grouped, preserving first-seen order).
fn build_document(docs: &[RunDoc], truncated: bool) -> String {
    use std::collections::BTreeMap;
    let mut order: Vec<String> = Vec::new();
    let mut by_project: BTreeMap<String, Vec<&RunDoc>> = BTreeMap::new();
    for d in docs {
        if !by_project.contains_key(&d.project_name) {
            order.push(d.project_name.clone());
        }
        by_project.entry(d.project_name.clone()).or_default().push(d);
    }

    let mut out = String::from("Transcript files to read:\n");
    for project in &order {
        out.push_str(&format!("\n## Project: {project}\n"));
        if let Some(runs) = by_project.get(project) {
            for d in runs {
                let when = d.started_at.as_deref().unwrap_or("");
                out.push_str(&format!("- {} ({}): {}\n", d.label, when, d.path));
            }
        }
    }
    if truncated {
        out.push_str(&format!(
            "\n(Only the first {MAX_SESSIONS} sessions are listed; there were more.)\n"
        ));
    }
    out
}

/// Distill a run's stream-json log into a compact markdown digest and write it
/// to `{out_dir}/run-{id}.md`. Keeps user requests, assistant text, and one-line
/// tool-action notes; drops tool_result payloads, thinking, and JSON noise.
/// Returns the digest path, or `None` when the log has no usable content.
fn condense_log(
    src: &str,
    out_dir: &Path,
    run_id: &str,
    project_name: &str,
    label: &str,
    budget: usize,
) -> Option<String> {
    let content = std::fs::read_to_string(src).ok()?;
    let mut body = String::new();
    let mut truncated = false;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
            continue;
        }
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        let Some(msg) = v.get("message") else { continue };
        let content = msg.get("content");

        match t {
            "user" => {
                let text = content.map(text_of).unwrap_or_default();
                let text = text.trim();
                if !text.is_empty() {
                    body.push_str(&format!("**User:** {}\n\n", truncate_inline(text, 2_000)));
                }
            }
            "assistant" => {
                if let Some(arr) = content.and_then(|c| c.as_array()) {
                    for block in arr {
                        match block.get("type").and_then(|x| x.as_str()).unwrap_or("") {
                            "text" => {
                                if let Some(tx) = block.get("text").and_then(|x| x.as_str()) {
                                    let tx = tx.trim();
                                    if !tx.is_empty() {
                                        body.push_str(&format!("**Assistant:** {tx}\n\n"));
                                    }
                                }
                            }
                            "tool_use" => {
                                if let Some(l) = tool_action_line(block) {
                                    body.push_str(&format!("- {l}\n"));
                                }
                            }
                            _ => {}
                        }
                    }
                } else if let Some(s) = content.and_then(|c| c.as_str()) {
                    let s = s.trim();
                    if !s.is_empty() {
                        body.push_str(&format!("**Assistant:** {s}\n\n"));
                    }
                }
            }
            _ => {}
        }

        if body.len() >= budget {
            truncated = true;
            break;
        }
    }

    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let mut out = format!("# {project_name} — {label}\n\n");
    out.push_str(body);
    if truncated {
        out.push_str("\n\n…[digest truncated]");
    }
    let path = out_dir.join(format!("run-{run_id}.md"));
    std::fs::write(&path, out).ok()?;
    Some(path.to_string_lossy().to_string())
}

/// Concatenate the text of a message `content` (a plain string, or the `text`
/// blocks of a block array). tool_use / tool_result / image blocks are ignored.
fn text_of(content: &Value) -> String {
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    if let Some(arr) = content.as_array() {
        let mut parts: Vec<String> = Vec::new();
        for b in arr {
            if b.get("type").and_then(|x| x.as_str()) == Some("text") {
                if let Some(t) = b.get("text").and_then(|x| x.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
        }
        return parts.join("\n");
    }
    String::new()
}

/// A one-line note for a `tool_use` block: the tool name plus a compact target
/// (file path / grep pattern / first line of a bash command / description).
fn tool_action_line(block: &Value) -> Option<String> {
    let name = block.get("name").and_then(|x| x.as_str()).unwrap_or("tool");
    let detail = block.get("input").and_then(|inp| {
        for k in ["file_path", "notebook_path", "path", "pattern"] {
            if let Some(v) = inp.get(k).and_then(|x| x.as_str()) {
                return Some(v.to_string());
            }
        }
        if let Some(cmd) = inp.get("command").and_then(|x| x.as_str()) {
            return Some(truncate_inline(cmd.lines().next().unwrap_or(cmd), 120));
        }
        if let Some(desc) = inp.get("description").and_then(|x| x.as_str()) {
            return Some(truncate_inline(desc, 120));
        }
        None
    });
    match detail {
        Some(d) if !d.is_empty() => Some(format!("{name} `{d}`")),
        _ => Some(name.to_string()),
    }
}

/// Trim and cap a string to `max` chars, appending `…` when cut.
fn truncate_inline(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

/// The transcript file for a run: prefer the rendered `.devdy/runs/{id}.log`
/// (stream-json `{type, message}` per line, sidechains/tool-noise already
/// dropped), else fall back to the raw native claude JSONL (`transcript_path`).
/// `None` when neither exists on disk.
fn resolve_transcript_path(
    transcript_path: Option<&str>,
    project_path: &str,
    run_id: &str,
) -> Option<String> {
    let conv = Path::new(project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{run_id}.log"));
    if conv.is_file() {
        return Some(conv.to_string_lossy().to_string());
    }
    if let Some(p) = transcript_path.filter(|p| !p.is_empty()) {
        if Path::new(p).is_file() {
            return Some(p.to_string());
        }
    }
    None
}
