use crate::commands::github::RunRecord;
use crate::db::Db;
use crate::runs::broker::BrokerHandle;
use crate::runs::sidecar::{
    augment_command_path, detach_process_group, drain_sidecar, kill_process_group,
    resolve_codex_sidecar, resolve_sidecar, sdk_permission_mode,
};
use crate::runs::ssh_access::{self, SshAccessGuard};
use crate::runs::{BrokerRunCtx, BrokerRunGuard, BrokerRuns, RunHandles, RunRegistry};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::sync::Mutex as TokioMutex;
use uuid::Uuid;

/// A pasted/attached image carried on a user turn. `data` is raw base64 (no
/// `data:` prefix). Both sidecars accept base64: Claude as an image content
/// block, Codex as a `data:` URL `image_url` input item — so no temp file.
#[derive(Debug, Clone, Deserialize)]
pub struct ImageAttachment {
    pub media_type: String,
    pub data: String,
}

#[derive(Debug, Deserialize)]
pub struct StartRunPayload {
    pub run_id: String,
    pub engine_override: Option<String>,
    /// Per-run override of permission mode. When None we fall back to settings.default_permission_mode.
    pub permission_mode_override: Option<String>,
    /// Custom user prompt to use instead of the default analyze/review template.
    /// The file reference is still appended automatically.
    pub prompt_override: Option<String>,
    /// Per-run model override. When None/empty we fall back to the per-engine
    /// default model setting (claude_model / codex_model); empty means engine default.
    pub model_override: Option<String>,
    /// Images pasted into the composer for the first turn.
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    /// Proceed even when the global token budget is exceeded (user confirmed).
    #[serde(default)]
    pub override_budget: bool,
}

/// JSON the sidecar consumes on its `prompt` line: a flat array of
/// `{ media_type, data }` the sidecar formats per engine.
fn images_payload(images: &[ImageAttachment]) -> Vec<serde_json::Value> {
    images
        .iter()
        .map(|img| serde_json::json!({ "media_type": img.media_type, "data": img.data }))
        .collect()
}

/// Stream-json `content` for the persisted log. With images, content becomes a
/// block array (Claude-shaped image blocks) so the history view re-renders the
/// thumbnails; without images it stays a plain string for back-compat.
fn log_user_content(text: &str, images: &[ImageAttachment]) -> serde_json::Value {
    if images.is_empty() {
        return serde_json::Value::String(text.to_string());
    }
    let mut blocks: Vec<serde_json::Value> = Vec::new();
    if !text.is_empty() {
        blocks.push(serde_json::json!({ "type": "text", "text": text }));
    }
    for img in images {
        blocks.push(serde_json::json!({
            "type": "image",
            "source": { "type": "base64", "media_type": img.media_type, "data": img.data },
        }));
    }
    serde_json::Value::Array(blocks)
}

async fn claude_usage_capture_mode(db: &Db) -> &'static str {
    match crate::commands::stats::budget_status_for(db, "claude").await {
        Ok(status) if status.source == "plan" && (status.is_warning || status.is_over) => "warning",
        _ => "normal",
    }
}

