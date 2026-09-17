use crate::commands::github::RunRecord;
use crate::db::Db;
use crate::runs::broker::BrokerHandle;
use crate::runs::sidecar::{
    apply_claude_config_dir, augment_command_path, detach_process_group, drain_sidecar,
    kill_process_group, resolve_codex_sidecar, resolve_sidecar, sdk_permission_mode,
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

async fn claude_usage_capture_mode(db: &Db, claude_account_id: Option<&str>) -> &'static str {
    match crate::commands::stats::budget_status_for(db, "claude", claude_account_id).await {
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
    start_run_inner(app, db.inner().clone(), registry.inner().clone(), payload).await
}

/// Core of [`start_run`] over owned clones so the Remote Control host agent
/// (FR-008 `start_run`) can launch a run from a background task without a Tauri
/// `State`. `Db` (SqlitePool) and `RunRegistry` (Arc) are cheap to clone.
///
/// SECURITY (BR-011/CON-05): the remote path MUST pass
/// `permission_mode_override = Some("default")`; this function honors whatever
/// override it is given, exactly as the command does — the lock is enforced by
/// the caller (the host agent) so the identical local behavior is preserved.
pub(crate) async fn start_run_inner(
    app: AppHandle,
    db: Db,
    registry: RunRegistry,
    payload: StartRunPayload,
) -> Result<(), String> {
    use sqlx::Row;
    let db = &db;
    let registry = &registry;

    // Load run + project info
    let run_row = sqlx::query(
        "SELECT r.id, r.project_id, r.type, r.ref_number, r.input_path, r.output_path, r.engine, r.status, r.claude_account_id,
                p.path as project_path
         FROM runs r
         JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&payload.run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let run_type: String = run_row.get("type");
    let project_id: String = run_row.get("project_id");
    let input_path: Option<String> = run_row.get("input_path");
    let output_path: Option<String> = run_row.get("output_path");
    let status: String = run_row.get("status");
    let project_path: String = run_row.get("project_path");
    let existing_claude_account_id: Option<String> = run_row.get("claude_account_id");

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
        .fetch_all(db)
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

    // Resolve the engine: an explicit per-run override wins, else the run's OWN
    // persisted engine, else the global default. Falling back to the run's engine
    // (not the global default) is critical: a follow-up/restart that omits the
    // engine — e.g. a remote `start_run` from the controller, whose selector may
    // be empty — must NOT silently switch engines. Clobbering the engine here
    // resets a Codex session to the default engine and loses its conversation.
    let existing_engine: Option<String> = run_row.get("engine");
    let engine = payload
        .engine_override
        .filter(|s| !s.trim().is_empty())
        .or_else(|| existing_engine.filter(|s| !s.trim().is_empty()))
        .unwrap_or(default_engine);

    // Resolve the Claude account. A run-level selection wins, else project-linked
    // → default → global. Snapshot the resolved id on the run so resume uses the
    // same profile unless the user explicitly changes the run account later.
    let claude_account = if engine == "claude" {
        crate::commands::claude_accounts::resolve_runtime_account(
            db,
            Some(&project_id),
            existing_claude_account_id.as_deref(),
        )
        .await?
    } else {
        None
    };
    if let Some(acct) = &claude_account {
        let _ = sqlx::query("UPDATE runs SET claude_account_id = ? WHERE id = ?")
            .bind(&acct.id)
            .bind(&payload.run_id)
            .execute(db)
            .await;
    }

    // Global budget guardrail: refuse to start a new run when over budget
    // (real plan utilization for this engine's resolved account, or the
    // self-imposed token fallback), unless the user explicitly overrode it.
    crate::commands::stats::enforce_budget(
        db,
        &engine,
        claude_account.as_ref().map(|a| a.id.as_str()),
        payload.override_budget,
    )
    .await?;

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
        let title = super::sessions::truncate(&prompt, super::sessions::TITLE_MAX_CHARS);
        let _ = sqlx::query("UPDATE runs SET title = ? WHERE id = ? AND title IS NULL")
            .bind(title.as_str())
            .bind(&payload.run_id)
            .execute(db)
            .await;
    }

    // Per-run scratch dir (also where the log lands).
    let runs_dir = Path::new(&project_path).join(".devdy").join("runs");
    fs::create_dir_all(&runs_dir).map_err(|e| e.to_string())?;
    let log_path = runs_dir.join(format!("{}.log", payload.run_id));

    // Update run status to running
    let started_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE runs SET status = 'running', engine = ?, started_at = ?, last_activity_at = ? WHERE id = ?",
    )
    .bind(&engine)
    .bind(&started_at)
    .bind(&started_at)
    .bind(&payload.run_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    // Build extra args vec
    let extra: Vec<String> = extra_args
        .split_whitespace()
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();

    let run_id = payload.run_id.clone();
    let db_pool = db.clone();
    let registry_arc = registry.clone();
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
            db,
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
            claude_usage_capture_mode(db, claude_account.as_ref().map(|a| a.id.as_str())).await,
        );
        cmd.env("DEVDY_USAGE_POLL_MS", "60000");
        if let Some(m) = &model {
            cmd.env("DEVDY_MODEL", m);
        }
        // Isolate this run to its resolved Claude account profile (clears inherited
        // auth env overrides). No-op when no managed account applies (legacy global).
        apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
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
            // Add the built-in `devdy` MCP server (notes + recall + project + VPS)
            // unless disabled in settings.
            let mcp = if crate::commands::mcp::builtin_devdy_enabled(&db_pool).await {
                crate::commands::mcp::with_builtin_devdy(
                    mcp,
                    &node_bin,
                    &crate::runs::sidecar::resolve_mcp_sidecar_script(&app),
                    &crate::runs::sidecar::resolve_db_path(&app),
                    &project_id,
                    &project_path,
                )
            } else {
                mcp
            };
            // Add the built-in `google` MCP server (Drive + Gmail + Calendar) when a
            // account is connected (no-op otherwise).
            let mcp = crate::commands::mcp::with_builtin_google(
                &db_pool,
                mcp,
                &node_bin,
                &crate::runs::sidecar::resolve_mcp_script(&app, "google.mjs"),
            )
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
        db,
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
    let codex_mcp = if crate::commands::mcp::builtin_devdy_enabled(&db_pool).await {
        crate::commands::mcp::with_builtin_devdy(
            codex_mcp,
            &node_bin,
            &crate::runs::sidecar::resolve_mcp_sidecar_script(&app),
            &crate::runs::sidecar::resolve_db_path(&app),
            &project_id,
            &project_path,
        )
    } else {
        codex_mcp
    };
    let codex_mcp = crate::commands::mcp::with_builtin_google(
        &db_pool,
        codex_mcp,
        &node_bin,
        &crate::runs::sidecar::resolve_mcp_script(&app, "google.mjs"),
    )
    .await;
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
    // Fallback working directory for the sidecar: if `process.cwd()` throws
    // (e.g. EPERM on macOS when the project folder is TCC-protected / on a
    // network volume / was deleted), the sidecar reports the error and uses
    // this path instead of crashing at startup.
    cmd.env("DEVDY_PROJECT_PATH", project_path);
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
    cancel_run_inner(registry.inner(), db.inner(), &run_id).await
}

