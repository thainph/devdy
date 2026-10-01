//! Loopback Streamable-HTTP MCP server exposing the `session_*` control tools to
//! conductor runs. Hand-rolled JSON-RPC 2.0 over a single `POST /mcp` endpoint,
//! served by axum on 127.0.0.1 only. Every request must carry the conductor's
//! bearer token; tools are scoped to workers that conductor spawned.

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::{tools, ConductorState};

const PROTOCOL_VERSION: &str = "2025-06-18";

#[derive(Clone)]
pub struct HttpState {
    pub app: AppHandle,
    pub conductor: ConductorState,
}

/// Start the loopback MCP server on an ephemeral port and record it in
/// `ConductorState`. Runs for the app's lifetime.
pub async fn start(app: AppHandle, conductor: ConductorState) -> std::io::Result<()> {
    let state = HttpState {
        app,
        conductor: conductor.clone(),
    };
    let router = Router::new()
        .route("/mcp", post(handle_post).get(handle_get).delete(handle_delete))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    conductor.set_port(port);
    tracing::info!(event = "conductor_mcp_listening", port = port);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(event = "conductor_mcp_serve_failed", error = %e);
        }
    });
    Ok(())
}

/// Streamable HTTP: the client opens a GET for a server->client SSE stream. We
/// don't push server-initiated messages, so decline it; the SDK tolerates 405.
async fn handle_get() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

/// Session teardown (DELETE) — we keep no HTTP session state, so just ack.
async fn handle_delete() -> Response {
    StatusCode::OK.into_response()
}

async fn handle_post(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    // ---- auth: bearer token -> conductor run id -----------------------------
    let token = bearer(&headers);
    let conductor_run_id = match token.and_then(|t| state.conductor.resolve_token(&t)) {
        Some(id) => id,
        None => {
            return (
                StatusCode::UNAUTHORIZED,
                Json(json!({ "error": "invalid or missing conductor token" })),
            )
                .into_response()
        }
    };

    // A JSON-RPC notification (no `id`) gets no response body.
    let id = body.get("id").cloned();
    let method = body.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = body.get("params").cloned().unwrap_or(Value::Null);

    if id.is_none() {
        // notifications/initialized, notifications/cancelled, etc.
        return StatusCode::ACCEPTED.into_response();
    }
    let id = id.unwrap();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "conductor", "version": "0.1.0" },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_specs() })),
        "tools/call" => {
            handle_tools_call(&state, &conductor_run_id, &params).await
        }
        other => Err(rpc_error(-32601, &format!("method not found: {}", other))),
    };

    let payload = match result {
        Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
        Err(err) => json!({ "jsonrpc": "2.0", "id": id, "error": err }),
    };
    (StatusCode::OK, Json(payload)).into_response()
}

/// Dispatch a `tools/call` to the matching handler, wrapping the structured
/// result in MCP `content` blocks. Tool-level failures come back as
/// `{ isError: true }` (per MCP) rather than a JSON-RPC error, so the model can
/// read and react to them.
async fn handle_tools_call(
    state: &HttpState,
    conductor_run_id: &str,
    params: &Value,
) -> Result<Value, Value> {
    let name = params
        .get("name")
        .and_then(|n| n.as_str())
        .ok_or_else(|| rpc_error(-32602, "missing tool name"))?;
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    let outcome = tools::dispatch(state, conductor_run_id, name, args).await;
    match outcome {
        Ok(value) => Ok(json!({
            "content": [{ "type": "text", "text": value.to_string() }],
            "isError": false,
        })),
        Err(msg) => Ok(json!({
            "content": [{ "type": "text", "text": json!({ "error": msg }).to_string() }],
            "isError": true,
        })),
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(|s| s.trim().to_string())
}

fn rpc_error(code: i64, message: &str) -> Value {
    json!({ "code": code, "message": message })
}

/// Static tool schema advertised via `tools/list`.
fn tool_specs() -> Value {
    json!([
        {
            "name": "session_spawn",
            "description": "Spawn a new worker session to do a piece of the work. Returns { worker_id }.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "role_label": { "type": "string", "description": "Short label for this worker's role, e.g. 'implementer'." },
                    "prompt": { "type": "string", "description": "The worker's first task message." },
                    "instruction": { "type": "string", "description": "Optional role/system framing appended to the worker's system prompt." },
                    "engine": { "type": "string", "enum": ["claude", "codex"], "description": "Engine for the worker. Defaults to project default." },
                    "model": { "type": "string", "description": "Optional model override. Must belong to the worker's engine; a mismatched id is ignored and the worker uses that engine's global default model. Omit to use the global default." },
                    "permission_mode": { "type": "string", "enum": ["default", "acceptEdits", "bypassPermissions", "plan"], "description": "Worker permission mode. Defaults to the project's default; the human answers any prompts in the permission drawer." }
                },
                "required": ["prompt"]
            }
        },
        {
            "name": "session_list",
            "description": "List all workers you have spawned and their current status.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "session_poll",
            "description": "Instant status of the given workers (or all yours if omitted).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "worker_ids": { "type": "array", "items": { "type": "string" } }
                }
            }
        },
        {
            "name": "session_wait",
            "description": "Block until the given workers finish their current turn (status != running) or timeout_ms elapses. Returns `waited_ms` = how long this call actually blocked; report that as the wait time, not timeout_ms (which is only the ceiling).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "worker_ids": { "type": "array", "items": { "type": "string" } },
                    "timeout_ms": { "type": "integer", "description": "Max wait in ms (default 120000, capped at 600000)." }
                },
                "required": ["worker_ids"]
            }
        },
        {
            "name": "session_read",
            "description": "Read a worker's latest assistant reply (its result for the last turn).",
            "inputSchema": {
                "type": "object",
                "properties": { "worker_id": { "type": "string" } },
                "required": ["worker_id"]
            }
        },
        {
            "name": "session_send",
            "description": "Give a worker a follow-up turn with new instructions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "worker_id": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["worker_id", "text"]
            }
        },
        {
            "name": "session_cancel",
            "description": "Cancel a worker's currently running turn.",
            "inputSchema": {
                "type": "object",
                "properties": { "worker_id": { "type": "string" } },
                "required": ["worker_id"]
            }
        }
    ])
}
