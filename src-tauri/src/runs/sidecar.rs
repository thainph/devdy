//! Agent-SDK sidecar integration.
//!
//! Replaces the direct `claude` subprocess for the Claude engine. We spawn the
//! Node sidecar (`sidecar/index.mjs`), which hosts `@anthropic-ai/claude-agent-sdk`
//! and bridges to us over NDJSON on stdin/stdout:
//!
//! - Raw stream-json SDK messages are emitted verbatim → parsed exactly like the
//!   old CLI output and forwarded on `run:event:<run_id>`.
//! - Control messages are namespaced `_devdy_*`:
//!     `_devdy_permission_request` → emitted on `run:permission_request:<run_id>`;
//!         the frontend answers via `respond_permission`, which writes a
//!         `permission_response` line back to the sidecar's stdin.
//!     `_devdy_stderr` / `_devdy_error` → surfaced on `run:output:<run_id>`.
//!     `_devdy_ready` / `_devdy_done` / `_devdy_closed` → lifecycle, swallowed.
//!
//! Auth: the SDK uses its bundled CLI, which reads the same macOS Keychain login
//! as `claude` — so runs bill against the user's subscription, no API key.

use crate::runs::permission::PermissionRequestEvent;
use crate::runs::RunRegistry;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex as TokioMutex;

/// Map a Devdy permission mode onto a permission mode the Agent SDK accepts.
/// Unknown/legacy modes (`auto`, `dontAsk`) fall back to `default`.
pub fn sdk_permission_mode(mode: &str) -> &'static str {
    match mode {
        "acceptEdits" => "acceptEdits",
        "bypassPermissions" => "bypassPermissions",
        "plan" => "plan",
        _ => "default",
    }
}

/// Resolve the Node binary and the sidecar entry script.
///
/// Resolution order for the script: explicit `sidecar_override` setting →
/// `DEVDY_SIDECAR_PATH` env → bundled `<resource_dir>/sidecar/index.mjs` →
/// dev fallback `<crate>/../sidecar/index.mjs`.
pub fn resolve_sidecar(
    app: &AppHandle,
    node_path: &str,
    sidecar_override: &str,
) -> Result<(String, PathBuf), String> {
    resolve_sidecar_script(app, node_path, sidecar_override, "DEVDY_SIDECAR_PATH", "sidecar", "Claude")
}

/// Resolve the Node binary and the Codex sidecar entry script
/// (`sidecar-codex/index.mjs`). Mirrors `resolve_sidecar`.
pub fn resolve_codex_sidecar(
    app: &AppHandle,
    node_path: &str,
    sidecar_override: &str,
) -> Result<(String, PathBuf), String> {
    resolve_sidecar_script(
        app,
        node_path,
        sidecar_override,
        "DEVDY_CODEX_SIDECAR_PATH",
        "sidecar-codex",
        "Codex",
    )
}

fn resolve_sidecar_script(
    app: &AppHandle,
    node_path: &str,
    sidecar_override: &str,
    env_var: &str,
    rel_dir: &str,
    label: &str,
) -> Result<(String, PathBuf), String> {
    let script = if !sidecar_override.trim().is_empty() {
        PathBuf::from(sidecar_override.trim())
    } else if let Ok(p) = std::env::var(env_var) {
        PathBuf::from(p)
    } else {
        let bundled = app
            .path()
            .resource_dir()
            .ok()
            .map(|r| r.join(rel_dir).join("index.mjs"))
            .filter(|p| p.exists());
        bundled.unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join(rel_dir)
                .join("index.mjs")
        })
    };

    if !script.exists() {
        return Err(format!(
            "{label} sidecar not found at {}. Ensure the {rel_dir}/ directory is \
             present, or set the matching path setting / {env_var} env.",
            script.display()
        ));
    }

    let node = resolve_node_bin(node_path);
    Ok((node, script))
}

use std::sync::OnceLock;

/// `(node_execpath, shell_PATH)` recovered from the user's login shell, cached.
///
/// GUI-launched apps (and some IDE-spawned dev servers) inherit a minimal PATH
/// that lacks Homebrew/nvm/fnm/volta, so `node`/`claude`/`codex` can't be found.
/// We probe the login shell once. Crucially, `node` is often a *lazy shell
/// function* (nvm/fnm) rather than a binary on PATH, so we resolve it by
/// actually running `node` and asking for `process.execPath` — that works no
/// matter how the version manager exposes it.
static SHELL_ENV: OnceLock<(Option<String>, Option<String>)> = OnceLock::new();

