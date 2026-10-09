//! Loopback HTTP server for the Slack Thread Inbox (axum, 127.0.0.1 only).
//!
//! Security layers, in order: `Host` must be `127.0.0.1:<port>` /
//! `localhost:<port>` (DNS-rebinding guard); an `Origin`, when present, must be
//! a browser-extension origin; every `/v1/*` route needs the bearer token. The
//! body is only buffered after auth passes, capped at `MAX_BODY_BYTES`.

use super::ingest::{self, CaptureKind, IngestError, PayloadKind, MAX_BODY_BYTES};
use super::store::{self, IngestResult, Overrides};
use super::InboxApiState;
use crate::db::Db;
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use sqlx::Row;
use std::path::PathBuf;
use std::sync::Arc;
use tauri::{AppHandle, Manager};

/// Ports tried in order; the first free one wins.
pub const PORT_RANGE: std::ops::RangeInclusive<u16> = 47821..=47830;
const API_VERSION: u32 = 1;
const ALLOWED_HEADERS: &str =
    "Authorization, Content-Type, X-Devdy-Project-Id, X-Devdy-Title, X-Devdy-Mode";

/// Called after every successful upsert (emit event + OS notification).
pub type OnIngest = Arc<dyn Fn(&IngestResult) + Send + Sync>;

#[derive(Clone)]
pub struct HttpState {
    pub db: Db,
    pub app_data: PathBuf,
    pub api: InboxApiState,
    pub version: String,
    /// Port this server is bound to (for the `Host` check).
    pub port: u16,
    pub on_ingest: OnIngest,
}

/// Bind the first free port in `PORT_RANGE`, record it in `InboxApiState` and
/// serve for the app's lifetime. Leaves the state "not running" if every port
/// is taken.
pub async fn start(app: AppHandle, db: Db, app_data: PathBuf, api: InboxApiState) {
    let mut bound = None;
    for port in PORT_RANGE {
        match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
            Ok(listener) => {
                bound = Some((listener, port));
                break;
            }
            Err(e) => tracing::debug!(event = "inbox_api_port_busy", port = port, error = %e),
        }
    }
    let Some((listener, port)) = bound else {
        tracing::error!(event = "inbox_api_no_free_port");
        api.set_port(None);
        return;
    };

    let notify_app = app.clone();
    let on_ingest: OnIngest = Arc::new(move |r: &IngestResult| {
        super::emit_changed(&notify_app, &r.id, r.kind, r.status.as_str());
        super::notify_received(&notify_app, &r.id, r.kind, &r.title);
    });
    let state = HttpState {
        db,
        app_data,
        api: api.clone(),
        version: app.package_info().version.to_string(),
        port,
        on_ingest,
    };
    api.set_port(Some(port));
    tracing::info!(event = "inbox_api_listening", port = port);
    if let Err(e) = axum::serve(listener, router(state)).await {
        tracing::error!(event = "inbox_api_serve_failed", error = %e);
    }
    api.set_port(None);
}

/// Resolve `<app_data>` the same way `lib.rs` does for `data.db`.
pub fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/projects", get(list_projects))
        .route("/v1/slack-threads", post(post_slack_thread))
        .route("/v1/web-pages", post(post_web_page))
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
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
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
    Json(json!({ "app": "devdy", "version": state.version, "api": API_VERSION })).into_response()
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

/// Header value as UTF-8 (raw non-ASCII bytes are accepted as UTF-8).
fn header_string(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .map(|v| String::from_utf8_lossy(v.as_bytes()).trim().to_string())
        .filter(|s| !s.is_empty())
}

async fn post_slack_thread(State(state): State<HttpState>, req: Request) -> Response {
    post_capture(state, CaptureKind::Slack, req).await
}

async fn post_web_page(State(state): State<HttpState>, req: Request) -> Response {
    post_capture(state, CaptureKind::Web, req).await
}