/// Core of [`cancel_run`] over concrete refs so the Remote Control host agent
/// (FR-009 `cancel_run`) can reuse it without a Tauri `State`.
pub(crate) async fn cancel_run_inner(
    registry: &RunRegistry,
    db: &sqlx::SqlitePool,
    run_id: &str,
) -> Result<(), String> {
    let run_id = run_id.to_string();
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

    let cancelled_at = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE runs SET status = 'cancelled', finished_at = ?, last_activity_at = ? WHERE id = ?")
        .bind(&cancelled_at)
        .bind(&cancelled_at)
        .bind(&run_id)
        .execute(db)
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
    send_user_message_inner(db.inner(), registry.inner(), payload).await
}

/// Core of [`send_user_message`] over concrete refs so the Remote Control host
/// agent (FR-009 `send_chat_message`) can reuse it without a Tauri `State`.
pub(crate) async fn send_user_message_inner(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    payload: SendUserMessagePayload,
) -> Result<(), String> {
    use sqlx::Row;
    // A follow-up turn consumes tokens like any other, so gate it too — against
    // the guardrail for this run's engine and its assigned Claude account.
    let run_meta = sqlx::query("SELECT engine, claude_account_id FROM runs WHERE id = ?")
        .bind(&payload.run_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let engine: String = run_meta.get("engine");
    let claude_account_id: Option<String> = run_meta.get("claude_account_id");
    crate::commands::stats::enforce_budget(
        db,
        &engine,
        claude_account_id.as_deref(),
        payload.override_budget,
    )
    .await?;

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
    // Registry lock released before the DB write — nothing below needs the
    // handle, and the sidecar registry must not be held across IO.
    drop(reg);
    // The turn is now the run's newest activity. Stamped here rather than only on
    // `run:done` so the History row moves the moment the user hits send, even for
    // a turn that then runs for minutes.
    let _ = sqlx::query("UPDATE runs SET last_activity_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(&payload.run_id)
        .execute(db)
        .await;
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
    /// The user picked "always" (or the decision came from a standing
    /// allow-list). Forwarded to the sidecar so an engine with its own
    /// session-scoped approval cache (codex `acceptForSession`) can stop
    /// re-asking. Remote decisions are once-only (BR-016/AC-17) and never set it.
    #[serde(default)]
    pub remember: bool,
}

#[tauri::command]
pub async fn respond_permission(
    registry: State<'_, RunRegistry>,
    broker_approvals: State<'_, crate::runs::BrokerApprovals>,
    payload: RespondPermissionPayload,
) -> Result<(), String> {
    respond_permission_inner(registry.inner(), broker_approvals.inner(), payload).await
}

/// Core of [`respond_permission`] over concrete refs so background tasks (the
/// Remote Control host agent, FR-007) can reuse the exact same permission-answer
/// path without a Tauri `State`. Same contract as the command.
pub(crate) async fn respond_permission_inner(
    registry: &RunRegistry,
    broker_approvals: &crate::runs::BrokerApprovals,
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
            "remember": payload.remember,
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
    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    Ok(RunLogPath {
        path: resolve_log_source(&row, &run_id)
            .map(|(_, p)| p.to_string_lossy().into_owned()),
    })
}

// ── Log revision + paged reads ───────────────────────────────────────────────
//
// `get_run_log` returns the ENTIRE log as one JSON string. Real logs reach ~29MB
// here, and the frontend re-requested one on every window focus — read, JSON
// encode, IPC, JSON decode, full re-parse, every time. The two commands below
// replace that: `get_run_log_revision` is a cheap fingerprint so an unchanged log
// is never re-read, and `get_run_log_page` reads a bounded window of records
// instead of the whole file.

/// Which of the three candidate files is actually backing a run right now.
#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LogSource {
    /// `.devdy/runs/<id>.log`, written by the run's own drain task.
    Conventional,
    /// Shared Claude/Codex transcript — also written by the CLI / IDE extension.
    Transcript,
    /// The `output_path` recorded on the run row.
    Output,
}

/// Cheap fingerprint of the file(s) backing a run's log.
///
/// Deliberately does NOT run the session upsert that `get_run_log` does: the
/// point is to answer "did anything change?" without paying for a sync. Instead
/// the SOURCE transcript is stat'd directly, so a session continued outside Devdy
/// (Claude CLI, the IDE extension) still shows up as a changed fingerprint and
/// the caller then goes through the full `get_run_log` path, upsert included.
/// Stat'ing only the conventional log would have missed exactly that case.
#[derive(Debug, Serialize)]
pub struct RunLogRevision {
    pub source: Option<LogSource>,
    pub path: Option<String>,
    pub size: u64,
    /// Nanoseconds since the epoch. Seconds resolution is not enough — a
    /// streaming run rewrites its log many times within one second.
    pub mtime_ns: i64,
    /// Set only for mirrored sessions: the upstream transcript's own stat, so a
    /// change made outside Devdy invalidates the fingerprint too.
    pub transcript_size: Option<u64>,
    pub transcript_mtime_ns: Option<i64>,
}

/// `(size, mtime_ns)` for a file, or `None` when it doesn't exist.
fn stat_file(path: &Path) -> Option<(u64, i64)> {
    let md = fs::metadata(path).ok()?;
    if !md.is_file() {
        return None;
    }
    let mtime = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0);
    Some((md.len(), mtime))
}

