//! Loopback HTTP server for the Devdy inbox (axum, 127.0.0.1 only): Slack
//! thread links and translation.
//!
//! Security layers, in order: `Host` must be `127.0.0.1:<port>` /
//! `localhost:<port>` (DNS-rebinding guard); an `Origin`, when present, must be
//! a browser-extension origin; every `/v1/*` route needs the bearer token. The
//! body is only buffered after auth passes, capped at `MAX_SLACK_BODY_BYTES`
//! (`/v1/slack-threads`) / `MAX_TRANSLATE_BODY_BYTES` (`/v1/translate`).
//!
//! The server can be switched off (`inbox_api_enabled`): `start_server` /
//! `stop_server` bind and release the port at runtime.

use super::InboxApiState;
use crate::commands::translate::{self, TranslateState, TurnEvent, MAX_INPUT_CHARS};
use crate::db::Db;
use crate::slack_threads::SlackThreadsState;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;
use std::convert::Infallible;
use std::future::Future;
use std::ops::RangeInclusive;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tauri::AppHandle;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};

/// Ports tried in order; the first free one wins.
pub const PORT_RANGE: RangeInclusive<u16> = 47821..=47830;
const API_VERSION: u32 = 1;
/// `X-Devdy-Project-Id` / `X-Devdy-Title` are ignored by every route but stay
/// allowed, so an old extension still sending them gets a real response (e.g.
/// 415 with a hint) instead of an opaque CORS failure.
const ALLOWED_HEADERS: &str = "Authorization, Content-Type, X-Devdy-Project-Id, X-Devdy-Title";

/// `/v1/slack-threads` request body cap.
pub const MAX_SLACK_BODY_BYTES: usize = 16 * 1024;
const SLACK_JSON_ONLY: &str = "Content-Type must be application/json ({\"url\": \"<slack thread link>\"}); zip/markdown uploads are no longer supported";

/// `/v1/translate` request body cap.
pub const MAX_TRANSLATE_BODY_BYTES: usize = 64 * 1024;
/// Concurrent `/v1/translate` requests allowed before answering 429.
pub const MAX_TRANSLATE_IN_FLIGHT: usize = 4;
/// JSON mode: max wait for the result. SSE mode: max gap between events.
pub const TRANSLATE_TIMEOUT: Duration = Duration::from_secs(90);
/// How long `stop_server` waits for a graceful shutdown before aborting.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// A submitted translation turn: its id, event channel and the resolved
/// target language code.
pub struct SubmittedTurn {
    pub turn: u64,
    pub rx: UnboundedReceiver<TurnEvent>,
    pub target: String,
}

pub type TranslateSubmit = Result<SubmittedTurn, String>;

/// Starts a translation of `(text, target_lang)` (injected so tests need no
/// `AppHandle` / sidecar).
pub type Translator = Arc<
    dyn Fn(String, Option<String>) -> Pin<Box<dyn Future<Output = TranslateSubmit> + Send>>
        + Send
        + Sync,
>;

/// Called with a turn id when a request gives up on it (timeout), so a hung
/// turn does not block the shared translation queue.
pub type AbandonTurn = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Clone)]
pub struct HttpState {
    pub db: Db,
    pub api: InboxApiState,
    pub version: String,
    /// Port this server is bound to (for the `Host` check).
    pub port: u16,
    pub translator: Translator,
    pub abandon: AbandonTurn,
    /// Permits for in-flight `/v1/translate` requests.
    pub translate_slots: Arc<Semaphore>,
    /// See `TRANSLATE_TIMEOUT` (shortened in tests).
    pub translate_timeout: Duration,
    /// Flips to `true` when the server is stopping, so long-lived translate
    /// requests (SSE streams) end instead of holding the shutdown open.
    pub shutdown: watch::Receiver<bool>,
    /// Slack thread add / re-import + job queue.
    pub slack: SlackThreadsState,
}