fn probe_shell_env() -> &'static (Option<String>, Option<String>) {
    SHELL_ENV.get_or_init(|| {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        // -l (login) + -i (interactive) so profile AND rc files (nvm/fnm shims)
        // are sourced. Markers bracket the values so rc-file noise is ignored.
        let script = "printf '__DEVDY_NODE__:'; \
             node -e 'process.stdout.write(process.execPath)' 2>/dev/null; \
             printf '\\n__DEVDY_PATH__:%s\\n' \"$PATH\"";
        let out = std::process::Command::new(&shell)
            .args(["-lic", script])
            .output()
            .ok()
            .filter(|o| o.status.success());
        let Some(out) = out else {
            return (None, None);
        };
        let s = String::from_utf8_lossy(&out.stdout);
        let extract = |marker: &str| -> Option<String> {
            let idx = s.find(marker)? + marker.len();
            let line = s[idx..].lines().next().unwrap_or("").trim();
            (!line.is_empty()).then(|| line.to_string())
        };
        (extract("__DEVDY_NODE__:"), extract("__DEVDY_PATH__:"))
    })
}

/// Resolve an absolute `node` binary. Honors an explicit configured path; else
/// asks the login shell to run node and report its own path; else falls back to
/// common install locations, then bare `node`.
pub fn resolve_node_bin(node_path: &str) -> String {
    let p = node_path.trim();
    if !p.is_empty() && p != "node" {
        return p.to_string();
    }

    if let Some(node) = probe_shell_env().0.as_deref() {
        if std::path::Path::new(node).is_file() {
            return node.to_string();
        }
    }

    for c in ["/opt/homebrew/bin/node", "/usr/local/bin/node"] {
        if std::path::Path::new(c).is_file() {
            return c.to_string();
        }
    }

    "node".to_string()
}

/// Merge the recovered login-shell PATH (plus the resolved node's own directory)
/// into a spawned command's environment, so the sidecar — and the `claude` /
/// `codex` CLIs it shells out to, which have `#!/usr/bin/env node` shebangs —
/// can all be found even from a GUI launch.
pub fn augment_command_path(cmd: &mut tokio::process::Command) {
    let (node, login) = probe_shell_env();
    let current = std::env::var("PATH").unwrap_or_default();

    let mut parts: Vec<&str> = Vec::new();
    // node's own dir first, so its shebang-based CLIs resolve `node`.
    let node_dir = node
        .as_deref()
        .and_then(|n| std::path::Path::new(n).parent())
        .map(|p| p.to_string_lossy().into_owned());
    if let Some(d) = node_dir.as_deref() {
        parts.push(d);
    }
    if let Some(l) = login.as_deref() {
        parts.push(l);
    }
    if !current.is_empty() {
        parts.push(&current);
    }
    if !parts.is_empty() {
        cmd.env("PATH", parts.join(":"));
    }
}

/// Put the spawned sidecar in its own process group (Unix) so the whole tree —
/// the `node` sidecar AND the `claude` / `codex` CLI it shells out to — can be
/// killed in one shot via the group. Without this, killing only the node
/// process orphans the CLI, which can keep running and burning API tokens.
pub fn detach_process_group(cmd: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        // 0 = put the child in a new group whose leader is the child itself,
        // so its pid doubles as the process-group id.
        cmd.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = cmd;
    }
}

