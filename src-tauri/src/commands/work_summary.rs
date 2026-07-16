//! Work Summary — one-shot LLM digest of the work done across a date range.
//!
//! Where `work_digest` only aggregates metadata (durations, tokens, cost),
//! this command feeds the actual session transcripts to Claude and asks for a
//! plain-English work report. It reuses the same run selection (date range +
//! optional project multi-select) as `get_work_digest`.
//!
//! A single session often contains several distinct tasks, so the prompt asks
//! the model to break each session into the tasks it actually accomplished and
//! group everything by project.
//!
//! The Claude engine is driven through the existing Agent-SDK sidecar in a
//! non-interactive, single-turn mode: `permissionMode: bypassPermissions` +
//! `allowedTools: []` so no tools run and no permission modal ever fires. We
//! send one prompt, close stdin, and drain the assistant text until the sidecar
//! exits.

use crate::commands::work_digest::WorkDigestFilter;
use crate::db::Db;
use crate::runs::sidecar::{augment_command_path, detach_process_group, resolve_sidecar};
use serde_json::Value;
use sqlx::{QueryBuilder, Row, Sqlite};
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, State};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// Per-session transcript budget (chars). Keeps a chatty session from crowding
/// out the others in the prompt.
const PER_SESSION_CHARS: usize = 8_000;
/// Overall transcript budget (chars) across all selected runs.
const TOTAL_CHARS: usize = 80_000;
/// Hard ceiling on how long we wait for the one-shot summary to finish.
const SUMMARY_TIMEOUT_SECS: u64 = 240;

/// One run's extracted, compacted conversation plus display metadata.
struct RunDoc {
    project_name: String,
    label: String,
    started_at: Option<String>,
    text: String,
}

#[tauri::command]
pub async fn summarize_work_digest(
    app: AppHandle,
    db: State<'_, Db>,
    filter: WorkDigestFilter,
) -> Result<String, String> {
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

    // ── extract & compact each run's conversation ─────────────────────────────
    let mut docs: Vec<RunDoc> = Vec::new();
    let mut cwd: Option<String> = None;
    let mut total = 0usize;

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

        let text = extract_conversation(
            transcript_path.as_deref(),
            &project_path,
            &id,
            PER_SESSION_CHARS,
        );
        if text.trim().is_empty() {
            continue;
        }
        if total >= TOTAL_CHARS {
            break;
        }
        total += text.len();

        docs.push(RunDoc {
            project_name,
            label: label_for(&run_type, ref_number, title.as_deref()),
            started_at,
            text,
        });
    }

    if docs.is_empty() {
        return Err(
            "No session transcripts found in this range to summarize.".into(),
        );
    }

    let Some(cwd) = cwd else {
        return Err("No valid project directory to run the summarizer in.".into());
    };

    // ── build the prompt ──────────────────────────────────────────────────────
    let document = build_document(&docs);
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

    let result = tokio::time::timeout(
        Duration::from_secs(SUMMARY_TIMEOUT_SECS),
        run_summary_sidecar(
            &app,
            &node_path,
            &sidecar_path,
            &claude_path,
            &claude_model,
            &cwd,
            &prompt,
        ),
    )
    .await
    .map_err(|_| "Timed out while generating the summary.".to_string())??;

    let trimmed = result.trim();
    if trimmed.is_empty() {
        return Err("The summarizer returned an empty response.".into());
    }
    Ok(trimmed.to_string())
}

/// System-style instructions prepended to the transcript document.
const PROMPT_INSTRUCTIONS: &str = "\
You are a work-log summarizer. Below are transcripts of AI coding sessions, \
grouped by project. Write a concise work report in ENGLISH describing what was \
accomplished. A single session frequently contains several distinct tasks — \
identify each task separately, do not merge unrelated work into one line.

Output GitHub-flavored Markdown only, with this structure:
- A short one-paragraph overview of the period.
- One `##` heading per project.
- Under each project, a bullet list where each bullet is a concrete task that \
was done (a feature added, a bug fixed, a refactor, a file changed, a decision \
made). Keep each bullet to one factual sentence.

Rules: be factual and specific — prefer naming the files, features, or bugs \
touched. Do not invent work that is not in the transcripts. Do not use any \
tools; just write the report. Do not include a preamble like \"Here is the \
report\" — start directly with the overview.";

/// Human-readable label for a run inside the prompt document.
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

/// Assemble the per-project, per-session prompt document from the extracted
/// runs (already ordered ascending by time from the query).
fn build_document(docs: &[RunDoc]) -> String {
    use std::collections::BTreeMap;
    // Group by project name, preserving insertion order of first appearance.
    let mut order: Vec<String> = Vec::new();
    let mut by_project: BTreeMap<String, Vec<&RunDoc>> = BTreeMap::new();
    for d in docs {
        if !by_project.contains_key(&d.project_name) {
            order.push(d.project_name.clone());
        }
        by_project.entry(d.project_name.clone()).or_default().push(d);
    }

    let mut out = String::new();
    for project in &order {
        out.push_str(&format!("# Project: {project}\n\n"));
        if let Some(runs) = by_project.get(project) {
            for d in runs {
                let when = d.started_at.as_deref().unwrap_or("");
                out.push_str(&format!("## {} ({})\n\n", d.label, when));
                out.push_str(&d.text);
                out.push_str("\n\n");
            }
        }
    }
    out
}