/// Row fields every log-resolving command needs.
struct RunLogRow {
    output_path: Option<String>,
    engine: Option<String>,
    session_id: Option<String>,
    transcript_path: Option<String>,
    project_path: String,
}

async fn fetch_run_log_row(db: &Db, run_id: &str) -> Result<RunLogRow, String> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT r.output_path, r.engine, r.session_id, r.transcript_path, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(RunLogRow {
        output_path: row.get("output_path"),
        engine: row.get("engine"),
        session_id: row.get("session_id"),
        transcript_path: row.get("transcript_path"),
        project_path: row.get("project_path"),
    })
}

/// The transcript this run mirrors, when it has one.
fn resolve_transcript(row: &RunLogRow) -> Option<PathBuf> {
    row.transcript_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .or_else(
            || match (row.engine.as_deref(), row.session_id.as_deref()) {
                (Some("claude"), Some(sid)) => {
                    crate::commands::sessions::claude_sessions_dir(&row.project_path)
                        .map(|d| d.join(format!("{}.jsonl", sid)))
                        .filter(|p| p.is_file())
                }
                (Some("codex"), Some(sid)) => {
                    crate::commands::codex_sessions::codex_session_file(sid)
                }
                _ => None,
            },
        )
}

/// Pick the file a read should come from, in the same priority order
/// `get_run_log` uses. Unlike the old `get_run_log_path`, emptiness is checked
/// with `metadata().len()` instead of reading the whole file in — that check
/// alone used to pull ~29MB off disk just to decide which path to use.
fn resolve_log_source(row: &RunLogRow, run_id: &str) -> Option<(LogSource, PathBuf)> {
    let conv = Path::new(&row.project_path)
        .join(".devdy")
        .join("runs")
        .join(format!("{}.log", run_id));
    if fs::metadata(&conv).map(|m| m.is_file() && m.len() > 0).unwrap_or(false) {
        return Some((LogSource::Conventional, conv));
    }
    if let Some(t) = resolve_transcript(row) {
        return Some((LogSource::Transcript, t));
    }
    row.output_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
        .map(|p| (LogSource::Output, p))
}

fn build_revision(row: &RunLogRow, run_id: &str) -> RunLogRevision {
    let resolved = resolve_log_source(row, run_id);
    let (source, path, size, mtime_ns) = match &resolved {
        Some((src, p)) => {
            let (size, mtime) = stat_file(p).unwrap_or((0, 0));
            (Some(*src), Some(p.to_string_lossy().into_owned()), size, mtime)
        }
        None => (None, None, 0, 0),
    };
    // Always stat the upstream transcript, even when the conventional log won
    // the resolution: it is the file that can change behind our back.
    let (transcript_size, transcript_mtime_ns) = match resolve_transcript(row) {
        Some(t) => match stat_file(&t) {
            Some((s, m)) => (Some(s), Some(m)),
            None => (None, None),
        },
        None => (None, None),
    };
    RunLogRevision {
        source,
        path,
        size,
        mtime_ns,
        transcript_size,
        transcript_mtime_ns,
    }
}

#[tauri::command]
pub async fn get_run_log_revision(
    db: State<'_, Db>,
    run_id: String,
) -> Result<RunLogRevision, String> {
    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    Ok(build_revision(&row, &run_id))
}

/// One window of log records, newest-last.
#[derive(Debug, Serialize)]
pub struct RunLogPage {
    /// Complete lines, oldest first. Never a partial record.
    pub records: Vec<String>,
    /// Byte offset the first returned record starts at. Pass it back as `before`
    /// to fetch the window immediately older than this one.
    pub cursor: u64,
    /// True when there is older content before `cursor`.
    pub has_more: bool,
    /// The run's `system` init line, when the window doesn't already start at the
    /// top of the file. The parser needs it for the model id (which drives the
    /// context-window limit) — that line lives at the very start of the log and
    /// would otherwise be invisible to a tail window.
    pub preamble: Option<String>,
    pub revision: RunLogRevision,
}