#[tauri::command]
pub async fn start_run(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    payload: StartRunPayload,
) -> Result<(), String> {
    use sqlx::Row;

    // Load run + project info
    let run_row = sqlx::query(
        "SELECT r.id, r.project_id, r.type, r.ref_number, r.input_path, r.output_path, r.engine, r.status,
                p.path as project_path
         FROM runs r
         JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&payload.run_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let run_type: String = run_row.get("type");
    let project_id: String = run_row.get("project_id");
    let input_path: Option<String> = run_row.get("input_path");
    let output_path: Option<String> = run_row.get("output_path");
    let status: String = run_row.get("status");
    let project_path: String = run_row.get("project_path");

    let is_session = run_type == "session";

    // Issue/PR runs need their cached input markdown; standalone sessions don't.
    // Prefer input_path, fall back to output_path only when still 'fetched'
    // (legacy rows pre-input_path migration).
    let input_path: Option<String> = if is_session {
        None
    } else {
        Some(
            input_path
                .or_else(|| {
                    if status == "fetched" {
                        output_path
                    } else {
                        None
                    }
                })
                .ok_or("No input file for this run — please re-fetch the issue/PR")?,
        )
    };

    // Load engine settings
    let settings_rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut claude_path = "claude".to_string();
    let mut codex_path = "codex".to_string();
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut codex_sidecar_path = String::new();
    let mut claude_model = String::new();
    let mut codex_model = String::new();
    let mut extra_args = String::new();
    let mut analyze_prompt =
        "Please analyze this GitHub issue and create a detailed implementation plan.".to_string();
    let mut review_prompt =
        "Please review this pull request according to the configured skills.".to_string();
    let mut default_permission_mode = "default".to_string();
    let mut default_engine = "claude".to_string();

    for row in &settings_rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "default_engine" => {
                if !value.trim().is_empty() {
                    default_engine = value;
                }
            }
            "claude_path" => claude_path = value,
            "codex_path" => codex_path = value,
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "codex_sidecar_path" => codex_sidecar_path = value,
            "claude_model" => claude_model = value,
            "codex_model" => codex_model = value,
            "extra_args" => extra_args = value,
            "analyze_issue_prompt" => analyze_prompt = value,
            "review_pr_prompt" => review_prompt = value,
            "default_permission_mode" => default_permission_mode = value,
            _ => {}
        }
    }

    // Resolve the engine: per-run override wins, else the global default engine.
    let engine = payload.engine_override.unwrap_or(default_engine);

    // Global budget guardrail: refuse to start a new run when over budget
    // (real plan utilization for this engine, or the self-imposed token
    // fallback), unless the user explicitly overrode it.
    crate::commands::stats::enforce_budget(db.inner(), &engine, payload.override_budget).await?;

    let permission_mode = payload
        .permission_mode_override
        .filter(|v| is_valid_permission_mode(v))
        .unwrap_or_else(|| {
            if is_valid_permission_mode(&default_permission_mode) {
                default_permission_mode
            } else {
                "default".to_string()
            }
        });

    // Resolve the model: per-run override wins, else the per-engine default
    // setting. An empty result means "let the engine pick its default".
    let engine_default_model = if engine == "codex" {
        codex_model
    } else {
        claude_model
    };
    let model = resolve_model(payload.model_override, engine_default_model);

    let custom_prompt = payload
        .prompt_override
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    let prompt = if is_session {
        // A session's prompt is purely the user's first message.
        custom_prompt
            .ok_or("Hãy nhập nội dung công việc để bắt đầu session.")?
            .to_string()
    } else if run_type == "analyze_issue" {
        let base = custom_prompt.unwrap_or(analyze_prompt.as_str());
        format!(
            "{}\n\nThe issue details are in: {}",
            base,
            input_path.as_deref().unwrap_or("")
        )
    } else {
        let base = custom_prompt.unwrap_or(review_prompt.as_str());
        format!(
            "{}\n\nThe PR details are in: {}",
            base,
            input_path.as_deref().unwrap_or("")
        )
    };

    // For a session, derive a sidebar title from the first message (once).
    if is_session {
        let title: String = prompt.chars().take(60).collect();
        let _ = sqlx::query("UPDATE runs SET title = ? WHERE id = ? AND title IS NULL")
            .bind(title.trim())
            .bind(&payload.run_id)
            .execute(db.inner())
            .await;
    }

    // Per-run scratch dir (also where the log lands).
    let runs_dir = Path::new(&project_path).join(".devdy").join("runs");
    fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
    let log_path = runs_dir.join(format!("{}.log", payload.run_id));

    // Update run status to running
    let started_at = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE runs SET status = 'running', engine = ?, started_at = ? WHERE id = ?")
        .bind(&engine)
        .bind(&started_at)
        .bind(&payload.run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    // Build extra args vec
    let extra: Vec<String> = extra_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let run_id = payload.run_id.clone();
    let db_pool = db.inner().clone();
    let registry_arc = registry.inner().clone();
    let session_id = Arc::new(TokioMutex::new(None::<String>));
    let log_buf = Arc::new(TokioMutex::new(String::new()));

    if engine == "claude" {
        // ---- Claude: drive the Agent-SDK sidecar (keeps subscription auth) ----
        let (node_bin, sidecar_script) = resolve_sidecar(&app, &node_path, &sidecar_path)?;
        let mut cmd = Command::new(&node_bin);
        cmd.current_dir(&project_path).arg(&sidecar_script);
        augment_command_path(&mut cmd);
        // GĐ3: per-run credential broker + gh/glab shim. Prepends the shim dir to
        // PATH and sets DEVDY_BROKER_SOCK/DEVDY_PROJECT_ID. Fail-closed: no token
        // is ever placed on the sidecar env. Held in RunHandles for Drop cleanup.
        let (broker_run, ssh_access) = wire_broker(
            &app,
            db.inner(),
            &mut cmd,
            &payload.run_id,
            &project_id,
            &project_path,
        )
        .await?;
        // Honor a custom claude binary if the user configured one; default uses
        // the SDK's bundled CLI (both read the same Keychain login).
        if claude_path != "claude" && !claude_path.trim().is_empty() {
            cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
        }
        cmd.env(
            "DEVDY_USAGE_CAPTURE_MODE",
            claude_usage_capture_mode(db.inner()).await,
        );
        cmd.env("DEVDY_USAGE_POLL_MS", "60000");
        if let Some(m) = &model {
            cmd.env("DEVDY_MODEL", m);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        detach_process_group(&mut cmd);
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn sidecar ({}): {}", node_bin, e))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut stdin_handle = child.stdin.take();

        // Kick off the first turn.
        if let Some(stdin) = stdin_handle.as_mut() {
            // GĐ6 (AC1): describe the project's pre-wired git account(s) to Claude
            // via an appended system prompt. `None` when nothing is linked → no
            // field is added and the run behaves exactly as before (AC6). Never
            // contains a token — label/username/host metadata only.
            let account_context =
                crate::runs::broker::token::build_account_context(&db_pool, &project_id).await;
            let mut options = serde_json::json!({
                "cwd": project_path,
                "permissionMode": sdk_permission_mode(&permission_mode),
                "model": model,
            });
            if let Some(ctx) = &account_context {
                options["appendSystemPrompt"] = serde_json::Value::String(ctx.clone());
            }
            // Inject the project's enabled MCP servers for the ACTUAL run engine
            // (QĐ-2). Claude gets the full 3-transport map via options.mcpServers;
            // MCP tools still flow through the existing canUseTool permission path.
            let (mcp, _skipped) =
                crate::commands::mcp::resolve_project_mcp_servers(&db_pool, &project_id, "claude")
                    .await;
            if !mcp.is_null() {
                options["mcpServers"] = mcp;
            }
            let first = serde_json::json!({
                "type": "prompt",
                "text": prompt,
                "images": images_payload(&payload.images),
                "options": options,
            });
            stdin
                .write_all(format!("{}\n", first).as_bytes())
                .await
                .ok();
            stdin.flush().await.ok();
        }

        // Record the initial user message in the log (the SDK doesn't echo it).
        {
            let mut buf = log_buf.lock().await;
            let synthetic = serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": log_user_content(&prompt, &payload.images) },
            });
            buf.push_str(&synthetic.to_string());
            buf.push('\n');
        }

        {
            let mut reg = registry.lock().await;
            reg.insert(
                payload.run_id.clone(),
                RunHandles {
                    child,
                    stdin: stdin_handle,
                    session_id: session_id.clone(),
                    log_buf: log_buf.clone(),
                    broker_run: Some(broker_run),
                    ssh_access,
                },
            );
        }

        tokio::spawn(drain_sidecar(
            app,
            run_id,
            project_path,
            stdout,
            stderr,
            db_pool,
            registry_arc,
            session_id,
            log_buf,
            log_path,
            false,
        ));
        return Ok(());
    }

    // ---- Codex: drive the `codex app-server` sidecar (subscription auth) ----
    // Same NDJSON `_devdy_*` protocol as the Claude sidecar, so drain_sidecar,
    // the stream renderer, and the permission modal all work unchanged. (Replaces
    // the old one-shot `codex exec` path, which had no streaming/approval/multi-turn.)
    let _ = &extra; // codex tuning is now via sandbox/approval env, not exec args
    let (node_bin, sidecar_script) = resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?;
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&project_path).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    let (broker_run, ssh_access) = wire_broker(
        &app,
        db.inner(),
        &mut cmd,
        &payload.run_id,
        &project_id,
        &project_path,
    )
    .await?;
    if codex_path != "codex" && !codex_path.trim().is_empty() {
        cmd.env("DEVDY_CODEX_PATH", &codex_path);
    }
    cmd.env("DEVDY_PERMISSION_MODE", &permission_mode);
    if let Some(m) = &model {
        cmd.env("DEVDY_CODEX_MODEL", m);
    }
    // Inject the project's enabled MCP servers for Codex (QĐ-2: actual engine).
    // Codex supports stdio + streamable HTTP; legacy SSE servers land in `skipped`.
    let (codex_mcp, mcp_skipped) =
        crate::commands::mcp::resolve_project_mcp_servers(&db_pool, &project_id, "codex").await;
    if !codex_mcp.is_null() {
        cmd.env("DEVDY_CODEX_MCP", codex_mcp.to_string());
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn codex sidecar ({}): {}", node_bin, e))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let mut stdin_handle = child.stdin.take();

    // Kick off the first turn.
    if let Some(stdin) = stdin_handle.as_mut() {
        let account_context =
            crate::runs::broker::token::build_account_context(&db_pool, &project_id).await;
        let prompt_text = account_context
            .as_ref()
            .map(|ctx| format!("{ctx}\n\nUser request:\n{prompt}"))
            .unwrap_or_else(|| prompt.clone());
        let first = serde_json::json!({
            "type": "prompt",
            "text": prompt_text,
            "images": images_payload(&payload.images),
        });
        stdin
            .write_all(format!("{}\n", first).as_bytes())
            .await
            .ok();
        stdin.flush().await.ok();
    }

    // Record the initial user message in the log (the sidecar doesn't echo it).
    {
        let mut buf = log_buf.lock().await;
        let synthetic = serde_json::json!({
            "type": "user",
            "message": { "role": "user", "content": log_user_content(&prompt, &payload.images) },
        });
        buf.push_str(&synthetic.to_string());
        buf.push('\n');

        // Codex app-server does not support legacy SSE. When such servers are dropped,
        // surface a one-line note in the run log so the user knows what/why.
        if !mcp_skipped.is_empty() {
            let text = format!(
                "Bỏ qua {} MCP server SSE vì Codex chỉ hỗ trợ stdio/streamable HTTP: {}",
                mcp_skipped.len(),
                mcp_skipped.join(", ")
            );
            let note = serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": text },
            });
            buf.push_str(&note.to_string());
            buf.push('\n');
        }
    }

    {
        let mut reg = registry.lock().await;
        reg.insert(
            payload.run_id.clone(),
            RunHandles {
                child,
                stdin: stdin_handle,
                session_id: session_id.clone(),
                log_buf: log_buf.clone(),
                broker_run: Some(broker_run),
                ssh_access,
            },
        );
    }

    tokio::spawn(drain_sidecar(
        app,
        run_id,
        project_path,
        stdout,
        stderr,
        db_pool,
        registry_arc,
        session_id,
        log_buf,
        log_path,
        false,
    ));
    Ok(())
}

