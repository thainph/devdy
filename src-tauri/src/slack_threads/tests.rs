//! Integration tests for the Slack thread pipeline: a mock Slack Web API
//! (axum on 127.0.0.1), an in-memory DB, a temp `<app_data>` and a fake
//! translation engine. No real network.

use super::api::HttpConfig;
use super::translate::tests::FakeEngine;
use super::*;
use axum::extract::{Path as AxPath, Query, State as AxState};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::path::Path;

// ---- shared helpers (also used by inbox_api::server tests) -----------------

/// Fake token store: `server_id → token`, plus matching `mcp_servers` rows.
#[derive(Clone, Default)]
pub struct Tokens(Arc<Mutex<HashMap<String, String>>>);

impl Tokens {
    pub fn lookup(&self) -> SecretLookup {
        let map = self.0.clone();
        Arc::new(move |id: &str| map.lock().unwrap().get(id).cloned())
    }

    /// Register an MCP server `server_id` holding `token`.
    pub async fn set(&self, db: &Db, server_id: &str, token: &str) {
        sqlx::query(
            "INSERT OR IGNORE INTO mcp_servers (id, name, transport, env_keys, created_at) \
             VALUES (?, ?, 'stdio', '[\"SLACK_MCP_XOXP_TOKEN\"]', ?)",
        )
        .bind(server_id)
        .bind(server_id)
        .bind(format!(
            "2026-01-01T00:00:0{}Z",
            self.0.lock().unwrap().len()
        ))
        .execute(db)
        .await
        .unwrap();
        self.0
            .lock()
            .unwrap()
            .insert(server_id.to_string(), token.to_string());
    }
}

/// Records every event.
#[derive(Default)]
pub struct RecEvents {
    pub changed: Mutex<Vec<(String, String)>>,
    pub progress: Mutex<Vec<SlackThreadProgress>>,
    pub finished: Mutex<Vec<(String, bool)>>,
}

impl SlackEvents for RecEvents {
    fn changed(&self, id: &str, action: &str) {
        self.changed
            .lock()
            .unwrap()
            .push((id.to_string(), action.to_string()));
    }
    fn progress(&self, p: &SlackThreadProgress) {
        self.progress.lock().unwrap().push(p.clone());
    }
    fn api_job_finished(&self, id: &str, _title: &str, ready: bool) {
        self.finished.lock().unwrap().push((id.to_string(), ready));
    }
}

/// State without network access (jobs are not run).
pub fn offline_state(db: &Db, app_data: &Path, tokens: &Tokens) -> SlackThreadsState {
    SlackThreadsState::new(SlackCtx::new(
        db.clone(),
        app_data.to_path_buf(),
        Arc::new(RecEvents::default()),
        tokens.lookup(),
        HttpConfig {
            api_base: "http://127.0.0.1:9/api/".into(),
            ..Default::default()
        },
        Arc::new(FakeEngine::new()),
    ))
}

pub async fn memory_db() -> Db {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at) VALUES \
         ('p1', 'proj', '/tmp/p1', '2026-01-01'), ('p2', 'proj2', '/tmp/p2', '2026-01-01')",
    )
    .execute(&db)
    .await
    .unwrap();
    db
}

fn temp_dir(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "devdy-slack-{}-{}-{}",
        label,
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

// ---- mock Slack -------------------------------------------------------------

#[derive(Default)]
struct MockData {
    /// token → (team_id, team, url)
    teams: HashMap<String, (String, String, String)>,
    /// `x-oauth-scopes` header (None = omitted)
    scopes: Option<String>,
    /// channel → conversations.info response
    channel_info: HashMap<String, Value>,
    /// thread ts → pages of messages
    replies: HashMap<String, Vec<Vec<Value>>>,
    replies_error: Option<Value>,
    /// 429 answers left (then normal); usize::MAX = always
    rate_limit: usize,
    users: HashMap<String, Value>,
    /// file id → (status, content type, body)
    files: HashMap<String, (u16, String, Vec<u8>)>,
    /// file id → (chunks, gap): stream the body slowly (each chunk = `body`)
    slow: HashMap<String, (usize, Duration)>,
    /// (path, authorization header) of every request
    log: Vec<(String, Option<String>)>,
}

#[derive(Clone, Default)]
struct Mock(Arc<Mutex<MockData>>);

impl Mock {
    fn with<R>(&self, f: impl FnOnce(&mut MockData) -> R) -> R {
        f(&mut self.0.lock().unwrap())
    }

    fn requests(&self, prefix: &str) -> Vec<(String, Option<String>)> {
        self.with(|d| {
            d.log
                .iter()
                .filter(|(p, _)| p.starts_with(prefix))
                .cloned()
                .collect()
        })
    }

    fn record(&self, path: String, headers: &HeaderMap) -> Option<String> {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string);
        self.with(|d| d.log.push((path, auth.clone())));
        auth
    }
}

fn token_of(auth: &Option<String>) -> String {
    auth.as_deref()
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("")
        .to_string()
}

async fn api(
    AxState(mock): AxState<Mock>,
    AxPath(method): AxPath<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let auth = mock.record(format!("/api/{method}"), &headers);
    let token = token_of(&auth);
    let limited = mock.with(|d| {
        if d.rate_limit > 0 {
            if d.rate_limit != usize::MAX {
                d.rate_limit -= 1;
            }
            true
        } else {
            false
        }
    });
    if limited {
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [("retry-after", "1")],
            "slow down",
        )
            .into_response();
    }
    let Some((team_id, team, url)) = mock.with(|d| d.teams.get(&token).cloned()) else {
        return Json(json!({ "ok": false, "error": "invalid_auth" })).into_response();
    };
    match method.as_str() {
        "auth.test" => {
            let body = Json(json!({ "ok": true, "team_id": team_id, "team": team, "url": url }));
            match mock.with(|d| d.scopes.clone()) {
                Some(s) => ([("x-oauth-scopes", s)], body).into_response(),
                None => body.into_response(),
            }
        }
        "conversations.info" => {
            let ch = q.get("channel").cloned().unwrap_or_default();
            let v = mock.with(|d| d.channel_info.get(&ch).cloned());
            Json(v.unwrap_or_else(|| {
                json!({ "ok": true, "channel": { "id": ch, "name": format!("ch-{}", ch.to_lowercase()) } })
            }))
            .into_response()
        }
        "conversations.replies" => {
            if let Some(e) = mock.with(|d| d.replies_error.clone()) {
                return Json(e).into_response();
            }
            let ts = q.get("ts").cloned().unwrap_or_default();
            let page: usize = q.get("cursor").and_then(|c| c.parse().ok()).unwrap_or(0);
            let pages = mock.with(|d| d.replies.get(&ts).cloned());
            let Some(pages) = pages else {
                return Json(json!({ "ok": false, "error": "thread_not_found" })).into_response();
            };
            let next = if page + 1 < pages.len() {
                (page + 1).to_string()
            } else {
                String::new()
            };
            Json(json!({
                "ok": true,
                "messages": pages[page],
                "has_more": !next.is_empty(),
                "response_metadata": { "next_cursor": next }
            }))
            .into_response()
        }
        "users.info" => {
            let u = q.get("user").cloned().unwrap_or_default();
            match mock.with(|d| d.users.get(&u).cloned()) {
                Some(user) => Json(json!({ "ok": true, "user": user })).into_response(),
                None => Json(json!({ "ok": false, "error": "user_not_found" })).into_response(),
            }
        }
        _ => Json(json!({ "ok": false, "error": "unknown_method" })).into_response(),
    }
}

