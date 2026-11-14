use crate::commands::github::RunRecord;
use crate::db::Db;
use crate::runs::broker::BrokerHandle;
use crate::runs::sidecar::{
    apply_claude_config_dir, apply_mcp_tool_timeout, augment_command_path,
    detach_sidecar_process_group, drain_sidecar, kill_process_group, resolve_codex_sidecar,
    resolve_sidecar, sdk_permission_mode,
};
use crate::runs::ssh_access::{self, SshAccessGuard};
use crate::runs::stdin_writer::SidecarInput;
use crate::runs::{BrokerRunCtx, BrokerRunGuard, BrokerRuns, RunHandles, RunRegistry};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Manager, State};
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
    /// Extra text appended to the system prompt, on top of any git-account
    /// context. Used by the Conductor to inject role/orchestration framing.
    #[serde(default)]
    pub append_system_prompt: Option<String>,
    /// When set, this run is a Conductor: inject the loopback `conductor` MCP
    /// server (Streamable HTTP) carrying this bearer token so the run can drive
    /// worker sessions via the `session_*` tools. `None` for ordinary runs.
    #[serde(default)]
    pub conductor_token: Option<String>,
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
                r.system_instruction, p.path as project_path, COALESCE(r.worktree_path, p.path) as run_cwd
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
    // Where the session works: the project root, or its own worktree. Logs and
    // per-run scratch files stay under the project root either way.
    let run_cwd: String = run_row.get("run_cwd");
    require_worktree(&project_path, &run_cwd)?;
    let existing_claude_account_id: Option<String> = run_row.get("claude_account_id");
    let stored_instruction: Option<String> = run_row.get("system_instruction");

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
    // Fixed prompts for the GitHub "Analyze issue" / "Review PR" actions. No longer
    // user-configurable; a per-run `custom_prompt` from the payload still overrides them.
    let analyze_prompt =
        "Please analyze this GitHub issue and create a detailed implementation plan.".to_string();
    let review_prompt =
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

    // Every top-level run launches as a conductor (any engine, any run type);
    // workers get `None`. Resolved here — not only by `start_conductor` — so
    // every start path (desktop, Remote Control, rerun/handoff) is covered.
    let conductor_launch = match app.try_state::<crate::conductor::ConductorState>() {
        Some(cst) => {
            crate::conductor::commands::ensure_conductor_launch(db, cst.inner(), &payload.run_id)
                .await?
        }
        None => None,
    };
    let conductor_token: Option<String> = payload
        .conductor_token
        .clone()
        .or_else(|| conductor_launch.as_ref().map(|c| c.token.clone()));
    // Role instruction (the caller's, else the one persisted for a worker) + the
    // session framing. Both engines get it: Claude as an appended system prompt,
    // Codex as thread developer instructions (which seeds the new thread).
    let instruction = payload
        .append_system_prompt
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(stored_instruction);
    let extra_system_prompt: Option<String> =
        crate::conductor::run_framing(instruction.as_deref(), conductor_launch.as_ref());
    // A conductor outside bypass mode still runs its own orchestration tools
    // without a modal; only real work stays permission-gated.
    let auto_allow_conductor_tools =
        conductor_token.is_some() && permission_mode != "bypassPermissions";

    // Resolve the model: per-run override wins, else the per-engine default
    // setting. An empty result means "let the engine pick its default".
    let engine_default_model = if engine == "codex" {
        codex_model
    } else {
        claude_model
    };
    let model = resolve_model_for_engine(&engine, payload.model_override, engine_default_model);

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

    // Update run status to running. Persist the resolved model so the History
    // list and composer can show which model the session actually runs on
    // (NULL means the engine's own default was used).
    let started_at = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        // A fresh start is a fresh Codex thread, re-seeded by its own thread/start
        // developer instructions (the sidecar reports `_devdy_framing_seeded`).
        "UPDATE runs SET status = 'running', engine = ?, model = ?, started_at = ?, last_activity_at = ?, framing_seeded = 0 WHERE id = ?",
    )
    .bind(&engine)
    .bind(&model)
    .bind(&started_at)
    .bind(&started_at)
    .bind(&payload.run_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    // A new (or restarted) run moves to the top of the phone's run browser too.
    crate::remote::live_lists::emit_runs_changed(&app);

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
        cmd.current_dir(&run_cwd).arg(&sidecar_script);
        augment_command_path(&mut cmd);
        apply_mcp_tool_timeout(&mut cmd);
        // GĐ3: per-run credential broker + gh/glab shim. Prepends the shim dir to
        // PATH and sets DEVDY_BROKER_SOCK/DEVDY_PROJECT_ID. Fail-closed: no token
        // is ever placed on the sidecar env. Held in RunHandles for Drop cleanup.
        let (broker_run, ssh_access) = wire_broker(
            &app,
            db,
            &mut cmd,
            &payload.run_id,
            &project_id,
            &run_cwd,
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
        // Stream text/thinking deltas so the chat renders as the model writes.
        cmd.env("DEVDY_PARTIAL_MESSAGES", "1");
        // "Only use Devdy-managed MCP servers": the sidecar sets strictMcpConfig.
        if crate::commands::mcp::strict_mcp_config_enabled(db).await {
            cmd.env("DEVDY_STRICT_MCP", "1");
        }
        if let Some(m) = &model {
            cmd.env("DEVDY_MODEL", m);
        }
        // Isolate this run to its resolved Claude account profile (clears inherited
        // auth env overrides). No-op when no managed account applies (legacy global).
        apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        detach_sidecar_process_group(&mut cmd);
        cmd.kill_on_drop(true);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn sidecar ({}): {}", node_bin, e))?;
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        // The sidecar's input writer exists from the start: no write to the pipe
        // ever happens on this task (a large first prompt or a sidecar stuck at
        // startup must not block start_run before the run is even registered).
        let input = child
            .stdin
            .take()
            .map(|w| SidecarInput::spawn(w, input_stall_hook(&app, &payload.run_id)));

        // Build the first turn (queued below, once the run is registered).
        let mut first_line: Option<String> = None;
        if input.is_some() {
            // GĐ6 (AC1): describe the project's pre-wired git account(s) to Claude
            // via an appended system prompt. `None` when nothing is linked → no
            // field is added and the run behaves exactly as before (AC6). Never
            // contains a token — label/username/host metadata only.
            let account_context =
                crate::runs::broker::token::build_account_context(
                &db_pool,
                &project_id,
                Some(&payload.run_id),
            )
            .await;
            let mut options = serde_json::json!({
                "cwd": run_cwd,
                "permissionMode": sdk_permission_mode(&permission_mode),
                "model": model,
            });
            // Combine the git-account context with any caller-supplied system
            // prompt (the Conductor's orchestration framing) into one append.
            let mut append_parts: Vec<String> = Vec::new();
            if let Some(ctx) = &account_context {
                append_parts.push(ctx.clone());
            }
            if let Some(extra) = &extra_system_prompt {
                append_parts.push(extra.clone());
            }
            if !append_parts.is_empty() {
                options["appendSystemPrompt"] =
                    serde_json::Value::String(append_parts.join("\n\n"));
            }
            if auto_allow_conductor_tools {
                options["allowedTools"] =
                    serde_json::json!([crate::conductor::CONDUCTOR_TOOLS_ALLOW]);
            }
            // Inject the project's enabled MCP servers for the ACTUAL run engine
            // (QĐ-2). Claude gets the full 3-transport map via options.mcpServers;
            // MCP tools still flow through the existing canUseTool permission path.
            let (mcp, _skipped) = assemble_run_mcp(
                &app,
                &db_pool,
                "claude",
                &node_bin,
                &project_id,
                &run_cwd,
                &payload.run_id,
                conductor_token.is_some(),
            )
            .await?;
            if !mcp.is_null() {
                options["mcpServers"] = mcp;
            }
            let first = serde_json::json!({
                "type": "prompt",
                "text": prompt,
                "images": images_payload(&payload.images),
                "options": options,
            });
            first_line = Some(format!("{}\n", first));
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
                    proc: crate::procs::track(
                        crate::procs::ProcSpec::new(crate::procs::ProcKind::Run, "Session")
                            .run(&payload.run_id, &project_id),
                        child.id(),
                    ),
                    pgid: child.id(),
                    child,
                    stdin: input.clone(),
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
            run_cwd,
            stdout,
            stderr,
            db_pool,
            registry_arc,
            session_id,
            log_buf,
            log_path,
            false,
        ));
        // Registered and drained: now hand the first turn to the writer.
        queue_first_prompt(input.as_ref(), first_line);
        return Ok(());
    }

    // ---- Codex: drive the `codex app-server` sidecar (subscription auth) ----
    // Same NDJSON `_devdy_*` protocol as the Claude sidecar, so drain_sidecar,
    // the stream renderer, and the permission modal all work unchanged. (Replaces
    // the old one-shot `codex exec` path, which had no streaming/approval/multi-turn.)
    let _ = &extra; // codex tuning is now via sandbox/approval env, not exec args
    let (node_bin, sidecar_script) = resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?;
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&run_cwd).arg(&sidecar_script);
    augment_command_path(&mut cmd);
    apply_mcp_tool_timeout(&mut cmd);
    let (broker_run, ssh_access) = wire_broker(
        &app,
        db,
        &mut cmd,
        &payload.run_id,
        &project_id,
        &run_cwd,
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
    let (codex_mcp, mcp_skipped) = assemble_run_mcp(
        &app,
        &db_pool,
        "codex",
        &node_bin,
        &project_id,
        &run_cwd,
        &payload.run_id,
        conductor_token.is_some(),
    )
    .await?;
    if !codex_mcp.is_null() {
        cmd.env("DEVDY_CODEX_MCP", codex_mcp.to_string());
    }
    // Conductor framing / worker instruction → Codex developer instructions.
    if let Some(extra) = &extra_system_prompt {
        cmd.env("DEVDY_CODEX_DEVELOPER_INSTRUCTIONS", extra);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_sidecar_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn codex sidecar ({}): {}", node_bin, e))?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // Input writer from the start; nothing writes the pipe on this task (see
    // the Claude path above).
    let input = child
        .stdin
        .take()
        .map(|w| SidecarInput::spawn(w, input_stall_hook(&app, &payload.run_id)));

    // Build the first turn (queued below, once the run is registered).
    let mut first_line: Option<String> = None;
    if input.is_some() {
        let account_context =
            crate::runs::broker::token::build_account_context(
                &db_pool,
                &project_id,
                Some(&payload.run_id),
            )
            .await;
        let prompt_text = account_context
            .as_ref()
            .map(|ctx| format!("{ctx}\n\nUser request:\n{prompt}"))
            .unwrap_or_else(|| prompt.clone());
        let first = serde_json::json!({
            "type": "prompt",
            "text": prompt_text,
            "images": images_payload(&payload.images),
        });
        first_line = Some(format!("{}\n", first));
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
                proc: crate::procs::track(
                    crate::procs::ProcSpec::new(crate::procs::ProcKind::Run, "Session")
                        .run(&payload.run_id, &project_id),
                    child.id(),
                ),
                pgid: child.id(),
                child,
                stdin: input.clone(),
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
        run_cwd,
        stdout,
        stderr,
        db_pool,
        registry_arc,
        session_id,
        log_buf,
        log_path,
        false,
    ));
    // Registered and drained: now hand the first turn to the writer.
    queue_first_prompt(input.as_ref(), first_line);
    Ok(())
}