/// Resolve the effective model from a per-run override and the per-engine
/// default. Returns None when neither is set (engine picks its own default).
fn resolve_model(override_model: Option<String>, default_model: String) -> Option<String> {
    override_model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let d = default_model.trim().to_string();
            if d.is_empty() {
                None
            } else {
                Some(d)
            }
        })
}

fn is_valid_permission_mode(mode: &str) -> bool {
    matches!(
        mode,
        "default" | "acceptEdits" | "bypassPermissions" | "plan" | "auto" | "dontAsk"
    )
}

/// Resolve the `sidecar-proxy/` directory holding brokered tool shims. Mirrors
/// `resolve_sidecar_script`: explicit `DEVDY_SHIM_DIR` env → bundled
/// `<resource_dir>/sidecar-proxy` → dev fallback `<crate>/../sidecar-proxy`.
fn resolve_shim_dir(app: &AppHandle) -> Result<PathBuf, String> {
    if let Ok(p) = std::env::var("DEVDY_SHIM_DIR") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Ok(p);
        }
    }
    let bundled = app
        .path()
        .resource_dir()
        .ok()
        .map(|r| r.join("sidecar-proxy"))
        .filter(|p| p.exists());
    let dir = bundled.unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("sidecar-proxy")
    });
    if !dir.exists() {
        return Err(format!(
            "shim dir not found at {}. Ensure the sidecar-proxy/ directory is \
             present, or set DEVDY_SHIM_DIR.",
            dir.display()
        ));
    }
    Ok(dir)
}