/// Best-effort SIGKILL of the entire process group led by `pid` (Unix). Used on
/// run cancellation and app exit so no `claude` / `codex` process lingers after
/// its run is gone. Pair with [`detach_process_group`] at spawn time.
pub fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        // Negative pgid targets every process in the group.
        unsafe {
            libc::killpg(pid as libc::pid_t, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Drain the sidecar's stdout/stderr until EOF, bridging events to the frontend
/// and persisting the stream-json log. On EOF, updates the run row and emits
/// `run:done:<run_id>`. Shared by both `start_run` and `resume_run`.
#[allow(clippy::too_many_arguments)]
pub async fn drain_sidecar(
    app: AppHandle,
    run_id: String,
    project_path: String,
    stdout: ChildStdout,
    stderr: ChildStderr,
    db_pool: sqlx::SqlitePool,
    registry: RunRegistry,
    session_id_arc: Arc<TokioMutex<Option<String>>>,
    log_buf: Arc<TokioMutex<String>>,
    log_path: PathBuf,
    merge_existing_log: bool,
) {
    let mut stdout_reader = BufReader::new(stdout).lines();
    let mut stderr_reader = BufReader::new(stderr).lines();

    async fn append_log(buf: &Arc<TokioMutex<String>>, line: &str) {
        buf.lock().await.push_str(line);
    }

    // Snapshot the run's project + engine once so usage rows are self-contained
    // (they survive deletion of the run/project). `engine` is already updated to
    // the effective engine before this task spawns.
    let (project_id, project_name, engine) = {
        use sqlx::Row;
        match sqlx::query(
            "SELECT r.project_id, r.engine, p.name AS project_name
             FROM runs r JOIN projects p ON p.id = r.project_id
             WHERE r.id = ?",
        )
        .bind(&run_id)
        .fetch_one(&db_pool)
        .await
        {
            Ok(row) => (
                row.get::<String, _>("project_id"),
                row.get::<String, _>("project_name"),
                row.get::<String, _>("engine"),
            ),
            Err(_) => (String::new(), String::new(), String::new()),
        }
    };
    // Most-recent model id seen on a `system.init` event; attached to usage rows.
    let mut last_model: Option<String> = None;

    // Incremental persistence: snapshot any pre-existing on-disk log once (for
    // resumes), then periodically flush `prefix + buf` to disk while the run
    // streams. This lets the frontend recover partial output after an app
    // restart mid-run, instead of only seeing the log once the run finishes.
    let existing_prefix = if merge_existing_log {
        std::fs::read_to_string(&log_path).unwrap_or_default()
    } else {
        String::new()
    };
    let flush_to_disk = |buf: &str| {
        if merge_existing_log {
            let mut merged = String::with_capacity(existing_prefix.len() + buf.len());
            merged.push_str(&existing_prefix);
            merged.push_str(buf);
            let _ = std::fs::write(&log_path, merged);
        } else {
            let _ = std::fs::write(&log_path, buf);
        }
    };
    let mut flush_tick = tokio::time::interval(std::time::Duration::from_millis(500));
    flush_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = flush_tick.tick() => {
                let buf = log_buf.lock().await;
                flush_to_disk(&buf);
            }
            line = stdout_reader.next_line() => {
                match line {
                    Ok(Some(l)) => {
                        let parsed = serde_json::from_str::<Value>(&l).ok();
                        let kind = parsed
                            .as_ref()
                            .and_then(|v| v.get("type"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("");

                        match (parsed.as_ref(), kind) {
                            // ---- control: permission request --------------------
                            (Some(v), "_devdy_permission_request") => {
                                let session_id = session_id_arc.lock().await.clone();
                                let evt = PermissionRequestEvent {
                                    run_id: run_id.clone(),
                                    request_id: v.get("requestId").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                                    tool_name: v.get("tool_name").and_then(|x| x.as_str()).unwrap_or("tool").to_string(),
                                    tool_input: v.get("tool_input").cloned().unwrap_or(Value::Null),
                                    session_id,
                                    cwd: Some(project_path.clone()),
                                    title: v.get("title").and_then(|x| x.as_str()).map(str::to_string),
                                    description: v.get("description").and_then(|x| x.as_str()).map(str::to_string),
                                    display_name: v.get("display_name").and_then(|x| x.as_str()).map(str::to_string),
                                };
                                let _ = app.emit(&format!("run:permission_request:{}", run_id), evt);
                            }
                            // ---- control: diagnostic log (codex tracing / notes) -
                            // Carries a level so the UI renders it calmly, not as a
                            // system error. Only `error` level counts as stderr.
                            (Some(v), "_devdy_log") => {
                                let text = v.get("text").and_then(|x| x.as_str()).unwrap_or("").to_string();
                                let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("info").to_string();
                                let is_err = level == "error";
                                append_log(&log_buf, &format!("[log:{}] {}\n", level, text)).await;
                                let _ = app.emit(
                                    &format!("run:output:{}", run_id),
                                    serde_json::json!({ "run_id": run_id, "line": text, "is_stderr": is_err, "level": level }),
                                );
                            }
                            // ---- control: sidecar stderr / errors ---------------
                            (Some(v), "_devdy_stderr") | (Some(v), "_devdy_error") => {
                                let text = v.get("text").or_else(|| v.get("error"))
                                    .and_then(|x| x.as_str()).unwrap_or("").to_string();
                                append_log(&log_buf, &format!("[stderr] {}\n", text)).await;
                                let _ = app.emit(
                                    &format!("run:output:{}", run_id),
                                    serde_json::json!({ "run_id": run_id, "line": text, "is_stderr": true, "level": "error" }),
                                );
                            }
                            // ---- control: structured /usage snapshot ------------
                            // Real claude.ai plan rate-limit utilization + reset
                            // windows from the live session. Persisted as the
                            // latest-known plan-usage state and broadcast so the
                            // budget badge / settings panel refresh.
                            (Some(v), "_devdy_usage") => {
                                if let Some(usage) = v.get("usage") {
                                    // Codex sidecar tags its snapshot `provider:"codex"`
                                    // and carries `rate_limits` directly; route it to
                                    // the separate Codex plan-usage state.
                                    let persisted = if usage.get("provider").and_then(|p| p.as_str()) == Some("codex") {
                                        if let Some(rl) = usage.get("rate_limits") {
                                            crate::commands::codex_sessions::persist_codex_plan_usage(&db_pool, rl).await
                                        } else {
                                            false
                                        }
                                    } else {
                                        persist_plan_usage(&db_pool, usage).await
                                    };
                                    if persisted {
                                        let _ = app.emit("plan_usage_updated", serde_json::json!({ "run_id": run_id }));
                                    }
                                }
                            }
                            // ---- control: lifecycle (swallow) -------------------
                            (Some(_), "_devdy_ready") | (Some(_), "_devdy_done") | (Some(_), "_devdy_closed") => {}
                            // ---- raw stream-json SDK message --------------------
                            (Some(v), _) => {
                                append_log(&log_buf, &format!("{}\n", l)).await;
                                capture_session_id(&session_id_arc, &db_pool, &run_id, v).await;
                                // Track the active model from the init event so it
                                // can be stamped onto the usage row at turn end.
                                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                                    && v.get("subtype").and_then(|x| x.as_str()) == Some("init")
                                {
                                    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                                        last_model = Some(m.to_string());
                                    }
                                    if persist_plan_init_rate_limits(&db_pool, v).await {
                                        let _ = app.emit(
                                            "plan_usage_updated",
                                            serde_json::json!({ "run_id": run_id }),
                                        );
                                    }
                                }
                                if v.get("type").and_then(|x| x.as_str()) == Some("rate_limit_event")
                                    && persist_plan_rate_limit_event(&db_pool, v).await
                                {
                                    let _ = app.emit(
                                        "plan_usage_updated",
                                        serde_json::json!({ "run_id": run_id }),
                                    );
                                }
                                // End the turn: closing stdin makes the sidecar's
                                // input stream close, the query finish, and the
                                // process exit so `run:done` can fire. Follow-ups
                                // continue via resume_run.
                                if v.get("type").and_then(|x| x.as_str()) == Some("result") {
                                    let usage_recorded = capture_usage(
                                        &db_pool, &run_id, &project_id, &project_name,
                                        &engine, last_model.as_deref(), v,
                                    )
                                    .await;
                                    if usage_recorded {
                                        let _ = app.emit(
                                            "budget_status_updated",
                                            serde_json::json!({ "run_id": run_id }),
                                        );
                                    }
                                    {
                                        let mut reg = registry.lock().await;
                                        if let Some(handles) = reg.get_mut(&run_id) {
                                            handles.stdin.take();
                                        }
                                    }
                                    // This turn's usage is now recorded. Re-check
                                    // the global budget; if it tipped over, tell
                                    // the UI to lock the composer so the next turn
                                    // (resume / follow-up) can't be started.
                                    if let Ok(status) =
                                        crate::commands::stats::budget_status(&db_pool).await
                                    {
                                        if status.is_over {
                                            let _ = app.emit(
                                                &format!("run:budget_exceeded:{}", run_id),
                                                serde_json::json!({
                                                    "run_id": run_id,
                                                    "source": status.source,
                                                    "percent": status.percent,
                                                }),
                                            );
                                        }
                                    }
                                }
                                let _ = app.emit(&format!("run:event:{}", run_id), v.clone());
                            }
                            // ---- non-JSON line (rare) ---------------------------
                            (None, _) => {
                                append_log(&log_buf, &format!("{}\n", l)).await;
                                let _ = app.emit(
                                    &format!("run:output:{}", run_id),
                                    serde_json::json!({ "run_id": run_id, "line": l, "is_stderr": false }),
                                );
                            }
                        }
                    }
                    _ => break,
                }
            }
            line = stderr_reader.next_line() => {
                if let Ok(Some(l)) = line {
                    append_log(&log_buf, &format!("[stderr] {}\n", l)).await;
                    let _ = app.emit(
                        &format!("run:output:{}", run_id),
                        serde_json::json!({ "run_id": run_id, "line": l, "is_stderr": true }),
                    );
                }
            }
        }
    }

    // Drain any trailing stderr after stdout closed.
    while let Ok(Some(l)) = stderr_reader.next_line().await {
        append_log(&log_buf, &format!("[stderr] {}\n", l)).await;
        let _ = app.emit(
            &format!("run:output:{}", run_id),
            serde_json::json!({ "run_id": run_id, "line": l, "is_stderr": true }),
        );
    }

    // Final authoritative persist (captures anything appended since the last
    // periodic flush). Uses the prefix snapshot rather than re-reading the file,
    // because the periodic flush has already written `prefix + buf` to disk.
    {
        let buf = log_buf.lock().await;
        flush_to_disk(&buf);
    }

    // Cancelled iff the entry was already removed by cancel_run.
    let was_cancelled = {
        let mut reg = registry.lock().await;
        reg.remove(&run_id).is_none()
    };
    let final_status = if was_cancelled { "cancelled" } else { "done" };
    let finished_at = chrono::Utc::now().to_rfc3339();
    let _ = sqlx::query("UPDATE runs SET status = ?, finished_at = ?, output_path = ? WHERE id = ?")
        .bind(final_status)
        .bind(&finished_at)
        .bind(log_path.to_string_lossy().as_ref())
        .bind(&run_id)
        .execute(&db_pool)
        .await;

    let _ = app.emit(
        &format!("run:done:{}", run_id),
        serde_json::json!({ "run_id": run_id, "status": final_status }),
    );
}

async fn capture_session_id(
    session_id_arc: &Arc<TokioMutex<Option<String>>>,
    db_pool: &sqlx::SqlitePool,
    run_id: &str,
    value: &Value,
) {
    if value.get("type").and_then(|v| v.as_str()) != Some("system") {
        return;
    }
    if value.get("subtype").and_then(|v| v.as_str()) != Some("init") {
        return;
    }
    let Some(sid) = value.get("session_id").and_then(|v| v.as_str()) else {
        return;
    };
    let sid = sid.to_string();
    {
        let mut g = session_id_arc.lock().await;
        if g.as_deref() == Some(sid.as_str()) {
            return;
        }
        *g = Some(sid.clone());
    }
    let _ = sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
        .bind(&sid)
        .bind(run_id)
        .execute(db_pool)
        .await;
}

/// Persist the latest structured `/usage` snapshot (claude.ai plan rate-limit
/// utilization + real per-account reset windows) into the `settings` KV under
/// `plan_usage`, as a normalized JSON blob stamped with the capture time. We
/// store only the fields the UI needs so we're insulated from churn in the
/// experimental SDK response shape.
pub(crate) async fn persist_plan_usage(db_pool: &sqlx::SqlitePool, usage: &Value) -> bool {
    if !usage
        .get("rate_limits_available")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        return false;
    }
    let limits = usage.get("rate_limits");
    if !limits.and_then(|v| v.as_object()).is_some() {
        return false;
    }

    // Extract one window ({ utilization, resets_at }) from the rate_limits map.
    let window = |limits: Option<&Value>, key: &str| -> Value {
        let w = limits.and_then(|l| l.get(key));
        serde_json::json!({
            "utilization": w.and_then(|x| x.get("utilization")).and_then(|x| x.as_f64()),
            "resets_at": w.and_then(|x| x.get("resets_at")).and_then(|x| x.as_str()),
            "status": null,
            "status_at": null,
        })
    };
    let mut snapshot = serde_json::json!({
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "subscription_type": usage.get("subscription_type").and_then(|v| v.as_str()),
        "rate_limits_available": usage
            .get("rate_limits_available")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        "windows": {
            "five_hour": window(limits, "five_hour"),
            "seven_day": window(limits, "seven_day"),
            "seven_day_opus": window(limits, "seven_day_opus"),
            "seven_day_sonnet": window(limits, "seven_day_sonnet"),
        },
    });
    // Keep the live status captured from rate_limit_events (which have no %).
    let prior = load_plan_usage_snapshot(db_pool).await;
    merge_prior_window_status(&mut snapshot, &prior);

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('plan_usage', ?)")
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

/// Persist `rate_limits` carried on Claude's `system.init`. This is the first
/// and often most reliable signal available to an idle usage probe, before the
/// experimental control `/usage` request has a chance to return.
pub(crate) async fn persist_plan_init_rate_limits(db_pool: &sqlx::SqlitePool, event: &Value) -> bool {
    if event.get("type").and_then(|v| v.as_str()) != Some("system")
        || event.get("subtype").and_then(|v| v.as_str()) != Some("init")
    {
        return false;
    }
    let Some(limits) = event.get("rate_limits") else {
        return false;
    };
    if !limits.is_object() {
        return false;
    }

    let window = |key: &str| -> Value {
        let w = limits.get(key);
        serde_json::json!({
            "utilization": w.and_then(|x| x.get("utilization")).and_then(|x| x.as_f64()),
            "resets_at": w.and_then(|x| x.get("resets_at")).and_then(|x| x.as_str()),
            "status": null,
            "status_at": null,
        })
    };
    let mut snapshot = serde_json::json!({
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "subscription_type": event.get("subscription_type").and_then(|v| v.as_str()),
        "rate_limits_available": true,
        "windows": {
            "five_hour": window("five_hour"),
            "seven_day": window("seven_day"),
            "seven_day_opus": window("seven_day_opus"),
            "seven_day_sonnet": window("seven_day_sonnet"),
        },
    });
    let prior = load_plan_usage_snapshot(db_pool).await;
    merge_prior_window_status(&mut snapshot, &prior);

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('plan_usage', ?)")
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

/// Merge a live Claude `rate_limit_event` into the same `plan_usage` snapshot
/// used by the global budget badge. These events can arrive before the
/// experimental `/usage` helper returns, so they keep the warning at the newest
/// known utilization while a turn is streaming.
pub(crate) async fn persist_plan_rate_limit_event(db_pool: &sqlx::SqlitePool, event: &Value) -> bool {
    let Some(info) = event.get("rate_limit_info").and_then(|v| v.as_object()) else {
        return false;
    };
    let Some(window_key) = info
        .get("rateLimitType")
        .or_else(|| info.get("rate_limit_type"))
        .and_then(|v| v.as_str())
        .and_then(plan_window_key)
    else {
        return false;
    };
    // These events carry live status + reset time but NO utilization %. Update
    // the reset/status without touching the % (only `/usage` knows the real %),
    // and DON'T bump `captured_at` — that stamps the freshness of the %.
    let resets_at = info
        .get("resetsAt")
        .or_else(|| info.get("resets_at"))
        .and_then(plan_reset_to_string);
    let status = info
        .get("status")
        .or_else(|| info.get("overageStatus"))
        .and_then(|v| v.as_str())
        .map(plan_status_severity);
    let now = chrono::Utc::now().to_rfc3339();

    let mut snapshot = load_plan_usage_snapshot(db_pool)
        .await
        .unwrap_or_else(empty_plan_usage_snapshot);

    snapshot["rate_limits_available"] = serde_json::json!(true);
    if snapshot.get("windows").and_then(|v| v.as_object()).is_none() {
        snapshot["windows"] = empty_plan_usage_snapshot()["windows"].clone();
    }
    if let Some(windows) = snapshot.get_mut("windows").and_then(|v| v.as_object_mut()) {
        let entry = windows.entry(window_key.to_string()).or_insert_with(
            || serde_json::json!({ "utilization": null, "resets_at": null, "status": null, "status_at": null }),
        );
        if let Some(obj) = entry.as_object_mut() {
            if resets_at.is_some() {
                obj.insert("resets_at".to_string(), serde_json::json!(resets_at));
            }
            if let Some(status) = status {
                obj.insert("status".to_string(), serde_json::json!(status));
                obj.insert("status_at".to_string(), serde_json::json!(now));
            }
        }
    }

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES ('plan_usage', ?)")
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

fn empty_plan_usage_snapshot() -> Value {
    serde_json::json!({
        "captured_at": chrono::Utc::now().to_rfc3339(),
        "subscription_type": null,
        "rate_limits_available": false,
        "windows": {
            "five_hour": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
            "seven_day": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
            "seven_day_opus": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
            "seven_day_sonnet": { "utilization": null, "resets_at": null, "status": null, "status_at": null },
        },
    })
}

/// Read the current `plan_usage` snapshot from settings, if any.
async fn load_plan_usage_snapshot(db_pool: &sqlx::SqlitePool) -> Option<Value> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = 'plan_usage'")
            .fetch_optional(db_pool)
            .await
            .ok()
            .flatten();
    stored
        .as_deref()
        .and_then(|json| serde_json::from_str::<Value>(json).ok())
}

/// Copy the live `status`/`status_at` per window from a prior snapshot onto a
/// freshly-built one. The `/usage` control response carries accurate
/// `utilization` but no status, while `rate_limit_event`s carry live status but
/// no `utilization` — so each source must preserve the other's fields.
fn merge_prior_window_status(snapshot: &mut Value, prior: &Option<Value>) {
    let Some(prior_windows) = prior
        .as_ref()
        .and_then(|s| s.get("windows"))
        .and_then(|w| w.as_object())
    else {
        return;
    };
    if let Some(windows) = snapshot.get_mut("windows").and_then(|v| v.as_object_mut()) {
        for (key, win) in windows.iter_mut() {
            let (Some(obj), Some(prior_win)) = (
                win.as_object_mut(),
                prior_windows.get(key).and_then(|v| v.as_object()),
            ) else {
                continue;
            };
            if let Some(status) = prior_win.get("status") {
                obj.insert("status".to_string(), status.clone());
            }
            if let Some(status_at) = prior_win.get("status_at") {
                obj.insert("status_at".to_string(), status_at.clone());
            }
        }
    }
}

/// Map a raw `rate_limit_event` status string onto our tri-state severity.
fn plan_status_severity(raw: &str) -> &'static str {
    let s = raw.to_ascii_lowercase();
    if s.contains("reject") || s.contains("block") || s.contains("exceed") || s.contains("over_limit")
    {
        "blocked"
    } else if s.contains("warn") || s.contains("approach") || s.contains("near") {
        "warning"
    } else {
        "allowed"
    }
}

fn plan_window_key(raw: &str) -> Option<&'static str> {
    match raw {
        "five_hour" => Some("five_hour"),
        "seven_day" => Some("seven_day"),
        "seven_day_opus" => Some("seven_day_opus"),
        "seven_day_sonnet" => Some("seven_day_sonnet"),
        _ => None,
    }
}

