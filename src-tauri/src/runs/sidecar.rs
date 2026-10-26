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

use crate::runs::liveness::{
    Action as LivenessAction, Liveness, SIDECAR_EXIT_GRACE, WATCHDOG_TICK,
};
use crate::runs::permission::PermissionRequestEvent;
use crate::runs::RunRegistry;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout};
use tokio::sync::Mutex as TokioMutex;

const CLAUDE_AUTH_OVERRIDE_ENV: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "ANTHROPIC_PROFILE",
];

/// Apply a Devdy-managed Claude profile to a spawned sidecar/CLI.
///
/// We only clear auth override env vars when a managed profile is selected, so
/// legacy installs that intentionally launch Devdy with global env auth keep the
/// old behavior until they opt into Claude accounts.
pub fn apply_claude_config_dir(cmd: &mut tokio::process::Command, config_dir: Option<&str>) {
    let Some(config_dir) = config_dir.map(str::trim).filter(|v| !v.is_empty()) else {
        return;
    };
    for key in CLAUDE_AUTH_OVERRIDE_ENV {
        cmd.env_remove(key);
    }
    cmd.env("CLAUDE_CONFIG_DIR", config_dir);
}

/// Give MCP tool calls a generous ceiling so long-blocking loopback tools aren't
/// aborted by the client's default tool timeout with "The operation timed out."
/// The conductor's `session_wait` streams progress and can block up to its
/// ~10-minute cap; without a high ceiling the client kills the call early and the
/// model never sees the real `waited_ms` (it then misreports the `timeout_ms`
/// ceiling as the elapsed wait). Respects an explicit user-provided value.
pub fn apply_mcp_tool_timeout(cmd: &mut tokio::process::Command) {
    if std::env::var_os("MCP_TOOL_TIMEOUT").is_none() {
        // 15 min — comfortably above session_wait's 10-min cap.
        cmd.env("MCP_TOOL_TIMEOUT", "900000");
    }
}

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

/// Best-effort path to the built-in `devdy` MCP server script
/// (`sidecar-mcp/index.mjs`). Mirrors `resolve_sidecar_script` resolution
/// (`DEVDY_MCP_SIDECAR_PATH` env → bundled resource → dev fallback) but never
/// errors: the caller skips injecting the built-in server when the file is
/// absent, so a missing script degrades gracefully instead of failing a run.
pub fn resolve_mcp_sidecar_script(app: &AppHandle) -> PathBuf {
    if let Ok(p) = std::env::var("DEVDY_MCP_SIDECAR_PATH") {
        return PathBuf::from(p);
    }
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|r| r.join("sidecar-mcp").join("index.mjs"))
        .filter(|p| p.exists());
    bundled.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sidecar-mcp")
            .join("index.mjs")
    })
}

/// Best-effort path to a sibling built-in MCP server script inside the bundled
/// `sidecar-mcp/` directory (e.g. `google.mjs`). Same resolution as
/// [`resolve_mcp_sidecar_script`] (`DEVDY_MCP_SIDECAR_DIR` env → bundled resource
/// → dev fallback); never errors — callers skip injection when the file is
/// absent.
pub fn resolve_mcp_script(app: &AppHandle, filename: &str) -> PathBuf {
    if let Ok(dir) = std::env::var("DEVDY_MCP_SIDECAR_DIR") {
        return PathBuf::from(dir).join(filename);
    }
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|r| r.join("sidecar-mcp").join(filename))
        .filter(|p| p.exists());
    bundled.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sidecar-mcp")
            .join(filename)
    })
}