/// A running server: signal `shutdown`, then await / abort `task`.
pub struct ServerHandle {
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

/// Resolves once `rx` reads `true`; never if the sender is gone without it.
async fn stopped(mut rx: watch::Receiver<bool>) {
    loop {
        if *rx.borrow_and_update() {
            return;
        }
        if rx.changed().await.is_err() {
            std::future::pending::<()>().await;
        }
    }
}

/// Start the server unless it is disabled or already running (startup path).
pub async fn start_server(
    app: AppHandle,
    db: Db,
    api: InboxApiState,
    translate_state: TranslateState,
    slack: SlackThreadsState,
) {
    let mut slot = api.server.lock().await;
    start_locked(&mut slot, &api, PORT_RANGE, |port, shutdown| {
        app_state(app, db, &api, translate_state, slack, port, shutdown)
    })
    .await;
}

/// Persist the on/off switch and start / stop the server accordingly. The
/// server lock is held throughout, so concurrent toggles (and the startup
/// spawn) apply in a consistent order.
pub async fn set_enabled(
    app: AppHandle,
    db: Db,
    api: InboxApiState,
    translate_state: TranslateState,
    slack: SlackThreadsState,
    enabled: bool,
) -> Result<(), String> {
    let persist_db = db.clone();
    let persist = async move {
        super::save_enabled(&persist_db, enabled)
            .await
            .map_err(|e| e.to_string())
    };
    toggle(&api, enabled, persist, PORT_RANGE, |port, shutdown| {
        app_state(app, db, &api, translate_state, slack, port, shutdown)
    })
    .await
}

/// `HttpState` wired to the real app: the shared translator + Slack queue.
fn app_state(
    app: AppHandle,
    db: Db,
    api: &InboxApiState,
    translate_state: TranslateState,
    slack: SlackThreadsState,
    port: u16,
    shutdown: watch::Receiver<bool>,
) -> HttpState {
    let (translate_app, translate_db, submit_state) =
        (app.clone(), db.clone(), translate_state.clone());
    let translator: Translator = Arc::new(move |text, target_lang| {
        let (app, db, ts) = (
            translate_app.clone(),
            translate_db.clone(),
            submit_state.clone(),
        );
        Box::pin(async move {
            // Run detached: a client disconnect drops this future, and `submit`
            // must not be cancelled between enqueueing the turn and writing its
            // prompt (that would leave a ghost turn in the FIFO).
            let (turn, rx, target) = tokio::spawn(async move {
                translate::submit(&app, &db, &ts, text, target_lang, true).await
            })
            .await
            .map_err(|e| e.to_string())??;
            let rx = rx.ok_or("translation channel unavailable")?;
            Ok(SubmittedTurn { turn, rx, target })
        })
    });
    let abandon: AbandonTurn = Arc::new(move |turn| {
        let ts = translate_state.clone();
        tokio::spawn(async move { ts.abandon_turn(turn).await });
    });
    HttpState {
        db,
        api: api.clone(),
        version: app.package_info().version.to_string(),
        port,
        translator,
        abandon,
        translate_slots: Arc::new(Semaphore::new(MAX_TRANSLATE_IN_FLIGHT)),
        translate_timeout: TRANSLATE_TIMEOUT,
        shutdown,
        slack,
    }
}

/// Persist `enabled` and apply it, all under the server lock.
async fn toggle<P, F>(
    api: &InboxApiState,
    enabled: bool,
    persist: P,
    ports: RangeInclusive<u16>,
    make_state: F,
) -> Result<(), String>
where
    P: Future<Output = Result<(), String>>,
    F: FnOnce(u16, watch::Receiver<bool>) -> HttpState,
{
    let mut slot = api.server.lock().await;
    persist.await?;
    api.set_enabled(enabled);
    if enabled {
        start_locked(&mut slot, api, ports, make_state).await;
    } else {
        stop_locked(&mut slot, api).await;
    }
    Ok(())
}

/// Bind the first free port in `ports`, record it in `InboxApiState` and serve
/// until stopped. No-op if disabled or already running (a handle whose task
/// has exited counts as stopped); leaves the state "not running" if every port
/// is taken. Caller holds the server lock.
async fn start_locked<F>(
    slot: &mut Option<ServerHandle>,
    api: &InboxApiState,
    ports: RangeInclusive<u16>,
    make_state: F,
) where
    F: FnOnce(u16, watch::Receiver<bool>) -> HttpState,
{
    if !api.enabled() {
        return;
    }
    if let Some(h) = slot.as_ref() {
        if !h.task.is_finished() {
            return;
        }
        *slot = None;
    }
    let mut bound = None;
    for port in ports {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                bound = Some(listener);
                break;
            }
            Err(e) => tracing::debug!(event = "inbox_api_port_busy", port = port, error = %e),
        }
    }
    let Some((listener, port)) = bound.and_then(|l| {
        let port = l.local_addr().ok()?.port();
        Some((l, port))
    }) else {
        tracing::error!(event = "inbox_api_no_free_port");
        api.set_port(None);
        return;
    };

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let state = make_state(port, shutdown_rx.clone());
    api.set_port(Some(port));
    tracing::info!(event = "inbox_api_listening", port = port);
    let serve_api = api.clone();
    let task = tokio::spawn(async move {
        let result = axum::serve(listener, router(state))
            .with_graceful_shutdown(stopped(shutdown_rx.clone()))
            .await;
        if let Err(e) = result {
            tracing::error!(event = "inbox_api_serve_failed", error = %e);
        }
        // An unexpected exit clears the port; a requested stop is handled by
        // `stop_locked` (a restart may already own a new port by now).
        if !*shutdown_rx.borrow() {
            serve_api.set_port(None);
        }
    });
    *slot = Some(ServerHandle {
        shutdown: shutdown_tx,
        task,
    });
}

/// Stop the server and release its port. Waits up to `SHUTDOWN_GRACE` for
/// open connections to finish, then aborts. No-op when not running. Caller
/// holds the server lock.
async fn stop_locked(slot: &mut Option<ServerHandle>, api: &InboxApiState) {
    let Some(ServerHandle { shutdown, mut task }) = slot.take() else {
        return;
    };
    let _ = shutdown.send(true);
    if tokio::time::timeout(SHUTDOWN_GRACE, &mut task)
        .await
        .is_err()
    {
        tracing::warn!(event = "inbox_api_shutdown_timeout");
        task.abort();
    }
    api.set_port(None);
    tracing::info!(event = "inbox_api_stopped");
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/projects", get(list_projects))
        .route("/v1/slack-threads", post(post_slack_thread))
        .route("/v1/translate", post(post_translate))
        .fallback(not_found)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state)
}

fn error(status: StatusCode, msg: &str) -> Response {
    (status, Json(json!({ "error": msg }))).into_response()
}

fn is_allowed_origin(origin: &str) -> bool {
    origin.starts_with("chrome-extension://") || origin.starts_with("moz-extension://")
}

/// Host / Origin checks, CORS preflight, and CORS headers on every response.
async fn guard(State(state): State<HttpState>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    if host != format!("127.0.0.1:{}", state.port) && host != format!("localhost:{}", state.port) {
        return error(StatusCode::FORBIDDEN, "forbidden host");
    }

    let origin = req.headers().get(header::ORIGIN).cloned();
    if let Some(o) = &origin {
        if !o.to_str().map(is_allowed_origin).unwrap_or(false) {
            return error(StatusCode::FORBIDDEN, "forbidden origin");
        }
    }

    let mut resp = if req.method() == Method::OPTIONS {
        let mut r = StatusCode::NO_CONTENT.into_response();
        let h = r.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOWED_HEADERS),
        );
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
        // Chrome Private Network Access preflight for loopback targets.
        h.insert(
            "access-control-allow-private-network",
            HeaderValue::from_static("true"),
        );
        r
    } else {
        next.run(req).await
    };
    if let Some(o) = origin {
        let h = resp.headers_mut();
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, o);
        h.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    resp
}

/// `None` when the bearer token matches; otherwise the 401 response.
fn check_auth(state: &HttpState, headers: &HeaderMap) -> Option<Response> {
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        })
        .map(str::trim);
    match token {
        Some(t) if state.api.check_token(t) => None,
        _ => Some(error(StatusCode::UNAUTHORIZED, "invalid or missing token")),
    }
}

async fn not_found() -> Response {
    error(StatusCode::NOT_FOUND, "not found")
}

async fn health(State(state): State<HttpState>) -> Response {
    Json(json!({
        "app": "devdy",
        "version": state.version,
        "api": API_VERSION,
        "features": ["translate", "slack-thread-link"],
    }))
    .into_response()
}