/// Shared POST handler: auth → content type → body (≤ 50 MB) → parse → upsert.
async fn post_capture(state: HttpState, capture_kind: CaptureKind, req: Request) -> Response {
    let headers = req.headers().clone();
    if let Some(r) = check_auth(&state, &headers) {
        return r;
    }
    let kind = match headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(PayloadKind::from_content_type)
    {
        Some(k) => k,
        None => {
            return error(
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "Content-Type must be application/zip, text/markdown or text/plain",
            )
        }
    };
    let too_large = || {
        error(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!("payload exceeds {} MB", MAX_BODY_BYTES / 1024 / 1024),
        )
    };
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > MAX_BODY_BYTES) {
        return too_large();
    }
    let body = match axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(_) => return too_large(),
    };
    if body.is_empty() {
        return error(StatusCode::BAD_REQUEST, "empty body");
    }

    // `X-Devdy-Mode: new` forces a new row; only web pages honour it
    // (`/v1/slack-threads` is unchanged).
    let force_new = capture_kind == CaptureKind::Web
        && header_string(&headers, "x-devdy-mode")
            .is_some_and(|m| m.eq_ignore_ascii_case("new"));
    let overrides = Overrides {
        project_id: header_string(&headers, "x-devdy-project-id"),
        title: header_string(&headers, "x-devdy-title"),
        force_new,
    };
    let parsed = match ingest::parse_payload(capture_kind, kind, &body) {
        Ok(p) => p,
        Err(e) => return ingest_error(e),
    };
    match store::ingest(&state.db, &state.app_data, parsed, overrides).await {
        Ok(result) => {
            (state.on_ingest)(&result);
            let status = match result.status {
                store::IngestStatus::Created => StatusCode::CREATED,
                store::IngestStatus::Updated => StatusCode::OK,
            };
            (status, Json(json!({ "id": result.id, "status": result.status }))).into_response()
        }
        Err(e) => ingest_error(e),
    }
}

fn ingest_error(e: IngestError) -> Response {
    let status = match &e {
        IngestError::BadRequest(_) => StatusCode::BAD_REQUEST,
        IngestError::TooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
        IngestError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
    };
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::error!(event = "inbox_api_ingest_failed", error = %e);
    }
    error(status, e.message())
}

#[cfg(test)]
mod tests {
    use super::super::ingest::tests::make_zip;
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Harness {
        base: String,
        port: u16,
        token: String,
        api: InboxApiState,
        app_data: PathBuf,
        ingested: Arc<AtomicUsize>,
        client: reqwest::Client,
    }