fn plan_reset_to_string(value: &Value) -> Option<String> {
    if let Some(s) = value.as_str() {
        return Some(s.to_string());
    }
    let raw = value
        .as_i64()
        .or_else(|| value.as_u64().and_then(|n| i64::try_from(n).ok()))
        .or_else(|| value.as_f64().map(|n| n as i64))?;
    let ms = if raw > 1_000_000_000_000 {
        raw
    } else {
        raw * 1000
    };
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms).map(|dt| dt.to_rfc3339())
}

/// Live-capture a usage row at turn end, stamped with the current time.
async fn capture_usage(
    db_pool: &sqlx::SqlitePool,
    run_id: &str,
    project_id: &str,
    project_name: &str,
    engine: &str,
    model: Option<&str>,
    value: &Value,
) -> bool {
    let created_at = chrono::Utc::now().to_rfc3339();
    insert_usage_from_result(
        db_pool, run_id, project_id, project_name, engine, model, value, &created_at,
    )
    .await
}

/// Persist one usage row from a `result` stream event. Prefers the SDK-reported
/// `total_cost_usd` (Claude); otherwise estimates the cost from token counts
/// (Codex) and flags it. Empty results (no tokens, no cost) are skipped and
/// return `false`. Shared by live capture and the log backfill command.
pub(crate) async fn insert_usage_from_result(
    db_pool: &sqlx::SqlitePool,
    run_id: &str,
    project_id: &str,
    project_name: &str,
    engine: &str,
    model: Option<&str>,
    value: &Value,
    created_at: &str,
) -> bool {
    if value.get("type").and_then(|v| v.as_str()) != Some("result") {
        return false;
    }

    let usage = value.get("usage");
    let tok = |k: &str| -> i64 {
        usage
            .and_then(|u| u.get(k))
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
    };
    let input = tok("input_tokens");
    let output = tok("output_tokens");
    let cache_creation = tok("cache_creation_input_tokens");
    let cache_read = tok("cache_read_input_tokens");
    let total = input + output + cache_creation + cache_read;

    // Effective model: explicit field on the event (codex sets it) else the
    // model captured from `system.init`.
    let eff_model = value
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| model.map(str::to_string));

    let (cost, estimated) = match value.get("total_cost_usd").and_then(|v| v.as_f64()) {
        Some(c) => (c, 0_i64),
        None => (
            crate::runs::pricing::estimate_cost(
                eff_model.as_deref().unwrap_or(""),
                input,
                output,
                cache_creation,
                cache_read,
            ),
            1_i64,
        ),
    };

    if total == 0 && cost == 0.0 {
        return false;
    }

    let num_turns = value.get("num_turns").and_then(|v| v.as_i64());
    let duration_ms = value.get("duration_ms").and_then(|v| v.as_i64());
    let id = uuid::Uuid::new_v4().to_string();

    let res = sqlx::query(
        "INSERT INTO run_usage (
            id, run_id, project_id, project_name, engine, model,
            input_tokens, output_tokens, cache_creation_tokens, cache_read_tokens,
            total_tokens, cost_usd, cost_estimated, num_turns, duration_ms, created_at, deleted_run
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(id)
    .bind(run_id)
    .bind(project_id)
    .bind(project_name)
    .bind(engine)
    .bind(eff_model)
    .bind(input)
    .bind(output)
    .bind(cache_creation)
    .bind(cache_read)
    .bind(total)
    .bind(cost)
    .bind(estimated)
    .bind(num_turns)
    .bind(duration_ms)
    .bind(created_at.to_string())
    .execute(db_pool)
    .await;

    res.is_ok()
}