async fn list_projects(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Some(r) = check_auth(&state, &headers) {
        return r;
    }
    match sqlx::query("SELECT id, name FROM projects ORDER BY position ASC, name ASC")
        .fetch_all(&state.db)
        .await
    {
        Ok(rows) => {
            let list: Vec<_> = rows
                .iter()
                .map(|r| {
                    json!({ "id": r.get::<String, _>("id"), "name": r.get::<String, _>("name") })
                })
                .collect();
            Json(list).into_response()
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn coded_error(status: StatusCode, code: &str, msg: &str) -> Response {
    (status, Json(json!({ "error": msg, "code": code }))).into_response()
}

/// `POST /v1/slack-threads`: JSON `{url, projectId?, title?}` → `202`; the
/// thread is fetched and translated in the background. Headers
/// `X-Devdy-Project-Id` / `X-Devdy-Title` are ignored here.
async fn post_slack_thread(State(state): State<HttpState>, req: Request) -> Response {
    let headers = req.headers().clone();
    if let Some(r) = check_auth(&state, &headers) {
        return r;
    }
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, SLACK_JSON_ONLY);
    }
    let too_large = || {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("payload exceeds {} KB", MAX_SLACK_BODY_BYTES / 1024),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_SLACK_BODY_BYTES) {
        return too_large();
    }
    let body = match axum::body::to_bytes(req.into_body(), MAX_SLACK_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return too_large(),
    };
    let Ok(serde_json::Value::Object(obj)) = serde_json::from_slice::<serde_json::Value>(&body)
    else {
        return coded_error(StatusCode::BAD_REQUEST, "invalid_json", "invalid JSON body");
    };
    let Some(url) = obj.get("url").and_then(|v| v.as_str()) else {
        return coded_error(
            StatusCode::BAD_REQUEST,
            "invalid_link",
            crate::slack_threads::link::INVALID_LINK_MESSAGE,
        );
    };
    let project_id = match obj.get("projectId") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(p)) => Some(p.as_str()),
        Some(_) => {
            return coded_error(
                StatusCode::BAD_REQUEST,
                "invalid_project",
                "projectId must be a string",
            )
        }
    };
    let title = match obj.get("title") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(t)) => Some(t.as_str()),
        Some(_) => {
            return coded_error(
                StatusCode::BAD_REQUEST,
                "invalid_title",
                "title must be a string",
            )
        }
    };
    if *state.shutdown.borrow() {
        return error(StatusCode::SERVICE_UNAVAILABLE, SERVER_STOPPING);
    }
    match state.slack.add_thread(url, project_id, title, true).await {
        Ok(r) => {
            let mut body = json!({
                "id": r.id,
                "status": r.status,
                "fetchStatus": r.fetch_status,
            });
            if let Some(code) = r.error_code {
                body["errorCode"] = json!(code);
            }
            (StatusCode::ACCEPTED, Json(body)).into_response()
        }
        Err(e) => match e.code.as_str() {
            "invalid_link" | "dm_unsupported" | "invalid_project" | "invalid_title" => {
                coded_error(StatusCode::BAD_REQUEST, &e.code, &e.detail)
            }
            _ => {
                tracing::error!(event = "inbox_api_slack_add_failed", code = %e.code);
                error(StatusCode::INTERNAL_SERVER_ERROR, &e.detail)
            }
        },
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TranslateRequest {
    text: String,
    target_lang: Option<String>,
}

/// What ended a wait for the next turn event.
enum Waited {
    Event(TurnEvent),
    /// The channel closed without a terminal event.
    Closed,
    Timeout,
    Stopping,
}

async fn next_event(
    rx: &mut UnboundedReceiver<TurnEvent>,
    timeout: Duration,
    shutdown: &watch::Receiver<bool>,
) -> Waited {
    tokio::select! {
        r = tokio::time::timeout(timeout, rx.recv()) => match r {
            Ok(Some(ev)) => Waited::Event(ev),
            Ok(None) => Waited::Closed,
            Err(_) => Waited::Timeout,
        },
        _ = stopped(shutdown.clone()) => Waited::Stopping,
    }
}

const ENDED_UNEXPECTEDLY: &str = "translation ended unexpectedly";
const SERVER_STOPPING: &str = "server stopping";

/// The translation engine reports errors in the UI's language; the API speaks
/// English. Known messages are mapped, anything else (sidecar / OS detail) is
/// passed through.
fn english_error(msg: &str) -> String {
    match msg {
        translate::ERR_SESSION_ENDED => ENDED_UNEXPECTEDLY.to_string(),
        translate::ERR_EMPTY_RESULT => "empty translation".to_string(),
        translate::ERR_FAILED => "translation failed".to_string(),
        _ => match msg.strip_prefix(translate::ERR_SEND_PREFIX) {
            Some(detail) => format!("failed to send the request: {detail}"),
            None => msg.to_string(),
        },
    }
}

/// `POST /v1/translate`: auth → content type → body (≤ 64 KB) → validate →
/// in-flight limit → submit to the shared warm translation sidecar, answering
/// as JSON or as an SSE stream (`Accept: text/event-stream`).
async fn post_translate(State(state): State<HttpState>, req: Request) -> Response {
    let headers = req.headers().clone();
    if let Some(r) = check_auth(&state, &headers) {
        return r;
    }
    let is_json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|m| m.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let too_large = || {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("payload exceeds {} KB", MAX_TRANSLATE_BODY_BYTES / 1024),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_TRANSLATE_BODY_BYTES) {
        return too_large();
    }
    let body = match axum::body::to_bytes(req.into_body(), MAX_TRANSLATE_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return too_large(),
    };
    let Ok(parsed) = serde_json::from_slice::<TranslateRequest>(&body) else {
        return error(StatusCode::BAD_REQUEST, "invalid JSON body");
    };
    let text = parsed.text.trim().to_string();
    if text.is_empty() {
        return error(StatusCode::BAD_REQUEST, "text is empty");
    }
    if text.chars().count() > MAX_INPUT_CHARS {
        return error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("text exceeds {} characters", MAX_INPUT_CHARS),
        );
    }
    let Ok(permit) = state.translate_slots.clone().try_acquire_owned() else {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "too many translations in flight",
        );
    };
    let wants_sse = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/event-stream"));

    let SubmittedTurn {
        turn,
        mut rx,
        target,
    } = match (state.translator)(text, parsed.target_lang).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(event = "inbox_api_translate_submit_failed", error = %e);
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                &format!("translation engine unavailable: {}", english_error(&e)),
            );
        }
    };

    if wants_sse {
        return translate_sse(state, turn, rx, target, permit);
    }

    // JSON mode: one overall deadline for the whole answer.
    let deadline = tokio::time::Instant::now() + state.translate_timeout;
    loop {
        let left = deadline.saturating_duration_since(tokio::time::Instant::now());
        match next_event(&mut rx, left, &state.shutdown).await {
            Waited::Event(TurnEvent::Chunk(_)) => {}
            Waited::Event(TurnEvent::Done(text)) => {
                return Json(json!({ "text": text, "targetLang": target })).into_response()
            }
            Waited::Event(TurnEvent::Error(e)) => {
                return error(StatusCode::BAD_GATEWAY, &english_error(&e))
            }
            Waited::Closed => return error(StatusCode::BAD_GATEWAY, ENDED_UNEXPECTEDLY),
            Waited::Timeout => {
                (state.abandon)(turn);
                return error(StatusCode::GATEWAY_TIMEOUT, "timeout");
            }
            Waited::Stopping => return error(StatusCode::SERVICE_UNAVAILABLE, SERVER_STOPPING),
        }
    }
}