/// How far into the file to look for the `system` init line.
const PREAMBLE_SCAN_BYTES: u64 = 256 * 1024;

/// Find the run's `system` init record near the start of the file.
fn read_preamble(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = fs::File::open(path).ok()?;
    let mut buf = vec![0u8; PREAMBLE_SCAN_BYTES as usize];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let text = String::from_utf8_lossy(&buf);
    let mut first_json: Option<String> = None;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() || line.starts_with("[stderr]") || line.starts_with("[log:") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "system" {
            return Some(line.to_string());
        }
        if first_json.is_none() {
            first_json = Some(line.to_string());
        }
    }
    // No system line in the scanned prefix — the first JSON record at least keeps
    // the parser's "this really is a stream log" check satisfied.
    first_json
}

/// Walk backwards from `before`, collecting whole records.
///
/// Returns `(records oldest-first, byte offset the first record starts at)`.
///
/// `max_bytes` is a soft cap, not a window size: a single record can legitimately
/// exceed it (a user turn carrying a pasted image is raw base64 and routinely
/// runs to megabytes), so the walk always yields at least one complete record
/// before the cap can stop it. Reading a fixed number of trailing bytes would
/// have sliced such a record in half.
fn read_records_backwards(
    path: &Path,
    before: u64,
    max_records: usize,
    max_bytes: u64,
) -> std::io::Result<(Vec<String>, u64)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let end = before.min(len);
    if end == 0 || max_records == 0 {
        return Ok((Vec::new(), 0));
    }

    const CHUNK: u64 = 64 * 1024;
    let mut region_start = end;
    let mut buf: Vec<u8> = Vec::new();

    loop {
        let next_start = region_start.saturating_sub(CHUNK);
        let read_len = (region_start - next_start) as usize;
        if read_len == 0 {
            break;
        }
        let mut chunk = vec![0u8; read_len];
        f.seek(SeekFrom::Start(next_start))?;
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
        region_start = next_start;

        if region_start == 0 {
            break;
        }

        // How many WHOLE records the buffer currently holds. Every '\n'
        // terminates one, but while the region starts mid-file the first of them
        // closes a record whose head we never read — that one gets discarded, so
        // it must not be counted. Bytes after the final '\n' are a whole record
        // too (the file's last line, when it isn't newline-terminated).
        let newlines = buf.iter().filter(|b| **b == b'\n').count();
        let trailing = usize::from(buf.last().map(|b| *b != b'\n').unwrap_or(false));
        let complete = newlines.saturating_sub(1) + trailing;

        if complete >= max_records {
            break;
        }
        // Soft byte cap — only allowed to stop the walk once at least one whole
        // record is in hand. Without that condition a record longer than the cap
        // would end the walk with nothing to return, and paging could never get
        // past it.
        if buf.len() as u64 >= max_bytes && complete >= 1 {
            break;
        }
    }

    // Record starts within `buf`. When the region begins mid-file, byte 0 is the
    // tail of a record whose head we never read — skip it.
    let mut starts: Vec<usize> = Vec::new();
    if region_start == 0 {
        starts.push(0);
    }
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            starts.push(i + 1);
        }
    }

    let mut segs: Vec<(usize, usize)> = Vec::new();
    for (i, s) in starts.iter().enumerate() {
        // Each record ends just before the next record's start (i.e. at its
        // newline); the final one runs to the end of the region.
        let e = starts.get(i + 1).map(|n| n - 1).unwrap_or(buf.len());
        if e > *s {
            segs.push((*s, e));
        }
    }
    if segs.is_empty() {
        return Ok((Vec::new(), region_start));
    }
    let take_from = segs.len().saturating_sub(max_records);
    let kept = &segs[take_from..];
    let cursor = region_start + kept[0].0 as u64;
    let records = kept
        .iter()
        .map(|(s, e)| {
            String::from_utf8_lossy(&buf[*s..*e])
                .trim_end_matches('\r')
                .to_string()
        })
        .collect();
    Ok((records, cursor))
}

/// Read a bounded window of a run's log instead of the whole file.
///
/// `before = None` means "the newest records". To page backwards, pass the
/// previous response's `cursor`.
///
/// Note this does NOT run the session upsert `get_run_log` does. Callers page
/// through a log they already decided to read; the decision of whether upstream
/// needs re-syncing belongs to the revision check.
#[tauri::command]
pub async fn get_run_log_page(
    db: State<'_, Db>,
    run_id: String,
    before: Option<u64>,
    limit: Option<usize>,
) -> Result<RunLogPage, String> {
    const DEFAULT_LIMIT: usize = 80;
    const MAX_LIMIT: usize = 500;
    const MAX_BYTES: u64 = 4 * 1024 * 1024;

    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    let revision = build_revision(&row, &run_id);
    let Some(path) = revision.path.as_ref().map(PathBuf::from) else {
        return Ok(RunLogPage {
            records: Vec::new(),
            cursor: 0,
            has_more: false,
            preamble: None,
            revision,
        });
    };

    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let before = before.unwrap_or(revision.size);
    let (records, cursor) = read_records_backwards(&path, before, limit, MAX_BYTES)
        .map_err(|e| format!("read log: {}", e))?;
    let has_more = cursor > 0;
    let preamble = if has_more { read_preamble(&path) } else { None };

    Ok(RunLogPage {
        records,
        cursor,
        has_more,
        preamble,
        revision,
    })
}