#[cfg(test)]
mod plan_usage_tests {
    use super::*;

    #[test]
    fn status_severity_mapping() {
        assert_eq!(plan_status_severity("allowed"), "allowed");
        assert_eq!(plan_status_severity("allowed_warning"), "warning");
        assert_eq!(plan_status_severity("approaching_limit"), "warning");
        assert_eq!(plan_status_severity("rejected"), "blocked");
        assert_eq!(plan_status_severity("blocked"), "blocked");
        assert_eq!(plan_status_severity("something_else"), "allowed");
    }

    #[test]
    fn merge_preserves_prior_live_status() {
        // Fresh /usage snapshot has the % but no status yet.
        let mut fresh = serde_json::json!({
            "windows": {
                "five_hour": { "utilization": 12.0, "resets_at": "R", "status": null, "status_at": null },
            }
        });
        // Prior snapshot carries live status from a rate_limit_event.
        let prior = Some(serde_json::json!({
            "windows": {
                "five_hour": { "utilization": null, "resets_at": "R", "status": "warning", "status_at": "T" },
            }
        }));
        merge_prior_window_status(&mut fresh, &prior);
        let w = &fresh["windows"]["five_hour"];
        assert_eq!(w["utilization"].as_f64(), Some(12.0)); // % kept
        assert_eq!(w["status"].as_str(), Some("warning")); // live status merged in
        assert_eq!(w["status_at"].as_str(), Some("T"));
    }

    #[test]
    fn merge_is_noop_without_prior() {
        let mut fresh = serde_json::json!({
            "windows": { "five_hour": { "utilization": 5.0, "status": null } }
        });
        merge_prior_window_status(&mut fresh, &None);
        assert_eq!(fresh["windows"]["five_hour"]["utilization"].as_f64(), Some(5.0));
    }
}