async fn file(
    AxState(mock): AxState<Mock>,
    AxPath(id): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    mock.record(format!("/files/{id}"), &headers);
    if let Some((chunks, gap)) = mock.with(|d| d.slow.get(&id).copied()) {
        let body = mock.with(|d| d.files[&id].2.clone());
        let stream = futures_util::stream::unfold(0usize, move |i| {
            let body = body.clone();
            async move {
                if i == chunks {
                    return None;
                }
                tokio::time::sleep(gap).await;
                Some((Ok::<_, std::io::Error>(body), i + 1))
            }
        });
        return (
            [("content-type", "application/octet-stream")],
            axum::body::Body::from_stream(stream),
        )
            .into_response();
    }
    match mock.with(|d| d.files.get(&id).cloned()) {
        Some((status, ct, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [("content-type", ct)],
            body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn avatar(
    AxState(mock): AxState<Mock>,
    AxPath(name): AxPath<String>,
    headers: HeaderMap,
) -> Response {
    mock.record(format!("/avatars/{name}"), &headers);
    ([("content-type", "image/png")], b"\x89PNGavatar".to_vec()).into_response()
}

struct Env {
    db: Db,
    app_data: PathBuf,
    mock: Mock,
    base: String,
    engine: Arc<FakeEngine>,
    events: Arc<RecEvents>,
    tokens: Tokens,
    state: SlackThreadsState,
}

const CH: &str = "C0B83GHDTFG";
const ROOT: &str = "1789877283.026359";
const LINK: &str = "https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359";
const TOKEN: &str = "xoxp-secret-1";

impl Env {
    async fn new(label: &str) -> Env {
        Self::with_engine(label, FakeEngine::new()).await
    }

    async fn with_engine(label: &str, engine: FakeEngine) -> Env {
        Self::with_config(label, engine, |_| {}).await
    }

    async fn with_config(
        label: &str,
        engine: FakeEngine,
        tweak: impl FnOnce(&mut HttpConfig),
    ) -> Env {
        let db = memory_db().await;
        let app_data = temp_dir(label);
        let mock = Mock::default();
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let router = Router::new()
            .route("/api/:method", get(api))
            .route("/files/:id", get(file))
            .route("/avatars/:name", get(avatar))
            .with_state(mock.clone());
        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let base = format!("http://127.0.0.1:{port}");
        let engine = Arc::new(engine);
        let events = Arc::new(RecEvents::default());
        let tokens = Tokens::default();
        tokens.set(&db, "srv1", TOKEN).await;
        mock.with(|d| {
            d.teams.insert(
                TOKEN.into(),
                (
                    "T1".into(),
                    "Malme".into(),
                    "https://malme.slack.com/".into(),
                ),
            );
            d.scopes = Some(
                "channels:history,groups:history,channels:read,groups:read,users:read,files:read"
                    .into(),
            );
        });
        let mut http = HttpConfig {
            api_base: format!("{base}/api/"),
            trusted_test_origin: Some(base.clone()),
            wait_unit: Duration::from_millis(5),
            timeout: Duration::from_secs(10),
            ..Default::default()
        };
        tweak(&mut http);
        let ctx = SlackCtx::new(
            db.clone(),
            app_data.clone(),
            events.clone(),
            tokens.lookup(),
            http,
            engine.clone(),
        );
        let state = SlackThreadsState::new(ctx);
        Env {
            db,
            app_data,
            mock,
            base,
            engine,
            events,
            tokens,
            state,
        }
    }

    fn msg(&self, ts: &str, user: &str, text: &str) -> Value {
        json!({ "type": "message", "ts": ts, "thread_ts": ROOT, "user": user, "text": text })
    }

    fn file_meta(&self, id: &str, name: &str, mime: &str, size: i64) -> Value {
        json!({
            "id": id, "name": name, "title": name, "mimetype": mime,
            "filetype": name.rsplit('.').next().unwrap(), "size": size, "created": 1789877283,
            "permalink": format!("https://malme.slack.com/files/U1/{id}/x"),
            "url_private_download": format!("{}/files/{id}", self.base),
        })
    }

    /// Default thread: parent (JA, mention, code, reaction, 2 files), a reply
    /// from U2 and an edited reply.
    fn seed_thread(&self) {
        let mut parent = self.msg(ROOT, "U1", "デプロイ失敗 <@U2> `npm run build` :fire:");
        parent["reactions"] = json!([{ "name": "+1", "count": 2 }]);
        parent["files"] = json!([
            self.file_meta("F1", "shot one.png", "image/png", 4),
            self.file_meta("F2", "log.pdf", "application/pdf", 3),
        ]);
        let r1 = self.msg(
            "1789877290.000100",
            "U2",
            "Looking into it\n```\nerror: <foo>\n```",
        );
        let mut r2 = self.msg(
            "1789877295.000200",
            "U1",
            "Fixed, see <https://ci.example.com|CI>",
        );
        r2["edited"] = json!({ "ts": "1789877299.000000" });
        let avatar = |u: &str| format!("{}/avatars/{u}.png", self.base);
        self.mock.with(|d| {
            d.replies.insert(ROOT.into(), vec![vec![parent, r1, r2]]);
            d.users.insert(
                "U1".into(),
                json!({ "name": "alice", "profile": { "display_name": "Alice", "real_name": "Alice A", "image_72": avatar("U1") } }),
            );
            d.users.insert(
                "U2".into(),
                json!({ "name": "bob", "profile": { "display_name": "", "real_name": "Bob B", "image_72": avatar("U2") } }),
            );
            d.files.insert("F1".into(), (200, "image/png".into(), b"\x89PNG".to_vec()));
            d.files.insert("F2".into(), (200, "application/pdf".into(), b"pdf".to_vec()));
        });
    }

    async fn add(&self, url: &str) -> String {
        self.state
            .add_thread(url, None, None, false)
            .await
            .unwrap()
            .id
    }

    /// Run the queued job(s) for `id` (and a re-run if one was flagged).
    async fn run(&self, id: &str) {
        let popped = self.state.pop();
        let (pid, api) = popped.unwrap_or((id.to_string(), false));
        self.state.run_one(&pid, api).await;
        while let Some((next, api)) = self.state.pop() {
            self.state.run_one(&next, api).await;
        }
    }

    async fn status(&self, id: &str) -> (String, Option<String>, Vec<String>) {
        let (s, c, w): (String, Option<String>, String) = sqlx::query_as(
            "SELECT fetch_status, fetch_error_code, warnings FROM slack_threads WHERE id = ?",
        )
        .bind(id)
        .fetch_one(&self.db)
        .await
        .unwrap();
        (s, c, serde_json::from_str(&w).unwrap())
    }

    async fn view(&self, id: &str) -> store::SlackThreadView {
        store::get_view(&self.db, &self.app_data, id).await.unwrap()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.app_data);
    }
}

// ---- tests ------------------------------------------------------------------

#[tokio::test]
async fn full_fetch_translate_and_view() {
    let env = Env::new("full").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    assert_eq!(env.status(&id).await.0, "pending");
    env.run(&id).await;
    assert_eq!(env.status(&id).await, ("ready".into(), None, vec![]));

    let v = env.view(&id).await;
    assert_eq!(v.messages.len(), 3);
    assert_eq!(v.summary.reply_count, 2);
    assert_eq!(v.summary.workspace.as_deref(), Some("Malme"));
    assert_eq!(
        v.summary.workspace_domain.as_deref(),
        Some("malme.slack.com")
    );
    assert_eq!(v.summary.team_id.as_deref(), Some("T1"));
    assert_eq!(v.summary.channel.as_deref(), Some("ch-c0b83ghdtfg"));
    let parent = &v.messages[0];
    assert_eq!(parent.seq, 0);
    assert_eq!(parent.author_name, "Alice");
    // Placeholders restored byte for byte inside the translation.
    assert_eq!(
        parent.text_vi.as_deref(),
        Some("VI:デプロイ失敗 <@U2> `npm run build` :fire:")
    );
    assert_eq!(parent.translate_status, "done");
    assert_eq!(
        parent.reactions,
        vec![store::SlackReaction {
            name: "+1".into(),
            count: 2
        }]
    );
    assert_eq!(parent.file_ids.len(), 2);
    assert!(v.messages[1]
        .text_vi
        .as_deref()
        .unwrap()
        .contains("```\nerror: <foo>\n```"));
    assert_eq!(
        v.messages[2].edited_ts.as_deref(),
        Some("1789877299.000000")
    );
    // Title from the translated parent; users incl. mentions; Bob falls back to real name.
    assert_eq!(
        v.summary.title,
        "VI:デプロイ失敗 @Bob B `npm run build` :fire:"
    );
    assert_eq!(v.users["U2"].display_name, "Bob B");
    assert!(v.users["U1"]
        .avatar_path
        .as_deref()
        .is_some_and(|p| Path::new(p).is_file()));
    assert_eq!(v.channels[CH], "ch-c0b83ghdtfg");

    // Files under slack-threads/<id>/files/<slack_file_id>/.
    let f1 = v
        .files
        .iter()
        .find(|f| f.slack_file_id.as_deref() == Some("F1"))
        .unwrap();
    assert_eq!(f1.download_status, "done");
    assert!(f1.is_image);
    let expected = env
        .app_data
        .join(format!("slack-threads/{id}/files/F1/shot_one.png"));
    assert_eq!(
        f1.file_path.as_deref(),
        Some(expected.to_string_lossy().as_ref())
    );
    assert_eq!(std::fs::read(&expected).unwrap(), b"\x89PNG");

    // Token on file requests, never on avatars (AC-21).
    for (_, auth) in env.mock.requests("/files/") {
        assert_eq!(auth.as_deref(), Some("Bearer xoxp-secret-1"));
    }
    let avatars = env.mock.requests("/avatars/");
    assert_eq!(avatars.len(), 2);
    assert!(avatars.iter().all(|(_, auth)| auth.is_none()));

    // Progress phases in order (AC-35), final `done`.
    let phases: Vec<String> = env
        .events
        .progress
        .lock()
        .unwrap()
        .iter()
        .map(|p| p.phase.clone())
        .fold(Vec::new(), |mut acc, p| {
            if acc.last() != Some(&p) {
                acc.push(p);
            }
            acc
        });
    assert_eq!(
        phases,
        vec![
            "queued",
            "auth",
            "messages",
            "users",
            "files",
            "translate",
            "done"
        ]
    );
    // Search transcript holds both versions with @names.
    let list = store::list(&env.db, None, Some("bob b")).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        list[0].preview,
        "VI:デプロイ失敗 @Bob B `npm run build` :fire:"
    );

    // The token never leaks into events or stored data (AC-20).
    let dump = format!(
        "{:?}{:?}",
        env.events.progress.lock().unwrap(),
        env.events.changed.lock().unwrap()
    );
    assert!(!dump.contains("xoxp-"));
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT COALESCE(fetch_error,'') || search_text || COALESCE(thread_url,'') FROM slack_threads",
    )
    .fetch_all(&env.db)
    .await
    .unwrap();
    assert!(rows.iter().all(|(r,)| !r.contains("xoxp-")));
    for p in env.engine.prompts.lock().unwrap().iter() {
        assert!(!p.contains("xoxp-"));
    }
}

#[tokio::test]
async fn refresh_is_incremental() {
    let env = Env::new("incr").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    let calls = env.engine.calls();
    let file_reqs = env.mock.requests("/files/").len();
    let f1 = env
        .app_data
        .join(format!("slack-threads/{id}/files/F1/shot_one.png"));
    let mtime = std::fs::metadata(&f1).unwrap().modified().unwrap();
    let vi_before = env.view(&id).await.messages[1].text_vi.clone();

    // Unchanged thread → 0 batches, no downloads (AC-26, AC-31).
    env.events.progress.lock().unwrap().clear();
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.engine.calls(), calls);
    assert_eq!(env.mock.requests("/files/").len(), file_reqs);
    assert_eq!(std::fs::metadata(&f1).unwrap().modified().unwrap(), mtime);
    assert!(env
        .events
        .progress
        .lock()
        .unwrap()
        .iter()
        .any(|p| p.phase == "translate" && p.total == Some(0)));
    assert_eq!(env.view(&id).await.messages[1].text_vi, vi_before);

    // Edit one message, add a reply, delete a reply, drop F2 (AC-27).
    env.mock.with(|d| {
        let pages = d.replies.get_mut(ROOT).unwrap();
        let msgs = &mut pages[0];
        msgs[0]["files"] = json!([msgs[0]["files"][0].clone()]);
        msgs[2]["text"] = json!("Fixed for real");
        msgs[2]["edited"] = json!({ "ts": "1789877400.000000" });
        msgs.remove(1);
        msgs.push(json!({ "type": "message", "ts": "1789877500.000300", "thread_ts": ROOT, "user": "U2", "text": "Thanks!" }));
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.engine.calls(), calls + 1);
    let last = env.engine.prompts.lock().unwrap().last().unwrap().clone();
    assert_eq!(last.matches("\n</m>\n").count(), 2);
    let v = env.view(&id).await;
    let texts: Vec<_> = v
        .messages
        .iter()
        .map(|m| m.text_vi.clone().unwrap())
        .collect();
    assert_eq!(texts[1], "VI:Fixed for real");
    assert_eq!(texts[2], "VI:Thanks!");
    assert_eq!(v.messages.len(), 3);
    assert_eq!(v.summary.reply_count, 2);
    assert_eq!(v.files.len(), 1);
    assert!(!env
        .app_data
        .join(format!("slack-threads/{id}/files/F2"))
        .exists());
}

#[tokio::test]
async fn slack_error_mapping() {
    // missing_scope on history.
    let env = Env::new("errs").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.mock.with(|d| {
        d.replies_error =
            Some(json!({ "ok": false, "error": "missing_scope", "needed": "channels:history" }))
    });
    env.run(&id).await;
    let (s, code, _) = env.status(&id).await;
    assert_eq!(
        (s.as_str(), code.as_deref()),
        ("error", Some("missing_scope"))
    );
    let fe: Option<String> =
        sqlx::query_scalar("SELECT fetch_error FROM slack_threads WHERE id = ?")
            .bind(&id)
            .fetch_one(&env.db)
            .await
            .unwrap();
    assert_eq!(fe.as_deref(), Some("channels:history"));

    // DM scope → dm_unsupported.
    env.mock.with(|d| {
        d.replies_error =
            Some(json!({ "ok": false, "error": "missing_scope", "needed": "mpim:history" }))
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.1.as_deref(), Some("dm_unsupported"));

    // is_mpim channel → dm_unsupported.
    env.mock.with(|d| {
        d.replies_error = None;
        d.channel_info.insert(
            CH.into(),
            json!({ "ok": true, "channel": { "id": CH, "is_mpim": true } }),
        );
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.1.as_deref(), Some("dm_unsupported"));

    // channel_not_found.
    env.mock.with(|d| {
        d.channel_info.insert(
            CH.into(),
            json!({ "ok": false, "error": "channel_not_found" }),
        );
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(
        env.status(&id).await.1.as_deref(),
        Some("channel_not_found")
    );

    // 429 twice then OK → ready.
    env.mock.with(|d| {
        d.channel_info.clear();
        d.rate_limit = 2;
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.0, "ready");

    // 429 forever → rate_limited; previous data kept (AC-16 style).
    env.mock.with(|d| d.rate_limit = usize::MAX);
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.1.as_deref(), Some("rate_limited"));
    assert_eq!(env.view(&id).await.messages.len(), 3);

    // Revoked token → invalid_auth, data still there.
    env.mock.with(|d| {
        d.rate_limit = 0;
        d.teams.clear();
    });
    env.state.ctx().auth_cache.clear();
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.1.as_deref(), Some("invalid_auth"));
    assert_eq!(env.view(&id).await.messages.len(), 3);
}

#[tokio::test]
async fn paginated_replies_keep_order() {
    let env = Env::new("pages").await;
    let pages: Vec<Vec<Value>> = (0..3)
        .map(|p| {
            (0..2)
                .map(|i| {
                    let ts = format!("17898772{:02}.000000", 83 + p * 2 + i);
                    let mut m = env.msg(&ts, "U1", &format!("m{}", p * 2 + i));
                    m["thread_ts"] = json!("1789877283.000000");
                    m
                })
                .collect()
        })
        .collect();
    let root = "1789877283.000000";
    env.mock.with(|d| {
        d.replies.insert(root.into(), pages);
    });
    let id = env
        .add("https://malme.slack.com/archives/C0B83GHDTFG/p1789877283000000")
        .await;
    env.run(&id).await;
    let v = env.view(&id).await;
    let raws: Vec<_> = v.messages.iter().map(|m| m.text_raw.clone()).collect();
    assert_eq!(raws, vec!["m0", "m1", "m2", "m3", "m4", "m5"]);
    assert_eq!(env.mock.requests("/api/conversations.replies").len(), 3);
}

#[tokio::test]
async fn missing_scopes_skip_steps_with_warnings() {
    let env = Env::new("scopes").await;
    env.seed_thread();
    env.mock
        .with(|d| d.scopes = Some("channels:history,groups:history,channels:read".into()));
    let id = env.add(LINK).await;
    env.run(&id).await;
    let (s, _, w) = env.status(&id).await;
    assert_eq!(s, "ready");
    assert_eq!(w, vec!["files_scope", "users_scope"]);
    assert!(env.mock.requests("/files/").is_empty());
    assert!(env.mock.requests("/api/users.info").is_empty());
    let v = env.view(&id).await;
    assert!(v
        .files
        .iter()
        .all(|f| f.download_status == "unavailable" && f.file_path.is_none()));
    assert!(v.files.iter().all(|f| f.permalink.is_some()));

    // No channel read scope → name unavailable, channel = id.
    env.mock
        .with(|d| d.scopes = Some("channels:history,files:read,users:read".into()));
    let id2 = env
        .add("https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359")
        .await;
    assert_eq!(id2, id);
    env.state.ctx().auth_cache.clear();
    env.run(&id).await;
    assert_eq!(env.status(&id).await.2, vec!["channel_name_unavailable"]);
}

#[tokio::test]
async fn token_selection_by_workspace() {
    let env = Env::new("tokens").await;
    env.seed_thread();
    // Second token for another workspace, registered later.
    env.tokens.set(&env.db, "srv2", "xoxp-other").await;
    env.mock.with(|d| {
        d.teams.insert(
            "xoxp-other".into(),
            ("T2".into(), "Acme".into(), "https://acme.slack.com/".into()),
        );
    });
    let id = env.add(LINK).await;
    env.run(&id).await;
    assert_eq!(env.status(&id).await.0, "ready");
    let replies = env.mock.requests("/api/conversations.replies");
    assert!(replies
        .iter()
        .all(|(_, a)| a.as_deref() == Some("Bearer xoxp-secret-1")));

    // A link from a workspace without a token.
    let other = env
        .add("https://nobody.slack.com/archives/C0000000009/p1789877283026359")
        .await;
    env.run(&other).await;
    assert_eq!(
        env.status(&other).await.1.as_deref(),
        Some("no_token_for_workspace")
    );
}

#[tokio::test]
async fn no_token_add_goes_straight_to_error() {
    let db = memory_db().await;
    let app_data = temp_dir("notoken");
    let tokens = Tokens::default();
    let st = offline_state(&db, &app_data, &tokens);
    let r = st.add_thread(LINK, Some("p1"), None, false).await.unwrap();
    assert_eq!((r.status, r.fetch_status.as_str()), ("created", "error"));
    assert_eq!(r.error_code.as_deref(), Some("no_token"));
    assert!(st.pop().is_none(), "nothing queued without a token");
    let status = token::token_status(&db, &tokens.lookup()).await.unwrap();
    assert_eq!(status.state, "none");
    // DM / bad link / unknown project never create a row.
    for (url, pid, code) in [
        (
            "https://malme.slack.com/archives/D0B83GHDTFG/p1789877283026359",
            None,
            "dm_unsupported",
        ),
        ("nope", None, "invalid_link"),
        (LINK, Some("missing"), "invalid_project"),
    ] {
        let e = st.add_thread(url, pid, None, false).await.unwrap_err();
        assert_eq!(e.code, code);
    }
    assert_eq!(store::list(&db, None, None).await.unwrap().len(), 1);
    // Tokens that don't look like Slack tokens are ignored; xoxb counts.
    tokens.set(&db, "a", "not-a-token").await;
    assert_eq!(
        token::token_status(&db, &tokens.lookup())
            .await
            .unwrap()
            .state,
        "none"
    );
    tokens.set(&db, "b", "xoxb-1").await;
    tokens.set(&db, "c", "xoxp-2").await;
    let s = token::token_status(&db, &tokens.lookup()).await.unwrap();
    assert_eq!(s.state, "multiple");
    assert_eq!(s.sources.len(), 2);
    assert!(!serde_json::to_string(&s).unwrap().contains("xox"));
    let _ = std::fs::remove_dir_all(&app_data);
}

#[tokio::test]
async fn reimport_keeps_title_project_position() {
    let db = memory_db().await;
    let app_data = temp_dir("reimport");
    let tokens = Tokens::default();
    tokens.set(&db, "s", TOKEN).await;
    let st = offline_state(&db, &app_data, &tokens);
    let a = st.add_thread(LINK, None, None, false).await.unwrap();
    let b = st
        .add_thread(
            "https://malme.slack.com/archives/C0000000002/p1789877283026359",
            None,
            None,
            false,
        )
        .await
        .unwrap();
    // New rows go on top.
    let ids: Vec<_> = store::list(&db, None, None)
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(ids, vec![b.id.clone(), a.id.clone()]);
    store::rename(&db, &a.id, "  My thread ").await.unwrap();
    store::set_project(&db, &a.id, Some("p1")).await.unwrap();
    store::reorder(&db, &[a.id.clone(), b.id.clone()])
        .await
        .unwrap();
    while st.pop().is_some() {}

    // Same thread via a reply link + other project + title → all kept.
    let reply = "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG";
    let again = st
        .add_thread(reply, Some("p2"), Some("Other"), true)
        .await
        .unwrap();
    assert_eq!(
        (again.id.as_str(), again.status),
        (a.id.as_str(), "existing")
    );
    let s = store::get_summary(&db, &a.id).await.unwrap();
    assert_eq!((s.title.as_str(), s.title_custom), ("My thread", true));
    assert_eq!(s.project_id.as_deref(), Some("p1"));
    assert_eq!(s.position, 0);
    assert_eq!(s.fetch_status, "pending");
    // Thread without project gets the requested one; title applies when not custom.
    let again = st
        .add_thread(&b_url(), Some("p2"), Some("Named"), true)
        .await
        .unwrap();
    assert_eq!(again.id, b.id);
    let s = store::get_summary(&db, &b.id).await.unwrap();
    assert_eq!(s.project_id.as_deref(), Some("p2"));
    assert_eq!((s.title.as_str(), s.title_custom), ("Named", true));
    assert_eq!(store::list(&db, None, None).await.unwrap().len(), 2);

    // Rename validation.
    assert_eq!(
        store::rename(&db, &a.id, " ").await.unwrap_err().code,
        "invalid_title"
    );
    assert_eq!(
        store::rename(&db, "zz", "x").await.unwrap_err().code,
        "not_found"
    );
    assert_eq!(
        store::set_project(&db, &a.id, Some("nope"))
            .await
            .unwrap_err()
            .code,
        "invalid_project"
    );
    let _ = std::fs::remove_dir_all(&app_data);
}

fn b_url() -> String {
    "https://malme.slack.com/archives/C0000000002/p1789877283026359".into()
}

#[tokio::test]
async fn running_job_gets_a_single_rerun() {
    let db = memory_db().await;
    let app_data = temp_dir("rerun");
    let tokens = Tokens::default();
    tokens.set(&db, "s", TOKEN).await;
    let st = offline_state(&db, &app_data, &tokens);
    let r = st.add_thread(LINK, None, None, false).await.unwrap();
    let (id, _) = st.pop().unwrap();
    assert_eq!(id, r.id);
    assert!(st.is_running(&id));
    sqlx::query("UPDATE slack_threads SET fetch_status = 'fetching' WHERE id = ?")
        .bind(&id)
        .execute(&db)
        .await
        .unwrap();
    for _ in 0..3 {
        st.refresh(&id).await.unwrap();
    }
    st.add_thread(LINK, None, None, true).await.unwrap();
    // Still fetching (not reset to pending), nothing queued, one re-run flagged.
    assert_eq!(
        store::get_summary(&db, &id).await.unwrap().fetch_status,
        "fetching"
    );
    {
        let q = st.queue.lock().unwrap();
        assert!(q.pending.is_empty());
        assert_eq!(q.rerun.len(), 1);
        assert_eq!(q.rerun.get(&id), Some(&true));
    }
    let _ = std::fs::remove_dir_all(&app_data);
}

#[tokio::test]
async fn files_html_and_too_large() {
    let env = Env::new("files").await;
    env.seed_thread();
    env.mock.with(|d| {
        d.files.insert(
            "F1".into(),
            (
                200,
                "text/html; charset=utf-8".into(),
                b"<html>login</html>".to_vec(),
            ),
        );
        d.files.insert(
            "F2".into(),
            (403, "application/json".into(), b"{}".to_vec()),
        );
        let parent = &mut d.replies.get_mut(ROOT).unwrap()[0][0];
        let mut big = parent["files"][1].clone();
        big["id"] = json!("F3");
        big["size"] = json!(200 * 1024 * 1024);
        parent["files"].as_array_mut().unwrap().push(big);
    });
    let id = env.add(LINK).await;
    env.run(&id).await;
    let (s, _, w) = env.status(&id).await;
    assert_eq!(s, "ready");
    assert_eq!(w, vec!["files_partial", "files_scope"]);
    let v = env.view(&id).await;
    let by = |sid: &str| {
        v.files
            .iter()
            .find(|f| f.slack_file_id.as_deref() == Some(sid))
            .unwrap()
            .clone()
    };
    assert_eq!(by("F1").download_status, "error");
    assert_eq!(
        by("F1").download_error.as_deref(),
        Some("no access to file (files:read?)")
    );
    assert_eq!(by("F2").download_status, "error");
    assert_eq!(by("F3").download_status, "too_large");
    assert!(env.mock.requests("/files/F3").is_empty());
    assert_eq!(v.messages.len(), 3);
    // Leftover .part files are cleaned up.
    let dir = env.app_data.join(format!("slack-threads/{id}/files/F1"));
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().count() == 0);
}

#[tokio::test]
async fn delete_removes_rows_and_dir_but_keeps_avatars() {
    let env = Env::new("delete").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    let dir = env.app_data.join(format!("slack-threads/{id}"));
    assert!(dir.exists());
    env.state.delete(std::slice::from_ref(&id)).await.unwrap();
    assert!(!dir.exists());
    for table in ["slack_threads", "slack_messages", "slack_files"] {
        let n: i64 = sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(&env.db)
            .await
            .unwrap();
        assert_eq!(n, 0, "{table}");
    }
    assert!(
        env.app_data
            .join("slack-cache/avatars/T1")
            .read_dir()
            .unwrap()
            .count()
            > 0
    );
    assert!(env
        .events
        .changed
        .lock()
        .unwrap()
        .contains(&(id.clone(), "deleted".into())));
    // A job for a deleted row stops without re-creating the dir.
    assert_eq!(
        job::run_job(env.state.ctx(), &id).await,
        job::JobEnd::Deleted
    );
    assert!(!dir.exists());
}

#[tokio::test]
async fn translation_partial_and_total_failure() {
    let mut engine = FakeEngine::new();
    engine.drop = vec!["Looking".into()];
    let env = Env::with_engine("partial", engine).await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    let (s, _, w) = env.status(&id).await;
    assert_eq!(
        (s.as_str(), w),
        ("ready", vec!["translate_partial".to_string()])
    );
    let v = env.view(&id).await;
    assert_eq!(v.messages[1].translate_status, "error");
    assert_eq!(
        v.messages[1].translate_error.as_deref(),
        Some("missing_block")
    );
    assert!(v.messages[1].text_vi.is_none());

    let mut engine = FakeEngine::new();
    engine.unavailable = true;
    let env = Env::with_engine("allfail", engine).await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    let (s, code, _) = env.status(&id).await;
    assert_eq!(
        (s.as_str(), code.as_deref()),
        ("error", Some("translate_failed"))
    );
    // Originals stay readable, the title is derived from the original parent
    // and original-text search still works (B-4).
    let v = env.view(&id).await;
    assert_eq!(v.messages.len(), 3);
    assert_eq!(
        v.summary.title,
        "デプロイ失敗 @Bob B `npm run build` :fire:"
    );
    assert_eq!(
        store::list(&env.db, None, Some("looking into"))
            .await
            .unwrap()
            .len(),
        1
    );
    let detail: Option<String> =
        sqlx::query_scalar("SELECT fetch_error FROM slack_threads WHERE id = ?")
            .bind(&id)
            .fetch_one(&env.db)
            .await
            .unwrap();
    assert_eq!(detail.as_deref(), Some("no engine"));
}

#[test]
fn engine_errors_are_english() {
    use crate::commands::translate as t;
    assert_eq!(
        english_engine_error(t::ERR_SESSION_ENDED),
        "translation ended unexpectedly"
    );
    assert_eq!(
        english_engine_error(t::ERR_NOTHING_TO_TRANSLATE),
        "nothing to translate"
    );
    assert_eq!(
        english_engine_error(&format!("{}broken pipe", t::ERR_SEND_PREFIX)),
        "failed to send the request: broken pipe"
    );
    assert_eq!(
        english_engine_error("Failed to spawn sidecar (node): x"),
        "Failed to spawn sidecar (node): x"
    );
    assert_eq!(english_engine_error("Lỗi lạ"), "translation engine error");
}

/// B-1: a download slower than the 30 s API timeout still completes while it
/// keeps making progress; a stalled one fails on the idle timeout.
#[tokio::test]
async fn slow_downloads_use_an_idle_timeout() {
    let env = Env::with_config("slow", FakeEngine::new(), |h| {
        h.timeout = Duration::from_millis(300);
        h.download_idle_timeout = Duration::from_millis(400);
    })
    .await;
    env.seed_thread();
    env.mock.with(|d| {
        // F1: 8 × 100 ms ≈ 0.8 s total, never idle > 400 ms.
        d.slow.insert("F1".into(), (8, Duration::from_millis(100)));
        // F2: a single chunk after 1 s of silence → stalled.
        d.slow.insert("F2".into(), (1, Duration::from_millis(1000)));
        let parent = &mut d.replies.get_mut(ROOT).unwrap()[0][0];
        parent["files"][0]["size"] = json!(32);
    });
    let id = env.add(LINK).await;
    env.run(&id).await;
    let v = env.view(&id).await;
    let by = |sid: &str| {
        v.files
            .iter()
            .find(|f| f.slack_file_id.as_deref() == Some(sid))
            .unwrap()
            .clone()
    };
    assert_eq!(by("F1").download_status, "done");
    let p = by("F1").file_path.unwrap();
    assert_eq!(std::fs::read(p).unwrap().len(), 32);
    assert_eq!(by("F2").download_status, "error");
    assert_eq!(by("F2").download_error.as_deref(), Some("download stalled"));
    assert_eq!(env.status(&id).await.2, vec!["files_partial"]);
}

/// B-5: a file that becomes unavailable loses its on-disk copy.
#[tokio::test]
async fn unavailable_file_removes_old_copy() {
    let env = Env::new("unavail").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    let dir = env.app_data.join(format!("slack-threads/{id}/files/F1"));
    assert!(dir.exists());
    env.mock.with(|d| {
        let parent = &mut d.replies.get_mut(ROOT).unwrap()[0][0];
        parent["files"][0]["mode"] = json!("hidden_by_limit");
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    let v = env.view(&id).await;
    let f1 = v
        .files
        .iter()
        .find(|f| f.slack_file_id.as_deref() == Some("F1"))
        .unwrap();
    assert_eq!(f1.download_status, "unavailable");
    assert!(f1.file_path.is_none());
    assert!(!dir.exists());
}

/// B-6 / AC-38: deleting a thread while its files download leaves nothing.
#[tokio::test]
async fn delete_during_download_leaves_no_files() {
    let env = Env::with_config("deldl", FakeEngine::new(), |h| {
        h.download_idle_timeout = Duration::from_secs(5);
    })
    .await;
    env.seed_thread();
    env.mock.with(|d| {
        d.slow.insert("F1".into(), (10, Duration::from_millis(100)));
        d.slow.insert("F2".into(), (10, Duration::from_millis(100)));
    });
    let id = env.add(LINK).await;
    let (pid, api) = env.state.pop().unwrap();
    let st = env.state.clone();
    let job = tokio::spawn(async move { job::run_job(st.ctx(), &pid).await });
    // Wait until both downloads are streaming, then delete the thread.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while env.mock.requests("/files/").len() < 2 {
        assert!(tokio::time::Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    env.state.delete(std::slice::from_ref(&id)).await.unwrap();
    assert_eq!(job.await.unwrap(), job::JobEnd::Deleted);
    let _ = api;
    // Give any straggler a chance to (wrongly) write.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!env.app_data.join(format!("slack-threads/{id}")).exists());
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_files")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// B-2: equal `updated_at` duplicates pick the same survivor (smaller id),
/// even when both jobs run at once.
#[tokio::test]
async fn equal_timestamp_duplicates_keep_one_row() {
    let env = Env::new("tie").await;
    env.seed_thread();
    let odd = "<https://Malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG>";
    legacy_row(&env, "b-row", Some(odd), None, "2026-10-05").await;
    legacy_row(&env, "a-row", Some(odd), None, "2026-10-05").await;
    let ctx = env.state.ctx().clone();
    let ctx2 = ctx.clone();
    let (ra, rb) = tokio::join!(
        async move { job::run_job(&ctx, "a-row").await },
        async move { job::run_job(&ctx2, "b-row").await },
    );
    let all = store::list(&env.db, None, None).await.unwrap();
    assert_eq!(all.len(), 1, "{ra:?} {rb:?}");
    assert_eq!(all[0].id, "a-row");
    assert_eq!(rb, job::JobEnd::MergedInto("a-row".into()));
}

/// B-3: merging into a row whose job is running only flags a re-run.
#[tokio::test]
async fn merge_into_running_row_flags_rerun() {
    let env = Env::new("mergerun").await;
    env.seed_thread();
    legacy_row(&env, "keep", Some(LINK), Some(CH), "2026-10-09").await;
    let odd = "<https://Malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG>";
    legacy_row(&env, "dup", Some(odd), None, "2026-10-01").await;
    sqlx::query("UPDATE slack_threads SET fetch_status = 'fetching' WHERE id = 'keep'")
        .execute(&env.db)
        .await
        .unwrap();
    // "keep" is being processed by another worker.
    env.state
        .queue
        .lock()
        .unwrap()
        .running
        .insert("keep".into());
    env.state.run_one("dup", false).await;
    let s = store::get_summary(&env.db, "keep").await.unwrap();
    assert_eq!(s.fetch_status, "fetching");
    let q = env.state.queue.lock().unwrap();
    assert!(q.pending.is_empty());
    assert_eq!(q.rerun.get("keep"), Some(&false));
}

#[test]
fn merge_rule_is_deterministic() {
    use super::job::keep_other;
    assert!(keep_other("2026-01-01", "b", "2026-01-02", "a"));
    assert!(!keep_other("2026-01-02", "b", "2026-01-01", "a"));
    assert!(keep_other("2026-01-01", "b", "2026-01-01", "a"));
    assert!(!keep_other("2026-01-01", "a", "2026-01-01", "b"));
}

#[tokio::test]
async fn api_jobs_notify_ui_jobs_do_not() {
    let env = Env::new("notify").await;
    env.seed_thread();
    let id = env
        .state
        .add_thread(LINK, None, None, true)
        .await
        .unwrap()
        .id;
    env.run(&id).await;
    assert_eq!(
        *env.events.finished.lock().unwrap(),
        vec![(id.clone(), true)]
    );
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    assert_eq!(env.events.finished.lock().unwrap().len(), 1);
}

/// Insert a legacy row like migration 0041 produces.
async fn legacy_row(
    env: &Env,
    id: &str,
    url: Option<&str>,
    channel_id: Option<&str>,
    updated: &str,
) {
    sqlx::query(
        "INSERT INTO slack_threads (id, title, title_custom, project_id, position, created_at, \
         updated_at, channel_id, thread_ts, thread_url, fetch_status, legacy, legacy_content) \
         VALUES (?, ?, 1, 'p1', 5, '2026-10-01', ?, ?, ?, ?, 'pending', 1, '# Old body')",
    )
    .bind(id)
    .bind(format!("Title {id}"))
    .bind(updated)
    .bind(channel_id)
    .bind(channel_id.map(|_| ROOT))
    .bind(url)
    .execute(&env.db)
    .await
    .unwrap();
    let dir = env.app_data.join(format!("slack-threads/{id}/files"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("old.png"), b"old").unwrap();
    sqlx::query(
        "INSERT INTO slack_files (id, thread_id, name, size, file_path, download_status, created_at) \
         VALUES (?, ?, 'old.png', 3, ?, 'done', '2026-10-01')",
    )
    .bind(format!("att-{id}"))
    .bind(id)
    .bind(format!("slack-threads/{id}/files/old.png"))
    .execute(&env.db)
    .await
    .unwrap();
}

#[tokio::test]
async fn legacy_refetch_replaces_snapshot() {
    let env = Env::new("legacy").await;
    env.seed_thread();
    legacy_row(&env, "leg-1", Some(LINK), Some(CH), "2026-10-02").await;
    let v = env.view("leg-1").await;
    assert!(v.summary.legacy);
    assert_eq!(v.legacy_content.as_deref(), Some("# Old body"));
    assert_eq!(
        v.files[0]
            .file_path
            .as_deref()
            .map(Path::new)
            .map(Path::is_file),
        Some(true)
    );
    let text = store::prompt_text(&env.db, &env.app_data, "leg-1", false)
        .await
        .unwrap();
    assert!(text.text.contains("# Old body"));

    env.state.startup_recover().await;
    env.run("leg-1").await;
    let s = env.status("leg-1").await;
    assert_eq!(s.0, "ready");
    let v = env.view("leg-1").await;
    assert!(!v.summary.legacy);
    assert!(v.legacy_content.is_none());
    assert_eq!(v.summary.title, "Title leg-1");
    assert_eq!(v.summary.project_id.as_deref(), Some("p1"));
    assert_eq!(v.summary.position, 5);
    assert!(v.files.iter().all(|f| f.slack_file_id.is_some()));
    assert!(!env
        .app_data
        .join("slack-threads/leg-1/files/old.png")
        .exists());
    assert!(env
        .app_data
        .join("slack-threads/leg-1/files/F1/shot_one.png")
        .exists());
}

#[tokio::test]
async fn legacy_without_url_stays_viewable() {
    let env = Env::new("legacy-nourl").await;
    legacy_row(&env, "leg-2", None, None, "2026-10-02").await;
    assert_eq!(
        env.state.refresh("leg-2").await.unwrap_err().code,
        "legacy_no_url"
    );
    env.state.startup_recover().await;
    env.run("leg-2").await;
    assert_eq!(
        env.status("leg-2").await.1.as_deref(),
        Some("legacy_no_url")
    );
    let v = env.view("leg-2").await;
    assert_eq!(v.legacy_content.as_deref(), Some("# Old body"));
    assert_eq!(v.files.len(), 1);
    assert!(env
        .app_data
        .join("slack-threads/leg-2/files/old.png")
        .exists());
}

#[tokio::test]
async fn duplicate_legacy_rows_are_merged_into_the_newest() {
    let env = Env::new("merge").await;
    env.seed_thread();
    // Older row with an SQL-parsable key, newer one whose URL only the full
    // parser understands (query string) → channel_id NULL.
    legacy_row(&env, "old-row", Some(LINK), Some(CH), "2026-10-01").await;
    let odd = "<https://Malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG>";
    legacy_row(&env, "new-row", Some(odd), None, "2026-10-05").await;
    env.state.startup_recover().await;
    env.run("old-row").await;
    // old-row's job found the newer twin first? Either way one row survives.
    let all = store::list(&env.db, None, None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].id, "new-row");
    assert_eq!(all[0].title, "Title new-row");
    assert_eq!(all[0].fetch_status, "ready");
    assert!(!env.app_data.join("slack-threads/old-row").exists());
    assert!(env
        .events
        .changed
        .lock()
        .unwrap()
        .contains(&("old-row".to_string(), "deleted".to_string())));
}

#[tokio::test]
async fn reply_link_without_thread_ts_resolves_to_the_root() {
    let env = Env::new("reply").await;
    env.seed_thread();
    let reply_ts = "1789877290.000100";
    env.mock.with(|d| {
        let reply = d.replies[ROOT][0][1].clone();
        d.replies.insert(reply_ts.into(), vec![vec![reply]]);
    });
    let root_id = env.add(LINK).await;
    env.run(&root_id).await;
    let id = env
        .add("https://malme.slack.com/archives/C0B83GHDTFG/p1789877290000100")
        .await;
    assert_ne!(id, root_id);
    env.run(&id).await;
    // The newer row wins the merge and now carries the root key.
    let all = store::list(&env.db, None, None).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].thread_ts.as_deref(), Some(ROOT));
    assert_eq!(all[0].thread_url.as_deref(), Some(LINK));
    assert_eq!(all[0].reply_count, 2);
}

#[tokio::test]
async fn startup_recover_resets_and_cleans_orphans() {
    let env = Env::new("recover").await;
    for (id, status, code, legacy) in [
        ("a", "fetching", None, 0),
        ("b", "translating", None, 0),
        ("c", "error", Some("network"), 0),
        ("d", "error", Some("thread_not_found"), 0),
        ("e", "error", Some("thread_not_found"), 1),
        ("f", "ready", None, 0),
    ] {
        sqlx::query(
            "INSERT INTO slack_threads (id, created_at, updated_at, fetch_status, fetch_error_code, legacy) \
             VALUES (?, '2026', '2026', ?, ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(code)
        .bind(legacy)
        .execute(&env.db)
        .await
        .unwrap();
    }
    sqlx::query("INSERT INTO slack_legacy_orphans (id) VALUES ('gone'), ('../evil')")
        .execute(&env.db)
        .await
        .unwrap();
    std::fs::create_dir_all(env.app_data.join("slack-threads/gone/files")).unwrap();
    env.state.startup_recover().await;
    let statuses: Vec<(String, String)> =
        sqlx::query_as("SELECT id, fetch_status FROM slack_threads ORDER BY id")
            .fetch_all(&env.db)
            .await
            .unwrap();
    let pending: Vec<_> = statuses
        .iter()
        .filter(|(_, s)| s == "pending")
        .map(|(i, _)| i.as_str())
        .collect();
    assert_eq!(pending, vec!["a", "b", "c", "e"]);
    assert!(!env.app_data.join("slack-threads/gone").exists());
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_legacy_orphans")
        .fetch_one(&env.db)
        .await
        .unwrap();
    assert_eq!(n, 0);
    assert_eq!(env.state.queue.lock().unwrap().pending.len(), 4);
}

#[tokio::test]
async fn search_is_unicode_case_insensitive_and_prompt_text_works() {
    let env = Env::new("search").await;
    env.seed_thread();
    let id = env.add(LINK).await;
    env.run(&id).await;
    store::rename(&env.db, &id, "Triển khai ＡＢＣ")
        .await
        .unwrap();
    for q in ["TRIỂN", "triển", "ａｂｃ", "デプロイ", "vi:fixed"] {
        assert_eq!(
            store::list(&env.db, None, Some(q)).await.unwrap().len(),
            1,
            "{q}"
        );
    }
    assert!(store::list(&env.db, None, Some("zzz"))
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        store::list(&env.db, Some("p1"), None).await.unwrap().len(),
        0
    );

    let p = store::prompt_text(&env.db, &env.app_data, &id, true)
        .await
        .unwrap();
    assert!(!p.truncated);
    assert!(p.text.starts_with(&format!(
        "[Slack] Triển khai ＡＢＣ (id: {id}) — #ch-c0b83ghdtfg · Malme · {LINK}\n"
    )));
    assert!(p
        .text
        .contains("Alice (2026-09-20 04:08 UTC): VI:デプロイ失敗 @Bob B"));
    assert!(p.text.contains("  > Gốc: デプロイ失敗 @Bob B"));
    assert!(p.text.contains("[CI](https://ci.example.com)"));
    let f1 = env
        .app_data
        .join(format!("slack-threads/{id}/files/F1/shot_one.png"));
    assert!(p
        .text
        .contains(&format!("  📎 shot one.png — {}", f1.display())));
    assert_eq!(p.chars, p.text.chars().count());

    // New-reply badge: seen → new reply → refresh → 1 new.
    store::mark_seen(&env.db, &id).await.unwrap();
    assert_eq!(store::get_summary(&env.db, &id).await.unwrap().new_count, 0);
    env.mock.with(|d| {
        d.replies.get_mut(ROOT).unwrap()[0].push(json!({
            "type": "message", "ts": "1789877600.000000", "thread_ts": ROOT, "user": "U1", "text": "new"
        }));
    });
    env.state.refresh(&id).await.unwrap();
    env.run(&id).await;
    let s = store::get_summary(&env.db, &id).await.unwrap();
    assert_eq!(s.new_count, 1);
    assert_eq!(s.title, "Triển khai ＡＢＣ");
    assert_eq!(s.last_reply_at.as_deref(), Some("2026-09-20T04:13:20.000Z"));
}

/// AC-01: migration 0041 on a fixture with slack + web captures, duplicate
/// legacy rows, a row without URL and attachments for each.
#[tokio::test]
#[allow(clippy::type_complexity)]
async fn migration_0041_moves_slack_captures() {
    use sqlx::migrate::Migrator;
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    let full = sqlx::migrate!("./migrations");
    let upto_0040 = Migrator {
        migrations: std::borrow::Cow::Owned(
            full.migrations
                .iter()
                .filter(|m| m.version <= 40)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: full.locking,
        no_tx: full.no_tx,
    };
    upto_0040.run(&pool).await.unwrap();
    for (id, kind, url, updated, created, title, custom, project, pos) in [
        (
            "dup-old",
            "slack",
            Some(LINK),
            "2026-10-01",
            "2026-09-01",
            "Old title",
            0,
            None,
            3,
        ),
        (
            "dup-new",
            "slack",
            Some(LINK),
            "2026-10-05",
            "2026-09-02",
            "New title",
            1,
            Some("p1"),
            7,
        ),
        (
            "no-url",
            "slack",
            None,
            "2026-10-03",
            "2026-09-03",
            "No url",
            1,
            None,
            1,
        ),
        (
            "web-1",
            "web",
            None,
            "2026-10-04",
            "2026-09-04",
            "Web",
            0,
            None,
            0,
        ),
    ] {
        sqlx::query(
            "INSERT INTO captures (id, kind, title, content, thread_url, channel, title_custom, \
             project_id, position, created_at, updated_at, source_url) \
             VALUES (?, ?, ?, ?, ?, '#dev', ?, ?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(kind)
        .bind(title)
        .bind(format!("body of {id}"))
        .bind(url)
        .bind(custom)
        .bind(project)
        .bind(pos)
        .bind(created)
        .bind(updated)
        .bind((kind == "web").then_some("https://example.com"))
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO capture_attachments (id, capture_id, name, file_path, size, mime, created_at) \
             VALUES (?, ?, 'a.png', ?, 3, 'image/png', '2026-10-08')",
        )
        .bind(format!("att-{id}"))
        .bind(id)
        .bind(format!("x/{id}/files/a.png"))
        .execute(&pool)
        .await
        .unwrap();
    }
    let upto_0041 = Migrator {
        migrations: std::borrow::Cow::Owned(
            full.migrations
                .iter()
                .filter(|m| m.version <= 41)
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: full.locking,
        no_tx: full.no_tx,
    };
    upto_0041.run(&pool).await.unwrap();

    let rows: Vec<(String, String, i64, Option<String>, i64, String, Option<String>, Option<String>, i64, String, Option<String>, Option<String>)> =
        sqlx::query_as(
            "SELECT id, title, title_custom, project_id, position, created_at, channel_id, \
             thread_ts, legacy, fetch_status, legacy_content, channel FROM slack_threads ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    let new = &rows[0];
    assert_eq!(
        (
            new.0.as_str(),
            new.1.as_str(),
            new.2,
            new.3.as_deref(),
            new.4,
            new.5.as_str()
        ),
        ("dup-new", "New title", 1, Some("p1"), 7, "2026-09-02")
    );
    assert_eq!(new.6.as_deref(), Some(CH));
    assert_eq!(new.7.as_deref(), Some(ROOT));
    assert_eq!(new.11.as_deref(), Some("dev"));
    let nourl = &rows[1];
    assert_eq!(nourl.0, "no-url");
    assert!(nourl.6.is_none() && nourl.7.is_none());
    assert_eq!((nourl.8, nourl.9.as_str()), (1, "pending"));
    assert_eq!(nourl.10.as_deref(), Some("body of no-url"));

    let files: Vec<(String, String, Option<String>, String)> = sqlx::query_as(
        "SELECT id, thread_id, file_path, download_status FROM slack_files ORDER BY id",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        files,
        vec![
            (
                "att-dup-new".into(),
                "dup-new".into(),
                Some("x/dup-new/files/a.png".into()),
                "done".into()
            ),
            (
                "att-no-url".into(),
                "no-url".into(),
                Some("x/no-url/files/a.png".into()),
                "done".into()
            ),
        ]
    );
    let orphans: Vec<String> = sqlx::query_scalar("SELECT id FROM slack_legacy_orphans")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(orphans, vec!["dup-old"]);
    let caps: Vec<String> = sqlx::query_scalar("SELECT id FROM captures")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(caps, vec!["web-1"]);
    let atts: Vec<String> = sqlx::query_scalar("SELECT id FROM capture_attachments")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(atts, vec!["att-web-1"]);
    // Index names collide no more: the new table has its own indexes.
    let idx: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'slack_threads' ORDER BY name",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    for name in [
        "idx_slack_threads_key",
        "idx_slack_threads_position",
        "idx_slack_threads_project",
        "idx_slack_threads_status",
    ] {
        assert!(idx.iter().any(|i| i == name), "{name}: {idx:?}");
    }
    let temp: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM sqlite_temp_master WHERE name = '_slack_legacy'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(temp, 0);

    // 0042 drops the captures core; Slack data is untouched.
    full.run(&pool).await.unwrap();
    let gone: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('captures', 'capture_attachments') \
         OR name LIKE 'idx_captures%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(gone, 0);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_files")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 2);
}

// ---- session (run) links ----------------------------------------------------

async fn insert_run(db: &Db, id: &str, project_id: &str, title: Option<&str>, activity: &str) {
    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, created_at, title, \
         last_activity_at) VALUES (?, ?, 'session', 'done', 'claude', ?, ?, ?)",
    )
    .bind(id)
    .bind(project_id)
    .bind(activity)
    .bind(title)
    .bind(activity)
    .execute(db)
    .await
    .unwrap();
}

/// A thread row (no token → straight to `error`, no job).
async fn offline_thread(state: &SlackThreadsState, url: &str, project_id: Option<&str>) -> String {
    state
        .add_thread(url, project_id, None, false)
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn run_links_are_idempotent_and_adopt_the_run_project() {
    let db = memory_db().await;
    let app_data = temp_dir("links");
    let state = offline_state(&db, &app_data, &Tokens::default());
    let id = offline_thread(&state, LINK, None).await;
    insert_run(&db, "r1", "p2", Some("Fix login"), "2026-02-01T00:00:00Z").await;
    insert_run(&db, "r2", "p2", None, "2026-03-01T00:00:00Z").await;

    let r = run_links::link(&db, &id, "r1", "picker").await.unwrap();
    assert_eq!(
        r,
        run_links::LinkRunResult {
            created: true,
            project_set: true
        }
    );
    let s = store::get_summary(&db, &id).await.unwrap();
    assert_eq!(s.project_id.as_deref(), Some("p2"));
    assert_eq!(s.linked_run_count, 1);

    // Re-link: no-op, the first source is kept.
    let r = run_links::link(&db, &id, "r1", "manual").await.unwrap();
    assert!(!r.created && !r.project_set);
    let links = run_links::runs_of_thread(&db, &id).await.unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].source, "picker");
    assert_eq!(links[0].title.as_deref(), Some("Fix login"));
    assert_eq!(links[0].project_id, "p2");
    assert_eq!(links[0].status, "done");

    // Most recently active first.
    run_links::link(&db, &id, "r2", "created_from_thread")
        .await
        .unwrap();
    let ids: Vec<String> = run_links::runs_of_thread(&db, &id)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.run_id)
        .collect();
    assert_eq!(ids, ["r2", "r1"]);

    // A thread that already has a project keeps it.
    let other = offline_thread(&state, &b_url(), Some("p1")).await;
    let r = run_links::link(&db, &other, "r1", "manual").await.unwrap();
    assert!(r.created && !r.project_set);
    let s = store::get_summary(&db, &other).await.unwrap();
    assert_eq!(s.project_id.as_deref(), Some("p1"));
    let threads: Vec<String> = run_links::threads_of_run(&db, "r1")
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.id)
        .collect();
    assert_eq!(threads.len(), 2);
    assert!(threads.contains(&id) && threads.contains(&other));

    // Errors.
    let e = run_links::link(&db, &id, "r1", "bogus").await.unwrap_err();
    assert_eq!(e.code, "invalid_source");
    let e = run_links::link(&db, &id, "nope", "manual")
        .await
        .unwrap_err();
    assert_eq!(e.code, "run_not_found");
    let e = run_links::link(&db, "nope", "r1", "manual")
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_found");
}

#[tokio::test]
async fn run_links_hide_missing_runs_and_are_cleaned_up() {
    let db = memory_db().await;
    let app_data = temp_dir("links-clean");
    let state = offline_state(&db, &app_data, &Tokens::default());
    let id = offline_thread(&state, LINK, Some("p1")).await;
    insert_run(&db, "r1", "p1", None, "2026-02-01T00:00:00Z").await;
    insert_run(&db, "r2", "p1", None, "2026-02-02T00:00:00Z").await;
    insert_run(&db, "r3", "p1", None, "2026-02-03T00:00:00Z").await;
    for r in ["r1", "r2", "r3"] {
        run_links::link(&db, &id, r, "manual").await.unwrap();
    }
    let count = |db: Db| async move {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM slack_thread_runs")
            .fetch_one(&db)
            .await
            .unwrap()
    };

    // A run removed behind our back: hidden, not counted.
    sqlx::query("DELETE FROM runs WHERE id = 'r1'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(run_links::runs_of_thread(&db, &id).await.unwrap().len(), 2);
    assert_eq!(
        store::get_summary(&db, &id).await.unwrap().linked_run_count,
        2
    );

    // The run-delete path drops the links.
    let registry = crate::runs::RunRegistry::default();
    crate::commands::runs::delete_run_inner(&db, &registry, "r2")
        .await
        .unwrap();
    assert_eq!(count(db.clone()).await, 2);
    assert_eq!(
        run_links::delete_for_run(&db, "r1").await.unwrap(),
        vec![id.clone()]
    );
    assert_eq!(count(db.clone()).await, 1);

    // Unlink.
    assert!(run_links::unlink(&db, &id, "r3").await.unwrap());
    assert!(!run_links::unlink(&db, &id, "r3").await.unwrap());
    assert_eq!(count(db.clone()).await, 0);

    // Deleting the thread drops its links.
    run_links::link(&db, &id, "r3", "manual").await.unwrap();
    state.delete(std::slice::from_ref(&id)).await.unwrap();
    assert_eq!(count(db.clone()).await, 0);
    assert!(run_links::threads_of_run(&db, "r3")
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn merged_thread_hands_its_links_over() {
    let db = memory_db().await;
    let app_data = temp_dir("links-merge");
    let state = offline_state(&db, &app_data, &Tokens::default());
    let a = offline_thread(&state, LINK, None).await;
    let b = offline_thread(&state, &b_url(), None).await;
    insert_run(&db, "r1", "p1", None, "2026-02-01T00:00:00Z").await;
    insert_run(&db, "r2", "p1", None, "2026-02-02T00:00:00Z").await;
    run_links::link(&db, &a, "r1", "picker").await.unwrap();
    run_links::link(&db, &a, "r2", "manual").await.unwrap();
    run_links::link(&db, &b, "r2", "created_from_thread")
        .await
        .unwrap();
    run_links::move_links(&db, &a, &b).await.unwrap();
    let links = run_links::runs_of_thread(&db, &b).await.unwrap();
    let got: Vec<(&str, &str)> = links
        .iter()
        .map(|l| (l.run_id.as_str(), l.source.as_str()))
        .collect();
    assert_eq!(got, [("r2", "created_from_thread"), ("r1", "picker")]);
    assert!(run_links::runs_of_thread(&db, &a).await.unwrap().is_empty());
}

#[tokio::test]
async fn project_removal_drops_its_run_links() {
    let db = memory_db().await;
    let app_data = temp_dir("links-project");
    let state = offline_state(&db, &app_data, &Tokens::default());
    let id = offline_thread(&state, LINK, None).await;
    insert_run(&db, "r1", "p1", None, "2026-02-01T00:00:00Z").await;
    insert_run(&db, "r2", "p2", None, "2026-02-02T00:00:00Z").await;
    run_links::link(&db, &id, "r1", "manual").await.unwrap();
    run_links::link(&db, &id, "r2", "manual").await.unwrap();
    assert_eq!(run_links::delete_for_project(&db, "p1").await.unwrap(), 1);
    let left: Vec<String> = run_links::runs_of_thread(&db, &id)
        .await
        .unwrap()
        .into_iter()
        .map(|l| l.run_id)
        .collect();
    assert_eq!(left, ["r2"]);
}