/// Records the "files changed" list needs, reduced to the fields it reads.
#[derive(Debug, Serialize)]
pub struct RunToolRecords {
    pub records: Vec<String>,
    pub revision: RunLogRevision,
}

/// `input` keys that can name a file. Mirrors `FILE_PATH_KEYS` plus the payload
/// keys `fileTargets()` inspects in `src/components/MentionedFiles.vue`.
const TOOL_INPUT_KEEP_KEYS: [&str; 6] =
    ["file_path", "notebook_path", "path", "changes", "input", "patch"];

/// Scan a whole log for the tool calls that touched files, WITHOUT sending the
/// log to the frontend.
///
/// This backs the "files changed" button, which needs a count over the entire run
/// — the run viewer now only loads a window, so deriving it from what's on screen
/// would under-report. Rather than reimplementing the (engine-specific) path
/// extraction in Rust and risking it drifting from the TypeScript, this returns
/// the matching records in their original shape and lets the existing
/// `writeActionOf` / `fileTargets` logic run on them unchanged.
///
/// Safe to do generically because the log is already normalised: Codex's
/// `fileChange` / `apply_patch` items are rewritten into Claude-shaped
/// `tool_use` blocks by the Codex sidecar before they are ever written
/// (`sidecar-codex/index.mjs`), so both engines look identical here.
///
/// Each record is PRUNED to the fields the caller actually reads. That matters:
/// a `Write` tool_use carries the entire file body in `input.content`, and a
/// `tool_result` carries whole file reads — keeping those would rebuild the very
/// payload this is meant to avoid. Only `is_error` is kept from results.
#[tauri::command]
pub async fn get_run_tool_records(
    db: State<'_, Db>,
    run_id: String,
) -> Result<RunToolRecords, String> {
    use serde_json::{json, Value};
    use std::io::{BufRead, BufReader};

    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    let revision = build_revision(&row, &run_id);
    let Some(path) = revision.path.as_ref().map(PathBuf::from) else {
        return Ok(RunToolRecords {
            records: Vec::new(),
            revision,
        });
    };

    let file = fs::File::open(&path).map_err(|e| format!("open log: {}", e))?;
    // Large enough that a multi-MB record (a pasted image) doesn't thrash, but
    // the reader still streams — the file is never held in memory as a whole.
    let reader = BufReader::with_capacity(256 * 1024, file);
    let mut records: Vec<String> = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        // Cheap substring reject before paying for a JSON parse. The vast
        // majority of records are assistant text and never match.
        if !line.contains("tool_use") && !line.contains("tool_result") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let Some(content) = v
            .get("message")
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_array())
        else {
            continue;
        };

        let mut kept: Vec<Value> = Vec::new();
        for block in content {
            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match (ty, btype) {
                ("assistant", "tool_use") => {
                    let mut input = serde_json::Map::new();
                    if let Some(obj) = block.get("input").and_then(|i| i.as_object()) {
                        for k in TOOL_INPUT_KEEP_KEYS {
                            if let Some(v) = obj.get(k) {
                                input.insert(k.to_string(), v.clone());
                            }
                        }
                    }
                    kept.push(json!({
                        "type": "tool_use",
                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "input": Value::Object(input),
                    }));
                }
                ("user", "tool_result") => {
                    kept.push(json!({
                        "type": "tool_result",
                        "tool_use_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                        "is_error": block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false),
                        // Deliberately empty: only `is_error` is read, and real
                        // contents are whole file reads.
                        "content": "",
                    }));
                }
                _ => {}
            }
        }
        if kept.is_empty() {
            continue;
        }
        let reduced = json!({ "type": ty, "message": { "content": kept } });
        records.push(reduced.to_string());
    }

    Ok(RunToolRecords { records, revision })
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
    claude_account_id: Option<String>,
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
        "SELECT r.project_id, r.repo_id, r.type, r.ref_number, r.input_path, r.output_path, r.engine, r.status, r.claude_account_id,
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
    let claude_account_id: Option<String> = row.get("claude_account_id");
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
        claude_account_id,
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
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, claude_account_id, input_path, output_path, created_at)
         VALUES (?, ?, ?, ?, ?, 'fetched', ?, ?, ?, ?, ?)",
    )
    .bind(&new_id)
    .bind(&src.project_id)
    .bind(&src.repo_id)
    .bind(&src.run_type)
    .bind(src.ref_number)
    .bind(&src.engine)
    .bind(&src.claude_account_id)
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
        claude_account_id: src.claude_account_id,
        started_at: None,
        finished_at: None,
        created_at: now,
        last_activity_at: None,
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
            claude_account_id: None,
            started_at: None,
            finished_at: None,
            created_at: now,
            last_activity_at: None,
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
        claude_account_id: None,
        started_at: None,
        finished_at: None,
        created_at: now,
        last_activity_at: None,
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

fn global_claude_transcript_path(project_path: &str, session_id: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".claude")
            .join("projects")
            .join(crate::commands::sessions::encode_project_dir(project_path))
            .join(format!("{session_id}.jsonl")),
    )
}

fn claude_transcript_path_for_account(
    config_dir: Option<&str>,
    project_path: &str,
    session_id: &str,
) -> Option<PathBuf> {
    match config_dir.map(str::trim).filter(|v| !v.is_empty()) {
        Some(dir) => Some(
            Path::new(dir)
                .join("projects")
                .join(crate::commands::sessions::encode_project_dir(project_path))
                .join(format!("{session_id}.jsonl")),
        ),
        None => global_claude_transcript_path(project_path, session_id),
    }
}