/// Prepend the shim dir to the command's PATH so brokered tools resolve to the
/// Devdy shims first. Must be called AFTER `augment_command_path` so the shim
/// dir ends up ABSOLUTELY first, ahead of the real binaries in the login PATH.
fn prepend_shim_path(cmd: &mut Command, shim_dir: &Path) {
    // Read the PATH already set on the command (by augment_command_path); fall
    // back to the process PATH if it was somehow not set.
    let current = cmd
        .as_std()
        .get_envs()
        .find(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
        .and_then(|(_, v)| v)
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_else(|| std::env::var("PATH").unwrap_or_default());
    let joined = if current.is_empty() {
        shim_dir.display().to_string()
    } else {
        format!("{}:{}", shim_dir.display(), current)
    };
    cmd.env("PATH", joined);
}

/// Register this run with the app-wide singleton broker and wire the broker
/// socket + shim PATH into the sidecar command. Returns a `BrokerRunGuard` to
/// store in `RunHandles`; its Drop deregisters the run (the socket itself lives
/// for the whole app and is NOT torn down per run — that is the fix for the
/// per-run "broker unreachable" race).
///
/// Fail-closed: this NEVER sets `GH_TOKEN`/`GITLAB_TOKEN` or AWS secret values
/// on the sidecar env — credentials only ever reach the real child tool via
/// shim / credential_process.
async fn wire_broker(
    app: &AppHandle,
    db: &Db,
    cmd: &mut Command,
    run_id: &str,
    project_id: &str,
    project_path: &str,
) -> Result<(BrokerRunGuard, Option<SshAccessGuard>), String> {
    // The singleton broker (and its socket) were started at app setup and live
    // in managed state. Read its stable socket path.
    let sock_path = app
        .try_state::<BrokerHandle>()
        .ok_or_else(|| "credential broker not initialized".to_string())?
        .path
        .clone();

    // Register the run as alive so its `Ask` modal can be routed. The guard
    // deregisters on Drop when the run's `RunHandles` is dropped.
    let runs = app.state::<BrokerRuns>().inner().clone();
    let guard = BrokerRunGuard::register(
        runs,
        run_id.to_string(),
        BrokerRunCtx {
            cwd: Some(project_path.to_string()),
        },
    );

    // Shim dir goes FIRST on PATH (after augment_command_path has run).
    let shim_dir = resolve_shim_dir(app)?;
    prepend_shim_path(cmd, &shim_dir);
    cmd.env("DEVDY_BROKER_SOCK", &sock_path);
    cmd.env("DEVDY_PROJECT_ID", project_id);
    // GĐ7: the shim echoes this back so the broker can route `Ask` to this run.
    cmd.env("DEVDY_RUN_ID", run_id);

    // GĐ4: route `git` HTTPS auth through the Devdy credential helper (per-run,
    // never touching global git config) + set the commit identity from the
    // linked account.
    let helper_path = shim_dir.join("git-credential-devdy");
    wire_git_config(cmd, &helper_path);
    wire_commit_identity(cmd, db, project_id).await;

    // AWS: route SDK credential loading through credential_process for keys
    // accounts. The `aws` CLI itself is broker-gated by the PATH shim.
    let aws_helper_path = shim_dir.join("aws-credential-devdy");
    wire_aws_config(app, db, cmd, run_id, project_id, &aws_helper_path).await;

    // ssh-transparent-connect: wire per-run transparent SSH for this project's
    // mapped VPS servers. No mapping → Ok(None) (no env set, no agent — AC-305).
    // The shim dir is already on PATH (prepend_shim_path above), so the ssh/scp
    // shims resolve; prepare_ssh_access only adds DEVDY_SSH_CONFIG + SSH_AUTH_SOCK.
    // Fail-soft: a genuine setup error degrades to no-ssh rather than aborting.
    let ssh_access = match ssh_access::prepare_ssh_access(app, db, cmd, run_id, project_id).await {
        Ok(guard) => guard,
        Err(e) => {
            eprintln!("devdy: transparent ssh setup failed (run continues without it): {e}");
            None
        }
    };

    Ok((guard, ssh_access))
}

/// Configure `git` for this run to use the Devdy credential helper via
/// `GIT_CONFIG_COUNT` + `GIT_CONFIG_KEY_n`/`GIT_CONFIG_VALUE_n`. This is per-run
/// and NEVER touches `~/.gitconfig` or system config.
///
/// The first `credential.helper=""` entry RESETS any inherited helper chain
/// (e.g. osxkeychain), so the run can only auth via Devdy → fail-closed to the
/// project's account. No token is ever set in the environment here.
fn wire_git_config(cmd: &mut Command, helper_path: &Path) {
    cmd.env("GIT_CONFIG_COUNT", "3");
    // 0: reset the accumulated credential helper list (system/global).
    cmd.env("GIT_CONFIG_KEY_0", "credential.helper");
    cmd.env("GIT_CONFIG_VALUE_0", "");
    // 1: the Devdy helper (absolute path → independent of PATH resolution).
    cmd.env("GIT_CONFIG_KEY_1", "credential.helper");
    cmd.env("GIT_CONFIG_VALUE_1", helper_path.as_os_str());
    // 2: credentials keyed per host, not per repo path.
    cmd.env("GIT_CONFIG_KEY_2", "credential.useHttpPath");
    cmd.env("GIT_CONFIG_VALUE_2", "false");
    // Fail-closed: when the Devdy helper returns no credential (project not
    // linked to an account), git must FAIL immediately rather than fall back to
    // an interactive username/password prompt (which would hang the sidecar).
    cmd.env("GIT_TERMINAL_PROMPT", "0");
}

async fn wire_aws_config(
    app: &AppHandle,
    db: &Db,
    cmd: &mut Command,
    run_id: &str,
    project_id: &str,
    helper_path: &Path,
) {
    // Strip inherited AWS credentials from the sidecar parent. AWS SDKs prefer
    // env credentials over credential_process, so leaving these in place would
    // bypass the broker.
    for key in [
        "AWS_ACCESS_KEY_ID",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AWS_PROFILE",
        "AWS_DEFAULT_PROFILE",
        "AWS_WEB_IDENTITY_TOKEN_FILE",
        "AWS_ROLE_ARN",
        "AWS_CONTAINER_CREDENTIALS_FULL_URI",
        "AWS_CONTAINER_CREDENTIALS_RELATIVE_URI",
        "AWS_CONTAINER_AUTHORIZATION_TOKEN",
    ] {
        cmd.env_remove(key);
    }
    if let Some(value) = std::env::var_os("AWS_CONFIG_FILE") {
        cmd.env("DEVDY_ORIGINAL_AWS_CONFIG_FILE", value);
    }
    if let Some(value) = std::env::var_os("AWS_SHARED_CREDENTIALS_FILE") {
        cmd.env("DEVDY_ORIGINAL_AWS_SHARED_CREDENTIALS_FILE", value);
    }
    cmd.env("AWS_EC2_METADATA_DISABLED", "true");

    let app_data_dir = match app.path().app_data_dir() {
        Ok(dir) => dir,
        Err(_) => return,
    };
    let run_dir = app_data_dir.join("aws-runs").join(run_id);
    if fs::create_dir_all(&run_dir).is_err() {
        return;
    }
    let config_path = run_dir.join("config");
    let credentials_path = run_dir.join("credentials");
    let _ = fs::write(&credentials_path, "");

    let meta = match crate::runs::broker::token::resolve_aws_runtime_metadata(db, project_id).await
    {
        Ok(Some(meta)) => Some(meta),
        _ => None,
    };

    let Some(meta) = meta else {
        let _ = fs::write(&config_path, "[default]\n");
        cmd.env("AWS_CONFIG_FILE", config_path);
        cmd.env("AWS_SHARED_CREDENTIALS_FILE", credentials_path);
        cmd.env("AWS_SDK_LOAD_CONFIG", "1");
        return;
    };

    cmd.env("AWS_REGION", &meta.region);
    cmd.env("AWS_DEFAULT_REGION", &meta.region);

    // For keys accounts, SDKs resolve credentials through the brokered
    // credential_process and must not fall back to global credentials. For named
    // profile/SSO accounts, the `aws` CLI shim injects AWS_PROFILE only into the
    // real CLI child. SDK credential_process cannot emit a profile name, so do
    // not set AWS_PROFILE on the sidecar parent.
    if meta.auth_method != "keys" {
        let _ = fs::write(&config_path, format!("[default]\nregion = {}\n", meta.region));
        cmd.env("AWS_CONFIG_FILE", config_path);
        cmd.env("AWS_SHARED_CREDENTIALS_FILE", credentials_path);
        cmd.env("AWS_SDK_LOAD_CONFIG", "1");
        return;
    }

    let config = format!(
        "[default]\nregion = {}\ncredential_process = {}\n",
        meta.region,
        shell_quote_path(helper_path),
    );
    if fs::write(&config_path, config).is_err() {
        return;
    }
    cmd.env("AWS_CONFIG_FILE", config_path);
    cmd.env("AWS_SHARED_CREDENTIALS_FILE", credentials_path);
    cmd.env("AWS_SDK_LOAD_CONFIG", "1");
}

fn shell_quote_path(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Set `GIT_AUTHOR_*`/`GIT_COMMITTER_*` from the project's linked account so
/// commits carry the right identity. Only sets an env var when the corresponding
/// account field is present — never fabricates a name/email. Identity resolution
/// failures degrade silently (identity is not a secret and must not fail a run).
async fn wire_commit_identity(cmd: &mut Command, db: &Db, project_id: &str) {
    let identity = crate::runs::broker::token::resolve_commit_identity(db, project_id).await;
    if let Some(name) = identity.name {
        cmd.env("GIT_AUTHOR_NAME", &name);
        cmd.env("GIT_COMMITTER_NAME", &name);
    }
    if let Some(email) = identity.email {
        cmd.env("GIT_AUTHOR_EMAIL", &email);
        cmd.env("GIT_COMMITTER_EMAIL", &email);
    }
}

#[tauri::command]
pub async fn cancel_run(
    registry: State<'_, RunRegistry>,
    db: State<'_, Db>,
    run_id: String,
) -> Result<(), String> {
    let handle = {
        let mut reg = registry.lock().await;
        reg.remove(&run_id)
    };

    if let Some(mut handles) = handle {
        // Kill the whole process group (node sidecar + claude/codex CLI), then
        // reap the node handle. Group kill prevents an orphaned CLI lingering.
        if let Some(pid) = handles.child.id() {
            kill_process_group(pid);
        }
        let _ = handles.child.kill().await;
    }

    sqlx::query("UPDATE runs SET status = 'cancelled', finished_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct SendUserMessagePayload {
    pub run_id: String,
    pub content: String,
    /// Images pasted into the composer for this follow-up turn.
    #[serde(default)]
    pub images: Vec<ImageAttachment>,
    /// Proceed even when the global budget is exceeded (user confirmed).
    #[serde(default)]
    pub override_budget: bool,
}

#[tauri::command]
pub async fn send_user_message(
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    payload: SendUserMessagePayload,
) -> Result<(), String> {
    // A follow-up turn consumes tokens like any other, so gate it too — against
    // the guardrail for this run's engine.
    let engine: String = sqlx::query_scalar("SELECT engine FROM runs WHERE id = ?")
        .bind(&payload.run_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    crate::commands::stats::enforce_budget(db.inner(), &engine, payload.override_budget).await?;

    let text = payload.content.trim();
    // A turn must carry text or at least one image.
    if text.is_empty() && payload.images.is_empty() {
        return Err("empty message".to_string());
    }
    // Command the sidecar to run another turn with this user text + images.
    let send_line = format!(
        "{}\n",
        serde_json::json!({ "type": "prompt", "text": text, "images": images_payload(&payload.images) })
    );
    // For the persisted log we record the equivalent stream-json user message so
    // the history view renders it (parseStreamLog only understands stream-json).
    let log_line = format!(
        "{}\n",
        serde_json::json!({ "type": "user", "message": { "role": "user", "content": log_user_content(text, &payload.images) } })
    );

    let mut reg = registry.lock().await;
    let handle = reg
        .get_mut(&payload.run_id)
        .ok_or_else(|| "run not active".to_string())?;
    let log_buf = handle.log_buf.clone();
    let stdin = handle
        .stdin
        .as_mut()
        .ok_or_else(|| "run does not accept follow-up input".to_string())?;
    stdin
        .write_all(send_line.as_bytes())
        .await
        .map_err(|e| format!("write stdin: {}", e))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("flush stdin: {}", e))?;
    {
        let mut buf = log_buf.lock().await;
        buf.push_str(&log_line);
    }
    Ok(())
}

#[tauri::command]
pub async fn end_run_input(registry: State<'_, RunRegistry>, run_id: String) -> Result<(), String> {
    let mut reg = registry.lock().await;
    let handle = reg
        .get_mut(&run_id)
        .ok_or_else(|| "run not active".to_string())?;
    // Dropping the ChildStdin closes the pipe, which tells Claude to exit after
    // finishing the current turn.
    handle.stdin.take();
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct RespondPermissionPayload {
    pub run_id: String,
    pub request_id: String,
    pub decision: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// AskUserQuestion answers (question text -> answer string). Folded into the
    /// tool's `updatedInput` by the sidecar so the model receives the selection.
    #[serde(default)]
    pub answers: Option<serde_json::Value>,
    /// Optional freeform text the user typed instead of a structured option.
    #[serde(default)]
    pub response: Option<String>,
}

#[tauri::command]
pub async fn respond_permission(
    registry: State<'_, RunRegistry>,
    broker_approvals: State<'_, crate::runs::BrokerApprovals>,
    payload: RespondPermissionPayload,
) -> Result<(), String> {
    if !matches!(payload.decision.as_str(), "allow" | "deny" | "ask") {
        return Err(format!("unknown permission decision: {}", payload.decision));
    }
    // GĐ3: a broker-side `Ask` (gh/glab) registers its request_id here. If this
    // response matches one, resolve its oneshot and return — the sidecar knows
    // nothing about it, so we must NOT route it over stdin. Non-matching ids fall
    // through to the unchanged sidecar path below.
    {
        let mut pending = broker_approvals.lock().await;
        if let Some(tx) = pending.remove(&payload.request_id) {
            let allow = payload.decision == "allow";
            let _ = tx.send(allow);
            return Ok(());
        }
    }
    // Route the decision back to the sidecar's canUseTool callback over stdin.
    let line = format!(
        "{}\n",
        serde_json::json!({
            "type": "permission_response",
            "requestId": payload.request_id,
            "decision": payload.decision,
            "reason": payload.reason,
            "answers": payload.answers,
            "response": payload.response,
        })
    );
    let mut reg = registry.lock().await;
    let handle = reg
        .get_mut(&payload.run_id)
        .ok_or_else(|| "run not active".to_string())?;
    let stdin = handle
        .stdin
        .as_mut()
        .ok_or_else(|| "run is not accepting input".to_string())?;
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("write stdin: {}", e))?;
    stdin
        .flush()
        .await
        .map_err(|e| format!("flush stdin: {}", e))?;
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct RunLog {
    pub content: String,
}

#[tauri::command]
pub async fn get_run_log(db: State<'_, Db>, run_id: String) -> Result<RunLog, String> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT r.project_id, r.output_path, r.engine, r.session_id, r.transcript_path,
                p.name as project_name, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&run_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let project_id: String = row.get("project_id");
    let output_path: Option<String> = row.get("output_path");
    let engine: Option<String> = row.get("engine");
    let session_id: Option<String> = row.get("session_id");
    let transcript_path: Option<String> = row.get("transcript_path");
    let project_name: String = row.get("project_name");
    let project_path: String = row.get("project_path");

    // Conventional log file written by start_run / resume_run drain task.
    // Prefer this over output_path because output_path may still point to the
    // input markdown when an earlier UPDATE silently failed or hasn't run yet.
    let conv_path = Path::new(&project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{}.log", run_id));

    // Both engines persist conversations to a shared transcript store the CLI /
    // VS Code extension also write. Run the same upsert core the watcher uses so
    // that, if the session was continued outside Devdy, the log AND usage are
    // refreshed (and the synced-size marker advanced) before we read. The core
    // skips live runs and no-ops when nothing changed.
    match (engine.as_deref(), session_id.as_deref()) {
        (Some("claude"), Some(sid)) => {
            let _ = crate::commands::sessions::upsert_claude_session(
                db.inner(),
                &project_id,
                &project_name,
                &project_path,
                sid,
                false,
            )
            .await;
        }
        (Some("codex"), Some(sid)) => {
            // Prefer the cached rollout path; only walk the tree if it's gone.
            let file = transcript_path
                .map(PathBuf::from)
                .filter(|p| p.is_file())
                .or_else(|| crate::commands::codex_sessions::codex_session_file(sid));
            if let Some(file) = file {
                let _ = crate::commands::codex_sessions::upsert_codex_session_file(
                    db.inner(),
                    &project_id,
                    &project_name,
                    &project_path,
                    sid,
                    &file,
                    false,
                )
                .await;
            }
        }
        _ => {}
    }

    if let Ok(content) = fs::read_to_string(&conv_path) {
        if !content.trim().is_empty() {
            return Ok(RunLog { content });
        }
    }

    let content = output_path
        .and_then(|p| fs::read_to_string(&p).ok())
        .unwrap_or_default();
    Ok(RunLog { content })
}

#[derive(Debug, Serialize)]
pub struct RunLogPath {
    /// Absolute path of the log file backing the run, or `None` when no log file
    /// exists on disk yet (so the UI can disable the copy / view actions).
    pub path: Option<String>,
}

/// Resolve the absolute path of the log file backing a run/session, using the
/// same priority `get_run_log` reads its content from:
///   1. the conventional `.devdy/runs/<id>.log` written by start_run/resume_run,
///   2. the shared Claude/Codex transcript (`transcript_path`) for mirrored
///      sessions — falling back to the conventional session-store location when
///      the cached path is missing,
///   3. the recorded `output_path`.
/// Unlike `get_run_log` this returns the path (not the content) so the frontend
/// can copy it to the clipboard or open it in the file viewer.
#[tauri::command]
pub async fn get_run_log_path(db: State<'_, Db>, run_id: String) -> Result<RunLogPath, String> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT r.output_path, r.engine, r.session_id, r.transcript_path, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&run_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let output_path: Option<String> = row.get("output_path");
    let engine: Option<String> = row.get("engine");
    let session_id: Option<String> = row.get("session_id");
    let transcript_path: Option<String> = row.get("transcript_path");
    let project_path: String = row.get("project_path");

    // 1. Conventional log written by the run drain task.
    let conv_path = Path::new(&project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{}.log", run_id));
    if let Ok(content) = fs::read_to_string(&conv_path) {
        if !content.trim().is_empty() {
            return Ok(RunLogPath {
                path: Some(conv_path.to_string_lossy().into_owned()),
            });
        }
    }

    // 2. Shared transcript store (mirrored Claude/Codex sessions). Prefer the
    // cached path; only re-derive it from the session id when it's stale/missing.
    let transcript = transcript_path
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(|| match (engine.as_deref(), session_id.as_deref()) {
            (Some("claude"), Some(sid)) => {
                crate::commands::sessions::claude_sessions_dir(&project_path)
                    .map(|d| d.join(format!("{}.jsonl", sid)))
                    .filter(|p| p.is_file())
            }
            (Some("codex"), Some(sid)) => crate::commands::codex_sessions::codex_session_file(sid),
            _ => None,
        });
    if let Some(p) = transcript {
        return Ok(RunLogPath {
            path: Some(p.to_string_lossy().into_owned()),
        });
    }

    // 3. Recorded output path, if it still points at a real file.
    let output = output_path.map(PathBuf::from).filter(|p| p.is_file());
    Ok(RunLogPath {
        path: output.map(|p| p.to_string_lossy().into_owned()),
    })
}

/// Source-run fields needed to clone a run into a new `fetched` record.
struct ClonableRun {
    project_id: String,
    repo_id: Option<String>,
    run_type: String,
    ref_number: Option<i64>,
    /// Resolved, existing input markdown path (issue/PR details).
    resolved_input: String,
    engine: String,
}

/// Source-run fields needed for cross-engine handoff. Unlike rerun, a handoff
/// can originate from a standalone chat session, which intentionally has no
/// input markdown file.
struct HandoffSourceRun {
    project_id: String,
    repo_id: Option<String>,
    run_type: String,
    ref_number: Option<i64>,
    input_path: Option<String>,
    engine: String,
    project_path: String,
    title: Option<String>,
}

/// Load a run and resolve its original input markdown so it can be re-run or
/// handed off to another engine without re-fetching from GitHub.
async fn load_clonable_run(db: &sqlx::SqlitePool, run_id: &str) -> Result<ClonableRun, String> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT r.project_id, r.repo_id, r.type, r.ref_number, r.input_path, r.output_path, r.engine, r.status,
                p.path as project_path
         FROM runs r
         JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let project_id: String = row.get("project_id");
    let repo_id: Option<String> = row.get("repo_id");
    let run_type: String = row.get("type");
    let ref_number: Option<i64> = row.get("ref_number");
    let stored_input: Option<String> = row.get("input_path");
    let stored_output: Option<String> = row.get("output_path");
    let status: String = row.get("status");
    let engine: String = row.get("engine");
    let project_path: String = row.get("project_path");

    // Resolve input file: prefer stored input_path, else output_path (only valid
    // when status='fetched'), else derive from run_type+ref_number for issues.
    let resolved_input = stored_input
        .or_else(|| {
            if status == "fetched" {
                stored_output
            } else {
                None
            }
        })
        .or_else(|| {
            if run_type == "analyze_issue" {
                ref_number.map(|n| {
                    PathBuf::from(&project_path)
                        .join(".devdy")
                        .join("tasks")
                        .join(format!("issue-{}", n))
                        .join("issue.md")
                        .to_string_lossy()
                        .to_string()
                })
            } else {
                None
            }
        })
        .ok_or("Cannot locate original input file — please fetch this PR/issue again")?;

    if !Path::new(&resolved_input).exists() {
        return Err(format!(
            "Input file no longer exists: {} — please fetch again",
            resolved_input
        ));
    }

    Ok(ClonableRun {
        project_id,
        repo_id,
        run_type,
        ref_number,
        resolved_input,
        engine,
    })
}

/// Load a run for cross-engine handoff. Free-form session runs have no input
/// file, so keep their input nullable; issue/PR runs still resolve and validate
/// their cached markdown because `start_run` needs it.
async fn load_handoff_source_run(
    db: &sqlx::SqlitePool,
    run_id: &str,
) -> Result<HandoffSourceRun, String> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT r.project_id, r.repo_id, r.type, r.ref_number, r.input_path, r.output_path, r.engine, r.status,
                r.title, p.path as project_path
         FROM runs r
         JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let project_id: String = row.get("project_id");
    let repo_id: Option<String> = row.get("repo_id");
    let run_type: String = row.get("type");
    let ref_number: Option<i64> = row.get("ref_number");
    let stored_input: Option<String> = row.get("input_path");
    let stored_output: Option<String> = row.get("output_path");
    let status: String = row.get("status");
    let engine: String = row.get("engine");
    let project_path: String = row.get("project_path");
    let title: Option<String> = row.get("title");

    let input_path = if run_type == "session" {
        None
    } else {
        let resolved_input = stored_input
            .or_else(|| {
                if status == "fetched" {
                    stored_output
                } else {
                    None
                }
            })
            .or_else(|| {
                if run_type == "analyze_issue" {
                    ref_number.map(|n| {
                        PathBuf::from(&project_path)
                            .join(".devdy")
                            .join("tasks")
                            .join(format!("issue-{}", n))
                            .join("issue.md")
                            .to_string_lossy()
                            .to_string()
                    })
                } else {
                    None
                }
            })
            .ok_or("Cannot locate original input file — please fetch this PR/issue again")?;

        if !Path::new(&resolved_input).exists() {
            return Err(format!(
                "Input file no longer exists: {} — please fetch again",
                resolved_input
            ));
        }

        Some(resolved_input)
    };

    Ok(HandoffSourceRun {
        project_id,
        repo_id,
        run_type,
        ref_number,
        input_path,
        engine,
        project_path,
        title,
    })
}

/// Create a new run record by cloning an existing one. Reuses the original input
/// markdown file so the AI can be re-executed without re-fetching from GitHub.
/// Returns the newly created RunRecord (status='fetched'); the frontend then
/// calls start_run with the new id.
#[tauri::command]
pub async fn rerun_run(db: State<'_, Db>, run_id: String) -> Result<RunRecord, String> {
    let src = load_clonable_run(db.inner(), &run_id).await?;

    let new_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, created_at)
         VALUES (?, ?, ?, ?, ?, 'fetched', ?, ?, ?, ?)",
    )
    .bind(&new_id)
    .bind(&src.project_id)
    .bind(&src.repo_id)
    .bind(&src.run_type)
    .bind(src.ref_number)
    .bind(&src.engine)
    .bind(&src.resolved_input)
    .bind(&src.resolved_input)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(RunRecord {
        id: new_id,
        project_id: src.project_id,
        repo_id: src.repo_id,
        run_type: src.run_type,
        ref_number: src.ref_number,
        status: "fetched".to_string(),
        engine: src.engine,
        input_path: Some(src.resolved_input.clone()),
        output_path: Some(src.resolved_input),
        session_id: None,
        started_at: None,
        finished_at: None,
        created_at: now,
        title: None,
        pinned: false,
    })
}

#[derive(Debug, Serialize, Clone)]
pub struct HandoffResult {
    pub run: RunRecord,
    /// Absolute path to the written transcript file the new run should read to
    /// pick up where the previous engine left off.
    pub context_path: String,
}

/// Fork the current conversation onto another engine. Clones the source run into
/// a new `fetched` record targeting `target_engine`, and writes the prior
/// conversation transcript to a context file the new run is told to read. This
/// is a cross-engine *handoff* (Claude↔Codex) — true session resume only works
/// within Claude (see `resume_run`), so context is carried via the transcript.
#[tauri::command]
pub async fn create_handoff_run(
    db: State<'_, Db>,
    run_id: String,
    target_engine: String,
    transcript: String,
) -> Result<HandoffResult, String> {
    if target_engine != "claude" && target_engine != "codex" {
        return Err(format!("Unknown engine: {target_engine}"));
    }

    let src = load_handoff_source_run(db.inner(), &run_id).await?;

    let new_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // Persist the transcript next to the per-run logs so the new run can read it.
    let runs_dir = Path::new(&src.project_path).join(".devdy").join("runs");
    fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
    let context_path = runs_dir.join(format!("{}.context.md", new_id));
    let body = format!(
        "# Ngữ cảnh phiên trước (engine: {})\n\nĐây là toàn bộ hội thoại/diễn tiến của phiên làm việc trước. \
Đọc kỹ để nắm những gì đã làm, rồi tiếp tục công việc đang dở.\n\n---\n\n{}\n",
        src.engine, transcript
    );
    fs::write(&context_path, body).map_err(|e| e.to_string())?;
    let context_path = context_path.to_string_lossy().to_string();

    sqlx::query(
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, created_at, title)
         VALUES (?, ?, ?, ?, ?, 'fetched', ?, ?, ?, ?, ?)",
    )
    .bind(&new_id)
    .bind(&src.project_id)
    .bind(&src.repo_id)
    .bind(&src.run_type)
    .bind(src.ref_number)
    .bind(&target_engine)
    .bind(src.input_path.as_deref())
    .bind(src.input_path.as_deref())
    .bind(&now)
    .bind(src.title.as_deref())
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(HandoffResult {
        run: RunRecord {
            id: new_id,
            project_id: src.project_id,
            repo_id: src.repo_id,
            run_type: src.run_type,
            ref_number: src.ref_number,
            status: "fetched".to_string(),
            engine: target_engine,
            input_path: src.input_path.clone(),
            output_path: src.input_path,
            session_id: None,
            started_at: None,
            finished_at: None,
            created_at: now,
            title: src.title,
            pinned: false,
        },
        context_path,
    })
}

/// Create a standalone `session` run not tied to any GitHub issue/PR. Returns a
/// `fetched` RunRecord; the frontend then drives it via `start_run` with the
/// user's first message as the prompt (which also sets the run title).
#[tauri::command]
pub async fn create_session_run(
    db: State<'_, Db>,
    project_id: String,
    engine_override: Option<String>,
) -> Result<RunRecord, String> {
    let default_engine = crate::commands::settings::resolve_default_engine(db.inner()).await;
    let engine = engine_override
        .filter(|e| !e.trim().is_empty())
        .unwrap_or(default_engine);

    let new_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, created_at)
         VALUES (?, ?, 'session', 'fetched', ?, ?)",
    )
    .bind(&new_id)
    .bind(&project_id)
    .bind(&engine)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(RunRecord {
        id: new_id,
        project_id,
        repo_id: None,
        run_type: "session".to_string(),
        ref_number: None,
        status: "fetched".to_string(),
        engine,
        input_path: None,
        output_path: None,
        session_id: None,
        started_at: None,
        finished_at: None,
        created_at: now,
        title: None,
        pinned: false,
    })
}