/// Read a run's transcript (claude JSONL or the `.devdy` run log) and produce a
/// compacted `User:` / `Assistant:` transcript, dropping tool traffic and
/// sidechains, capped at `budget` chars.
fn extract_conversation(
    transcript_path: Option<&str>,
    project_path: &str,
    run_id: &str,
    budget: usize,
) -> String {
    let mut text = extract_from_file(transcript_path, budget);
    if text.trim().is_empty() {
        let conv = Path::new(project_path)
            .join(".devdy")
            .join("runs")
            .join(format!("{run_id}.log"));
        text = extract_from_file(Some(&conv.to_string_lossy()), budget);
    }
    text
}

fn extract_from_file(path: Option<&str>, budget: usize) -> String {
    let Some(path) = path.filter(|p| !p.is_empty()) else {
        return String::new();
    };
    let Ok(content) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let mut out = String::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // Skip refinement/tool sidechains.
        if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) {
            continue;
        }
        let t = v.get("type").and_then(|x| x.as_str()).unwrap_or("");
        if t != "user" && t != "assistant" {
            continue;
        }
        let Some(msg) = v.get("message") else { continue };
        let Some(txt) = message_text(msg) else { continue };
        let txt = txt.trim();
        if txt.is_empty() {
            continue;
        }
        let role = if t == "user" { "User" } else { "Assistant" };
        out.push_str(role);
        out.push_str(": ");
        out.push_str(txt);
        out.push('\n');
        if out.len() >= budget {
            out.truncate(budget);
            out.push_str("\n…[truncated]");
            break;
        }
    }
    out
}

/// Extract plain text from a message's `content` (a string, or a block array
/// where only `text` blocks contribute — `tool_use` / `tool_result` are noise).
fn message_text(msg: &Value) -> Option<String> {
    let content = msg.get("content")?;
    if let Some(s) = content.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = content.as_array() {
        let mut parts: Vec<String> = Vec::new();
        for block in arr {
            if block.get("type").and_then(|v| v.as_str()) == Some("text") {
                if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() {
                        parts.push(t.to_string());
                    }
                }
            }
        }
        if !parts.is_empty() {
            return Some(parts.join("\n"));
        }
    }
    None
}

/// Spawn the Claude Agent-SDK sidecar for a single, tool-free turn and return
/// the concatenated assistant text.
async fn run_summary_sidecar(
    app: &AppHandle,
    node_path: &str,
    sidecar_path: &str,
    claude_path: &str,
    claude_model: &str,
    cwd: &str,
    prompt: &str,
) -> Result<String, String> {
    let (node_bin, sidecar_script) = resolve_sidecar(app, node_path, sidecar_path)?;
    let mut cmd = tokio::process::Command::new(&node_bin);
    cmd.current_dir(cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    if claude_path != "claude" && !claude_path.trim().is_empty() {
        cmd.env("DEVDY_CLAUDE_PATH", claude_path);
    }
    // No plan-usage polling for this lightweight one-shot.
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

    // One tool-free, auto-approved turn.
    let mut options = serde_json::json!({
        "cwd": cwd,
        "permissionMode": "bypassPermissions",
        "allowedTools": [],
        "includePartialMessages": false,
    });
    if !claude_model.trim().is_empty() {
        options["model"] = Value::String(claude_model.trim().to_string());
    }
    let first = serde_json::json!({
        "type": "prompt",
        "text": prompt,
        "options": options,
    });
    stdin
        .write_all(format!("{first}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    stdin.flush().await.ok();
    // Single turn: close input so the query loop finishes after the answer.
    stdin.write_all(b"{\"type\":\"end_input\"}\n").await.ok();
    stdin.flush().await.ok();
    drop(stdin);

    // Drain stdout: collect assistant text, surface a fatal sidecar error.
    let mut answer = String::new();
    let mut sidecar_error: Option<String> = None;
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|x| x.as_str()) {
            Some("assistant") => {
                if let Some(msg) = v.get("message") {
                    if let Some(t) = message_text(msg) {
                        answer.push_str(&t);
                    }
                }
            }
            Some("_devdy_error") => {
                sidecar_error = v
                    .get("error")
                    .and_then(|e| e.as_str())
                    .map(|s| s.to_string());
            }
            _ => {}
        }
    }

    let _ = child.wait().await;

    if answer.trim().is_empty() {
        if let Some(err) = sidecar_error {
            return Err(format!("Summarizer failed: {err}"));
        }
    }
    Ok(answer)
}