async fn find_existing_claude_transcript(
    db: &Db,
    project_path: &str,
    session_id: &str,
    transcript_path: Option<&str>,
) -> Option<PathBuf> {
    if let Some(path) = transcript_path.map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(path);
    }

    for dir in crate::commands::sessions::claude_sessions_dirs(db, project_path).await {
        let candidate = dir.join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Ensure a resumed Claude session is present in the target account profile.
/// Claude Code stores transcript history under the active `CLAUDE_CONFIG_DIR`;
/// copying the known transcript lets a finished run continue under another
/// account instead of failing because the new profile has never seen that
/// session id.
async fn mirror_claude_transcript_for_account(
    db: &Db,
    project_path: &str,
    session_id: &str,
    transcript_path: Option<&str>,
    target_config_dir: Option<&str>,
) -> Result<Option<PathBuf>, String> {
    let Some(dest) =
        claude_transcript_path_for_account(target_config_dir, project_path, session_id)
    else {
        return Ok(None);
    };
    if dest.is_file() {
        return Ok(Some(dest));
    }

    let Some(src) =
        find_existing_claude_transcript(db, project_path, session_id, transcript_path).await
    else {
        return Ok(None);
    };
    if src == dest {
        return Ok(Some(dest));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(&src, &dest).map_err(|e| {
        format!(
            "Failed to copy Claude transcript from {} to {}: {e}",
            src.display(),
            dest.display()
        )
    })?;
    Ok(Some(dest))
}

/// Change the Claude account assigned to a run/session. Applies to the next
/// start/resume; an already-running sidecar keeps the environment it spawned
/// with, so running runs are refused.
#[tauri::command]
pub async fn set_run_claude_account(
    db: State<'_, Db>,
    run_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    set_run_claude_account_inner(db.inner(), run_id, account_id).await
}

/// The body of [`set_run_claude_account`], callable without a Tauri `State`.
///
/// Split out so the Remote Control handler can reuse it: switching accounts is
/// not a plain column write — it re-resolves the runtime account and mirrors the
/// Claude transcript into that account's config dir — so the remote path must go
/// through exactly this logic rather than reimplementing it.
pub async fn set_run_claude_account_inner(
    db: &Db,
    run_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    use sqlx::Row;

    let account_id = account_id.and_then(|v| {
        let v = v.trim().to_string();
        (!v.is_empty()).then_some(v)
    });

    let row = sqlx::query(
        "SELECT r.engine, r.status, r.session_id, r.transcript_path, p.path as project_path
         FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.id = ?",
    )
    .bind(&run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;

    let engine: String = row.get("engine");
    if engine != "claude" {
        return Err("Only Claude runs can use Claude accounts".to_string());
    }

    let status: String = row.get("status");
    if status == "running" {
        return Err("Cannot change Claude account while the run is running".to_string());
    }

    let account = match account_id.as_deref() {
        Some(id) => {
            crate::commands::claude_accounts::resolve_runtime_account(db, None, Some(id))
                .await?
        }
        None => None,
    };

    let session_id: Option<String> = row.get("session_id");
    let transcript_path: Option<String> = row.get("transcript_path");
    let project_path: String = row.get("project_path");
    let mirrored = if let Some(sid) = session_id.as_deref() {
        mirror_claude_transcript_for_account(
            db,
            &project_path,
            sid,
            transcript_path.as_deref(),
            account.as_ref().map(|a| a.config_dir.as_str()),
        )
        .await?
    } else {
        None
    };

    if let Some(path) = mirrored {
        sqlx::query(
            "UPDATE runs SET claude_account_id = ?, transcript_path = ?, transcript_synced_size = NULL WHERE id = ?",
        )
        .bind(account_id)
        .bind(path.to_string_lossy().as_ref())
        .bind(&run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    } else {
        sqlx::query("UPDATE runs SET claude_account_id = ? WHERE id = ?")
            .bind(account_id)
            .bind(&run_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    }
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
        "SELECT r.engine, r.status, r.session_id, r.project_id, r.claude_account_id, p.path as project_path
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
    let claude_account_id: Option<String> = row.get("claude_account_id");
    let project_path: String = row.get("project_path");

    // Same budget guardrail as start_run — resuming a finished run starts a new
    // turn and consumes tokens, so it must be gated too. Checked against the
    // run's assigned Claude account, not the current default.
    crate::commands::stats::enforce_budget(
        db.inner(),
        &engine,
        claude_account_id.as_deref(),
        override_budget.unwrap_or(false),
    )
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
    // Resuming is the clearest signal of "I'm working on this one again", so it
    // also stamps last_activity_at and floats the session to the top of History.
    let started_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE runs SET status = 'running', started_at = ?, finished_at = NULL, last_activity_at = ? WHERE id = ?",
    )
    .bind(&started_at)
    .bind(&started_at)
    .bind(&run_id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    // Resume uses the account currently assigned to the run. A legacy/global run
    // without an assigned account (`claude_account_id = NULL`) keeps the global
    // `~/.claude` profile instead of silently switching to the current default.
    // Errors if the assigned account was deleted.
    let claude_account = if engine == "claude" {
        match claude_account_id.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => {
                crate::commands::claude_accounts::resolve_runtime_account(
                    db.inner(),
                    None,
                    Some(id),
                )
                .await?
            }
            _ => None,
        }
    } else {
        None
    };

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
        let codex_mcp = if crate::commands::mcp::builtin_devdy_enabled(db.inner()).await {
            crate::commands::mcp::with_builtin_devdy(
                codex_mcp,
                &node_bin,
                &crate::runs::sidecar::resolve_mcp_sidecar_script(&app),
                &crate::runs::sidecar::resolve_db_path(&app),
                &project_id,
                &project_path,
            )
        } else {
            codex_mcp
        };
        let codex_mcp = crate::commands::mcp::with_builtin_google(
            db.inner(),
            codex_mcp,
            &node_bin,
            &crate::runs::sidecar::resolve_mcp_script(&app, "google.mjs"),
        )
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
            claude_usage_capture_mode(db.inner(), claude_account.as_ref().map(|a| a.id.as_str())).await,
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
        let mcp = if crate::commands::mcp::builtin_devdy_enabled(db.inner()).await {
            crate::commands::mcp::with_builtin_devdy(
                mcp,
                &node_bin,
                &crate::runs::sidecar::resolve_mcp_sidecar_script(&app),
                &crate::runs::sidecar::resolve_db_path(&app),
                &project_id,
                &project_path,
            )
        } else {
            mcp
        };
        let mcp = crate::commands::mcp::with_builtin_google(
            db.inner(),
            mcp,
            &node_bin,
            &crate::runs::sidecar::resolve_mcp_script(&app, "google.mjs"),
        )
        .await;
        if !mcp.is_null() {
            cmd.env("DEVDY_MCP_SERVERS", mcp.to_string());
        }
        // Reuse the original run's Claude account profile (clears inherited auth
        // env overrides). No-op for legacy runs → keeps the global profile.
        apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
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

#[cfg(test)]
mod log_page_tests {
    use super::*;
    use std::io::Write;

    fn temp_log(name: &str, content: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "devdy_logpage_{}_{}_{}.log",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = fs::File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    /// Enough lines to force the backward walk across several 64KB chunks.
    fn numbered(n: usize, pad: usize) -> String {
        (0..n)
            .map(|i| format!("{}{}\n", i, "x".repeat(pad)))
            .collect()
    }

    #[test]
    fn reads_the_newest_records_in_order() {
        let p = temp_log("tail", "a\nb\nc\nd\ne\n");
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 2, 1 << 20).unwrap();
        assert_eq!(recs, vec!["d".to_string(), "e".to_string()]);
        // "d" starts after "a\nb\nc\n" = 6 bytes.
        assert_eq!(cursor, 6);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn paging_backwards_by_cursor_covers_every_record_exactly_once() {
        let p = temp_log("page", "a\nb\nc\nd\ne\n");
        let mut seen: Vec<String> = Vec::new();
        let mut before = u64::MAX;
        loop {
            let (recs, cursor) = read_records_backwards(&p, before, 2, 1 << 20).unwrap();
            if recs.is_empty() {
                break;
            }
            let mut next = recs;
            next.extend(seen);
            seen = next;
            if cursor == 0 {
                break;
            }
            before = cursor;
        }
        assert_eq!(seen, vec!["a", "b", "c", "d", "e"]);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn never_returns_a_partial_record() {
        // 5000 lines of ~200 bytes forces many chunk reads; every returned record
        // must still be whole.
        let content = numbered(5000, 200);
        let p = temp_log("partial", &content);
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 80, 1 << 20).unwrap();
        assert_eq!(recs.len(), 80);
        assert_eq!(recs[79], format!("4999{}", "x".repeat(200)));
        assert_eq!(recs[0], format!("4920{}", "x".repeat(200)));
        // The cursor must land exactly on a record boundary.
        assert_eq!(content.as_bytes()[cursor as usize - 1], b'\n');
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn stops_at_start_of_file_without_dropping_the_first_record() {
        let p = temp_log("head", "first\nsecond\n");
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 80, 1 << 20).unwrap();
        assert_eq!(recs, vec!["first".to_string(), "second".to_string()]);
        assert_eq!(cursor, 0);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn returns_a_newest_record_larger_than_the_byte_cap() {
        // A pasted image arrives as one huge base64 record. When it IS the newest
        // record the cap must not stop the walk early, or the caller would get an
        // empty page and paging could never advance past it.
        let big = "z".repeat(300_000);
        let p = temp_log("big_tail", &format!("small\n{}\n", big));
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 2, 64 * 1024).unwrap();
        assert_eq!(recs.len(), 2);
        assert_eq!(recs[0], "small");
        assert_eq!(recs[1].len(), big.len());
        assert_eq!(cursor, 0);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn byte_cap_stops_early_rather_than_truncating_a_record() {
        // With a whole record already in hand the cap wins, so the oversized
        // record is left for the next page instead of being cut in half.
        let big = "z".repeat(300_000);
        let p = temp_log("big_mid", &format!("small\n{}\ntail\n", big));
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 2, 64 * 1024).unwrap();
        assert_eq!(recs, vec!["tail".to_string()]);
        assert!(cursor > 0);

        // …and the next page picks the big record up whole.
        let (older, _) = read_records_backwards(&p, cursor, 2, 64 * 1024).unwrap();
        assert_eq!(older.last().unwrap().len(), big.len());
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn handles_a_file_with_no_trailing_newline() {
        let p = temp_log("nonl", "a\nb\nlast-unterminated");
        let (recs, _) = read_records_backwards(&p, u64::MAX, 2, 1 << 20).unwrap();
        assert_eq!(recs, vec!["b".to_string(), "last-unterminated".to_string()]);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn empty_file_yields_nothing() {
        let p = temp_log("empty", "");
        let (recs, cursor) = read_records_backwards(&p, u64::MAX, 80, 1 << 20).unwrap();
        assert!(recs.is_empty());
        assert_eq!(cursor, 0);
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn preamble_finds_the_system_line_past_leading_noise() {
        let p = temp_log(
            "pre",
            "[stderr] starting\n[log:info] warming up\n{\"type\":\"system\",\"model\":\"claude-x\"}\n{\"type\":\"assistant\"}\n",
        );
        let pre = read_preamble(&p).unwrap();
        assert!(pre.contains("\"type\":\"system\""));
        let _ = fs::remove_file(&p);
    }

    #[test]
    fn preamble_falls_back_to_the_first_json_record() {
        let p = temp_log("pre2", "[stderr] noise\n{\"type\":\"assistant\"}\n");
        assert_eq!(read_preamble(&p).unwrap(), "{\"type\":\"assistant\"}");
        let _ = fs::remove_file(&p);
    }
}

#[cfg(test)]
mod tool_record_tests {
    use super::*;
    use serde_json::{json, Value};

    /// The pruning half of `get_run_tool_records`, lifted out so it can be
    /// exercised without a database. Mirrors the loop body exactly.
    fn reduce(line: &str) -> Option<String> {
        if !line.contains("tool_use") && !line.contains("tool_result") {
            return None;
        }
        let v: Value = serde_json::from_str(line).ok()?;
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let content = v.get("message")?.get("content")?.as_array()?;
        let mut kept: Vec<Value> = Vec::new();
        for block in content {
            let btype = block.get("type").and_then(|t| t.as_str()).unwrap_or("");
            match (ty, btype) {
                ("assistant", "tool_use") => {
                    let mut input = serde_json::Map::new();
                    if let Some(obj) = block.get("input").and_then(|i| i.as_object()) {
                        for k in TOOL_INPUT_KEEP_KEYS {
                            if let Some(v) = obj.get(k) {
                                input.insert(k.to_string(), v.clone());
                            }
                        }
                    }
                    kept.push(json!({
                        "type": "tool_use",
                        "id": block.get("id").cloned().unwrap_or(Value::Null),
                        "name": block.get("name").cloned().unwrap_or(Value::Null),
                        "input": Value::Object(input),
                    }));
                }
                ("user", "tool_result") => {
                    kept.push(json!({
                        "type": "tool_result",
                        "tool_use_id": block.get("tool_use_id").cloned().unwrap_or(Value::Null),
                        "is_error": block.get("is_error").and_then(|e| e.as_bool()).unwrap_or(false),
                        "content": "",
                    }));
                }
                _ => {}
            }
        }
        if kept.is_empty() {
            return None;
        }
        Some(json!({ "type": ty, "message": { "content": kept } }).to_string())
    }

    #[test]
    fn drops_the_file_body_but_keeps_the_path() {
        let body = "x".repeat(100_000);
        let line = json!({
            "type": "assistant",
            "message": { "content": [{
                "type": "tool_use", "id": "t1", "name": "Write",
                "input": { "file_path": "/proj/a.ts", "content": body }
            }]}
        })
        .to_string();
        let out = reduce(&line).unwrap();
        assert!(out.contains("/proj/a.ts"));
        assert!(out.contains("\"name\":\"Write\""));
        // The 100KB payload must not survive.
        assert!(out.len() < 500, "reduced record still huge: {}", out.len());
    }

    #[test]
    fn keeps_the_codex_patch_text_which_names_the_files() {
        let line = json!({
            "type": "assistant",
            "message": { "content": [{
                "type": "tool_use", "id": "t2", "name": "Edit",
                "input": { "input": "*** Update File: src/main.rs\n-a\n+b\n" }
            }]}
        })
        .to_string();
        let out = reduce(&line).unwrap();
        assert!(out.contains("*** Update File: src/main.rs"));
    }

    #[test]
    fn keeps_codex_changes_payload() {
        let line = json!({
            "type": "assistant",
            "message": { "content": [{
                "type": "tool_use", "id": "t3", "name": "Edit",
                "input": { "changes": [{ "path": "src/lib.rs", "kind": "update" }], "unified_diff": "noise" }
            }]}
        })
        .to_string();
        let out = reduce(&line).unwrap();
        assert!(out.contains("src/lib.rs"));
        assert!(!out.contains("noise"));
    }

    #[test]
    fn tool_result_keeps_only_the_error_flag() {
        let line = json!({
            "type": "user",
            "message": { "content": [{
                "type": "tool_result", "tool_use_id": "t1",
                "is_error": true, "content": "y".repeat(50_000)
            }]}
        })
        .to_string();
        let out = reduce(&line).unwrap();
        assert!(out.contains("\"is_error\":true"));
        assert!(out.contains("\"tool_use_id\":\"t1\""));
        assert!(out.len() < 300);
    }

    #[test]
    fn ignores_records_with_no_tool_blocks() {
        let line = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "just talking about tool_use" }]}
        })
        .to_string();
        assert!(reduce(&line).is_none());
    }

    #[test]
    fn ignores_plain_assistant_text() {
        let line = json!({
            "type": "assistant",
            "message": { "content": [{ "type": "text", "text": "hello" }]}
        })
        .to_string();
        assert!(reduce(&line).is_none());
    }
}