/// Core logic for deleting a single run. Removes the DB row, per-run log file,
/// and cached input markdown when no sibling run still references it. Empty
/// task folder is removed. Refuses to delete runs currently in the registry.
pub(crate) async fn delete_run_inner(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    run_id: &str,
) -> Result<(), String> {
    use sqlx::Row;

    {
        let reg = registry.lock().await;
        if reg.contains_key(run_id) {
            return Err("Run is still active — cancel it before deleting".to_string());
        }
    }

    let row = sqlx::query(
        "SELECT r.status, r.input_path, r.project_id, r.session_id, r.engine, r.transcript_path, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let status: String = row.get("status");
    if status == "running" {
        return Err("Run is still running — cancel it before deleting".to_string());
    }
    let input_path: Option<String> = row.get("input_path");
    let project_path: String = row.get("project_path");
    let project_id: String = row.get("project_id");
    let session_id: Option<String> = row.get("session_id");
    let engine: String = row.get("engine");
    let transcript_path: Option<String> = row.get("transcript_path");

    sqlx::query("DELETE FROM runs WHERE id = ?")
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;

    // Tombstone the session so the reconcile/watcher can't re-import it from the
    // still-present shared transcript on the next launch. Only session runs are
    // mirrored, so only they can come back — but recording any run with a
    // session_id is harmless and future-proof.
    if let Some(sid) = session_id.as_deref() {
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO deleted_sessions (project_id, session_id, engine, deleted_at) VALUES (?, ?, ?, ?)",
        )
        .bind(&project_id)
        .bind(sid)
        .bind(&engine)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db)
        .await;
    }

    // Usage rows are an independent ledger: keep them, just flag that their
    // originating run no longer exists (so the UI can't link back to it).
    let _ = sqlx::query("UPDATE run_usage SET deleted_run = 1 WHERE run_id = ?")
        .bind(run_id)
        .execute(db)
        .await;

    let runs_dir = Path::new(&project_path).join(".devdy").join("runs");
    let _ = fs::remove_file(runs_dir.join(format!("{}.log", run_id)));
    let _ = fs::remove_dir_all(runs_dir.join(run_id));

    // Also delete the shared engine transcript so the session can't be
    // re-mirrored back into history (and disappears from the CLI/VS Code resume
    // list too). Prefer the exact path we mirrored from; otherwise reconstruct it
    // from the engine + session id. The tombstone above still guards the window
    // in case this file can't be removed.
    if let Some(sid) = session_id.as_deref() {
        let transcript_file: Option<PathBuf> = match transcript_path.as_deref() {
            Some(p) => Some(PathBuf::from(p)),
            None => match engine.as_str() {
                "claude" => crate::commands::sessions::claude_sessions_dir(&project_path)
                    .map(|dir| dir.join(format!("{}.jsonl", sid))),
                "codex" => crate::commands::codex_sessions::codex_session_file(sid),
                _ => None,
            },
        };
        if let Some(file) = transcript_file {
            let _ = fs::remove_file(file);
        }
    }

    if let Some(path) = input_path.as_deref() {
        let still_referenced: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE input_path = ? OR output_path = ?")
                .bind(path)
                .bind(path)
                .fetch_one(db)
                .await
                .map_err(|e| e.to_string())?;
        if still_referenced == 0 {
            let file = Path::new(path);
            let _ = fs::remove_file(file);
            if let Some(parent) = file.parent() {
                let _ = fs::remove_dir(parent);
            }
        }
    }

    Ok(())
}