    /// Serve the real router on an ephemeral port against an in-memory DB.
    async fn spawn() -> Harness {
        let db = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
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
        let ingested = Arc::new(AtomicUsize::new(0));
        let counter = ingested.clone();
        let state = HttpState {
            db,
            app_data: app_data.clone(),
            api: api.clone(),
            version: "test".into(),
            port,
            on_ingest: Arc::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        };
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.unwrap();
        });
        Harness {
            base: format!("http://127.0.0.1:{}", port),
            port,
            token,
            api,
            app_data,
            ingested,
            client: reqwest::Client::new(),
        }
    }

    impl Harness {
        fn post(&self, ct: &str, body: Vec<u8>) -> reqwest::RequestBuilder {
            self.client
                .post(format!("{}/v1/slack-threads", self.base))
                .bearer_auth(&self.token)
                .header("Content-Type", ct)
                .body(body)
        }

        fn post_web(&self, body: &str) -> reqwest::RequestBuilder {
            self.client
                .post(format!("{}/v1/web-pages", self.base))
                .bearer_auth(&self.token)
                .header("Content-Type", "text/markdown")
                .body(body.to_string())
        }
    }

    async fn status_and_id(r: reqwest::RequestBuilder) -> (u16, String, String) {
        let r = r.send().await.unwrap();
        let code = r.status().as_u16();
        let v: serde_json::Value = r.json().await.unwrap();
        (
            code,
            v["id"].as_str().unwrap_or("").to_string(),
            v["status"].as_str().unwrap_or("").to_string(),
        )
    }

    #[tokio::test]
    async fn web_pages_endpoint() {
        let h = spawn().await;
        let page = |url: &str, extra: &str| format!("---\nurl: \"{}\"\n{}---\n# P\n", url, extra);
        let url = "https://example.com/docs/";

        let (code, id, status) = status_and_id(h.post_web(&page(url, ""))).await;
        assert_eq!((code, status.as_str()), (201, "created"));
        let (code, id2, status) =
            status_and_id(h.post_web(&page("https://Example.com/docs?utm_source=x#frag", ""))).await;
        assert_eq!((code, status.as_str(), id2.as_str()), (200, "updated", id.as_str()));
        let (code, id3, _) =
            status_and_id(h.post_web(&page(url, "")).header("X-Devdy-Mode", "new")).await;
        assert_eq!(code, 201);
        assert_ne!(id3, id);
        let (code, id4, _) = status_and_id(h.post_web(&page(url, "selection: true\n"))).await;
        assert_eq!(code, 201);
        assert_ne!(id4, id);
        assert_eq!(h.ingested.load(Ordering::SeqCst), 4);

        // Slack endpoint ignores X-Devdy-Mode (unchanged contract).
        let md = "---\nthread_url: \"https://x.slack.com/archives/C1/p9\"\n---\nx";
        let (c1, s1, _) = status_and_id(h.post("text/markdown", md.as_bytes().to_vec())).await;
        let (c2, s2, _) = status_and_id(
            h.post("text/markdown", md.as_bytes().to_vec()).header("X-Devdy-Mode", "new"),
        )
        .await;
        assert_eq!((c1, c2), (201, 200));
        assert_eq!(s1, s2);

        // Auth + CORS apply to the new route too.
        let r = h
            .client
            .post(format!("{}/v1/web-pages", h.base))
            .header("Content-Type", "text/markdown")
            .body("x")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401);
        let r = h
            .client
            .request(Method::OPTIONS, format!("{}/v1/web-pages", h.base))
            .header("Origin", "chrome-extension://abc")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 204);
        assert!(r
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("X-Devdy-Mode"));
        let _ = std::fs::remove_dir_all(&h.app_data);
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
        assert!(r
            .headers()
            .get("access-control-allow-headers")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("X-Devdy-Project-Id"));

        // Markdown create, then same thread_url → update.
        let md = "---\nthread_url: \"https://x.slack.com/archives/C1/p1\"\n---\n# Hi\n";
        let r = h
            .post("text/markdown; charset=utf-8", md.as_bytes().to_vec())
            .header("X-Devdy-Project-Id", "p1")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 201);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v["status"], "created");
        let id = v["id"].as_str().unwrap().to_string();
        let z = make_zip(&[("t.md", md.as_bytes()), ("a.png", b"png"), ("b.txt", b"txt")]);
        let r = h.post("application/zip", z).send().await.unwrap();
        assert_eq!(r.status(), 200);
        let v: serde_json::Value = r.json().await.unwrap();
        assert_eq!(v, json!({ "id": id, "status": "updated" }));
        assert_eq!(h.ingested.load(Ordering::SeqCst), 2);
        assert!(h.app_data.join("slack-threads").join(&id).join("files/a.png").exists());

        // Bad payloads.
        let r = h.post("application/json", b"{}".to_vec()).send().await.unwrap();
        assert_eq!(r.status(), 415);
        let slip = make_zip(&[("t.md", b"t"), ("../x", b"x")]);
        let r = h.post("application/zip", slip).send().await.unwrap();
        assert_eq!(r.status(), 400);
        let two = make_zip(&[("a.md", b"a"), ("b.md", b"b")]);
        let r = h.post("application/zip", two).send().await.unwrap();
        assert_eq!(r.status(), 400);
        let v: serde_json::Value = r.json().await.unwrap();
        assert!(v["error"].is_string());
        let r = h
            .post("text/markdown", b"x".to_vec())
            .header("X-Devdy-Project-Id", "nope")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 400);
        // Oversized body: announce Content-Length > 50 MB but send only the
        // headers over a raw socket. The server must answer 413 from the header
        // alone (before reading the body), so this is deterministic — streaming
        // the real 50 MB could race with the server closing the connection.
        {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut sock = tokio::net::TcpStream::connect(("127.0.0.1", h.port)).await.unwrap();
            let req = format!(
                "POST /v1/slack-threads HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
                 Authorization: Bearer {}\r\nContent-Type: application/zip\r\n\
                 Content-Length: {}\r\n\r\n",
                h.port,
                h.token,
                MAX_BODY_BYTES + 1
            );
            sock.write_all(req.as_bytes()).await.unwrap();
            let mut buf = vec![0u8; 1024];
            let n = sock.read(&mut buf).await.unwrap();
            let head = String::from_utf8_lossy(&buf[..n]);
            assert!(head.starts_with("HTTP/1.1 413"), "{}", head);
            assert!(head.contains("payload exceeds 50 MB"), "{}", head);
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
}