/// Resolve the effective model from a per-run override and the per-engine
/// default. Returns None when neither is set (engine picks its own default).
/// Classify a model id to the engine that serves it, by family prefix.
///
/// Returns `None` for ids we don't recognize (a custom or newly-pinned alias)
/// so callers do NOT drop them — only a KNOWN cross-engine mismatch (a Claude id
/// handed to a Codex run, or vice versa) should be rejected.
fn engine_of_model(model: &str) -> Option<&'static str> {
    let m = model.trim().to_ascii_lowercase();
    if m.starts_with("claude-") || m.starts_with("claude[") || m == "claude" {
        Some("claude")
    } else if m.starts_with("gpt-") || m.starts_with("gpt[") || m == "gpt" {
        Some("codex")
    } else {
        None
    }
}

/// Resolve the model a run of `engine` should use. Single source of truth for
/// every launch/resume path (desktop sessions, conductor, workers):
///
/// 1. the explicit `override_model` — but only when it belongs to `engine` (an
///    unrecognized id is trusted and kept),
/// 2. else the engine's global default (`claude_model` / `codex_model`),
/// 3. else `None` so the engine picks its own default.
///
/// Dropping a cross-engine override is what lets a conductor spawn a worker on a
/// DIFFERENT engine without its own model leaking in: the worker falls back to
/// its own engine's global default (requirement #3).
fn resolve_model_for_engine(
    engine: &str,
    override_model: Option<String>,
    default_model: String,
) -> Option<String> {
    override_model
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .filter(|s| match engine_of_model(s) {
            Some(e) => e == engine,
            None => true,
        })
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

/// The MCP map for a run on `engine` ("claude" | "codex"): the project's
/// enabled servers plus the built-ins — `devdy` and `google` unless switched off,
/// and the loopback `conductor` server for a conductor run. Credentials are left
/// to the broker (see `McpLaunch`), so the map is safe to put on a command line.
/// Returns the map (`Null` when empty) and the servers skipped for the engine.
#[allow(clippy::too_many_arguments)]
async fn assemble_run_mcp(
    app: &AppHandle,
    db: &Db,
    engine: &str,
    node_bin: &str,
    project_id: &str,
    project_path: &str,
    run_id: &str,
    is_conductor: bool,
) -> Result<(serde_json::Value, Vec<String>), String> {
    use crate::commands::mcp;
    let launch = mcp::McpLaunch {
        node_bin: node_bin.to_string(),
        launcher: resolve_shim_dir(app)?.join("devdy-mcp-launch.mjs"),
        broker_sock: app
            .try_state::<BrokerHandle>()
            .ok_or_else(|| "credential broker not initialized".to_string())?
            .path
            .clone(),
        project_id: project_id.to_string(),
        run_id: run_id.to_string(),
    };
    let (map, skipped) = mcp::resolve_project_mcp_servers(db, project_id, engine, &launch).await;
    let map = if mcp::builtin_mcp_enabled(db, "devdy", project_id).await {
        mcp::with_builtin_devdy(
            map,
            node_bin,
            &crate::runs::sidecar::resolve_mcp_sidecar_script(app),
            &crate::runs::sidecar::resolve_db_path(app),
            project_id,
            project_path,
        )
    } else {
        map
    };
    let map = mcp::with_builtin_google(
        db,
        map,
        &launch,
        &crate::runs::sidecar::resolve_mcp_script(app, "google.mjs"),
        project_id,
    )
    .await;
    let map = match (is_conductor, app.try_state::<crate::conductor::ConductorState>()) {
        (true, Some(cst)) if engine == "codex" => {
            crate::conductor::with_conductor_codex(map, &cst.base_url(), &launch)
        }
        (true, Some(cst)) => crate::conductor::with_conductor(map, &cst.base_url(), &launch),
        _ => map,
    };
    Ok((map, skipped))
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
            project_id: Some(project_id.to_string()),
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

    // AWS: write a minimal per-run config; the `aws` CLI itself is broker-gated by
    // the PATH shim, which injects broker-minted credentials.
    wire_aws_config(app, db, cmd, project_id).await;

    // Azure: never let a bare `az`/SDK in the run see `~/.azure` or an inherited
    // credential env; the `az` / `terraform` shims hand the linked account's
    // config dir to their child per call.
    wire_azure_config(app, cmd);

    // gh / glab: a real binary reached around the shim (e.g. a login shell that
    // reorders PATH) must not find the user's own login.
    wire_git_cli_config(app, cmd);

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

/// A session whose worktree was deleted can't start or resume (its checkout and
/// uncommitted work are gone); refuse before the run is marked running.
fn require_worktree(project_path: &str, run_cwd: &str) -> Result<(), String> {
    if run_cwd != project_path && !Path::new(run_cwd).is_dir() {
        return Err(format!("worktree_gone: this session's worktree no longer exists ({run_cwd})"));
    }
    Ok(())
}

/// Wire a backend command run on a project's behalf (worktree fetch / checkout /
/// commit / push, the worktree setup command) like a run: the Devdy credential
/// helper and shims, fail-closed git config, the project's commit identity — so
/// git authenticates as the project's linked account and never through the
/// user's own helpers. Keep the returned guard alive until the command has
/// finished; it registers `call_id` with the broker for auditing.
pub(crate) async fn wire_backend_command(
    app: &AppHandle,
    db: &Db,
    cmd: &mut Command,
    project_id: &str,
    call_id: &str,
    cwd: &str,
) -> Result<BrokerRunGuard, String> {
    let sock_path = app
        .try_state::<BrokerHandle>()
        .ok_or_else(|| "credential broker not initialized".to_string())?
        .path
        .clone();
    let guard = BrokerRunGuard::register(
        app.state::<BrokerRuns>().inner().clone(),
        call_id.to_string(),
        BrokerRunCtx {
            cwd: Some(cwd.to_string()),
            project_id: Some(project_id.to_string()),
        },
    );
    augment_command_path(cmd);
    let shim_dir = resolve_shim_dir(app)?;
    prepend_shim_path(cmd, &shim_dir);
    cmd.env("DEVDY_BROKER_SOCK", &sock_path);
    cmd.env("DEVDY_PROJECT_ID", project_id);
    cmd.env("DEVDY_RUN_ID", call_id);
    wire_git_config(cmd, &shim_dir.join("git-credential-devdy"));
    wire_commit_identity(cmd, db, project_id).await;
    Ok(guard)
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

async fn wire_aws_config(app: &AppHandle, db: &Db, cmd: &mut Command, project_id: &str) {
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

    // Read-only: the sandbox files are written when the project's AWS account is
    // configured (and backfilled at startup), never here. A project with no
    // linked account shares the static `_empty` sandbox.
    let meta = match crate::runs::broker::token::resolve_aws_runtime_metadata(db, project_id).await
    {
        Ok(Some(meta)) => Some(meta),
        _ => None,
    };

    let Some(meta) = meta else {
        let empty_dir = crate::aws_sandbox::empty_aws_dir(&app_data_dir);
        cmd.env("AWS_CONFIG_FILE", empty_dir.join("config"));
        cmd.env("AWS_SHARED_CREDENTIALS_FILE", empty_dir.join("credentials"));
        cmd.env("AWS_SDK_LOAD_CONFIG", "1");
        return;
    };

    cmd.env("AWS_REGION", &meta.region);
    cmd.env("AWS_DEFAULT_REGION", &meta.region);

    // The `aws` / `terraform` shims inject broker-minted credentials into the
    // real child. SDK credential_process cannot emit a profile name, so the per-project
    // sandbox only pins a region; AWS_PROFILE is never set on the sidecar parent.
    let project_dir = crate::aws_sandbox::project_aws_dir(&app_data_dir, project_id);
    cmd.env("AWS_CONFIG_FILE", project_dir.join("config"));
    cmd.env("AWS_SHARED_CREDENTIALS_FILE", project_dir.join("credentials"));
    cmd.env("AWS_SDK_LOAD_CONFIG", "1");
}

/// Strip inherited Azure / azurerm credential env from the sidecar and point
/// `AZURE_CONFIG_DIR` at the shared EMPTY sandbox, so nothing in the run
/// authenticates to Azure except through the broker-gated shims.
fn wire_azure_config(app: &AppHandle, cmd: &mut Command) {
    for key in crate::commands::azure_accounts::AZURE_CREDENTIAL_ENV {
        cmd.env_remove(key);
    }
    // Also any other inherited AZURE_*/ARM_* (az config overrides, azurerm
    // endpoint/environment overrides).
    for (key, _) in std::env::vars_os() {
        if key
            .to_str()
            .map(crate::commands::azure_accounts::is_azure_env)
            .unwrap_or(false)
        {
            cmd.env_remove(key);
        }
    }
    // Never leave AZURE_CONFIG_DIR unset (that means `~/.azure`), even if the
    // app data dir cannot be resolved.
    let empty_dir = match app.path().app_data_dir() {
        Ok(app_data_dir) => crate::azure_sandbox::empty_azure_dir(&app_data_dir),
        Err(_) => std::env::temp_dir().join("devdy-azure-empty"),
    };
    // Normally created at startup; recreate if something removed it.
    let _ = std::fs::create_dir_all(&empty_dir);
    cmd.env("AZURE_CONFIG_DIR", empty_dir);
}

/// Token env vars gh / glab read on their own. The shims inject the
/// broker's token per call; an inherited one would bypass the broker.
const GIT_CLI_TOKEN_ENV: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "GITLAB_TOKEN",
    "GITLAB_ACCESS_TOKEN",
    "GITLAB_PRIVATE_TOKEN",
    "OAUTH_TOKEN",
    "CI_JOB_TOKEN",
];

/// Strip inherited gh / glab tokens and point `GH_CONFIG_DIR`
/// / `GLAB_CONFIG_DIR` at empty sandboxes, so a real `gh` / `glab` that runs
/// without the shim (login shell, absolute path) is signed out instead of
/// using `~/.config/gh` / `~/.config/glab-cli`. Through the shim the child gets
/// `GH_TOKEN` / `GITLAB_TOKEN`, which needs no config.
fn wire_git_cli_config(app: &AppHandle, cmd: &mut Command) {
    for key in GIT_CLI_TOKEN_ENV {
        cmd.env_remove(key);
    }
    let root = match app.path().app_data_dir() {
        Ok(app_data_dir) => app_data_dir.join("cli-sandbox"),
        Err(_) => std::env::temp_dir().join("devdy-cli-sandbox"),
    };
    for (var, name) in [("GH_CONFIG_DIR", "gh"), ("GLAB_CONFIG_DIR", "glab")] {
        let dir = root.join(name);
        let _ = std::fs::create_dir_all(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        cmd.env(var, dir);
    }
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
    cancel_run_inner(registry.inner(), db.inner(), &run_id, true).await
}

/// Remove a run from the registry and kill its sidecar; false when it was not
/// live here. Writes nothing: the drain sees the missing entry as a cancel.
pub(crate) async fn stop_local_sidecar(registry: &RunRegistry, run_id: &str) -> bool {
    let handle = registry.lock().await.remove(run_id);
    let Some(mut handles) = handle else { return false };
    // Kill the whole process group (node sidecar + claude/codex CLI), then
    // reap the node handle. Group kill prevents an orphaned CLI lingering.
    // The stored group id, not `child.id()`: once the drain has reaped the
    // sidecar, `id()` is None while its CLI/MCP descendants may live on.
    if let Some(pgid) = handles.pgid {
        kill_process_group(pgid);
    }
    let _ = handles.child.kill().await;
    true
}

/// Core of [`cancel_run`] over concrete refs so the Remote Control host agent
/// (FR-009 `cancel_run`) can reuse it without a Tauri `State`.
///
/// `stamp_activity`: whether this cancel counts as user activity and floats the
/// run in History. False when an agent (Conductor) cancels its own worker.
pub(crate) async fn cancel_run_inner(
    registry: &RunRegistry,
    db: &sqlx::SqlitePool,
    run_id: &str,
    stamp_activity: bool,
) -> Result<(), String> {
    let run_id = run_id.to_string();
    stop_local_sidecar(registry, &run_id).await;

    let cancelled_at = chrono::Utc::now().to_rfc3339();
    let sql = if stamp_activity {
        "UPDATE runs SET status = 'cancelled', finished_at = ?, last_activity_at = ? WHERE id = ?"
    } else {
        "UPDATE runs SET status = 'cancelled', finished_at = ?, last_activity_at = COALESCE(last_activity_at, ?) WHERE id = ?"
    };
    sqlx::query(sql)
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
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    payload: SendUserMessagePayload,
) -> Result<(), String> {
    send_user_message_inner(db.inner(), registry.inner(), payload, true).await?;
    // The turn stamped last_activity_at: the phone's run order follows.
    crate::remote::live_lists::emit_runs_changed(&app);
    Ok(())
}

/// Core of [`send_user_message`] over concrete refs so the Remote Control host
/// agent (FR-009 `send_chat_message`) can reuse it without a Tauri `State`.
///
/// `stamp_activity`: whether this turn counts as user activity and floats the
/// run in History. False for agent-sent turns (Conductor follow-ups and wakes),
/// so sessions don't reshuffle while the user isn't touching them.
pub(crate) async fn send_user_message_inner(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    payload: SendUserMessagePayload,
    stamp_activity: bool,
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

    let log_buf = {
        let reg = registry.lock().await;
        let handle = reg
            .get(&payload.run_id)
            .ok_or_else(|| "run not active".to_string())?;
        handle.log_buf.clone()
    };
    write_run_stdin(
        registry,
        &payload.run_id,
        &send_line,
        "run does not accept follow-up input",
    )
    .await?;
    {
        let mut buf = log_buf.lock().await;
        buf.push_str(&log_line);
    }
    // The turn is now the run's newest activity. Stamped here rather than only on
    // `run:done` so the History row moves the moment the user hits send, even for
    // a turn that then runs for minutes.
    if stamp_activity {
        let _ = sqlx::query("UPDATE runs SET last_activity_at = ? WHERE id = ?")
            .bind(chrono::Utc::now().to_rfc3339())
            .bind(&payload.run_id)
            .execute(db)
            .await;
    }
    Ok(())
}

#[tauri::command]
pub async fn end_run_input(registry: State<'_, RunRegistry>, run_id: String) -> Result<(), String> {
    let mut reg = registry.lock().await;
    let handle = reg
        .get_mut(&run_id)
        .ok_or_else(|| "run not active".to_string())?;
    // Dropping the run's input handle closes the pipe (after any queued lines),
    // which tells the sidecar to exit after finishing the current turn.
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
    app: AppHandle,
    registry: State<'_, RunRegistry>,
    broker_approvals: State<'_, crate::runs::BrokerApprovals>,
    payload: RespondPermissionPayload,
) -> Result<(), String> {
    let (run_id, request_id) = (payload.run_id.clone(), payload.request_id.clone());
    match respond_permission_inner(registry.inner(), broker_approvals.inner(), payload).await {
        Ok(_) => {}
        // Someone else (a paired phone) answered first; the prompt is gone
        // either way, so there is nothing to report or to republish.
        Err(e) if e == PERMISSION_ALREADY_HANDLED => return Ok(()),
        Err(e) => return Err(e),
    }
    // Answered here: a paired phone still shows the prompt (its Inbox lists
    // every run's), so tell it to drop it. No-op with Remote Control off.
    if let Some(bus) = app.try_state::<crate::remote::RemoteBus>() {
        bus.publish(crate::remote::RemoteRunEvent::PermissionResolved { run_id, request_id });
    }
    Ok(())
}

/// Error of [`respond_permission_inner`] when the request is no longer open:
/// another surface answered it first, or it was withdrawn.
pub(crate) const PERMISSION_ALREADY_HANDLED: &str = "permission already handled";

/// Core of [`respond_permission`] over concrete refs so background tasks (the
/// Remote Control host agent, FR-007) can reuse the exact same permission-answer
/// path without a Tauri `State`. Same contract as the command.
///
/// The request is CLAIMED first (taken out of the pending store), so when the
/// desktop and a phone answer at once exactly one reaches the sidecar; the
/// other gets [`PERMISSION_ALREADY_HANDLED`] and writes nothing. A delivery
/// failure puts the request back so it can be retried. Returns the request.
pub(crate) async fn respond_permission_inner(
    registry: &RunRegistry,
    broker_approvals: &crate::runs::BrokerApprovals,
    payload: RespondPermissionPayload,
) -> Result<crate::runs::permission::PermissionRequestEvent, String> {
    if !matches!(payload.decision.as_str(), "allow" | "deny" | "ask") {
        return Err(format!("unknown permission decision: {}", payload.decision));
    }
    let Some(claimed) =
        crate::runs::permission::take_pending(&payload.run_id, &payload.request_id)
    else {
        return Err(PERMISSION_ALREADY_HANDLED.to_string());
    };
    // GĐ3: a broker-side `Ask` (gh/glab) registers its request_id here. If this
    // response matches one, resolve its oneshot and return — the sidecar knows
    // nothing about it, so we must NOT route it over stdin. Non-matching ids fall
    // through to the unchanged sidecar path below.
    {
        let mut pending = broker_approvals.lock().await;
        if let Some(tx) = pending.remove(&payload.request_id) {
            let allow = payload.decision == "allow";
            let _ = tx.send(allow);
            return Ok(claimed.event);
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
    if let Err(e) = write_run_stdin(
        registry,
        &payload.run_id,
        &line,
        "run is not accepting input",
    )
    .await
    {
        crate::runs::permission::restore_pending(claimed);
        return Err(e);
    }
    Ok(claimed.event)
}

/// Queue one NDJSON line for a live run's sidecar and wait (bounded) for it to
/// be written. The registry lock is held only to clone the run's input handle.
/// A timeout never cancels the write or closes the input: the error says the
/// line is still queued (see `stdin_writer`).
pub(crate) async fn write_run_stdin(
    registry: &RunRegistry,
    run_id: &str,
    line: &str,
    not_accepting: &str,
) -> Result<(), String> {
    let input = {
        let reg = registry.lock().await;
        let handle = reg
            .get(run_id)
            .ok_or_else(|| "run not active".to_string())?;
        handle
            .stdin
            .clone()
            .ok_or_else(|| not_accepting.to_string())?
    };
    input
        .send(line.to_string(), crate::runs::stdin_writer::ACK_TIMEOUT)
        .await
}

/// Hand a freshly started run's first turn to its input writer. Never waits
/// for the write: the run is already registered (cancellable) and drained, and
/// a sidecar that doesn't read raises the input-stalled notice instead.
fn queue_first_prompt(input: Option<&SidecarInput>, first_line: Option<String>) {
    if let (Some(input), Some(line)) = (input, first_line) {
        // Only fails when the input is already closed (the sidecar died at
        // startup); the drain reports that run's end. Logged for diagnosis.
        if let Err(e) = input.queue(line) {
            tracing::warn!("failed to queue first prompt for sidecar: {}", e);
        }
    }
}

/// Surface a sidecar that stopped reading its input (a write blocked for long)
/// as a liveness notice with a stop action, and clear it once the write lands.
fn input_stall_hook(app: &AppHandle, run_id: &str) -> crate::runs::stdin_writer::StallHook {
    use tauri::Emitter;
    let app = app.clone();
    let run_id = run_id.to_string();
    Box::new(move |stalled| {
        let topic = format!("run:attention:{}", run_id);
        let payload = if stalled {
            let message = "The session is not reading its input (a message to it has been waiting). It may be busy or stuck — stop the session if it doesn't recover.";
            let _ = app.emit(
                &format!("run:output:{}", run_id),
                serde_json::json!({ "run_id": run_id, "line": format!("⚠ {}", message), "is_stderr": false, "level": "warn" }),
            );
            serde_json::json!({
                "run_id": run_id, "key": "input_stalled", "kind": "input_stalled",
                "message": message, "params": {}, "active": true,
            })
        } else {
            serde_json::json!({ "run_id": run_id, "key": "input_stalled", "active": false })
        };
        let _ = app.emit(&topic, payload);
    })
}

/// Permission requests a live run is still blocked on. Lets a webview listener
/// that attached (or re-attached) after the `run:permission_request` event was
/// emitted recover prompts it never received, instead of leaving the run hung.
#[tauri::command]
pub async fn list_pending_permissions(
    run_id: String,
) -> Result<Vec<crate::runs::permission::PermissionRequestEvent>, String> {
    Ok(crate::runs::permission::pending_for(&run_id)
        .into_iter()
        .map(|p| p.event)
        .collect())
}

/// Ids of every run whose sidecar is still registered (alive). The webview uses
/// it to reconcile sessions it still shows as "running" after missing their
/// `run:done` (e.g. the run ended before its listener attached).
#[tauri::command]
pub async fn list_live_runs(registry: State<'_, RunRegistry>) -> Result<Vec<String>, String> {
    Ok(registry.lock().await.keys().cloned().collect())
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
                p.name as project_name, p.path as project_path, COALESCE(r.worktree_path, p.path) as run_cwd
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
    let run_cwd: String = row.get("run_cwd");

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
                &run_cwd,
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

    // Whole-file reads (logs reach tens of MB) — keep them off the async workers.
    tokio::task::spawn_blocking(move || {
        if let Ok(content) = fs::read_to_string(&conv_path) {
            if !content.trim().is_empty() {
                return RunLog { content };
            }
        }
        let content = output_path
            .and_then(|p| fs::read_to_string(&p).ok())
            .unwrap_or_default();
        RunLog { content }
    })
    .await
    .map_err(|e| e.to_string())
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
    run_cwd: String,
}

async fn fetch_run_log_row(db: &Db, run_id: &str) -> Result<RunLogRow, String> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT r.output_path, r.engine, r.session_id, r.transcript_path, p.path as project_path,
                COALESCE(r.worktree_path, p.path) as run_cwd
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
        run_cwd: row.get("run_cwd"),
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
                    crate::commands::sessions::claude_sessions_dir(&row.run_cwd)
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

/// Forward sibling of `read_records_backwards`: whole records starting AT `after`
/// (a record boundary — a cursor from a previous read) reading toward EOF.
/// Returns the records and the byte offset just past the last one, which the
/// next forward read resumes from.
fn read_records_forward(
    path: &Path,
    after: u64,
    max_records: usize,
    max_bytes: u64,
) -> std::io::Result<(Vec<String>, u64)> {
    use std::io::{Read, Seek, SeekFrom};

    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = after.min(len);
    if start >= len || max_records == 0 {
        return Ok((Vec::new(), len));
    }
    f.seek(SeekFrom::Start(start))?;

    const CHUNK: usize = 64 * 1024;
    let mut chunk = vec![0u8; CHUNK];
    let mut buf: Vec<u8> = Vec::new();
    let mut records: Vec<String> = Vec::new();
    // Bytes of COMPLETE records consumed (newline included), so the cursor lands
    // on the next record's first byte.
    let mut consumed: u64 = 0;
    let mut done = false;

    while !done {
        let n = f.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            consumed += line.len() as u64;
            let s = String::from_utf8_lossy(&line[..line.len() - 1])
                .trim_end_matches('\r')
                .to_string();
            if !s.is_empty() {
                records.push(s);
            }
            if records.len() >= max_records || (consumed >= max_bytes && !records.is_empty()) {
                done = true;
                break;
            }
        }
    }
    // The file's final record isn't newline-terminated — include it if there's
    // still room (and we didn't already stop on a limit).
    if !done && records.len() < max_records && !buf.is_empty() {
        let s = String::from_utf8_lossy(&buf)
            .trim_end_matches('\r')
            .to_string();
        if !s.is_empty() {
            consumed += buf.len() as u64;
            records.push(s);
        }
    }

    Ok((records, start + consumed))
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

/// One window of log records reading FORWARD from `after` (a cursor from an
/// earlier read), newest last. Backs appending as the reader scrolls down past
/// the loaded window and the "jump to an old prompt" path, which loads a fresh
/// window starting at the prompt's byte offset instead of paging from the tail.
#[tauri::command]
pub async fn get_run_log_forward(
    db: State<'_, Db>,
    run_id: String,
    after: u64,
    limit: Option<usize>,
) -> Result<RunLogPage, String> {
    const DEFAULT_LIMIT: usize = 120;
    const MAX_LIMIT: usize = 500;
    const MAX_BYTES: u64 = 4 * 1024 * 1024;

    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    let revision = build_revision(&row, &run_id);
    let Some(path) = revision.path.as_ref().map(PathBuf::from) else {
        return Ok(RunLogPage {
            records: Vec::new(),
            cursor: after,
            has_more: false,
            preamble: None,
            revision,
        });
    };

    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let (records, cursor) = read_records_forward(&path, after, limit, MAX_BYTES)
        .map_err(|e| format!("read log: {}", e))?;
    // `cursor` is the offset past the last record; anything beyond it is older
    // content still on disk. The preamble (model id) is only needed by a tail
    // window that can't see the top of the file — a forward window above it will
    // already carry `system.init`, and a mid-file jump is seeded by the caller.
    let has_more = cursor < revision.size;

    Ok(RunLogPage {
        records,
        cursor,
        has_more,
        preamble: None,
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
    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    // Same reasoning as `get_run_user_records`: blocking scan off the async
    // workers, unchanged logs served from cache.
    tokio::task::spawn_blocking(move || {
        let revision = build_revision(&row, &run_id);
        let Some(key) = ScanKey::of(&revision) else {
            return Ok(RunToolRecords {
                records: Vec::new(),
                revision,
            });
        };
        let records = cached_scan(&TOOL_SCAN_CACHE, key, scan_tool_records)?;
        Ok(RunToolRecords {
            records: (*records).clone(),
            revision,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

// ── Whole-log scan cache ─────────────────────────────────────────────────────
//
// `get_run_tool_records` / `get_run_user_records` each scan the entire log
// (8–29MB in practice) and RunView calls both on every run switch. Results are
// cached per file, valid only while the file's `(size, mtime_ns)` — the same
// fingerprint `build_revision` reports — is unchanged, so a growing or rewritten
// log always misses. Bounded LRU so switching through many runs can't grow it.

/// How many runs' scan results each cache keeps.
const SCAN_CACHE_CAP: usize = 16;

/// The file a scan was computed from, plus the stat it is valid for.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ScanKey {
    path: String,
    size: u64,
    mtime_ns: i64,
}

impl ScanKey {
    fn of(rev: &RunLogRevision) -> Option<Self> {
        rev.path.as_ref().map(|p| ScanKey {
            path: p.clone(),
            size: rev.size,
            mtime_ns: rev.mtime_ns,
        })
    }
}

/// Tiny LRU: front = least recently used. At 16 entries a linear scan beats any
/// map, and one entry per path means a stale result is replaced, not duplicated.
struct ScanCache<T> {
    cap: usize,
    entries: std::collections::VecDeque<(ScanKey, Arc<T>)>,
}

impl<T> ScanCache<T> {
    const fn new(cap: usize) -> Self {
        Self {
            cap,
            entries: std::collections::VecDeque::new(),
        }
    }

    /// Hit only when path AND stat match. A same-path entry with a stale stat is
    /// dropped on the spot.
    fn get(&mut self, key: &ScanKey) -> Option<Arc<T>> {
        let idx = self.entries.iter().position(|(k, _)| k.path == key.path)?;
        let (k, v) = self.entries.remove(idx)?;
        if k != *key {
            return None;
        }
        let hit = v.clone();
        self.entries.push_back((k, v));
        Some(hit)
    }

    fn insert(&mut self, key: ScanKey, value: Arc<T>) {
        self.entries.retain(|(k, _)| k.path != key.path);
        self.entries.push_back((key, value));
        while self.entries.len() > self.cap {
            self.entries.pop_front();
        }
    }
}

static TOOL_SCAN_CACHE: std::sync::Mutex<ScanCache<Vec<String>>> =
    std::sync::Mutex::new(ScanCache::new(SCAN_CACHE_CAP));
static USER_SCAN_CACHE: std::sync::Mutex<ScanCache<Vec<UserRecord>>> =
    std::sync::Mutex::new(ScanCache::new(SCAN_CACHE_CAP));

/// Serve `key` from `cache`, or run `scan` and remember the result. The lock is
/// NOT held across the scan, so two runs can be scanned concurrently (a
/// duplicate scan of the same file is harmless — last insert wins).
fn cached_scan<T>(
    cache: &std::sync::Mutex<ScanCache<T>>,
    key: ScanKey,
    scan: impl FnOnce(&Path) -> Result<T, String>,
) -> Result<Arc<T>, String> {
    if let Some(hit) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return Ok(hit);
    }
    let value = Arc::new(scan(Path::new(&key.path))?);
    cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, value.clone());
    Ok(value)
}

/// The whole-file half of `get_run_tool_records`.
fn scan_tool_records(path: &Path) -> Result<Vec<String>, String> {
    use std::io::{BufRead, BufReader};

    let file = fs::File::open(path).map_err(|e| format!("open log: {}", e))?;
    // Large enough that a multi-MB record (a pasted image) doesn't thrash, but
    // the reader still streams — the file is never held in memory as a whole.
    let reader = BufReader::with_capacity(256 * 1024, file);
    let mut records: Vec<String> = Vec::new();

    for line in reader.lines() {
        let Ok(line) = line else { continue };
        if let Some(reduced) = reduce_tool_record(&line) {
            records.push(reduced);
        }
    }
    Ok(records)
}

/// Prune one log line to the tool_use / tool_result fields the "files changed"
/// view reads, or `None` when it carries no tool blocks.
fn reduce_tool_record(line: &str) -> Option<String> {
    use serde_json::{json, Value};

    // Cheap substring reject before paying for a JSON parse. The vast
    // majority of records are assistant text and never match.
    if !line.contains("tool_use") && !line.contains("tool_result") {
        return None;
    }
    let v: Value = serde_json::from_str(line).ok()?;
    let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
    let content = v
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_array())?;

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
        return None;
    }
    Some(json!({ "type": ty, "message": { "content": kept } }).to_string())
}

/// One pruned `type: "user"` record plus the byte offset it starts at, so the
/// viewer can jump straight to it with a forward read instead of paging back.
#[derive(Debug, Clone, Serialize)]
pub struct UserRecord {
    pub offset: u64,
    pub json: String,
}

/// The `type: "user"` records of a run, pruned to just what the turn navigator
/// needs. One per record, in chronological order.
#[derive(Debug, Serialize)]
pub struct RunUserRecords {
    pub records: Vec<UserRecord>,
    pub revision: RunLogRevision,
}

/// Longest user-text kept per block. A preview only ever shows the first ~160
/// chars (see `turnLabel`), so anything beyond a small bound is dead weight.
const USER_TEXT_MAX: usize = 4000;

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Prune a `type: "user"` record down to what the frontend turn navigator reads.
///
/// Keeps every top-level field (so `isCompactSummary` and friends still drive the
/// SAME frontend detection) but strips the message content to text + image
/// placeholders: `tool_result` blocks (the SDK's echo of tool output — often
/// whole file reads) are dropped, and an image block's multi-MB base64 payload is
/// replaced with a stub so an image-only turn still registers without shipping
/// the bytes. Returns the record re-serialised as one JSON line.
fn prune_user_record(mut v: serde_json::Value) -> String {
    use serde_json::{json, Value};
    if let Some(msg) = v.get_mut("message").and_then(|m| m.as_object_mut()) {
        if let Some(content) = msg.remove("content") {
            let pruned = match content {
                Value::String(s) => Value::String(truncate_chars(&s, USER_TEXT_MAX)),
                Value::Array(blocks) => {
                    let mut kept: Vec<Value> = Vec::new();
                    for block in &blocks {
                        match block.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                            "text" => {
                                let text = block.get("text").and_then(|t| t.as_str()).unwrap_or("");
                                kept.push(json!({
                                    "type": "text",
                                    "text": truncate_chars(text, USER_TEXT_MAX),
                                }));
                            }
                            "image" => {
                                let media = block
                                    .get("source")
                                    .and_then(|s| s.get("media_type"))
                                    .and_then(|m| m.as_str())
                                    .unwrap_or("image/png");
                                kept.push(json!({
                                    "type": "image",
                                    "source": { "type": "base64", "media_type": media, "data": "x" },
                                }));
                            }
                            // tool_result and anything else carry no user text.
                            _ => {}
                        }
                    }
                    Value::Array(kept)
                }
                other => other,
            };
            msg.insert("content".to_string(), pruned);
        }
    }
    v.to_string()
}

/// Scan a whole log for the user's own prompts, WITHOUT sending the log to the
/// frontend. Backs the turn navigator, which must reflect every prompt in the
/// session — the run viewer only loads a tail window, so deriving the list from
/// what's on screen under-reports (a single turn spans many tool records).
///
/// Returns the pruned `type: "user"` records in their original shape so the
/// existing `parseStreamLogWindow` logic runs on them unchanged (compaction
/// summaries, slash-command messages and tool_result-only echoes are filtered
/// there, exactly as in the main view) — no risk of a Rust reimplementation
/// drifting from the TypeScript.
#[tauri::command]
pub async fn get_run_user_records(
    db: State<'_, Db>,
    run_id: String,
) -> Result<RunUserRecords, String> {
    let row = fetch_run_log_row(db.inner(), &run_id).await?;
    // Stat + whole-file scan are blocking and the log can be tens of MB — keep
    // them off the async workers. RunView fires this on every run switch, so an
    // unchanged log is served from `USER_SCAN_CACHE` instead of re-parsed.
    tokio::task::spawn_blocking(move || {
        let revision = build_revision(&row, &run_id);
        let Some(key) = ScanKey::of(&revision) else {
            return Ok(RunUserRecords {
                records: Vec::new(),
                revision,
            });
        };
        let records = cached_scan(&USER_SCAN_CACHE, key, scan_user_records)?;
        Ok(RunUserRecords {
            records: (*records).clone(),
            revision,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

/// The whole-file half of `get_run_user_records`.
fn scan_user_records(path: &Path) -> Result<Vec<UserRecord>, String> {
    use serde_json::Value;
    use std::io::{BufRead, BufReader};

    let file = fs::File::open(path).map_err(|e| format!("open log: {}", e))?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut records: Vec<UserRecord> = Vec::new();

    // Track the byte offset of each line start — `read_until` keeps the raw bytes
    // (newline included) so the running total matches what the forward reader
    // seeks to.
    let mut offset: u64 = 0;
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        let n = reader
            .read_until(b'\n', &mut raw)
            .map_err(|e| format!("read log: {}", e))?;
        if n == 0 {
            break;
        }
        let start = offset;
        offset += n as u64;

        let line = String::from_utf8_lossy(&raw[..n]);
        let line = line.trim_end_matches('\n').trim_end_matches('\r');
        // Cheap substring reject before paying for a JSON parse.
        if !line.contains("\"user\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("user") {
            continue;
        }
        records.push(UserRecord {
            offset: start,
            json: prune_user_record(v),
        });
    }

    Ok(records)
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
        model: None,
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
        protected: false,
        role: None,
        conductor_run_id: None,
        worktree_branch: None,
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
        // The handed-off run keeps working in the same checkout (worktree or root).
        "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, input_path, output_path, created_at, title, worktree_path)
         VALUES (?, ?, ?, ?, ?, 'fetched', ?, ?, ?, ?, ?, (SELECT worktree_path FROM runs WHERE id = ?))",
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
    .bind(&run_id)
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
            model: None,
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
            protected: false,
            role: None,
            conductor_run_id: None,
            worktree_branch: None,
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
    create_session_run_inner(db.inner(), project_id, engine_override).await
}

/// `create_session_run` for callers holding a plain pool (Focus "Start with AI").
pub async fn create_session_run_inner(
    db: &Db,
    project_id: String,
    engine_override: Option<String>,
) -> Result<RunRecord, String> {
    let default_engine = crate::commands::settings::resolve_default_engine(db).await;
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
    .execute(db)
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
        model: None,
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
        protected: false,
        role: None,
        conductor_run_id: None,
        worktree_branch: None,
    })
}

/// Delete a run and, for a session, every worker it spawned (runs whose
/// `conductor_run_id` is this run) — each exactly like a normal run delete (DB
/// row, log, transcript, links). All-or-nothing up front: refuses, deleting
/// nothing, while the run or any of its workers is still active. When
/// `conductor` is given, the deleted runs' in-memory sessions and bearer tokens
/// are revoked too.
pub(crate) async fn delete_run_tree(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    conductor: Option<&crate::conductor::ConductorState>,
    run_id: &str,
) -> Result<DeleteOutcome, String> {
    let worker_ids: Vec<String> =
        sqlx::query_scalar("SELECT id FROM runs WHERE conductor_run_id = ? AND id != ?")
            .bind(run_id)
            .bind(run_id)
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;

    // All-or-nothing: validate the session AND every worker (live handle or a
    // DB status still 'running', e.g. left stuck by a crash) before deleting
    // anything, so a refusal never leaves a half-deleted tree.
    let parent_status: Option<String> = sqlx::query_scalar("SELECT status FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let parent_status = parent_status.ok_or_else(|| "run not found".to_string())?;
    let (parent_live, worker_live) = {
        let reg = registry.lock().await;
        (
            reg.contains_key(run_id),
            worker_ids.iter().any(|w| reg.contains_key(w)),
        )
    };
    if parent_live {
        return Err("Run is still active — cancel it before deleting".to_string());
    }
    if parent_status == "running" {
        return Err("Run is still running — cancel it before deleting".to_string());
    }
    let workers_running: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM runs WHERE conductor_run_id = ? AND id != ? AND status = 'running'",
    )
    .bind(run_id)
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    if worker_live || workers_running > 0 {
        return Err(
            "A worker of this session is still running — cancel it before deleting".to_string(),
        );
    }

    let mut outcome = DeleteOutcome::default();
    for worker_id in &worker_ids {
        outcome.leftover_files.extend(delete_single_run(db, registry, worker_id).await?);
        if let Some(c) = conductor {
            c.unregister_session(worker_id);
        }
        outcome.deleted_ids.push(worker_id.clone());
    }
    outcome.leftover_files.extend(delete_single_run(db, registry, run_id).await?);
    if let Some(c) = conductor {
        c.unregister_session(run_id);
    }
    outcome.deleted_ids.push(run_id.to_string());
    outcome.deleted = outcome.deleted_ids.len() as u32;
    Ok(outcome)
}

/// Drop the Remote Control audit entries of deleted runs.
pub(crate) async fn prune_run_audit(db: &sqlx::SqlitePool, run_ids: &[String]) {
    for id in run_ids {
        let _ = crate::remote::audit::delete_for_run(db, id).await;
    }
}

/// Result of [`delete_run_tree`].
#[derive(Debug, Default)]
pub(crate) struct DeleteOutcome {
    /// Runs deleted (the session plus its workers).
    pub deleted: u32,
    /// Their ids.
    pub deleted_ids: Vec<String>,
    /// Engine transcript paths that could not be removed from disk.
    pub leftover_files: Vec<String>,
}

/// [`delete_run_tree`] without in-memory token revocation (tests and callers
/// without app state).
#[cfg(test)]
pub(crate) async fn delete_run_inner(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    run_id: &str,
) -> Result<(), String> {
    delete_run_tree(db, registry, None, run_id).await.map(|_| ())
}

/// Core logic for deleting a single run. Removes the DB row and its linked rows,
/// the per-run log file, every engine transcript of the session, and cached
/// input markdown when no sibling run still references it. Empty task folder is
/// removed. Usage rows are kept for statistics. Refuses to delete runs currently
/// in the registry. Returns the transcript paths that could not be removed.
async fn delete_single_run(
    db: &sqlx::SqlitePool,
    registry: &RunRegistry,
    run_id: &str,
) -> Result<Vec<String>, String> {
    use sqlx::Row;

    {
        let reg = registry.lock().await;
        if reg.contains_key(run_id) {
            return Err("Run is still active — cancel it before deleting".to_string());
        }
    }

    let row = sqlx::query(
        "SELECT r.status, r.input_path, r.project_id, r.session_id, r.engine, r.transcript_path, p.path as project_path,
                COALESCE(r.worktree_path, p.path) as run_cwd
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
    let run_cwd: String = row.get("run_cwd");
    let project_id: String = row.get("project_id");
    let session_id: Option<String> = row.get("session_id");
    let engine: String = row.get("engine");
    let transcript_path: Option<String> = row.get("transcript_path");

    sqlx::query("DELETE FROM runs WHERE id = ?")
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;

    // Every top-level run owns a conductor session row (events cascade). Drop it
    // so a deleted run is not re-registered on every startup.
    let _ = sqlx::query("DELETE FROM conductor_sessions WHERE id = ?")
        .bind(run_id)
        .execute(db)
        .await;

    // Slack threads linked to this session (no FK to runs in this DB).
    let _ = crate::slack_threads::run_links::delete_for_run(db, run_id).await;
    let _ = crate::item_links::delete_for_run(db, run_id).await;

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
    crate::runs::model_usage::clear_baseline(db, run_id).await;

    let runs_dir = Path::new(&project_path).join(".devdy").join("runs");
    let _ = fs::remove_file(runs_dir.join(format!("{}.log", run_id)));
    let _ = fs::remove_dir_all(runs_dir.join(run_id));

    // Also delete every engine transcript of the session — in every Claude
    // account store (a switched account holds its own copy) plus subagent data —
    // so the session can't be re-mirrored back into history and disappears from
    // the CLI/VS Code resume list too. The tombstone above still guards the
    // window in case a file can't be removed; those are reported to the caller.
    let mut leftover = Vec::new();
    if let Some(sid) = session_id.as_deref() {
        let claude_dirs = if engine == "claude" {
            crate::commands::sessions::claude_sessions_dirs(db, &run_cwd).await
        } else {
            Vec::new()
        };
        let codex_root = crate::commands::codex_sessions::codex_sessions_root();
        let targets = engine_transcript_targets(
            &engine,
            sid,
            transcript_path.as_deref(),
            &claude_dirs,
            codex_root.as_deref(),
        );
        leftover = remove_paths(&targets);
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

    Ok(leftover)
}

/// Session ids come from the CLI but are joined into paths that are removed
/// recursively, so anything other than a plain id is refused.
fn is_plain_session_id(sid: &str) -> bool {
    !sid.is_empty()
        && sid
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Everything on disk holding a session's engine transcript. Claude: the cached
/// `transcript_path`, plus `<sid>.jsonl` and the `<sid>/` folder (subagent
/// transcripts, tool results) in every account store in `claude_dirs`. Codex:
/// the cached path plus the thread's rollout and its subagent/fork rollouts.
fn engine_transcript_targets(
    engine: &str,
    session_id: &str,
    transcript_path: Option<&str>,
    claude_dirs: &[PathBuf],
    codex_root: Option<&Path>,
) -> Vec<PathBuf> {
    let mut targets: Vec<PathBuf> = transcript_path.map(PathBuf::from).into_iter().collect();
    if !is_plain_session_id(session_id) {
        return targets;
    }
    match engine {
        "claude" => {
            for dir in claude_dirs {
                targets.push(dir.join(format!("{session_id}.jsonl")));
                targets.push(dir.join(session_id));
            }
        }
        "codex" => {
            if let Some(root) = codex_root {
                targets.extend(crate::commands::codex_sessions::codex_thread_files(
                    root, session_id,
                ));
            }
        }
        _ => {}
    }
    targets.sort();
    targets.dedup();
    targets
}

/// Remove each path (file or directory tree) that exists, returning the ones
/// that could not be removed.
fn remove_paths(paths: &[PathBuf]) -> Vec<String> {
    let mut failed = Vec::new();
    for path in paths {
        let Ok(meta) = fs::symlink_metadata(path) else {
            continue;
        };
        let res = if meta.is_dir() {
            fs::remove_dir_all(path)
        } else {
            fs::remove_file(path)
        };
        if res.is_err() {
            failed.push(path.to_string_lossy().into_owned());
        }
    }
    failed
}

/// Delete a run together with everything belonging to it (see
/// [`delete_single_run`]). Running runs are refused — cancel first. Returns the
/// transcript paths that could not be removed, so the UI can warn about them.
#[tauri::command]
pub async fn delete_run(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    run_id: String,
) -> Result<Vec<String>, String> {
    let conductor = app.try_state::<crate::conductor::ConductorState>();
    let outcome = delete_run_tree(
        db.inner(),
        registry.inner(),
        conductor.as_ref().map(|c| c.inner()),
        &run_id,
    )
    .await?;
    prune_run_audit(db.inner(), &outcome.deleted_ids).await;
    crate::slack_threads::emit_all_changed(&app);
    crate::item_links::emit_changed(&app, None);
    crate::remote::live_lists::emit_runs_changed(&app);
    Ok(outcome.leftover_files)
}

/// Rename a run's title (shown in the History list). An empty/whitespace title
/// clears it back to NULL, letting the UI fall back to its derived label.
#[tauri::command]
pub async fn rename_run(
    app: AppHandle,
    db: State<'_, Db>,
    run_id: String,
    title: String,
) -> Result<(), String> {
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
    crate::remote::live_lists::emit_runs_changed(&app);
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
    // The target profile may already hold an older copy (the run used this
    // account before and continued elsewhere since). The run's recorded
    // transcript is the live one, so refresh the copy when it is newer —
    // resuming the stale copy would silently drop the later turns.
    if dest.is_file() {
        if let Some(live) = transcript_path.map(PathBuf::from).filter(|p| p.is_file()) {
            if live != dest && is_newer_file(&live, &dest) {
                fs::copy(&live, &dest).map_err(|e| {
                    format!(
                        "Failed to refresh Claude transcript from {} to {}: {e}",
                        live.display(),
                        dest.display()
                    )
                })?;
            }
        }
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

/// `a` was modified strictly after `b` (false when either mtime is unreadable).
fn is_newer_file(a: &Path, b: &Path) -> bool {
    let mtime = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok();
    match (mtime(a), mtime(b)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The Claude account a run actually executes under. A run-level pick wins;
/// with none (`claude_account_id = NULL`, shown as "Default" in the UI) the run
/// follows project → default account, exactly like a fresh start — only when no
/// managed account exists at all does it fall back to the global `~/.claude`.
async fn effective_run_claude_account(
    db: &Db,
    project_id: &str,
    run_account_id: Option<&str>,
) -> Result<Option<crate::commands::claude_accounts::ClaudeRuntimeAccount>, String> {
    let explicit = run_account_id.map(str::trim).filter(|v| !v.is_empty());
    crate::commands::claude_accounts::resolve_runtime_account(db, Some(project_id), explicit)
        .await
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
        "SELECT r.engine, r.status, r.session_id, r.transcript_path, r.project_id,
                COALESCE(r.worktree_path, p.path) as run_cwd
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

    // `None` = the "Default" tier: mirror into the profile the next resume will
    // really use (project → default account), not the global `~/.claude`.
    let project_id: String = row.get("project_id");
    let account = effective_run_claude_account(db, &project_id, account_id.as_deref()).await?;

    let session_id: Option<String> = row.get("session_id");
    let transcript_path: Option<String> = row.get("transcript_path");
    let run_cwd: String = row.get("run_cwd");
    let mirrored = if let Some(sid) = session_id.as_deref() {
        mirror_claude_transcript_for_account(
            db,
            &run_cwd,
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
pub async fn set_run_pinned(
    app: AppHandle,
    db: State<'_, Db>,
    run_id: String,
    pinned: bool,
) -> Result<(), String> {
    sqlx::query("UPDATE runs SET pinned = ? WHERE id = ?")
        .bind(if pinned { 1 } else { 0 })
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    // The phone's run browser follows the desktop's pinned-first order.
    crate::remote::live_lists::emit_runs_changed(&app);
    Ok(())
}

/// Protect or unprotect a run so "Clear all" skips it during a bulk cleanup.
#[tauri::command]
pub async fn set_run_protected(
    db: State<'_, Db>,
    run_id: String,
    protected: bool,
) -> Result<(), String> {
    sqlx::query("UPDATE runs SET protected = ? WHERE id = ?")
        .bind(if protected { 1 } else { 0 })
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Bulk delete all non-running, non-protected runs for a project. Returns the
/// number deleted. Protected runs are kept so an important session survives, and
/// protection cascades: a worker whose conductor is protected is kept too (users
/// can still delete such a worker individually via `delete_run`).
#[tauri::command]
pub async fn delete_all_runs(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    project_id: String,
) -> Result<u32, String> {
    use sqlx::Row;

    let rows = sqlx::query(
        "SELECT id FROM runs r \
         WHERE r.project_id = ? AND r.status != 'running' AND r.protected = 0 \
         AND NOT EXISTS ( \
             SELECT 1 FROM runs c WHERE c.id = r.conductor_run_id AND c.protected = 1 \
         )",
    )
        .bind(&project_id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let conductor = app.try_state::<crate::conductor::ConductorState>();
    let mut deleted = 0u32;
    for row in &rows {
        let id: String = row.get("id");
        // A worker already removed with its session simply fails to load here.
        if let Ok(outcome) = delete_run_tree(
            db.inner(),
            registry.inner(),
            conductor.as_ref().map(|c| c.inner()),
            &id,
        )
        .await
        {
            deleted += outcome.deleted;
            prune_run_audit(db.inner(), &outcome.deleted_ids).await;
            for path in outcome.leftover_files {
                eprintln!("[delete_all_runs] could not remove transcript {path}");
            }
        }
    }
    if deleted > 0 {
        crate::slack_threads::emit_all_changed(&app);
        crate::item_links::emit_changed(&app, None);
        crate::remote::live_lists::emit_runs_changed(&app);
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
    resume_run_with(
        app,
        db,
        registry,
        run_id,
        permission_mode_override,
        model_override,
        override_budget,
        true,
    )
    .await
}

/// [`resume_run`] with control over whether the resume counts as user activity.
/// The Conductor passes `stamp_activity = false` when it resumes a worker or
/// wakes itself, so agent-driven turns don't reorder History.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn resume_run_with(
    app: AppHandle,
    db: State<'_, Db>,
    registry: State<'_, RunRegistry>,
    run_id: String,
    permission_mode_override: Option<String>,
    model_override: Option<String>,
    override_budget: Option<bool>,
    stamp_activity: bool,
) -> Result<(), String> {
    use sqlx::Row;

    let row = sqlx::query(
        "SELECT r.engine, r.status, r.session_id, r.transcript_path, r.project_id, r.claude_account_id, p.path as project_path,
                COALESCE(r.worktree_path, p.path) as run_cwd
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
    let transcript_path: Option<String> = row.get("transcript_path");
    let project_id: String = row.get("project_id");
    let claude_account_id: Option<String> = row.get("claude_account_id");
    let project_path: String = row.get("project_path");
    let run_cwd: String = row.get("run_cwd");
    require_worktree(&project_path, &run_cwd)?;

    // Resume uses the run-level account, else (NULL = "Default" in the UI) the
    // project → default account — the same profile a fresh start would pick and
    // the one the UI names. Only with no managed account at all does it stay on
    // the global `~/.claude`. The transcript is mirrored into that profile below,
    // so switching profiles keeps the conversation. Errors if the assigned
    // account was deleted.
    let claude_account = if engine == "claude" {
        effective_run_claude_account(db.inner(), &project_id, claude_account_id.as_deref())
            .await?
    } else {
        None
    };

    // Same budget guardrail as start_run — resuming a finished run starts a new
    // turn and consumes tokens, so it must be gated too. Checked against the
    // account the resume will actually run under.
    crate::commands::stats::enforce_budget(
        db.inner(),
        &engine,
        claude_account.as_ref().map(|a| a.id.as_str()),
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

    // A resumed top-level run is (or becomes) a conductor: re-inject its
    // `conductor` MCP server + framing on BOTH engines — neither the Claude SDK
    // nor Codex keeps an appended system prompt across a resume. Workers get None.
    let cst = app.try_state::<crate::conductor::ConductorState>();
    let crate::conductor::commands::ResumeFraming {
        launch: conductor_launch,
        framing,
        seeded: framing_seeded,
    } = crate::conductor::commands::resume_framing(db.inner(), cst.as_ref().map(|c| c.inner()), &run_id)
        .await?;

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
    let model = resolve_model_for_engine(&engine, model_override, engine_default_model);

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

    // The CLI transcript behind `session_id` can be gone (e.g. the Stats screen's
    // storage cleanup wiped ~/.claude/projects or ~/.codex/sessions). Resuming a
    // missing session hard-fails in the CLI ("No conversation found"), so start a
    // fresh session in the same run instead; the earlier context is lost.
    let (transcript_available, mirrored_transcript) = if engine == "codex" {
        (crate::commands::codex_sessions::codex_session_file(&session_id).is_some(), None)
    } else {
        let mirrored = mirror_claude_transcript_for_account(
            db.inner(),
            &run_cwd,
            &session_id,
            transcript_path.as_deref(),
            claude_account.as_ref().map(|a| a.config_dir.as_str()),
        )
        .await?;
        (mirrored.is_some(), mirrored)
    };
    // The resumed CLI appends to the copy inside the resolved profile; point the
    // run at it so transcript readers follow the live file, not the old one.
    if let Some(path) = mirrored_transcript
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .filter(|p| transcript_path.as_deref() != Some(p.as_str()))
    {
        let _ = sqlx::query(
            "UPDATE runs SET transcript_path = ?, transcript_synced_size = NULL WHERE id = ?",
        )
        .bind(&path)
        .bind(&run_id)
        .execute(db.inner())
        .await;
    }

    // Mark running; finished_at cleared so the UI reflects the active state.
    // Resuming is the clearest signal of "I'm working on this one again", so it
    // also stamps last_activity_at and floats the session to the top of History.
    // A fresh fallback session also drops the stale transcript pointer.
    // An agent-driven resume (stamp_activity = false) instead pins
    // last_activity_at to the run's current sort key — the old row values, since
    // SQLite evaluates SET expressions against them — so overwriting started_at /
    // finished_at here can't move it either.
    let started_at = chrono::Utc::now().to_rfc3339();
    let activity = if stamp_activity {
        "?"
    } else {
        "COALESCE(last_activity_at, finished_at, started_at, created_at, ?)"
    };
    let mark_running = if transcript_available {
        format!(
            "UPDATE runs SET status = 'running', model = ?, started_at = ?, finished_at = NULL, last_activity_at = {activity} WHERE id = ?"
        )
    } else {
        format!(
            "UPDATE runs SET status = 'running', model = ?, started_at = ?, finished_at = NULL, last_activity_at = {activity}, \
             transcript_path = NULL, transcript_synced_size = NULL, framing_seeded = 0 WHERE id = ?"
        )
    };
    sqlx::query(&mark_running)
        .bind(&model)
        .bind(&started_at)
        .bind(&started_at)
        .bind(&run_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    if stamp_activity {
        crate::remote::live_lists::emit_runs_changed(&app);
    }

    let (node_bin, sidecar_script) = if engine == "codex" {
        resolve_codex_sidecar(&app, &node_path, &codex_sidecar_path)?
    } else {
        resolve_sidecar(&app, &node_path, &sidecar_path)?
    };
    let mut cmd = Command::new(&node_bin);
    cmd.current_dir(&run_cwd).arg(&sidecar_script);
    if transcript_available {
        // The sidecar resumes this session/thread on its first prompt.
        cmd.env("DEVDY_RESUME_SESSION", &session_id);
    }
    augment_command_path(&mut cmd);
    apply_mcp_tool_timeout(&mut cmd);
    // Wire the broker + shims for both Claude and Codex resumes so resumed
    // sessions get the same per-project credential treatment as fresh runs.
    let (broker, ssh_access) = wire_broker(
        &app,
        db.inner(),
        &mut cmd,
        &run_id,
        &project_id,
        &run_cwd,
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
        let (codex_mcp, _skipped) = assemble_run_mcp(
            &app,
            db.inner(),
            "codex",
            &node_bin,
            &project_id,
            &run_cwd,
            &run_id,
            conductor_launch.is_some(),
        )
        .await?;
        if !codex_mcp.is_null() {
            cmd.env("DEVDY_CODEX_MCP", codex_mcp.to_string());
        }
        // Codex ignores developerInstructions on thread/resume (verified against a
        // real Codex), so a thread never started with this framing (e.g. a legacy
        // plain session) is seeded ONCE by prepending it to the next turn's prompt.
        // The sidecar reports `_devdy_framing_seeded`, which sets
        // runs.framing_seeded so later resumes don't repeat it.
        if !transcript_available {
            // Fresh thread: the framing goes in as developer instructions, as on start.
            if let Some(framing) = &framing {
                cmd.env("DEVDY_CODEX_DEVELOPER_INSTRUCTIONS", framing);
            }
        } else if let Some(seed) =
            crate::conductor::codex_resume_seed(framing.as_deref(), framing_seeded)
        {
            cmd.env("DEVDY_CODEX_SEED_FRAMING", seed);
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
        cmd.env("DEVDY_PARTIAL_MESSAGES", "1");
        if crate::commands::mcp::strict_mcp_config_enabled(db.inner()).await {
            cmd.env("DEVDY_STRICT_MCP", "1");
        }
        if let Some(m) = &model {
            cmd.env("DEVDY_MODEL", m);
        }
        // Re-inject MCP on resume (see the Codex branch above). The sidecar reads
        // DEVDY_MCP_SERVERS as a fallback when the first prompt carries no options.
        // A resumed conductor keeps its `session_*` control tools: its session
        // (restored on startup for pre-restart sessions) is still registered.
        let (mcp, _skipped) = assemble_run_mcp(
            &app,
            db.inner(),
            "claude",
            &node_bin,
            &project_id,
            &run_cwd,
            &run_id,
            conductor_launch.is_some(),
        )
        .await?;
        if !mcp.is_null() {
            cmd.env("DEVDY_MCP_SERVERS", mcp.to_string());
        }
        // Sidecar fallbacks for a resumed first prompt that carries no options:
        // the Claude SDK does not persist an appended system prompt, so the
        // framing (session framing and/or a worker's instruction) is re-sent on
        // every resume.
        if let Some(framing) = &framing {
            cmd.env("DEVDY_APPEND_SYSTEM_PROMPT", framing);
        }
        if conductor_launch.is_some() && sdk_permission_mode(&permission_mode) != "bypassPermissions" {
            cmd.env(
                "DEVDY_ALLOWED_TOOLS",
                serde_json::json!([crate::conductor::CONDUCTOR_TOOLS_ALLOW]).to_string(),
            );
        }
        // Reuse the original run's Claude account profile (clears inherited auth
        // env overrides). No-op for legacy runs → keeps the global profile.
        apply_claude_config_dir(&mut cmd, claude_account.as_ref().map(|a| a.config_dir.as_str()));
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    detach_sidecar_process_group(&mut cmd);
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn sidecar ({}): {}", node_bin, e))?;

    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    // The first prompt of a resume arrives later via send_user_message (through
    // the writer); nothing writes the pipe here.
    let stdin_handle = child.stdin.take();

    let session_id_state = Arc::new(TokioMutex::new(Some(session_id.clone())));
    let log_buf = Arc::new(TokioMutex::new(String::new()));

    // Seed the buffer with a resume marker so the persisted log notes the resume
    // boundary between the original conversation and the new turns. The actual
    // follow-up text arrives via send_user_message, which kicks off the turn.
    {
        let mut buf = log_buf.lock().await;
        if transcript_available {
            buf.push_str(&format!(
                "[stderr] --- resumed session {} at {} ---\n",
                session_id, started_at
            ));
        } else {
            buf.push_str(&format!(
                "[stderr] --- transcript for session {} no longer exists; started a new session at {} (earlier context is not available to the agent) ---\n",
                session_id, started_at
            ));
        }
    }

    {
        let mut reg = registry.lock().await;
        reg.insert(
            run_id.clone(),
            RunHandles {
                proc: crate::procs::track(
                    crate::procs::ProcSpec::new(crate::procs::ProcKind::Run, "Session")
                        .run(&run_id, &project_id),
                    child.id(),
                ),
                pgid: child.id(),
                child,
                stdin: stdin_handle
                    .map(|w| SidecarInput::spawn(w, input_stall_hook(&app, &run_id))),
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
        run_cwd,
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
mod transcript_purge_tests {
    use super::{engine_transcript_targets, remove_paths};
    use std::fs;
    use std::path::PathBuf;

    #[test]
    fn claude_purge_covers_every_account_and_subagent_folder() {
        let root = std::env::temp_dir().join(format!("devdy_purge_{}", std::process::id()));
        let global = root.join("global").join("projects").join("-proj");
        let account = root.join("acct").join("projects").join("-proj");
        for dir in [&global, &account] {
            fs::create_dir_all(dir.join("sid-1").join("subagents")).unwrap();
            fs::write(dir.join("sid-1.jsonl"), "{}").unwrap();
            fs::write(dir.join("sid-1").join("subagents").join("a.jsonl"), "{}").unwrap();
            fs::write(dir.join("other.jsonl"), "{}").unwrap();
        }

        let targets = engine_transcript_targets(
            "claude",
            "sid-1",
            None,
            &[global.clone(), account.clone()],
            None,
        );
        assert!(remove_paths(&targets).is_empty());
        for dir in [&global, &account] {
            assert!(!dir.join("sid-1.jsonl").exists());
            assert!(!dir.join("sid-1").exists());
            // Another session in the same store is untouched.
            assert!(dir.join("other.jsonl").exists());
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn unsafe_session_id_only_targets_cached_path() {
        let dirs = [PathBuf::from("/tmp/x")];
        let targets =
            engine_transcript_targets("claude", "../..", Some("/tmp/x/t.jsonl"), &dirs, None);
        assert_eq!(targets, vec![PathBuf::from("/tmp/x/t.jsonl")]);
        assert!(engine_transcript_targets("claude", "", None, &dirs, None).is_empty());
    }
}

#[cfg(test)]
mod model_resolution_tests {
    use super::{engine_of_model, resolve_model_for_engine};

    #[test]
    fn classifies_known_families() {
        assert_eq!(engine_of_model("claude-opus-4-8[1m]"), Some("claude"));
        assert_eq!(engine_of_model("claude-sonnet-4-6"), Some("claude"));
        assert_eq!(engine_of_model("gpt-5.5"), Some("codex"));
        assert_eq!(engine_of_model("gpt-5.3-codex"), Some("codex"));
        // Unrecognized ids are left unclassified so they are never dropped.
        assert_eq!(engine_of_model("o3-mini"), None);
        assert_eq!(engine_of_model(""), None);
    }

    #[test]
    fn override_of_same_engine_wins() {
        let m = resolve_model_for_engine(
            "claude",
            Some("claude-opus-4-8[1m]".into()),
            "claude-sonnet-4-6".into(),
        );
        assert_eq!(m.as_deref(), Some("claude-opus-4-8[1m]"));
    }

    #[test]
    fn cross_engine_override_falls_back_to_global() {
        // A conductor hands a Claude model to a Codex worker: drop it, use the
        // Codex global default instead (requirement #3).
        let m = resolve_model_for_engine(
            "codex",
            Some("claude-opus-4-8[1m]".into()),
            "gpt-5.5".into(),
        );
        assert_eq!(m.as_deref(), Some("gpt-5.5"));
    }

    #[test]
    fn unknown_override_is_trusted() {
        let m = resolve_model_for_engine("claude", Some("o3-custom".into()), "claude-x".into());
        assert_eq!(m.as_deref(), Some("o3-custom"));
    }

    #[test]
    fn no_override_uses_global() {
        let m = resolve_model_for_engine("claude", None, "claude-opus-4-8[1m]".into());
        assert_eq!(m.as_deref(), Some("claude-opus-4-8[1m]"));
    }

    #[test]
    fn empty_override_and_empty_global_is_none() {
        assert_eq!(resolve_model_for_engine("claude", Some("  ".into()), "".into()), None);
        assert_eq!(resolve_model_for_engine("codex", None, "   ".into()), None);
    }

    #[test]
    fn cross_engine_override_with_empty_global_is_none() {
        // Dropped override + no global default => let the engine pick its own.
        let m = resolve_model_for_engine("codex", Some("claude-opus-4-8[1m]".into()), "".into());
        assert_eq!(m, None);
    }
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
    use serde_json::json;

    /// The real reducer `get_run_tool_records` runs per line.
    fn reduce(line: &str) -> Option<String> {
        reduce_tool_record(line)
    }

    fn key(path: &str, size: u64, mtime_ns: i64) -> ScanKey {
        ScanKey {
            path: path.to_string(),
            size,
            mtime_ns,
        }
    }

    #[test]
    fn scan_cache_hits_only_on_identical_stat() {
        let mut c: ScanCache<u32> = ScanCache::new(4);
        c.insert(key("/a.log", 10, 1), Arc::new(1));
        assert_eq!(c.get(&key("/a.log", 10, 1)).as_deref(), Some(&1));
        // Grown or touched file → miss, and the stale entry is dropped.
        assert!(c.get(&key("/a.log", 11, 1)).is_none());
        assert!(c.get(&key("/a.log", 10, 1)).is_none());
        c.insert(key("/a.log", 10, 1), Arc::new(1));
        assert!(c.get(&key("/a.log", 10, 2)).is_none());
        assert!(c.get(&key("/b.log", 10, 1)).is_none());
    }

    #[test]
    fn scan_cache_replaces_same_path_instead_of_duplicating() {
        let mut c: ScanCache<u32> = ScanCache::new(4);
        c.insert(key("/a.log", 10, 1), Arc::new(1));
        c.insert(key("/a.log", 20, 2), Arc::new(2));
        assert_eq!(c.entries.len(), 1);
        assert_eq!(c.get(&key("/a.log", 20, 2)).as_deref(), Some(&2));
    }

    #[test]
    fn scan_cache_evicts_least_recently_used() {
        let mut c: ScanCache<u32> = ScanCache::new(2);
        c.insert(key("/a.log", 1, 1), Arc::new(1));
        c.insert(key("/b.log", 1, 1), Arc::new(2));
        // Touch /a so /b becomes the LRU entry.
        assert!(c.get(&key("/a.log", 1, 1)).is_some());
        c.insert(key("/c.log", 1, 1), Arc::new(3));
        assert_eq!(c.entries.len(), 2);
        assert!(c.get(&key("/b.log", 1, 1)).is_none());
        assert!(c.get(&key("/a.log", 1, 1)).is_some());
        assert!(c.get(&key("/c.log", 1, 1)).is_some());
    }

    #[test]
    fn cached_scan_runs_the_scan_once_per_stat() {
        let cache: std::sync::Mutex<ScanCache<u32>> = std::sync::Mutex::new(ScanCache::new(4));
        let calls = std::cell::Cell::new(0);
        let scan = |_: &Path| {
            calls.set(calls.get() + 1);
            Ok(7)
        };
        assert_eq!(*cached_scan(&cache, key("/x.log", 5, 5), scan).unwrap(), 7);
        assert_eq!(*cached_scan(&cache, key("/x.log", 5, 5), scan).unwrap(), 7);
        assert_eq!(calls.get(), 1);
        cached_scan(&cache, key("/x.log", 6, 5), scan).unwrap();
        assert_eq!(calls.get(), 2);
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

#[cfg(test)]
mod startup_input_tests {
    use super::*;
    use std::time::{Duration, Instant};

    // Startup path: the sidecar doesn't read its stdin (stuck at startup) and the
    // first prompt is far larger than the pipe buffer (e.g. base64 images). Start
    // must still return at once with the run registered — and Stop must work.
    #[tokio::test]
    async fn large_first_prompt_to_non_reading_sidecar_never_blocks_start() {
        let mut cmd = Command::new("sleep");
        cmd.arg("30").stdin(std::process::Stdio::piped());
        crate::runs::sidecar::detach_process_group(&mut cmd);
        let mut child = cmd.spawn().unwrap();
        let pgid = child.id().unwrap();
        let input = child
            .stdin
            .take()
            .map(|w| SidecarInput::spawn(w, Box::new(|_| {})));
        let first_line = Some(format!(
            "{{\"type\":\"prompt\",\"text\":\"{}\"}}\n",
            "x".repeat(8 * 1024 * 1024)
        ));

        // Same sequence as start_run_inner: register, then queue the first turn.
        let registry = crate::runs::new_registry();
        let started = Instant::now();
        registry.lock().await.insert(
            "r1".to_string(),
            RunHandles {
                proc: None,
                pgid: Some(pgid),
                child,
                stdin: input.clone(),
                session_id: Arc::new(TokioMutex::new(None)),
                log_buf: Arc::new(TokioMutex::new(String::new())),
                broker_run: None,
                ssh_access: None,
            },
        );
        queue_first_prompt(input.as_ref(), first_line);
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "start must not wait on the pipe"
        );
        assert!(registry.lock().await.contains_key("r1"));

        // Stop works while the first prompt is still stuck in the pipe.
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE runs (id TEXT PRIMARY KEY, status TEXT, finished_at TEXT, last_activity_at TEXT)")
            .execute(&db)
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            cancel_run_inner(&registry, &db, "r1", true),
        )
        .await
        .expect("cancel must not block on the stuck write")
        .unwrap();
        assert!(!registry.lock().await.contains_key("r1"));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !crate::runs::sidecar::kill_process_group(pgid),
            "the sidecar group was killed"
        );
    }
}

#[cfg(test)]
mod claude_account_resume_tests {
    use super::*;

    async fn db_with_accounts(default_dir: &str) -> Db {
        let db = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query("CREATE TABLE projects (id TEXT PRIMARY KEY, claude_account_id TEXT)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE claude_accounts (id TEXT PRIMARY KEY, label TEXT, config_dir TEXT, is_default INTEGER, created_at TEXT)",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("INSERT INTO projects (id, claude_account_id) VALUES ('p1', NULL)")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO claude_accounts VALUES ('acc-default', 'Claude 1', ?, 1, '2026-01-01')",
        )
        .bind(default_dir)
        .execute(&db)
        .await
        .unwrap();
        db
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("devdy-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[tokio::test]
    async fn default_tier_run_resumes_under_default_account_not_global() {
        let dir = temp_dir("claude-default");
        let db = db_with_accounts(dir.to_str().unwrap()).await;
        // NULL run account ("Default" in the UI) must resolve to the default
        // profile, so the sidecar gets CLAUDE_CONFIG_DIR instead of ~/.claude.
        let acct = effective_run_claude_account(&db, "p1", None).await.unwrap();
        assert_eq!(acct.map(|a| a.id).as_deref(), Some("acc-default"));
        let acct = effective_run_claude_account(&db, "p1", Some("  ")).await.unwrap();
        assert_eq!(acct.map(|a| a.id).as_deref(), Some("acc-default"));
        let _ = fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn mirror_refreshes_a_stale_copy_from_the_live_transcript() {
        let dir = temp_dir("claude-mirror");
        let db = db_with_accounts(dir.to_str().unwrap()).await;
        let project_path = "/tmp/devdy-mirror-project";
        let dest = claude_transcript_path_for_account(dir.to_str(), project_path, "s1").unwrap();
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(&dest, "old\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let live = dir.join("live.jsonl");
        fs::write(&live, "old\nnew turn\n").unwrap();

        let got = mirror_claude_transcript_for_account(
            &db,
            project_path,
            "s1",
            live.to_str(),
            dir.to_str(),
        )
        .await
        .unwrap();
        assert_eq!(got.as_deref(), Some(dest.as_path()));
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old\nnew turn\n");

        // A copy newer than the recorded transcript is never overwritten.
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&dest, "old\nnew turn\nnewest\n").unwrap();
        mirror_claude_transcript_for_account(&db, project_path, "s1", live.to_str(), dir.to_str())
            .await
            .unwrap();
        assert_eq!(fs::read_to_string(&dest).unwrap(), "old\nnew turn\nnewest\n");
        let _ = fs::remove_dir_all(dir);
    }
}