/// SSE mode: `chunk` events, then one terminal `done` / `error` event. The
/// in-flight permit lives as long as the stream (dropped on client disconnect).
fn translate_sse(
    state: HttpState,
    turn: u64,
    rx: UnboundedReceiver<TurnEvent>,
    target: String,
    permit: OwnedSemaphorePermit,
) -> Response {
    struct Stream {
        turn: u64,
        abandon: AbandonTurn,
        rx: UnboundedReceiver<TurnEvent>,
        target: String,
        timeout: Duration,
        shutdown: watch::Receiver<bool>,
        finished: bool,
        _permit: OwnedSemaphorePermit,
    }
    let init = Stream {
        turn,
        abandon: state.abandon.clone(),
        rx,
        target,
        timeout: state.translate_timeout,
        shutdown: state.shutdown.clone(),
        finished: false,
        _permit: permit,
    };
    let error_event = |msg: &str| {
        Event::default()
            .event("error")
            .data(json!({ "error": msg }).to_string())
    };
    let stream = futures_util::stream::unfold(init, move |mut st| async move {
        if st.finished {
            return None;
        }
        let event = match next_event(&mut st.rx, st.timeout, &st.shutdown).await {
            Waited::Event(TurnEvent::Chunk(delta)) => Event::default()
                .event("chunk")
                .data(json!({ "delta": delta }).to_string()),
            Waited::Event(TurnEvent::Done(text)) => {
                st.finished = true;
                Event::default()
                    .event("done")
                    .data(json!({ "text": text, "targetLang": st.target }).to_string())
            }
            Waited::Event(TurnEvent::Error(e)) => {
                st.finished = true;
                error_event(&english_error(&e))
            }
            Waited::Closed => {
                st.finished = true;
                error_event(ENDED_UNEXPECTEDLY)
            }
            Waited::Timeout => {
                st.finished = true;
                (st.abandon)(st.turn);
                error_event("timeout")
            }
            Waited::Stopping => {
                st.finished = true;
                error_event(SERVER_STOPPING)
            }
        };
        Some((Ok::<_, Infallible>(event), st))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::atomic::Ordering;
    use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

    struct Harness {
        base: String,
        port: u16,
        token: String,
        api: InboxApiState,
        app_data: std::path::PathBuf,
        client: reqwest::Client,
        db: Db,
        fakes: Fakes,
        shutdown: watch::Sender<bool>,
        served: tokio::task::JoinHandle<()>,
    }

    /// Test doubles shared by an `HttpState` and the test body.
    #[derive(Clone)]
    struct Fakes {
        /// Senders of "hang" translations, kept so their turns never finish.
        held: Arc<std::sync::Mutex<Vec<UnboundedSender<TurnEvent>>>>,
        /// Turn ids passed to `abandon`.
        abandoned: Arc<std::sync::Mutex<Vec<u64>>>,
        turn_seq: Arc<std::sync::atomic::AtomicU64>,
        slots: Arc<Semaphore>,
        /// Slack state on the same DB (fake token lookup, no worker).
        slack: SlackThreadsState,
        slack_tokens: crate::slack_threads::tests::Tokens,
    }

    impl Fakes {
        fn new(db: &Db, app_data: &std::path::Path) -> Self {
            let slack_tokens = crate::slack_threads::tests::Tokens::default();
            Fakes {
                held: Arc::default(),
                abandoned: Arc::default(),
                turn_seq: Arc::default(),
                slots: Arc::new(Semaphore::new(MAX_TRANSLATE_IN_FLIGHT)),
                slack: crate::slack_threads::tests::offline_state(db, app_data, &slack_tokens),
                slack_tokens,
            }
        }

        fn abandoned(&self) -> Vec<u64> {
            self.abandoned.lock().unwrap().clone()
        }
    }

    /// Fake engine keyed by the request text: `fail-submit` → submit error,
    /// `engine-error` → turn error, `session-ended` → the engine's (Vietnamese)
    /// session-ended error, `hang` → never answers, anything else → two chunks
    /// then the full text. Turn ids count up from 1.
    fn fake_translator(fakes: Fakes) -> Translator {
        Arc::new(move |text: String, lang: Option<String>| {
            let fakes = fakes.clone();
            Box::pin(async move {
                if text == "fail-submit" {
                    return Err("Failed to spawn sidecar".to_string());
                }
                let turn = fakes.turn_seq.fetch_add(1, Ordering::SeqCst) + 1;
                let (tx, rx) = unbounded_channel();
                match text.as_str() {
                    "engine-error" => tx.send(TurnEvent::Error("boom".into())).unwrap(),
                    "session-ended" => tx
                        .send(TurnEvent::Error(translate::ERR_SESSION_ENDED.into()))
                        .unwrap(),
                    "hang" => fakes.held.lock().unwrap().push(tx),
                    _ => {
                        tx.send(TurnEvent::Chunk("Xin ".into())).unwrap();
                        tx.send(TurnEvent::Chunk("chào".into())).unwrap();
                        tx.send(TurnEvent::Done("Xin chào".into())).unwrap();
                    }
                }
                Ok(SubmittedTurn {
                    turn,
                    rx,
                    target: lang.unwrap_or_else(|| "vi".to_string()),
                })
            }) as Pin<Box<dyn Future<Output = TranslateSubmit> + Send>>
        })
    }

    fn test_state(
        db: &Db,
        api: &InboxApiState,
        fakes: &Fakes,
        translate_timeout: Duration,
        port: u16,
        shutdown: watch::Receiver<bool>,
    ) -> HttpState {
        let abandoned = fakes.abandoned.clone();
        HttpState {
            db: db.clone(),
            api: api.clone(),
            version: "test".into(),
            port,
            translator: fake_translator(fakes.clone()),
            abandon: Arc::new(move |turn| abandoned.lock().unwrap().push(turn)),
            translate_slots: fakes.slots.clone(),
            translate_timeout,
            shutdown,
            slack: fakes.slack.clone(),
        }
    }

    async fn memory_db() -> Db {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        db
    }

    async fn spawn() -> Harness {
        spawn_with(TRANSLATE_TIMEOUT).await
    }

    /// Serve the real router on an ephemeral port against an in-memory DB.
    async fn spawn_with(translate_timeout: Duration) -> Harness {
        let db = memory_db().await;
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'proj', '/tmp/p1', '2026-01-01')")
            .execute(&db)
            .await
            .unwrap();
        let token = super::super::load_or_create_token(&db).await.unwrap();
        // A second load returns the persisted token instead of a new one.
        assert_eq!(super::super::load_or_create_token(&db).await.unwrap(), token);
        let api = InboxApiState::default();
        api.set_token(token.clone());
        let app_data = std::env::temp_dir().join(format!(
            "devdy-inbox-http-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&app_data).unwrap();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let fakes = Fakes::new(&db, &app_data);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let state = test_state(
            &db,
            &api,
            &fakes,
            translate_timeout,
            port,
            shutdown_rx.clone(),
        );
        let served = tokio::spawn(async move {
            axum::serve(listener, router(state))
                .with_graceful_shutdown(stopped(shutdown_rx))
                .await
                .unwrap();
        });
        Harness {
            base: format!("http://127.0.0.1:{}", port),
            port,
            token,
            api,
            app_data,
            client: reqwest::Client::new(),
            db,
            fakes,
            shutdown,
            served,
        }
    }

    impl Harness {
        fn translate(&self, body: serde_json::Value) -> reqwest::RequestBuilder {
            self.client
                .post(format!("{}/v1/translate", self.base))
                .bearer_auth(&self.token)
                .header("Content-Type", "application/json")
                .body(body.to_string())
        }

        fn translate_sse(&self, text: &str) -> reqwest::RequestBuilder {
            self.translate(json!({ "text": text }))
                .header("Accept", "text/event-stream")
        }
    }

    #[tokio::test]
    async fn end_to_end_contract() {
        let h = spawn().await;

        // /health needs no auth.
        let r = h.client.get(format!("{}/health", h.base)).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["app"], "devdy");
        assert_eq!(v["api"], 1);
        assert_eq!(v["features"], json!(["translate", "slack-thread-link"]));

        // Auth.
        let r = h.client.get(format!("{}/v1/projects", h.base)).send().await.unwrap();
        assert_eq!(r.status(), 401);
        let r = h
            .client
            .get(format!("{}/v1/projects", h.base))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let r = h
            .client
            .get(format!("{}/v1/projects", h.base))
            .bearer_auth(&h.token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v, json!([{ "id": "p1", "name": "proj" }]));

        // Origin / Host guards.
        let r = h
            .client
            .get(format!("{}/health", h.base))
            .header("Origin", "https://evil.com")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let r = h
            .client
            .get(format!("{}/health", h.base))
            .header("Host", format!("evil.com:{}", h.port))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 403);
        let r = h
            .client
            .get(format!("{}/health", h.base))
            .header("Host", format!("localhost:{}", h.port))
            .header("Origin", "chrome-extension://abc")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            r.headers().get("access-control-allow-origin").unwrap(),
            "chrome-extension://abc"
        );

        // CORS preflight.
        let r = h
            .client
            .request(Method::OPTIONS, format!("{}/v1/slack-threads", h.base))
            .header("Origin", "chrome-extension://abc")
            .header("Access-Control-Request-Method", "POST")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
        let allowed = r
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        for h in [
            "Authorization",
            "Content-Type",
            "X-Devdy-Project-Id",
            "X-Devdy-Title",
        ] {
            assert!(allowed.contains(h), "{allowed}");
        }
        // An old extension's zip upload (with the legacy headers) passes CORS
        // and gets the 415 hint.
        let r = h
            .client
            .post(format!("{}/v1/slack-threads", h.base))
            .bearer_auth(&h.token)
            .header("Origin", "chrome-extension://abc")
            .header("Content-Type", "application/zip")
            .header("X-Devdy-Project-Id", "p1")
            .header("X-Devdy-Title", "Old")
            .body("zip")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 415);
        assert_eq!(
            r.headers().get("access-control-allow-origin").unwrap(),
            "chrome-extension://abc"
        );

        // Oversized body: announce Content-Length > 16 KB but send only the
        // headers over a raw socket. The server must answer 413 from the header
        // alone (before reading the body), so this is deterministic.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
            let req = format!(
                "POST /v1/slack-threads HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
                 Authorization: Bearer {}\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\n\r\n",
                h.port,
                h.token,
                MAX_SLACK_BODY_BYTES + 1
            );
            sock.write_all(req.as_bytes()).await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]);
            assert!(head.starts_with("HTTP/1.1 413"), "{}", head);
            assert!(head.contains("payload exceeds 16 KB"), "{}", head);
        }

        // Token regeneration takes effect immediately.
        let old = h.token.clone();
        h.api.set_token(super::super::generate_token());
        let r = h
            .client
            .get(format!("{}/v1/projects", h.base))
            .bearer_auth(&old)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);

        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    async fn code_and_error(r: reqwest::RequestBuilder) -> (u16, String) {
        let r = r.send().await.unwrap();
        let code = r.status().as_u16();
        let v: serde_json::Value = r.json().await.unwrap();
        (code, v["error"].as_str().unwrap_or("").to_string())
    }

    /// Parse an SSE body into `(event, data)` pairs (comments skipped).
    fn sse_events(body: &str) -> Vec<(String, serde_json::Value)> {
        body.split("\n\n")
            .filter_map(|block| {
                let mut event = None;
                let mut data = None;
                for line in block.lines() {
                    if let Some(e) = line.strip_prefix("event:") {
                        event = Some(e.trim().to_string());
                    } else if let Some(d) = line.strip_prefix("data:") {
                        data = serde_json::from_str(d.trim()).ok();
                    }
                }
                Some((event?, data?))
            })
            .collect()
    }

    #[tokio::test]
    async fn translate_validation_errors() {
        let h = spawn().await;
        // 401 before anything else.
        let r = h
            .client
            .post(format!("{}/v1/translate", h.base))
            .header("Content-Type", "application/json")
            .body(r#"{"text":"hi"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        // 415 non-JSON.
        let (code, _) = code_and_error(
            h.client
                .post(format!("{}/v1/translate", h.base))
                .bearer_auth(&h.token)
                .header("Content-Type", "text/plain")
                .body(r#"{"text":"hi"}"#),
        )
        .await;
        assert_eq!(code, 415);
        // 400 invalid JSON / empty text.
        let (code, _) = code_and_error(
            h.client
                .post(format!("{}/v1/translate", h.base))
                .bearer_auth(&h.token)
                .header("Content-Type", "application/json; charset=utf-8")
                .body("{nope"),
        )
        .await;
        assert_eq!(code, 400);
        let (code, err) = code_and_error(h.translate(json!({ "text": "  \n " }))).await;
        assert_eq!((code, err.as_str()), (400, "text is empty"));
        // 413 text too long, and body too large.
        let long = "a".repeat(MAX_INPUT_CHARS + 1);
        let (code, err) = code_and_error(h.translate(json!({ "text": long }))).await;
        assert_eq!((code, err.as_str()), (413, "text exceeds 8000 characters"));
        let huge = "a".repeat(MAX_TRANSLATE_BODY_BYTES + 1);
        let (code, _) = code_and_error(h.translate(json!({ "text": huge }))).await;
        assert_eq!(code, 413);
        // 503 submit failure, 502 engine error.
        let (code, err) = code_and_error(h.translate(json!({ "text": "fail-submit" }))).await;
        assert_eq!(
            (code, err.as_str()),
            (
                503,
                "translation engine unavailable: Failed to spawn sidecar"
            )
        );
        let (code, err) = code_and_error(h.translate(json!({ "text": "engine-error" }))).await;
        assert_eq!((code, err.as_str()), (502, "boom"));
        // Engine messages meant for the UI are mapped to English.
        let (code, err) = code_and_error(h.translate(json!({ "text": "session-ended" }))).await;
        assert_eq!(
            (code, err.as_str()),
            (502, "translation ended unexpectedly")
        );
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn translate_json_happy_path() {
        let h = spawn().await;
        let r = h
            .translate(json!({ "text": "Hello" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v, json!({ "text": "Xin chào", "targetLang": "vi" }));
        let r = h
            .translate(json!({ "text": "Hello", "targetLang": "ja" }))
            .send()
            .await
            .unwrap();
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["targetLang"], "ja");
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn translate_sse_streams_chunks_then_done() {
        let h = spawn().await;
        let r = h.translate_sse("Hello").send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert!(r
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("text/event-stream"));
        let events = sse_events(&r.text().await.unwrap());
        assert_eq!(
            events,
            vec![
                ("chunk".to_string(), json!({ "delta": "Xin " })),
                ("chunk".to_string(), json!({ "delta": "chào" })),
                (
                    "done".to_string(),
                    json!({ "text": "Xin chào", "targetLang": "vi" })
                ),
            ]
        );
        // Engine error → terminal `error` event (status already 200).
        let r = h.translate_sse("engine-error").send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            sse_events(&r.text().await.unwrap()),
            vec![("error".to_string(), json!({ "error": "boom" }))]
        );
        // Errors before streaming starts stay plain JSON responses.
        let (code, _) = code_and_error(h.translate_sse("fail-submit")).await;
        assert_eq!(code, 503);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn translate_limits_in_flight_requests() {
        let h = spawn().await;
        let mut open = Vec::new();
        for _ in 0..MAX_TRANSLATE_IN_FLIGHT {
            let r = h.translate_sse("hang").send().await.unwrap();
            assert_eq!(r.status(), 200);
            open.push(r);
        }
        let (code, _) = code_and_error(h.translate(json!({ "text": "Hello" }))).await;
        assert_eq!(code, 429);
        // Finishing one stream frees its slot.
        let tx = h.fakes.held.lock().unwrap().remove(0);
        tx.send(TurnEvent::Done("ok".into())).unwrap();
        let first = open.remove(0);
        assert_eq!(sse_events(&first.text().await.unwrap())[0].0, "done");
        let r = h
            .translate(json!({ "text": "Hello" }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn translate_timeouts() {
        let h = spawn_with(Duration::from_millis(200)).await;
        let (code, err) = code_and_error(h.translate(json!({ "text": "hang" }))).await;
        assert_eq!((code, err.as_str()), (504, "timeout"));
        let r = h.translate_sse("hang").send().await.unwrap();
        assert_eq!(
            sse_events(&r.text().await.unwrap()),
            vec![("error".to_string(), json!({ "error": "timeout" }))]
        );
        // Both timed-out turns were handed back so they can't block the queue.
        assert_eq!(h.fakes.abandoned(), vec![1, 2]);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn shutdown_ends_open_streams_and_frees_the_port() {
        let h = spawn().await;
        let r = h.translate_sse("hang").send().await.unwrap();
        assert_eq!(r.status(), 200);
        h.shutdown.send(true).unwrap();
        assert_eq!(
            sse_events(&r.text().await.unwrap()),
            vec![("error".to_string(), json!({ "error": "server stopping" }))]
        );
        tokio::time::timeout(Duration::from_secs(2), h.served)
            .await
            .expect("graceful shutdown finished")
            .unwrap();
        assert!(tokio::net::TcpListener::bind(("127.0.0.1", h.port))
            .await
            .is_ok());
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn enabled_setting_defaults_to_true() {
        let h = spawn().await;
        assert!(super::super::load_enabled(&h.db).await);
        super::super::save_enabled(&h.db, false).await.unwrap();
        assert!(!super::super::load_enabled(&h.db).await);
        super::super::save_enabled(&h.db, true).await.unwrap();
        assert!(super::super::load_enabled(&h.db).await);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    /// Poll until all translate permits are free again (or fail after 5 s).
    async fn wait_all_permits(slots: &Semaphore) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while slots.available_permits() < MAX_TRANSLATE_IN_FLIGHT {
            assert!(
                tokio::time::Instant::now() < deadline,
                "permit not released: {} free",
                slots.available_permits()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    #[tokio::test]
    async fn client_disconnect_releases_the_translate_permit() {
        let h = spawn().await;
        // SSE: dropping the open stream frees its slot.
        let r = h.translate_sse("hang").send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            h.fakes.slots.available_permits(),
            MAX_TRANSLATE_IN_FLIGHT - 1
        );
        drop(r);
        wait_all_permits(&h.fakes.slots).await;
        // JSON: the client gives up while the server is still waiting.
        let r = h
            .translate(json!({ "text": "hang" }))
            .timeout(Duration::from_millis(300))
            .send()
            .await;
        assert!(r.unwrap_err().is_timeout());
        wait_all_permits(&h.fakes.slots).await;
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn translate_cors_preflight_from_extension() {
        let h = spawn().await;
        let r = h
            .client
            .request(Method::OPTIONS, format!("{}/v1/translate", h.base))
            .header("Origin", "chrome-extension://abc")
            .header("Access-Control-Request-Method", "POST")
            .header(
                "Access-Control-Request-Headers",
                "authorization,content-type",
            )
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
        assert_eq!(
            r.headers().get("access-control-allow-origin").unwrap(),
            "chrome-extension://abc"
        );
        let allowed = r
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .to_string();
        assert!(allowed.contains("Authorization") && allowed.contains("Content-Type"));
        // The actual request carries ACAO too.
        let r = h
            .translate(json!({ "text": "Hello" }))
            .header("Origin", "chrome-extension://abc")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(
            r.headers().get("access-control-allow-origin").unwrap(),
            "chrome-extension://abc"
        );
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    #[tokio::test]
    async fn slack_post_in_flight_during_shutdown_is_rejected() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let h = spawn().await;
        let body = json!({ "url": LINK }).to_string();
        let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", h.port))
            .await
            .unwrap();
        let head = format!(
            "POST /v1/slack-threads HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
             Authorization: Bearer {}\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\n\r\n",
            h.port,
            h.token,
            body.len()
        );
        // Headers now, body only after the server has started stopping.
        sock.write_all(head.as_bytes()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        h.shutdown.send(true).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        sock.write_all(body.as_bytes()).await.unwrap();
        let mut resp = String::new();
        sock.read_to_string(&mut resp).await.unwrap();
        assert!(resp.starts_with("HTTP/1.1 503"), "{}", resp);
        assert!(resp.contains("server stopping"), "{}", resp);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads")
            .fetch_one(&h.db)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    fn slack(h: &Harness, body: &str) -> reqwest::RequestBuilder {
        h.client
            .post(format!("{}/v1/slack-threads", h.base))
            .bearer_auth(&h.token)
            .header("Content-Type", "application/json; charset=utf-8")
            .body(body.to_string())
    }

    async fn slack_json(r: reqwest::RequestBuilder) -> (u16, serde_json::Value) {
        let r = r.send().await.unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    }

    const LINK: &str = "https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359";

    #[tokio::test]
    async fn slack_threads_endpoint_status_codes() {
        let h = spawn().await;
        // No token → created straight into error/no_token (still 202).
        let (code, v) = slack_json(slack(&h, &json!({ "url": LINK }).to_string())).await;
        assert_eq!(code, 202);
        assert_eq!(v["status"], "created");
        assert_eq!(v["fetchStatus"], "error");
        assert_eq!(v["errorCode"], "no_token");
        let first = v["id"].as_str().unwrap().to_string();

        // With a token → pending; re-sending the same thread → existing, same id.
        h.fakes.slack_tokens.set(&h.db, "s1", "xoxp-test").await;
        let reply = format!(
            "{}?thread_ts=1789877283.026359&cid=C0B83GHDTFG",
            LINK.replace("p1789877283026359", "p1789877300000100")
        );
        let (code, v) = slack_json(slack(
            &h,
            &json!({ "url": reply, "projectId": "p1" }).to_string(),
        ))
        .await;
        assert_eq!(code, 202);
        assert_eq!(
            v,
            json!({ "id": first, "status": "existing", "fetchStatus": "pending" })
        );
        let (code, v) = slack_json(slack(&h, &json!({ "url": "https://acme.slack.com/archives/C0000000001/p1789877283026359", "title": "Mine" }).to_string())).await;
        assert_eq!(code, 202);
        assert_eq!(v["status"], "created");
        assert_eq!(v["fetchStatus"], "pending");
        assert!(v.get("errorCode").is_none());
        // Project only assigned while the thread had none; title kept custom.
        let (pid, title, custom): (Option<String>, String, i64) = sqlx::query_as(
            "SELECT project_id, title, title_custom FROM slack_threads WHERE id = ?",
        )
        .bind(&first)
        .fetch_one(&h.db)
        .await
        .unwrap();
        assert_eq!(
            (pid.as_deref(), title.as_str(), custom),
            (Some("p1"), "Slack thread", 0)
        );
        let id2 = v["id"].as_str().unwrap();
        let t: (String, i64) =
            sqlx::query_as("SELECT title, title_custom FROM slack_threads WHERE id = ?")
                .bind(id2)
                .fetch_one(&h.db)
                .await
                .unwrap();
        assert_eq!(t, ("Mine".to_string(), 1));

        // 400s with codes.
        for (body, code) in [
            (r#"{"url":"abc"}"#.to_string(), "invalid_link"),
            (r#"{}"#.to_string(), "invalid_link"),
            (
                json!({ "url": LINK.replace("C0B83GHDTFG", "D0B83GHDTFG") }).to_string(),
                "dm_unsupported",
            ),
            (
                json!({ "url": LINK, "projectId": "nope" }).to_string(),
                "invalid_project",
            ),
            (
                json!({ "url": LINK, "title": "  " }).to_string(),
                "invalid_title",
            ),
            (
                json!({ "url": LINK, "title": "x".repeat(201) }).to_string(),
                "invalid_title",
            ),
            ("{nope".to_string(), "invalid_json"),
            ("[1]".to_string(), "invalid_json"),
        ] {
            let (status, v) = slack_json(slack(&h, &body)).await;
            assert_eq!((status, v["code"].as_str()), (400, Some(code)), "{}", body);
            assert!(v["error"].is_string());
        }
        // Zip / markdown → 415 with the migration hint.
        for ct in ["application/zip", "text/markdown", "text/plain"] {
            let r = h
                .client
                .post(format!("{}/v1/slack-threads", h.base))
                .bearer_auth(&h.token)
                .header("Content-Type", ct)
                .body("x")
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 415);
            let v: serde_json::Value = r.json().await.unwrap();
            assert_eq!(v["error"], SLACK_JSON_ONLY);
        }
        // > 16 KB → 413; no auth → 401.
        let big = json!({ "url": LINK, "title": "x".repeat(MAX_SLACK_BODY_BYTES) }).to_string();
        let (code, _) = slack_json(slack(&h, &big)).await;
        assert_eq!(code, 413);
        let r = h
            .client
            .post(format!("{}/v1/slack-threads", h.base))
            .header("Content-Type", "application/json")
            .body(json!({ "url": LINK }).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads")
            .fetch_one(&h.db)
            .await
            .unwrap();
        assert_eq!(n, 2);
        let _ = std::fs::remove_dir_all(&h.app_data);
    }

    /// Everything `toggle` / `start_locked` need, minus the HTTP harness.
    struct Lifecycle {
        db: Db,
        api: InboxApiState,
        fakes: Fakes,
    }

    impl Lifecycle {
        async fn new() -> Self {
            let api = InboxApiState::default();
            api.set_token("t".into());
            let db = memory_db().await;
            let app_data = std::env::temp_dir();
            Lifecycle {
                fakes: Fakes::new(&db, &app_data),
                db,
                api,
            }
        }

        fn make(&self) -> impl FnOnce(u16, watch::Receiver<bool>) -> HttpState + '_ {
            move |port, shutdown| {
                test_state(
                    &self.db,
                    &self.api,
                    &self.fakes,
                    TRANSLATE_TIMEOUT,
                    port,
                    shutdown,
                )
            }
        }

        /// `set_enabled` without an `AppHandle`, on an ephemeral port.
        async fn set(&self, enabled: bool) {
            let db = self.db.clone();
            let persist = async move {
                super::super::save_enabled(&db, enabled)
                    .await
                    .map_err(|e| e.to_string())
            };
            toggle(&self.api, enabled, persist, 0..=0, self.make())
                .await
                .unwrap();
        }

        async fn health(&self) -> bool {
            let Some(port) = self.api.port() else {
                return false;
            };
            reqwest::get(format!("http://127.0.0.1:{}/health", port))
                .await
                .is_ok_and(|r| r.status() == 200)
        }
    }

    #[tokio::test]
    async fn toggle_starts_and_stops_the_listener() {
        let l = Lifecycle::new().await;
        l.set(true).await;
        let port = l.api.port().expect("listening");
        assert!(l.health().await);
        assert!(l.api.info().enabled && l.api.info().running);
        l.set(true).await; // already running → same server
        assert_eq!(l.api.port(), Some(port));

        l.set(false).await;
        assert!(!l.api.info().running && !l.api.info().enabled);
        assert!(!super::super::load_enabled(&l.db).await);
        // Port released: nothing listens, it can be bound again.
        assert!(tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err());
        assert!(tokio::net::TcpListener::bind(("127.0.0.1", port))
            .await
            .is_ok());

        l.set(true).await;
        assert!(l.health().await);
        l.set(false).await;
    }

    #[tokio::test]
    async fn start_is_a_noop_when_disabled() {
        // Startup spawn racing with an early "off": the re-check under the
        // lock keeps the server down.
        let l = Lifecycle::new().await;
        l.api.set_enabled(false);
        let mut slot = l.api.server.lock().await;
        start_locked(&mut slot, &l.api, 0..=0, l.make()).await;
        assert!(slot.is_none());
        assert!(l.api.port().is_none());
    }

    #[tokio::test]
    async fn start_replaces_a_handle_whose_task_already_exited() {
        // e.g. `axum::serve` failed: the stale handle must not block a restart.
        let l = Lifecycle::new().await;
        l.api.set_enabled(true);
        {
            let (shutdown, _) = watch::channel(false);
            let task = tokio::spawn(async {});
            while !task.is_finished() {
                tokio::task::yield_now().await;
            }
            *l.api.server.lock().await = Some(ServerHandle { shutdown, task });
        }
        let mut slot = l.api.server.lock().await;
        start_locked(&mut slot, &l.api, 0..=0, l.make()).await;
        assert!(slot.as_ref().is_some_and(|h| !h.task.is_finished()));
        drop(slot);
        assert!(l.health().await);
        l.set(false).await;
    }

    #[tokio::test]
    async fn concurrent_toggles_leave_db_and_server_consistent() {
        let l = Arc::new(Lifecycle::new().await);
        let mut tasks = Vec::new();
        for i in 0..12 {
            let l = l.clone();
            tasks.push(tokio::spawn(async move { l.set(i % 2 == 0).await }));
        }
        for t in tasks {
            t.await.unwrap();
        }
        let stored = super::super::load_enabled(&l.db).await;
        assert_eq!(stored, l.api.enabled());
        assert_eq!(stored, l.api.info().running);
        assert_eq!(stored, l.health().await);
        l.set(false).await;
    }
}