/// Delete a fetched run together with its cached files. Running runs are
/// refused — cancel first.
#[tauri::command]
pub async fn delete_run(
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    run_id: String,
) -> Result<(), String> {
    delete_run_inner(db.inner(), registry.inner(), &run_id).await
}

/// Rename a run's title (shown in the History list). An empty/whitespace title
/// clears it back to NULL, letting the UI fall back to its derived label.
#[tauri::command]
pub async fn rename_run(db: State<'_, Db>, run_id: String, title: String) -> Result<(), String> {
    let trimmed = title.trim();
    let value: Option<&str> = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    };
    sqlx::query("UPDATE runs SET title = ? WHERE id = ?")
        .bind(value)
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Pin or unpin a run so it sorts to the top of the History list.
#[tauri::command]
pub async fn set_run_pinned(db: State<'_, Db>, run_id: String, pinned: bool) -> Result<(), String> {
    sqlx::query("UPDATE runs SET pinned = ? WHERE id = ?")
        .bind(if pinned { 1 } else { 0 })
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bulk delete all non-running runs for a project. Returns the number deleted.
#[tauri::command]
pub async fn delete_all_runs(
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    project_id: String,
) -> Result<u32, String> {
    use sqlx::Row;

    let rows = sqlx::query("SELECT id FROM runs WHERE project_id = ? AND status != 'running'")
        .bind(&project_id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut deleted = 0u32;
    for row in &rows {
        let id: String = row.get("id");
        if delete_run_inner(db.inner(), registry.inner(), &id)
            .await
            .is_ok()
        {
            deleted += 1;
        }
    }
    Ok(deleted)
}

#[tauri::command]
pub async fn read_run_input(db: State<'_, Db>, run_id: String) -> Result<String, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT input_path FROM runs WHERE id = ?")
        .bind(&run_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let path: Option<String> = row.get("input_path");
    // Standalone session runs have no input markdown — return empty so the UI
    // simply shows no Content tab rather than an error.
    let Some(path) = path else {
        return Ok(String::new());
    };
    fs::read_to_string(&path).map_err(|e| format!("read {}: {}", path, e))
}

/// Resume a previously completed Claude run by spawning `claude --resume <session_id>`.
/// Only valid for runs where the engine is `claude` and a session id was captured
/// from the original run's `system.init` event.
#[tauri::command]
pub async fn resume_run(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    run_id: String,
    permission_mode_override: Option<String>,
    model_override: Option<String>,
    override_budget: Option<bool>,
) -> Result<(), String> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT r.engine, r.status, r.session_id, r.project_id, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&run_id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let engine: String = row.get("engine");
    let status: String = row.get("status");
    let session_id: Option<String> = row.get("session_id");
    let project_id: String = row.get("project_id");
    let project_path: String = row.get("project_path");

    // Same budget guardrail as start_run — resuming a finished run starts a new
    // turn and consumes tokens, so it must be gated too (per this run's engine).
    crate::commands::stats::enforce_budget(db.inner(), &engine, override_budget.unwrap_or(false))
        .await?;

    if engine != "claude" && engine != "codex" {
        return Err("Only Claude and Codex runs can be resumed".to_string());
    }
    if status == "running" {
        return Err("Run is already active".to_string());
    }
    let session_id = session_id
        .ok_or_else(|| "This run has no captured session id — cannot resume".to_string())?;

    // Refuse if a handle for this run is still in the registry (shouldn't happen).
    {
        let reg = registry.lock().await;
        if reg.contains_key(&run_id) {
            return Err("Run already has an active subprocess".to_string());
        }
    }

    // Load engine settings (path, extra_args, default permission mode).
    let settings_rows = sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut claude_path = "claude".to_string();
    let mut codex_path = "codex".to_string();
    let mut node_path = "node".to_string();
    let mut sidecar_path = String::new();
    let mut codex_sidecar_path = String::new();
    let mut claude_model = String::new();
    let mut codex_model = String::new();
    let mut default_permission_mode = "default".to_string();
    for row in &settings_rows {
        let key: String = row.get("key");
        let value: String = row.get("value");
        match key.as_str() {
            "claude_path" => claude_path = value,
            "codex_path" => codex_path = value,
            "node_path" => node_path = value,
            "sidecar_path" => sidecar_path = value,
            "codex_sidecar_path" => codex_sidecar_path = value,
            "claude_model" => claude_model = value,
            "codex_model" => codex_model = value,
            "default_permission_mode" => default_permission_mode = value,
            _ => {}
        }
    }

    let engine_default_model = if engine == "codex" {
        codex_model
    } else {
        claude_model
    };
    let model = resolve_model(model_override, engine_default_model);

    let permission_mode = permission_mode_override
        .filter(|v| is_valid_permission_mode(v))
        .unwrap_or_else(|| {
            if is_valid_permission_mode(&default_permission_mode) {
                default_permission_mode
            } else {
                "default".to_string()
            }
        });

    let runs_dir = Path::new(&project_path).join(".devdy").join("runs");
    fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
    let log_path = runs_dir.join(format!("{}.log", run_id));

    // Mark running; finished_at cleared so the UI reflects the active state.
    let started_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE runs SET status = 'running', started_at = ?, finished_at = NULL WHERE id = ?",
    )
    .bind(&started_at)
    .bind(&run_id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let (node_bin, sidecar_script) = if engine == "codex" {
        resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?
    } else {
        resolve_sidecar(&app, &node_path, &sidecar_path)?
    };
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&project_path)
        .arg(&sidecar_script)
        // The sidecar resumes this session/thread on its first prompt.
        .env("DEVDY_RESUME_SESSION", &session_id);
    augment_command_path(&mut cmd);
    // Wire the broker + shims for both Claude and Codex resumes so resumed
    // sessions get the same per-project credential treatment as fresh runs.
    let (broker, ssh_access) = wire_broker(
        &app,
        db.inner(),
        &mut cmd,
        &run_id,
        &project_id,
        &project_path,
    )
    .await?;
    let broker_run: Option<BrokerRunGuard> = Some(broker);
    if engine == "codex" {
        cmd.env("DEVDY_PERMISSION_MODE", &permission_mode);
        if codex_path != "codex" && !codex_path.trim().is_empty() {
            cmd.env("DEVDY_CODEX_PATH", &codex_path);
        }
        if let Some(m) = &model {
            cmd.env("DEVDY_CODEX_MODEL", m);
        }
        // Re-inject the project's MCP servers on resume: a resumed run spawns a
        // FRESH sidecar/query whose first prompt arrives via send_user_message
        // WITHOUT options, so (unlike start_run) MCP would otherwise be lost.
        let (codex_mcp, _skipped) =
            crate::commands::mcp::resolve_project_mcp_servers(db.inner(), &project_id, "codex")
                .await;
        if !codex_mcp.is_null() {
            cmd.env("DEVDY_CODEX_MCP", codex_mcp.to_string());
        }
    } else {
        cmd.env(
            "DEVDY_PERMISSION_MODE",
            sdk_permission_mode(&permission_mode),
        );
        if claude_path != "claude" && !claude_path.trim().is_empty() {
            cmd.env("DEVDY_CLAUDE_PATH", &claude_path);
        }
        cmd.env(
            "DEVDY_USAGE_CAPTURE_MODE",
            claude_usage_capture_mode(db.inner()).await,
        );
        cmd.env("DEVDY_USAGE_POLL_MS", "60000");
        if let Some(m) = &model {
            cmd.env("DEVDY_MODEL", m);
        }
        // Re-inject MCP on resume (see the Codex branch above). The sidecar reads
        // DEVDY_MCP_SERVERS as a fallback when the first prompt carries no options.
        let (mcp, _skipped) =
            crate::commands::mcp::resolve_project_mcp_servers(db.inner(), &project_id, "claude")
                .await;
        if !mcp.is_null() {
            cmd.env("DEVDY_MCP_SERVERS", mcp.to_string());
        }
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar ({}): {}", node_bin, e))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdin_handle = child.stdin.take();

    let session_id_state = Arc::new(TokioMutex::new(Some(session_id.clone())));
    let log_buf = Arc::new(TokioMutex::new(String::new()));

    // Seed the buffer with a resume marker so the persisted log notes the resume
    // boundary between the original conversation and the new turns. The actual
    // follow-up text arrives via send_user_message, which kicks off the turn.
    {
        let mut buf = log_buf.lock().await;
        buf.push_str(&format!(
            "[stderr] --- resumed session {} at {} ---\n",
            session_id, started_at
        ));
    }

    {
        let mut reg = registry.lock().await;
        reg.insert(
            run_id.clone(),
            RunHandles {
                child,
                stdin: stdin_handle,
                session_id: session_id_state.clone(),
                log_buf: log_buf.clone(),
                broker_run,
                ssh_access,
            },
        );
    }

    let db_pool = db.inner().clone();
    let registry_arc = registry.inner().clone();

    tokio::spawn(drain_sidecar(
        app,
        run_id.clone(),
        project_path,
        stdout,
        stderr,
        db_pool,
        registry_arc,
        session_id_state,
        log_buf,
        log_path,
        true,
    ));

    Ok(())
}