/// Path to Devdy's SQLite store (`<app_data_dir>/data.db`), passed to the
/// built-in MCP server so it can read notes/sessions. Falls back to a bare
/// `data.db` if the app data dir can't be resolved (the MCP tools then error
/// out cleanly rather than the run failing).
pub fn resolve_db_path(app: &AppHandle) -> PathBuf {
    app.path()
        .app_data_dir()
        .map(|d| d.join("data.db"))
        .unwrap_or_else(|_| PathBuf::from("data.db"))
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

/// [`detach_process_group`] for a Devdy Node sidecar (`sidecar/index.mjs`,
/// `sidecar-codex/index.mjs`): also tells it that it leads its own group, so it
/// tears the group down if Devdy dies (see the orphan guard there). The sidecar
/// removes the variable from its env before spawning anything.
pub fn detach_sidecar_process_group(cmd: &mut tokio::process::Command) {
    detach_process_group(cmd);
    cmd.env("DEVDY_OWN_PROCESS_GROUP", "1");
}

/// Best-effort SIGKILL of the entire process group led by `pid` (Unix). Used on
/// run cancellation and app exit so no `claude` / `codex` process lingers after
/// its run is gone. Pair with [`detach_process_group`] at spawn time. Returns
/// whether the signal was delivered (false: no such group any more).
pub fn kill_process_group(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // Negative pgid targets every process in the group.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
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

    // Remote Control (C1) event tap: publish each run event onto the in-process
    // broadcast bus AS WE emit it locally (FR-004 "ưu tiên tap tại nguồn"). When
    // Remote Control is disabled / no forwarder is subscribed, publish is a
    // no-op, so there is zero cost on the local path. Cloned once here (cheap:
    // a broadcast::Sender handle). Absent state → None → never publishes.
    let remote_bus = app
        .try_state::<crate::remote::RemoteBus>()
        .map(|s| s.inner().clone());

    async fn append_log(buf: &Arc<TokioMutex<String>>, line: &str) {
        buf.lock().await.push_str(line);
    }

    // Snapshot the run's project + engine once so usage rows are self-contained
    // (they survive deletion of the run/project). `engine` is already updated to
    // the effective engine before this task spawns.
    let (project_id, project_name, engine, claude_account_id) = {
        use sqlx::Row;
        match sqlx::query(
            "SELECT r.project_id, r.engine, r.claude_account_id, p.name AS project_name
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
                row.get::<Option<String>, _>("claude_account_id"),
            ),
            Err(_) => (String::new(), String::new(), String::new(), None),
        }
    };

    // Which per-account Claude snapshot key this run's live /usage writes to, so
    // parallel runs on different accounts never overwrite each other's badge.
    let plan_usage_key =
        crate::commands::stats::claude_plan_usage_key(claude_account_id.as_deref());

    // Announce this run to the desktop webview so app-wide surfaces (the active-
    // runs dock, the permission notifier and its native OS notification) attach
    // their per-run listeners even when the user never opened this run's view —
    // e.g. a run started/resumed entirely from a remote Controller, or one still
    // live after an app restart. Without this, `startListening` only ever runs
    // from RunView, so a remotely-driven run's permission prompts reach the phone
    // but nothing on the desktop. Keyed globally (not by run id) because the
    // webview cannot subscribe to a run id it does not yet know. Idempotent on the
    // JS side, so this is a no-op for runs the desktop already started locally.
    if !project_id.is_empty() {
        let _ = app.emit(
            "run:activated",
            serde_json::json!({ "run_id": run_id, "project_id": project_id }),
        );
    }

    // Most-recent model id seen on a `system.init` event; attached to usage rows.
    let mut last_model: Option<String> = None;
    // CLI version + results seen in this process: decide whether `modelUsage`
    // continues the previous turn's running totals (see `model_usage`).
    let mut cli_version: Option<String> = None;
    let mut results_in_process: usize = 0;

    // How many background tasks (backgrounded Bash, Monitor watchers…) the CLI
    // currently has alive, taken from the authoritative `background_tasks_changed`
    // event (its `tasks` array is the full live set, resent on every change).
    // A `result` can arrive with tasks still running: the CLI keeps the session
    // alive and WAKES it in-process when a task finishes (a fresh `init` + turn,
    // no resume). If we close stdin on that result we tear down the permission
    // channel for the wake-up turn, and every tool needing approval then fails
    // with "AbortError: Stream closed". So we only close once this reaches 0.
    let mut live_bg_tasks: usize = 0;

    // Set once the user's own foreground turn has ended (an origin-less `result`)
    // in THIS sidecar. It distinguishes the two look-alike shapes of an
    // origin-tagged `task-notification` result, both of which carry
    // `live_bg_tasks == 0`:
    //   • BEFORE any user turn — a spurious leftover from a previous session that
    //     lands on resume; closing stdin here would strand the real turn's
    //     permission channel (flag false → keep open);
    //   • AFTER the user turn plus all the background tasks it spawned have
    //     drained — the genuine end of the session's work (flag true → close).
    // Without it, a turn that spawns background subagents and finishes via a
    // task-notification wake-up never closes stdin: the process never exits, so
    // `run:done`/`status='done'` never fire and the run is stuck "running".
    let mut user_turn_ended_once = false;

    // Deferred drain-close (fixes "AbortError: Stream closed" on in-process wake
    // turns). `Some(deadline)` means a background task drained to 0 after the
    // user's turn and we are inside the grace window before closing stdin — see
    // the `background_tasks_changed` branch. Disarmed the moment a wake turn shows
    // activity; fires (closes) only if the session stays idle through the window.
    let mut drain_close_deadline: Option<tokio::time::Instant> = None;

    // True while a MAIN-thread turn is streaming (main-thread content since the
    // last `result`). A drain landing mid-turn must not arm the close — see
    // `drain_should_close_stdin`. Background subagents keep streaming after the
    // parent's `result`; they must not count (see `is_main_turn_activity`).
    let mut turn_active = false;

    // Watchdog: nothing may wait forever silently, and nothing is stopped or
    // answered on suspicion. Policy lives in `runs::liveness` (notices only);
    // the drain itself only tears down on definitive evidence (process exited,
    // stdout EOF, `_devdy_closed`).
    let mut liveness = Liveness::new(tokio::time::Instant::now());
    let mut watchdog_tick = tokio::time::interval(WATCHDOG_TICK);
    watchdog_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Armed once the session is provably over (`_devdy_closed`, or the sidecar
    // process exited). If stdout hasn't reached EOF by then, a descendant holds
    // it: we stop waiting and finish the run (descendants are left running —
    // they may be legitimate — and reported), instead of waiting forever.
    let mut exit_deadline: Option<tokio::time::Instant> = None;
    // The exit code the sidecar reported with `_devdy_closed` (codex: the
    // app-server's code; claude: whether the session loop failed).
    let mut closed_code: Option<i64> = None;
    // …and the signal, when the engine was killed by one (codex: `code` is
    // then null). Any signal the sidecar reports is a crash: it never reports
    // the one it sends itself on a normal shutdown.
    let mut closed_signal: Option<String> = None;
    // We stopped reading because the session was over but its output pipes were
    // still held open by descendants.
    let mut detached = false;
    // The sidecar reported a fatal `_devdy_error` that no later successful turn
    // superseded → the run ends `failed`.
    let mut unresolved_error = false;

    // Surface a watchdog message as a log line in the run (live + log + remote).
    async fn watchdog_line(
        app: &AppHandle,
        buf: &Arc<TokioMutex<String>>,
        bus: &Option<crate::remote::RemoteBus>,
        run_id: &str,
        level: &str,
        msg: &str,
    ) {
        let is_err = level == "error";
        append_log(buf, &format!("[log:{}] {}\n", level, msg)).await;
        if let Some(bus) = bus {
            bus.publish(crate::remote::RemoteRunEvent::Output {
                run_id: run_id.to_string(),
                line: msg.to_string(),
                is_stderr: is_err,
            });
        }
        let _ = app.emit(
            &format!("run:output:{}", run_id),
            serde_json::json!({ "run_id": run_id, "line": msg, "is_stderr": is_err, "level": level }),
        );
    }

    // Carry out the liveness decisions: notices go to the UI as a banner with a
    // stop action (plus a log line); reminders re-alert about an open prompt.
    async fn apply_liveness(
        actions: Vec<LivenessAction>,
        app: &AppHandle,
        buf: &Arc<TokioMutex<String>>,
        bus: &Option<crate::remote::RemoteBus>,
        run_id: &str,
    ) {
        for action in actions {
            match action {
                LivenessAction::Notice(notice) => {
                    let msg = notice.message();
                    watchdog_line(app, buf, bus, run_id, "warn", &format!("⚠ {}", msg)).await;
                    let _ = app.emit(
                        &format!("run:attention:{}", run_id),
                        serde_json::json!({
                            "run_id": run_id, "key": notice.key(), "kind": notice.kind(),
                            "message": msg, "params": notice.params(), "active": true,
                        }),
                    );
                }
                LivenessAction::Clear { key } => {
                    let _ = app.emit(
                        &format!("run:attention:{}", run_id),
                        serde_json::json!({ "run_id": run_id, "key": key, "active": false }),
                    );
                }
                LivenessAction::RemindPermission { request_id } => {
                    let _ = app.emit(
                        &format!("run:permission_reminder:{}", run_id),
                        serde_json::json!({ "run_id": run_id, "request_id": request_id }),
                    );
                }
            }
        }
    }

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
            // Watchdog: surface long waits (notices only), and detect the one
            // definitive hang the drain can see itself — the sidecar process has
            // exited but stdout never reached EOF (an orphan still holds the pipe).
            _ = watchdog_tick.tick() => {
                let pending: Vec<(String, String)> = crate::runs::permission::pending_for(&run_id)
                    .into_iter()
                    .map(|p| (p.event.request_id, p.event.tool_name))
                    .collect();
                // Quiet notices are Claude only: codex emits nothing while a long
                // command runs.
                let actions = liveness.tick(
                    tokio::time::Instant::now(),
                    turn_active,
                    &pending,
                    engine == "claude",
                );
                apply_liveness(actions, &app, &log_buf, &remote_bus, &run_id).await;

                if exit_deadline.is_none() {
                    let exited = {
                        let mut reg = registry.lock().await;
                        reg.get_mut(&run_id)
                            .map(|h| matches!(h.child.try_wait(), Ok(Some(_))))
                            .unwrap_or(false)
                    };
                    if exited {
                        exit_deadline = Some(tokio::time::Instant::now() + SIDECAR_EXIT_GRACE);
                    }
                }
            }
            // Deferred drain-close safety net: a background task drained to 0 and no
            // wake turn started within the grace window, so the session is genuinely
            // idle — close stdin now so the process exits and the run isn't stuck
            // "running". Pends forever while unarmed so this arm never wins then.
            _ = async {
                match drain_close_deadline {
                    Some(dl) => tokio::time::sleep_until(dl).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                drain_close_deadline = None;
                if drain_should_close_stdin(live_bg_tasks, user_turn_ended_once, turn_active) {
                    let mut reg = registry.lock().await;
                    if let Some(handles) = reg.get_mut(&run_id) {
                        handles.stdin.take();
                    }
                }
            }
            // The session is provably over (`_devdy_closed`, or the sidecar process
            // exited) yet stdout is still open: something it started inherited
            // the pipe. Nothing more can come from the session, so stop waiting
            // and finish the run. The descendants are NOT killed (they may be
            // legitimate, e.g. a dev server); they are reported with their group.
            _ = async {
                match exit_deadline {
                    Some(dl) => tokio::time::sleep_until(dl).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                let pgid = registry.lock().await.get(&run_id).and_then(|h| h.pgid);
                watchdog_line(&app, &log_buf, &remote_bus, &run_id, "warn", &detach_notice(pgid)).await;
                detached = true;
                break;
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

                        // Liveness bookkeeping: `_devdy_*` lines prove the sidecar
                        // alive; anything else is the engine making progress.
                        let now = tokio::time::Instant::now();
                        let actions = match (parsed.as_ref(), kind) {
                            (Some(_), "_devdy_heartbeat") => liveness.on_heartbeat(now),
                            (Some(v), "_devdy_ready") => {
                                let heartbeat = ready_declares_heartbeat(v);
                                if !heartbeat {
                                    watchdog_line(
                                        &app, &log_buf, &remote_bus, &run_id, "info",
                                        "This session bridge doesn't report liveness; only long silences during a turn will be flagged.",
                                    )
                                    .await;
                                }
                                liveness.on_ready(now, heartbeat)
                            }
                            (Some(_), k) if k.starts_with("_devdy_") => liveness.on_output(now),
                            (Some(v), _) => liveness.on_engine_event(now, v),
                            (None, _) => liveness.on_output(now),
                        };
                        apply_liveness(actions, &app, &log_buf, &remote_bus, &run_id).await;
                        match (parsed.as_ref(), kind) {
                            (Some(_), "_devdy_error") => unresolved_error = true,
                            (Some(v), "result") if v.get("subtype").and_then(|x| x.as_str()) == Some("success") => {
                                unresolved_error = false;
                            }
                            _ => {}
                        }

                        match (parsed.as_ref(), kind) {
                            // ---- control: sidecar heartbeat (liveness only) -----
                            (Some(_), "_devdy_heartbeat") => {}
                            // ---- control: permission request withdrawn ----------
                            // The sidecar settled a request itself (the CLI cancelled
                            // the tool, or its input channel closed so no answer could
                            // ever arrive). Drop the prompt everywhere.
                            (Some(v), "_devdy_permission_cancelled") => {
                                let request_id = v.get("requestId").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                                crate::runs::permission::resolve_pending(&run_id, &request_id);
                                let _ = app.emit(
                                    &format!("run:permission_resolved:{}", run_id),
                                    serde_json::json!({ "request_id": request_id }),
                                );
                                if let Some(text) = v.get("text").and_then(|x| x.as_str()) {
                                    let level = v.get("level").and_then(|x| x.as_str()).unwrap_or("info");
                                    watchdog_line(&app, &log_buf, &remote_bus, &run_id, level, text).await;
                                }
                            }
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
                                // Remote tap: forward the permission_request with full
                                // context so a Controller can decide (FR-006/BR-007).
                                // Conductor workers use the SAME flow as any session:
                                // the request surfaces in the standard permission drawer
                                // (shown in the Conductor tab for worker runs), with the
                                // per-project "always allow/deny" list applying as usual.
                                if let Some(bus) = &remote_bus {
                                    bus.publish(crate::remote::RemoteRunEvent::PermissionRequest {
                                        run_id: run_id.clone(),
                                        request_id: evt.request_id.clone(),
                                        tool: evt.tool_name.clone(),
                                        input: evt.tool_input.clone(),
                                        cwd: evt.cwd.clone(),
                                    });
                                }
                                // Remember it until answered: the emit below is lost if
                                // no webview listener is attached right now, and a
                                // re-attached listener recovers it from this store.
                                crate::runs::permission::track_pending(evt.clone());
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
                                if let Some(bus) = &remote_bus {
                                    bus.publish(crate::remote::RemoteRunEvent::Output {
                                        run_id: run_id.clone(),
                                        line: text.clone(),
                                        is_stderr: is_err,
                                    });
                                }
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
                                if let Some(bus) = &remote_bus {
                                    bus.publish(crate::remote::RemoteRunEvent::Output {
                                        run_id: run_id.clone(),
                                        line: text.clone(),
                                        is_stderr: true,
                                    });
                                }
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
                                    let is_codex = usage.get("provider").and_then(|p| p.as_str()) == Some("codex");
                                    let persisted = if is_codex {
                                        if let Some(rl) = usage.get("rate_limits") {
                                            crate::commands::codex_sessions::persist_codex_plan_usage(&db_pool, rl).await
                                        } else {
                                            false
                                        }
                                    } else {
                                        persist_plan_usage(&db_pool, &plan_usage_key, usage).await
                                    };
                                    if persisted {
                                        let _ = app.emit("plan_usage_updated", serde_json::json!({ "run_id": run_id }));
                                        // Fresh Claude plan utilization → re-balance the default account.
                                        if !is_codex {
                                            crate::commands::claude_accounts::request_balance_eval(&app);
                                        }
                                    }
                                }
                            }
                            // ---- control: Codex thread now carries its framing ----
                            // (thread/start developer instructions, or a one-time
                            // <session-framing> seed on a resumed turn). Persisted so
                            // later resumes don't seed it again.
                            (Some(_), "_devdy_framing_seeded") => {
                                crate::conductor::commands::mark_framing_seeded(&db_pool, &run_id).await;
                            }
                            // ---- control: lifecycle (swallow) -------------------
                            (Some(_), "_devdy_ready") | (Some(_), "_devdy_done") => {}
                            // The session is torn down for good (finished, or the
                            // engine died). Release stdin so the sidecar can exit,
                            // and make sure it does: a sidecar lingering here would
                            // keep the run "running" and silently swallow follow-ups.
                            (Some(v), "_devdy_closed") => {
                                closed_code = v.get("code").and_then(|c| c.as_i64());
                                closed_signal = v
                                    .get("signal")
                                    .and_then(|c| c.as_str())
                                    .map(str::to_string);
                                let mut reg = registry.lock().await;
                                if let Some(handles) = reg.get_mut(&run_id) {
                                    handles.stdin.take();
                                }
                                drop(reg);
                                exit_deadline.get_or_insert(tokio::time::Instant::now() + SIDECAR_EXIT_GRACE);
                            }
                            // ---- raw stream-json SDK message --------------------
                            (Some(v), _) => {
                                // Partial-message deltas are UI-only: the complete
                                // `assistant` message that follows carries the same
                                // text, so persisting (or forwarding) every delta would
                                // only bloat the log and the remote stream.
                                let is_delta = is_partial_delta(v);
                                if !is_delta {
                                    append_log(&log_buf, &format!("{}\n", l)).await;
                                }
                                capture_session_id(&session_id_arc, &db_pool, &run_id, v).await;
                                // Track the active model from the init event so it
                                // can be stamped onto the usage row at turn end.
                                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                                    && v.get("subtype").and_then(|x| x.as_str()) == Some("init")
                                {
                                    if let Some(m) = v.get("model").and_then(|x| x.as_str()) {
                                        last_model = Some(m.to_string());
                                    }
                                    cli_version = v
                                        .get("claude_code_version")
                                        .and_then(|x| x.as_str())
                                        .map(str::to_string);
                                    results_in_process = 0;
                                    // A conductor's turn is live: a worker finishing now
                                    // defers its wake until this turn's `result`.
                                    if let Some(cst) = app.try_state::<crate::conductor::ConductorState>() {
                                        cst.turn_started(&run_id);
                                    }
                                    if persist_plan_init_rate_limits(&db_pool, &plan_usage_key, v).await {
                                        let _ = app.emit(
                                            "plan_usage_updated",
                                            serde_json::json!({ "run_id": run_id }),
                                        );
                                        crate::commands::claude_accounts::request_balance_eval(&app);
                                    }
                                }
                                if v.get("type").and_then(|x| x.as_str()) == Some("rate_limit_event")
                                    && persist_plan_rate_limit_event(&db_pool, &plan_usage_key, v).await
                                {
                                    let _ = app.emit(
                                        "plan_usage_updated",
                                        serde_json::json!({ "run_id": run_id }),
                                    );
                                    crate::commands::claude_accounts::request_balance_eval(&app);
                                }
                                // Keep the live background-task count current: its
                                // `tasks` array is the full set the CLI still has
                                // running, so we gate the turn-end stdin close on it.
                                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                                    && v.get("subtype").and_then(|x| x.as_str())
                                        == Some("background_tasks_changed")
                                {
                                    if let Some(tasks) = v.get("tasks").and_then(|t| t.as_array()) {
                                        live_bg_tasks = tasks.len();
                                    }
                                    // A `background_tasks_changed` that drains the last
                                    // task must EVENTUALLY close stdin (otherwise the
                                    // process never exits and the run is stuck "running"),
                                    // but it must NOT close it immediately: the CLI reacts
                                    // to a task finishing by WAKING the session in-process
                                    // for a fresh turn, and that drain event usually
                                    // arrives just BEFORE the wake turn starts. Closing on
                                    // it tears down the permission channel for the wake
                                    // turn, so every tool needing approval there fails with
                                    // "AbortError: Stream closed".
                                    //
                                    // So we DEFER: arm a short grace deadline instead of
                                    // closing. If a wake turn starts (any assistant/init/
                                    // stream activity) we disarm it below and let the wake
                                    // turn's own `result` close stdin via
                                    // `result_should_close_stdin`. If nothing comes (the
                                    // session is genuinely idle — the drain arrived after
                                    // the wake turn's result), the timer fires and closes.
                                    if drain_should_close_stdin(live_bg_tasks, user_turn_ended_once, turn_active) {
                                        drain_close_deadline =
                                            Some(tokio::time::Instant::now() + DRAIN_CLOSE_GRACE);
                                    } else {
                                        // Count went back up (a new task spawned) — cancel
                                        // any pending deferred close; the new work will
                                        // re-arm when it drains.
                                        drain_close_deadline = None;
                                    }
                                }
                                // Any wake-turn activity cancels a pending deferred
                                // drain-close: the turn is live, so its own `result` will
                                // close stdin once its tasks drain. Closing mid-turn here
                                // is exactly what caused "AbortError: Stream closed".
                                if is_wake_activity(v) {
                                    drain_close_deadline = None;
                                }
                                if is_main_turn_activity(v) {
                                    turn_active = true;
                                }
                                // End the turn: closing stdin makes the sidecar's
                                // input stream close, the query finish, and the
                                // process exit so `run:done` can fire. Follow-ups
                                // continue via resume_run.
                                if v.get("type").and_then(|x| x.as_str()) == Some("result") {
                                    let usage_recorded = capture_usage(
                                        &db_pool, &run_id, &project_id, &project_name,
                                        &engine, last_model.as_deref(), cli_version.as_deref(),
                                        results_in_process, v,
                                    )
                                    .await;
                                    results_in_process += 1;
                                    if usage_recorded {
                                        let _ = app.emit(
                                            "budget_status_updated",
                                            serde_json::json!({ "run_id": run_id }),
                                        );
                                    }
                                    // …but ONLY when this result truly ends the
                                    // session's work. Two cases must NOT close it,
                                    // or the permission channel dies mid-turn and
                                    // tools fail with "AbortError: Stream closed":
                                    //   1. a `result` the user never asked for (a
                                    //      resumed session's leftover background
                                    //      task) — see `result_ends_user_turn`;
                                    //   2. a `result` emitted while background tasks
                                    //      are still running — the CLI keeps the
                                    //      session alive and wakes it in-process
                                    //      when a task finishes.
                                    // The run stays "running" until the last task
                                    // drains; follow-ups then go to this same live
                                    // sidecar rather than a resume.
                                    turn_active = false;
                                    if result_ends_user_turn(v) {
                                        user_turn_ended_once = true;
                                    }
                                    if result_should_close_stdin(v, live_bg_tasks, user_turn_ended_once) {
                                        let mut reg = registry.lock().await;
                                        if let Some(handles) = reg.get_mut(&run_id) {
                                            handles.stdin.take();
                                        }
                                    } else if let Some(cst) = app.try_state::<crate::conductor::ConductorState>() {
                                        // A conductor's turn ended but its sidecar stays
                                        // alive (e.g. a backgrounded Bash), so `run:done`
                                        // won't fire. Signal the turn end on the wake
                                        // channel so a wake a worker deferred meanwhile is
                                        // delivered to the live sidecar instead of lost.
                                        // (When stdin closes, `run:done` covers it.)
                                        if cst.session(&run_id).is_some() {
                                            cst.notify_run_finished(run_id.clone());
                                        }
                                    }
                                    // This turn's usage is now recorded. Re-check
                                    // the global budget; if it tipped over, tell
                                    // the UI to lock the composer so the next turn
                                    // (resume / follow-up) can't be started.
                                    if let Ok(status) =
                                        crate::commands::stats::budget_status_for(&db_pool, &engine, claude_account_id.as_deref()).await
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
                                if let Some(bus) = remote_bus.as_ref().filter(|_| !is_delta) {
                                    bus.publish(crate::remote::RemoteRunEvent::Event {
                                        run_id: run_id.clone(),
                                        event: v.clone(),
                                    });
                                }
                                let _ = app.emit(&format!("run:event:{}", run_id), v.clone());
                            }
                            // ---- non-JSON line (rare) ---------------------------
                            (None, _) => {
                                append_log(&log_buf, &format!("{}\n", l)).await;
                                if let Some(bus) = &remote_bus {
                                    bus.publish(crate::remote::RemoteRunEvent::Output {
                                        run_id: run_id.clone(),
                                        line: l.clone(),
                                        is_stderr: false,
                                    });
                                }
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
                    let actions = liveness.on_output(tokio::time::Instant::now());
                    apply_liveness(actions, &app, &log_buf, &remote_bus, &run_id).await;
                    append_log(&log_buf, &format!("[stderr] {}\n", l)).await;
                    if let Some(bus) = &remote_bus {
                        bus.publish(crate::remote::RemoteRunEvent::Output {
                            run_id: run_id.clone(),
                            line: l.clone(),
                            is_stderr: true,
                        });
                    }
                    let _ = app.emit(
                        &format!("run:output:{}", run_id),
                        serde_json::json!({ "run_id": run_id, "line": l, "is_stderr": true }),
                    );
                }
            }
        }
    }

    // Drain any trailing stderr after stdout closed — bounded: a descendant may
    // hold stderr open long after the session ended. (Skipped when we already
    // stopped waiting on the pipes.)
    let (trailing, stderr_complete) = if detached {
        (Vec::new(), true)
    } else {
        drain_trailing_lines(&mut stderr_reader, SIDECAR_EXIT_GRACE).await
    };
    if !stderr_complete {
        let pgid = registry.lock().await.get(&run_id).and_then(|h| h.pgid);
        watchdog_line(
            &app,
            &log_buf,
            &remote_bus,
            &run_id,
            "warn",
            &detach_notice(pgid),
        )
        .await;
    }
    // Stop reading for good: from here on nothing is emitted for this run except
    // its `run:done` below.
    drop(stdout_reader);
    drop(stderr_reader);
    for l in trailing {
        append_log(&log_buf, &format!("[stderr] {}\n", l)).await;
        if let Some(bus) = &remote_bus {
            bus.publish(crate::remote::RemoteRunEvent::Output {
                run_id: run_id.clone(),
                line: l.clone(),
                is_stderr: true,
            });
        }
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

    // The sidecar is gone, so any request it was blocked on can never be answered.
    crate::runs::permission::clear_pending(&run_id);

    // How the sidecar process ended. stdout EOF usually lands just before the
    // exit, so poll briefly — without holding the registry lock across the wait.
    let mut exit_status: Option<std::process::ExitStatus> = None;
    for _ in 0..60 {
        let polled = {
            let mut reg = registry.lock().await;
            match reg.get_mut(&run_id) {
                Some(h) => h.child.try_wait().ok().flatten().map(Some),
                None => Some(None), // cancelled: removed by cancel_run
            }
        };
        if let Some(status) = polled {
            exit_status = status;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    // Hold the registry lock until `run:done` is emitted. A resume (e.g. a
    // conductor `session_send` that sees the DB status flip) needs this lock to
    // register the next turn, so its `run:activated` can never overtake this
    // turn's `run:done` — otherwise the webview applies them out of order and
    // shows the live worker as finished.
    let mut reg = registry.lock().await;
    // Cancelled iff the entry was already removed by cancel_run.
    let was_cancelled = reg.remove(&run_id).is_none();
    let final_status = final_run_status(
        was_cancelled,
        unresolved_error,
        closed_code,
        closed_signal.as_deref(),
        exit_status,
    );
    let finished_at = chrono::Utc::now().to_rfc3339();
    // A turn finishing is not user activity, so it must not float the run in
    // History. last_activity_at is only pinned to the run's existing sort key
    // (old row values) so the new finished_at can't take over the COALESCE.
    let _ = sqlx::query(
        "UPDATE runs SET status = ?, finished_at = ?, \
         last_activity_at = COALESCE(last_activity_at, finished_at, started_at, created_at, ?), \
         output_path = ? WHERE id = ?",
    )
    .bind(final_status)
    .bind(&finished_at)
    .bind(&finished_at)
    .bind(log_path.to_string_lossy().as_ref())
    .bind(&run_id)
    .execute(&db_pool)
    .await;

    if let Some(bus) = &remote_bus {
        bus.publish(crate::remote::RemoteRunEvent::Done {
            run_id: run_id.clone(),
            status: final_status.to_string(),
        });
    }
    let _ = app.emit(
        &format!("run:done:{}", run_id),
        serde_json::json!({ "run_id": run_id, "status": final_status }),
    );
    drop(reg);

    // Close the conductor control loop: hand this finished run's id to the
    // conductor wake consumer (a channel send, never a direct resume). If the run
    // is a worker, the consumer auto-resumes its idle conductor so it reads the
    // result and continues without the human having to prod it; if the run is
    // itself a conductor, the consumer frees its auto-wake slot. The channel hop
    // keeps this drain future independent of `resume_run` (which spawns another
    // drain), which would otherwise make the drain's auto-traits cyclic.
    if let Some(cst) = app.try_state::<crate::conductor::ConductorState>() {
        cst.notify_run_finished(run_id.clone());
    }
}

/// Grace window after a background task drains to 0 before the deferred
/// drain-close fires. Long enough for the CLI's in-process wake-up turn to start
/// emitting (which disarms the close), short enough that a genuinely idle session
/// still finishes promptly. Biased generous: closing late only delays `run:done`
/// by this much, while closing early reintroduces "AbortError: Stream closed".
const DRAIN_CLOSE_GRACE: std::time::Duration = std::time::Duration::from_millis(3000);

/// Read whatever is left on a pipe after the session ended, but give up after
/// `grace`: a descendant that inherited the pipe can hold it open indefinitely.
/// Returns the lines read and whether EOF was reached.
async fn drain_trailing_lines<R>(
    reader: &mut tokio::io::Lines<R>,
    grace: std::time::Duration,
) -> (Vec<String>, bool)
where
    R: tokio::io::AsyncBufRead + Unpin,
{
    let mut lines = Vec::new();
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Ok(Ok(Some(l))) => lines.push(l),
            Ok(_) => return (lines, true),
            Err(_) => return (lines, false),
        }
    }
}

/// Is any process of group `pgid` still alive (signal 0 = existence check)?
fn process_group_alive(pgid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::killpg(pgid as libc::pid_t, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pgid;
        false
    }
}

/// Visible notice, always emitted when we stop waiting on pipes some process
/// still holds open after the session ended. Those processes are left running
/// (never killed on this path). When the session's original process group is
/// still alive the notice names it so the user can stop it; otherwise the holder
/// left the group (e.g. `setsid()`) and can't be addressed by it.
fn detach_notice(pgid: Option<u32>) -> String {
    match pgid.filter(|g| process_group_alive(*g)) {
        Some(pgid) => format!(
            "⚠ The session has ended, but process(es) it started are still running (process group {pgid}) and kept its output open. They were left running and are listed under Background processes, where you can stop them (or run `kill -TERM -{pgid}`)."
        ),
        None => "⚠ The session has ended, but a process it started (outside its process group) is still holding its output open. Devdy stopped waiting for it and left it running; it is listed under Background processes.".to_string(),
    }
}

/// Final status of a run whose sidecar is gone. `cancelled` wins (the user
/// stopped it); otherwise `failed` when the session ended on an unresolved
/// `_devdy_error`, reported a non-zero code or a signal with `_devdy_closed`, or
/// the process exited unsuccessfully. An unknown exit status (not reaped yet,
/// or left running when we stopped waiting) counts as success.
fn final_run_status(
    was_cancelled: bool,
    unresolved_error: bool,
    closed_code: Option<i64>,
    closed_signal: Option<&str>,
    exit_status: Option<std::process::ExitStatus>,
) -> &'static str {
    if was_cancelled {
        return "cancelled";
    }
    let bad_close = closed_code.is_some_and(|c| c != 0) || closed_signal.is_some();
    let bad_exit = exit_status.is_some_and(|s| !s.success());
    if unresolved_error || bad_close || bad_exit {
        "failed"
    } else {
        "done"
    }
}

/// Does this `_devdy_ready` declare the sidecar's liveness heartbeat?
fn ready_declares_heartbeat(ready: &Value) -> bool {
    ready
        .pointer("/capabilities/heartbeat_ms")
        .and_then(|v| v.as_u64())
        .is_some_and(|ms| ms > 0)
}

/// Is this raw SDK message the MAIN thread's turn making progress? Unlike
/// [`is_wake_activity`], messages of subagents (`parent_tool_use_id` set) don't
/// count: background subagents keep streaming after the parent turn's `result`
/// (seen in real logs), and treating that as an active turn would block the
/// drain-close forever and flag a finished turn as "quiet".
fn is_main_turn_activity(value: &Value) -> bool {
    let main_thread = matches!(value.get("parent_tool_use_id"), None | Some(Value::Null));
    match value.get("type").and_then(|t| t.as_str()) {
        Some("assistant") | Some("user") | Some("stream_event") => main_thread,
        Some("system") => value.get("subtype").and_then(|s| s.as_str()) == Some("init"),
        _ => false,
    }
}

/// Is this raw SDK message a partial-message delta (`includePartialMessages`)?
fn is_partial_delta(value: &Value) -> bool {
    value.get("type").and_then(|t| t.as_str()) == Some("stream_event")
}

/// Does this raw SDK stream message mean a (wake-up) turn is actively running?
/// Used to disarm a pending deferred drain-close: once the CLI has started a turn,
/// its own `result` is what should close stdin — not the drain event that preceded
/// it. Only `system/init` counts among system messages; `background_tasks_changed`
/// (also `type: system`) is the drain signal itself and must not disarm.
fn is_wake_activity(value: &Value) -> bool {
    match value.get("type").and_then(|t| t.as_str()) {
        Some("assistant") | Some("user") | Some("stream_event") => true,
        Some("system") => {
            value.get("subtype").and_then(|s| s.as_str()) == Some("init")
        }
        _ => false,
    }
}

/// Does this `result` event close the turn the user started?
///
/// A user turn ends with a plain `result` (no `origin`). But the CLI also runs
/// turns nobody typed a prompt for — a background task that outlived the
/// previous session, a scheduled wake-up — and those emit their own `result`,
/// tagged with `origin: { kind: … }`. On a resume these land BEFORE the user's
/// real turn even starts, so treating them as "turn over" closes the sidecar's
/// stdin too early: the SDK then ends the CLI's input stream and every
/// subsequent permission request dies instantly with "AbortError: Stream
/// closed", leaving the turn running but unable to ask for approval.
///
/// Only origin-less results end the user's turn; the rest are pass-through.
fn result_ends_user_turn(value: &Value) -> bool {
    match value.get("origin") {
        None | Some(Value::Null) => true,
        Some(_) => false,
    }
}

/// Should this `result` cause the drain to close the sidecar's stdin (ending the
/// turn and letting the process exit)?
///
/// A live background task always keeps stdin open: the CLI will wake the session
/// in-process when it finishes, and closing stdin would kill that wake-up turn's
/// permission channel. With no tasks left, stdin closes when EITHER:
///   • this `result` ends the user's own turn (origin-less); or
///   • the user's turn already ended earlier in this sidecar (`user_turn_ended`)
///     and this is the origin-tagged `task-notification` result that fires once
///     the last background task the turn spawned drains — the true end of work.
/// The `user_turn_ended` guard is what stops a spurious leftover task-notification
/// result (arriving BEFORE the user's turn on resume) from closing stdin early.
fn result_should_close_stdin(value: &Value, live_bg_tasks: usize, user_turn_ended: bool) -> bool {
    if live_bg_tasks != 0 {
        return false;
    }
    result_ends_user_turn(value) || user_turn_ended
}

/// Is a `background_tasks_changed` draining to an empty set eligible to (eventually)
/// close stdin? Same shape as `result_should_close_stdin` minus the `result` check,
/// for the case where the count reaches 0 via a `background_tasks_changed` event.
///
/// NOTE: a `true` here no longer closes stdin immediately — the caller ARMS a
/// [`DRAIN_CLOSE_GRACE`] deadline instead, because the drain event usually lands
/// just before the CLI's in-process wake-up turn and closing now would kill that
/// turn's permission channel ("AbortError: Stream closed"). The deadline is
/// disarmed by [`is_wake_activity`] if a wake turn starts, and only fires (closes)
/// if the session stays idle. The `user_turn_ended` guard still stops a spurious
/// leftover task draining on resume (before the real turn runs) from arming at all.
///
/// `turn_active` (a turn has streamed content since the last `result`) also
/// blocks it: `user_turn_ended` stays true for the sidecar's whole life, so a
/// task draining in the MIDDLE of a later follow-up turn — e.g. a backgrounded
/// apply finishing while a foreground poll loop is still running — would
/// otherwise close stdin mid-turn. The next tool needing approval then hangs
/// with no prompt ever shown. That turn's own `result` closes instead.
fn drain_should_close_stdin(
    live_bg_tasks: usize,
    user_turn_ended: bool,
    turn_active: bool,
) -> bool {
    live_bg_tasks == 0 && user_turn_ended && !turn_active
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
pub(crate) async fn persist_plan_usage(
    db_pool: &sqlx::SqlitePool,
    key: &str,
    usage: &Value,
) -> bool {
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
    let prior = load_plan_usage_snapshot(db_pool, key).await;
    merge_prior_window_status(&mut snapshot, &prior);

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

/// Persist `rate_limits` carried on Claude's `system.init`. This is the first
/// and often most reliable signal available to an idle usage probe, before the
/// experimental control `/usage` request has a chance to return.
pub(crate) async fn persist_plan_init_rate_limits(
    db_pool: &sqlx::SqlitePool,
    key: &str,
    event: &Value,
) -> bool {
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
    let prior = load_plan_usage_snapshot(db_pool, key).await;
    merge_prior_window_status(&mut snapshot, &prior);

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
        .bind(snapshot.to_string())
        .execute(db_pool)
        .await
        .is_ok()
}

/// Merge a live Claude `rate_limit_event` into the same `plan_usage` snapshot
/// used by the global budget badge. These events can arrive before the
/// experimental `/usage` helper returns, so they keep the warning at the newest
/// known utilization while a turn is streaming.
pub(crate) async fn persist_plan_rate_limit_event(
    db_pool: &sqlx::SqlitePool,
    key: &str,
    event: &Value,
) -> bool {
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

    let mut snapshot = load_plan_usage_snapshot(db_pool, key)
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

    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(key)
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

/// Read the current plan-usage snapshot for `key` from settings, if any.
async fn load_plan_usage_snapshot(db_pool: &sqlx::SqlitePool, key: &str) -> Option<Value> {
    let stored: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
            .bind(key)
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

/// Live-capture usage at turn end, stamped with the current time. Claude
/// results carry `modelUsage` — complete (subagents included) and per model —
/// so the turn is recorded from it as one row per model; results without it
/// (Codex) or runs without a baseline take the single-row legacy path.
#[allow(clippy::too_many_arguments)]
async fn capture_usage(
    db_pool: &sqlx::SqlitePool,
    run_id: &str,
    project_id: &str,
    project_name: &str,
    engine: &str,
    model: Option<&str>,
    cli_version: Option<&str>,
    results_in_process: usize,
    value: &Value,
) -> bool {
    use crate::runs::model_usage as mu;
    let created_at = chrono::Utc::now().to_rfc3339();
    let Some(cur) = mu::parse_model_usage(value) else {
        return insert_usage_from_result(
            db_pool, run_id, project_id, project_name, engine, model, value, &created_at,
        )
        .await;
    };
    let per_model = mu::live_turn_usage(db_pool, run_id, &cur, cli_version, results_in_process).await;
    let mut recorded = false;
    if per_model.is_none() {
        recorded = insert_usage_from_result(
            db_pool, run_id, project_id, project_name, engine, model, value, &created_at,
        )
        .await;
    }
    let meta = mu::RowMeta {
        run_id,
        project_id,
        project_name,
        engine,
        created_at: &created_at,
        deleted_run: false,
    };
    let baseline = mu::Baseline { usage: cur, carries_over: mu::carries_over(cli_version) };
    match mu::commit_live_turn(db_pool, &meta, per_model.as_ref(), value, &baseline).await {
        Ok(n) => recorded || n > 0,
        Err(e) => {
            eprintln!("[usage] recording turn of run {run_id} failed: {e}");
            recorded
        }
    }
}

/// Persist one usage row from a `result` stream event. Prefers the SDK-reported
/// `total_cost_usd` (Claude) — a session running total, so the row stores its
/// difference from the run's previous total (see `usage_cost`); otherwise
/// estimates the cost from token counts (Codex) and flags it. Empty results (no tokens, no cost) are skipped and
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

    let turn_estimate = crate::runs::pricing::estimate_cost(
        eff_model.as_deref().unwrap_or(""),
        input,
        output,
        cache_creation,
        cache_read,
    );
    let reported = value.get("total_cost_usd").and_then(|v| v.as_f64());
    let (cost, estimated) = match reported {
        Some(total) => {
            let prev = crate::runs::usage_cost::previous_total(db_pool, run_id).await;
            let (c, est) = crate::runs::usage_cost::turn_cost(prev, total, turn_estimate);
            (c, est as i64)
        }
        None => (turn_estimate, 1_i64),
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
            total_tokens, cost_usd, cost_cumulative_usd, cost_estimated, num_turns, duration_ms,
            created_at, deleted_run
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
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
    .bind(reported)
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

#[cfg(test)]
mod turn_end_tests {
    use super::*;

    #[test]
    fn plain_result_ends_the_user_turn() {
        let v = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 61, "origin": null,
        });
        assert!(result_ends_user_turn(&v));
        // Older CLIs omit the field entirely.
        let v = serde_json::json!({ "type": "result", "subtype": "success", "num_turns": 61 });
        assert!(result_ends_user_turn(&v));
    }

    #[test]
    fn background_origin_result_does_not_end_the_user_turn() {
        // Shape seen on resume when a background shell outlived the last session.
        let v = serde_json::json!({
            "type": "result",
            "subtype": "success",
            "num_turns": 0,
            "duration_ms": 9,
            "origin": { "kind": "task-notification" },
        });
        assert!(!result_ends_user_turn(&v));
        // Any other system-initiated origin is treated the same way.
        let v = serde_json::json!({
            "type": "result", "subtype": "success", "origin": { "kind": "wakeup" },
        });
        assert!(!result_ends_user_turn(&v));
    }

    // The four `result` events from the run that surfaced this bug, keyed by
    // (origin, live background tasks at that point) → whether stdin should close.
    #[test]
    fn close_gate_matches_the_real_run() {
        // Turn 2 (resume): spurious task-notification result BEFORE the user's
        // turn runs, no live tasks → must NOT close (user turn hasn't ended yet).
        let bg = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 0,
            "origin": { "kind": "task-notification" },
        });
        assert!(!result_should_close_stdin(&bg, 0, false));

        // Turn 3: real result but a docker build + 2 monitors still running →
        // the CLI will wake this session in-process, so keep stdin open.
        let with_tasks = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 14, "origin": null,
        });
        assert!(!result_should_close_stdin(&with_tasks, 3, false));

        // Turn 4: real result, all background tasks drained → close for real.
        let done = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 1, "origin": null,
        });
        assert!(result_should_close_stdin(&done, 0, false));
    }

    // Regression for run e075dd38: a user turn spawns background subagents, its
    // own foreground `result` lands while they're still running (kept open), then
    // the FINAL `result` that drains the last task is tagged
    // `origin: task-notification`. Once the user turn has ended and no tasks
    // remain, that task-notification result must close stdin — otherwise the
    // process never exits and the run is stuck "running".
    #[test]
    fn task_notification_end_after_user_turn_closes() {
        // Foreground result while 2 subagents still run → keep open, but it marks
        // the user turn as ended.
        let fg = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 15, "origin": null,
        });
        assert!(result_ends_user_turn(&fg));
        assert!(!result_should_close_stdin(&fg, 2, false));

        // Final wake-up result once the last task drained → close for real.
        let wake = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 1,
            "origin": { "kind": "task-notification" },
        });
        assert!(!result_ends_user_turn(&wake));
        assert!(result_should_close_stdin(&wake, 0, /* user_turn_ended */ true));
    }

    // Regression: the `task-notification` wake `result` can arrive BEFORE the
    // `background_tasks_changed` that clears the count. The result branch then
    // sees a non-zero count and keeps stdin open, so the close must happen when
    // the drain event finally brings the count to 0 — otherwise the process
    // never exits and the run (e.g. a conductor worker) is stuck "running".
    #[test]
    fn drain_after_wake_result_closes_when_count_hits_zero() {
        // User turn already ended earlier in this sidecar.
        let user_turn_ended = true;

        // Wake result arrives while the drain event is still in flight (count 1).
        let wake = serde_json::json!({
            "type": "result", "subtype": "success", "num_turns": 1,
            "origin": { "kind": "task-notification" },
        });
        assert!(!result_should_close_stdin(&wake, 1, user_turn_ended));

        // The delayed `background_tasks_changed` drains the last task → close now.
        assert!(drain_should_close_stdin(0, user_turn_ended, false));

        // A leftover task draining BEFORE the user's turn (resume) must NOT close.
        assert!(!drain_should_close_stdin(
            0, /* user_turn_ended */ false, false
        ));
        // Tasks still live never close on drain.
        assert!(!drain_should_close_stdin(2, user_turn_ended, false));
    }

    // Regression: a follow-up turn on a live sidecar (an earlier turn already
    // ended, so `user_turn_ended` is true) whose background task drains while the
    // turn is still running (foreground tool in flight). Arming the close here
    // shut stdin mid-turn and the next tool needing approval hung with no prompt.
    #[test]
    fn drain_mid_turn_does_not_close() {
        assert!(!drain_should_close_stdin(0, true, /* turn_active */ true));
    }

    // Regression: the sidecar exits (and is reaped, so `child.id()` is None) while
    // a descendant that inherited its stdout/stderr keeps the pipes open. The
    // drain must reach teardown anyway — bounded reads, then a detach notice
    // naming the group — WITHOUT killing the descendant.
    #[cfg(unix)]
    #[tokio::test]
    async fn teardown_detaches_from_descendant_holding_pipes() {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg("echo last-line >&2; sleep 30 & exit 0")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        detach_process_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pgid = child.id();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut stderr = BufReader::new(child.stderr.take().unwrap()).lines();
        assert!(child.wait().await.unwrap().success());
        assert_eq!(child.id(), None, "reaped child has no id any more");

        let grace = std::time::Duration::from_millis(300);
        let started = std::time::Instant::now();
        let (out, out_eof) = drain_trailing_lines(&mut stdout, grace).await;
        let (err, err_eof) = drain_trailing_lines(&mut stderr, grace).await;
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "reads are bounded"
        );
        assert!(
            out.is_empty() && !out_eof,
            "stdout still held → no EOF, but we returned"
        );
        assert_eq!(
            err,
            vec!["last-line".to_string()],
            "trailing output is kept"
        );
        assert!(!err_eof);

        let notice = detach_notice(pgid);
        assert!(
            notice.contains(&format!("kill -TERM -{}", pgid.unwrap())),
            "{notice}"
        );
        // Nothing was killed: the descendant is still running.
        assert!(process_group_alive(pgid.unwrap()));

        // Test cleanup only (the drain never does this).
        kill_process_group(pgid.unwrap());
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!process_group_alive(pgid.unwrap()));
        // Original group gone (e.g. the holder did setsid()): still a visible
        // notice, just without a group to kill.
        let gone = detach_notice(pgid);
        assert!(
            gone.starts_with("⚠") && !gone.contains("kill -TERM"),
            "{gone}"
        );
        assert!(!detach_notice(None).contains("kill -TERM"));
    }

    // The holder escaped the session's process group (setsid): the original
    // group is gone, yet the pipe is still held. Detaching must still notify.
    #[cfg(unix)]
    #[tokio::test]
    async fn detach_notice_when_holder_left_the_group() {
        let pid_file = std::env::temp_dir().join(format!("devdy-setsid-{}", uuid::Uuid::new_v4()));
        let mut cmd = tokio::process::Command::new("sh");
        // The holder starts a new session, so it is no longer in our process
        // group (perl's POSIX::setsid is a portable setsid), records its pid,
        // then becomes `sleep` while still holding our stdout.
        cmd.arg("-c")
            .arg("perl -e 'use POSIX qw(setsid); setsid(); open(F, \">$ENV{PIDF}\"); print F $$; close F; exec @ARGV' sleep 30 & exit 0")
            .env("PIDF", &pid_file)
            .stdout(std::process::Stdio::piped());
        detach_process_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pgid = child.id();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        assert!(child.wait().await.unwrap().success());
        // Poll rather than sleep a fixed time: perl may not have written its pid
        // yet on a slow CI runner (the file can also exist but still be empty).
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let holder: i32 = loop {
            let pid = std::fs::read_to_string(&pid_file)
                .ok()
                .and_then(|s| s.trim().parse().ok());
            match pid {
                Some(pid) => break pid,
                None if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await
                }
                None => panic!("holder never wrote its pid to {}", pid_file.display()),
            }
        };

        let (_, eof) =
            drain_trailing_lines(&mut stdout, std::time::Duration::from_millis(300)).await;
        assert!(!eof, "the escaped holder keeps stdout open");
        assert!(
            !process_group_alive(pgid.unwrap()),
            "original group is gone"
        );
        let notice = detach_notice(pgid);
        assert!(
            notice.starts_with("⚠") && !notice.contains("kill -TERM"),
            "{notice}"
        );
        // Detaching killed nothing: the holder is still alive.
        assert_eq!(unsafe { libc::kill(holder, 0) }, 0);

        // Test cleanup only.
        unsafe {
            libc::kill(holder, libc::SIGKILL);
        }
        let _ = std::fs::remove_file(&pid_file);
        drop(stdout);
    }

    #[test]
    fn final_status_rules() {
        use std::os::unix::process::ExitStatusExt;
        let ok = Some(std::process::ExitStatus::from_raw(0));
        let exit1 = Some(std::process::ExitStatus::from_raw(1 << 8));
        let sigkill = Some(std::process::ExitStatus::from_raw(9));
        assert_eq!(final_run_status(false, false, None, None, ok), "done");
        assert_eq!(final_run_status(false, false, None, None, None), "done");
        assert_eq!(final_run_status(false, false, Some(0), None, ok), "done");
        assert_eq!(final_run_status(false, true, None, None, ok), "failed");
        assert_eq!(final_run_status(false, false, None, None, exit1), "failed");
        assert_eq!(
            final_run_status(false, false, None, None, sigkill),
            "failed"
        );
        // `_devdy_closed { code: 1 }` (e.g. codex app-server exited 1): failed even
        // when the process itself exited cleanly or was left running (detached).
        assert_eq!(final_run_status(false, false, Some(1), None, ok), "failed");
        assert_eq!(
            final_run_status(false, false, Some(1), None, None),
            "failed"
        );
        // `_devdy_closed { code: null, signal: "SIGTERM" }` (codex killed by a
        // signal): failed, even if the process exit itself looked clean.
        assert_eq!(
            final_run_status(false, false, None, Some("SIGTERM"), ok),
            "failed"
        );
        assert_eq!(
            final_run_status(false, false, None, Some("SIGTERM"), None),
            "failed"
        );
        // A user cancel is never relabelled.
        assert_eq!(
            final_run_status(true, true, Some(1), Some("SIGKILL"), exit1),
            "cancelled"
        );
    }

    #[test]
    fn ready_capability_detection() {
        let ready = serde_json::json!({ "type": "_devdy_ready", "capabilities": { "heartbeat_ms": 30000 } });
        assert!(ready_declares_heartbeat(&ready));
        assert!(!ready_declares_heartbeat(
            &serde_json::json!({ "type": "_devdy_ready" })
        ));
    }

    // Background subagents stream after the parent's `result`; only main-thread
    // content (or a fresh init) marks the parent turn active.
    #[test]
    fn subagent_messages_are_not_main_turn_activity() {
        let sub = serde_json::json!({
            "type": "assistant", "parent_tool_use_id": "toolu_01CDnJDdZrRyPtWw6Eb3ujjS",
            "message": { "content": [] },
        });
        assert!(is_wake_activity(&sub));
        assert!(!is_main_turn_activity(&sub));

        let main =
            serde_json::json!({ "type": "assistant", "parent_tool_use_id": null, "message": {} });
        assert!(is_main_turn_activity(&main));
        let main_no_field = serde_json::json!({ "type": "user", "message": {} });
        assert!(is_main_turn_activity(&main_no_field));
        let init = serde_json::json!({ "type": "system", "subtype": "init" });
        assert!(is_main_turn_activity(&init));
        let progress = serde_json::json!({ "type": "system", "subtype": "task_progress" });
        assert!(!is_main_turn_activity(&progress));
        let drain = serde_json::json!({ "type": "system", "subtype": "background_tasks_changed", "tasks": [] });
        assert!(!is_main_turn_activity(&drain));
    }

    // Deferred drain-close (fix for "AbortError: Stream closed"): when a background
    // task drains to 0, the drain event often arrives just BEFORE the CLI's
    // in-process wake-up turn starts. Closing stdin on it kills that turn's
    // permission channel. So the drain now only ARMS a grace deadline; any wake-turn
    // activity disarms it (the wake turn's own `result` closes instead), and the
    // timer closes only if the session stays idle. `is_wake_activity` is the disarm
    // signal — it must fire for turn content but NOT for the drain event itself.
    #[test]
    fn only_stream_events_are_partial_deltas() {
        assert!(is_partial_delta(&serde_json::json!({ "type": "stream_event" })));
        assert!(!is_partial_delta(&serde_json::json!({ "type": "assistant" })));
        assert!(!is_partial_delta(&serde_json::json!({ "type": "result" })));
    }

    #[test]
    fn wake_activity_disarms_but_drain_signal_does_not() {
        // Wake-turn content → disarm.
        assert!(is_wake_activity(&serde_json::json!({ "type": "assistant" })));
        assert!(is_wake_activity(&serde_json::json!({ "type": "user" })));
        assert!(is_wake_activity(&serde_json::json!({ "type": "stream_event" })));
        assert!(is_wake_activity(
            &serde_json::json!({ "type": "system", "subtype": "init" })
        ));

        // The drain signal itself is `type: system` too, but must NOT disarm —
        // otherwise it would cancel the very deadline it just armed.
        assert!(!is_wake_activity(&serde_json::json!({
            "type": "system", "subtype": "background_tasks_changed", "tasks": []
        })));
        // A `result` is handled by its own close gate, not by the disarm path.
        assert!(!is_wake_activity(&serde_json::json!({ "type": "result" })));
    }
}
