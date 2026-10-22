//! Watch rules, project lookup, overview, the BR-11 queue and the refresh
//! pipeline on an in-memory DB with fake Slack / GitHub / AI services. No
//! network.

use super::source::{parse_source, Source};
use super::store::{self, WatchRequest};
use super::*;
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;
use uuid::Uuid;

async fn mem_db() -> Db {
    let db = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at) VALUES \
         ('p1', 'Alpha', '/tmp/p1', '2026-01-01'), ('p2', 'Beta', '/tmp/p2', '2026-01-01'), \
         ('p3', 'beta', '/tmp/p3', '2026-01-01')",
    )
    .execute(&db)
    .await
    .unwrap();
    db
}

fn src(input: &str) -> Source {
    parse_source(input, &[]).unwrap()
}

fn req<'a>(project_id: &'a str, source: &'a Source, move_project: bool) -> WatchRequest<'a> {
    WatchRequest {
        project_id,
        source,
        via: "mcp",
        run_id: Some("run-1"),
        move_project,
    }
}

const SLACK: &str = "https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359";
const ISSUE: &str = "org/app#12";

#[derive(Default)]
struct FakeSlack {
    calls: Mutex<Vec<(String, String)>>,
    fail: Mutex<Option<String>>,
    refreshed: Mutex<Vec<String>>,
    refresh_fail: Mutex<Option<String>>,
}

#[async_trait]
impl SlackCapture for FakeSlack {
    async fn ensure_thread(
        &self,
        url: &str,
        project_id: &str,
    ) -> Result<CapturedThread, CodedError> {
        self.calls
            .lock()
            .unwrap()
            .push((url.to_string(), project_id.to_string()));
        if let Some(code) = self.fail.lock().unwrap().clone() {
            return Err(CodedError::new(code, "fake"));
        }
        Ok(CapturedThread {
            id: "thread-1".into(),
            title: Some("Deploy thread".into()),
            error_code: None,
        })
    }

    async fn refresh_and_wait(&self, thread_id: &str) -> Result<(), CodedError> {
        self.refreshed.lock().unwrap().push(thread_id.to_string());
        match self.refresh_fail.lock().unwrap().clone() {
            Some(code) => Err(CodedError::new(code, "fake")),
            None => Ok(()),
        }
    }
}

/// Issues by `o/r#n`: `Ok` data or `Err` HTTP status.
#[derive(Default)]
struct FakeGithub {
    issues: Mutex<HashMap<String, Result<content::IssueData, u16>>>,
    calls: AtomicUsize,
}

impl FakeGithub {
    fn set(&self, r: &str, v: Result<content::IssueData, u16>) {
        self.issues.lock().unwrap().insert(r.to_string(), v);
    }
}

fn issue(title: &str, body: &str) -> content::IssueData {
    content::IssueData {
        owner: "org".into(),
        repo: "app".into(),
        number: 0,
        title: title.into(),
        state: "open".into(),
        author: "alice".into(),
        created_at: "2026-10-01T00:00:00+00:00".into(),
        body: body.into(),
        ..Default::default()
    }
}

#[async_trait]
impl github::GithubFetch for FakeGithub {
    async fn fetch_issue(
        &self,
        _project_id: &str,
        owner: &str,
        repo: &str,
        number: u64,
    ) -> Result<content::IssueData, CodedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let key = format!("{owner}/{repo}#{number}");
        match self.issues.lock().unwrap().get(&key).cloned() {
            Some(Ok(mut i)) => {
                // 0 = "the number asked for"; a transfer fixture sets its own.
                if i.number == 0 {
                    i.number = number;
                }
                Ok(i)
            }
            Some(Err(status)) => Err(github::status_error(status, "fake")),
            None => Err(github::status_error(404, "fake")),
        }
    }

    async fn my_login(&self, _project_id: &str) -> Option<String> {
        Some("thai".into())
    }
}

const ANSWER: &str = r#"{"status":"on_track","ball":"customer","ball_since":null,
  "summary":"Đang chờ khách.","next_action":"Nhắc khách",
  "deadlines":[{"title":"Demo","due_at":"2026-10-30T10:00:00+07:00","all_day":false,"evidence":"Demo 30/10"}],
  "decisions":[],"open_questions":[]}"#;

/// Scripted answers (then `ANSWER`), optional per-call delay / gate, and a
/// record of prompts and concurrency.
#[derive(Default)]
struct FakeEngine {
    answers: Mutex<std::collections::VecDeque<Result<String, String>>>,
    prompts: Mutex<Vec<String>>,
    delay_ms: AtomicUsize,
    gate: Mutex<Option<Arc<tokio::sync::Semaphore>>>,
    current: AtomicUsize,
    max: AtomicUsize,
    shut: AtomicUsize,
}

impl FakeEngine {
    fn push(&self, a: Result<&str, &str>) {
        self.answers
            .lock()
            .unwrap()
            .push_back(a.map(str::to_string).map_err(str::to_string));
    }
    fn calls(&self) -> usize {
        self.prompts.lock().unwrap().len()
    }
}

#[async_trait]
impl DigestEngine for FakeEngine {
    async fn complete(&self, prompt: String) -> Result<String, String> {
        let now = self.current.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        self.prompts.lock().unwrap().push(prompt);
        let gate = self.gate.lock().unwrap().clone();
        if let Some(g) = gate {
            let _ = g.acquire().await.map(|p| p.forget());
        }
        let ms = self.delay_ms.load(Ordering::SeqCst);
        if ms > 0 {
            tokio::time::sleep(Duration::from_millis(ms as u64)).await;
        }
        self.current.fetch_sub(1, Ordering::SeqCst);
        let next = self.answers.lock().unwrap().pop_front();
        next.unwrap_or_else(|| Ok(ANSWER.to_string()))
    }

    async fn model(&self) -> Option<String> {
        Some("claude:haiku".into())
    }

    async fn shutdown(&self) {
        self.shut.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Default)]
struct RecEvents {
    changed: Mutex<Vec<Vec<String>>>,
    progress: Mutex<Vec<(usize, usize)>>,
}

impl PulseEvents for RecEvents {
    fn changed(&self, watch_ids: &[String]) {
        self.changed.lock().unwrap().push(watch_ids.to_vec());
    }
    fn progress(&self, done: usize, total: usize) {
        self.progress.lock().unwrap().push((done, total));
    }
}

struct Harness {
    db: Db,
    state: PulseState,
    slack: Arc<FakeSlack>,
    github: Arc<FakeGithub>,
    engine: Arc<FakeEngine>,
    events: Arc<RecEvents>,
}

fn harness(db: &Db) -> Harness {
    let slack = Arc::new(FakeSlack::default());
    let github = Arc::new(FakeGithub::default());
    let engine = Arc::new(FakeEngine::default());
    let events = Arc::new(RecEvents::default());
    let state = PulseState::new(
        db.clone(),
        Services {
            slack: slack.clone(),
            github: github.clone(),
            engine: engine.clone(),
            events: events.clone(),
        },
    );
    Harness {
        db: db.clone(),
        state,
        slack,
        github,
        engine,
        events,
    }
}

impl Harness {
    async fn watch(&self, input: &str) -> store::Watch {
        store::watch(&self.db, req("p1", &src(input), false))
            .await
            .unwrap()
            .watch
    }
    async fn get(&self, id: &str) -> store::Watch {
        store::get(&self.db, id).await.unwrap().unwrap()
    }
    async fn digests(&self, id: &str) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT content_hash, status, deadlines FROM pulse_digests WHERE watch_id = ? \
             ORDER BY created_at DESC, id DESC",
        )
        .bind(id)
        .fetch_all(&self.db)
        .await
        .unwrap()
    }
    async fn run(&self) {
        self.state.process_pending().await;
        self.state.idle().await;
    }
    async fn refresh(&self, id: &str) {
        self.state.refresh_scope(None, Some(id)).await.unwrap();
    }
}

#[tokio::test]
async fn watch_dedupes_per_project_and_refuses_other_project() {
    let db = mem_db().await;
    let s = src(ISSUE);
    let first = store::watch(&db, req("p1", &s, false)).await.unwrap();
    assert!(!first.already_watching);
    let w = &first.watch;
    assert_eq!(
        (
            w.source_type.as_str(),
            w.source_ref.as_str(),
            w.resolve_status.as_str()
        ),
        ("github_issue", "org/app#12", "pending")
    );
    assert_eq!(w.source_url, "https://github.com/org/app/issues/12");
    assert_eq!(
        (w.added_via.as_str(), w.added_run_id.as_deref()),
        ("mcp", Some("run-1"))
    );
    assert!(w.active);

    // Same source, other spelling, same project → already watching, no new row.
    let again_src = src("https://github.com/Org/App/issues/12");
    let again = store::watch(&db, req("p1", &again_src, false))
        .await
        .unwrap();
    assert!(again.already_watching);
    assert_eq!(again.watch.id, w.id);
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pulse_watches")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);

    // Other project → error naming the owner project.
    let e = store::watch(&db, req("p2", &s, false)).await.unwrap_err();
    assert_eq!(e.code, "watched_in_other_project");
    assert!(e.detail.contains("Alpha"), "{}", e.detail);

    // move=true → moved, digests kept, re-resolved.
    sqlx::query(
        "INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, created_at) \
         VALUES ('d1', ?, 'h', 'on_track', 'none', '2026-01-01')",
    )
    .bind(&w.id)
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("UPDATE pulse_watches SET resolve_status = 'ready' WHERE id = ?")
        .bind(&w.id)
        .execute(&db)
        .await
        .unwrap();
    let moved = store::watch(&db, req("p2", &s, true)).await.unwrap();
    assert!(!moved.already_watching);
    assert_eq!(moved.watch.id, w.id);
    assert_eq!(moved.watch.project_id, "p2");
    assert_eq!(moved.watch.project_name, "Beta");
    assert_eq!(moved.watch.resolve_status, "pending");
    let digests: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pulse_digests")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(digests, 1);

    let bad = WatchRequest {
        via: "cli",
        ..req("p1", &s, false)
    };
    assert_eq!(
        store::watch(&db, bad).await.unwrap_err().code,
        "invalid_via"
    );
}

#[tokio::test]
async fn unwatch_then_watch_reactivates() {
    let db = mem_db().await;
    let s = src(SLACK);
    let w = store::watch(&db, req("p1", &s, false)).await.unwrap().watch;
    sqlx::query("UPDATE pulse_watches SET resolve_status = 'ready' WHERE id = ?")
        .bind(&w.id)
        .execute(&db)
        .await
        .unwrap();
    store::unwatch(&db, &w.id).await.unwrap();
    assert!(!store::get(&db, &w.id).await.unwrap().unwrap().active);
    assert!(store::is_watched(&db, "slack", &s.source_ref)
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        store::unwatch(&db, "nope").await.unwrap_err().code,
        "not_found"
    );

    let back = store::watch(&db, req("p1", &s, false)).await.unwrap();
    assert!(!back.already_watching);
    assert_eq!(back.watch.id, w.id);
    assert!(back.watch.active);
    assert_eq!(back.watch.resolve_status, "pending");
    assert_eq!(
        store::is_watched(&db, "slack", "C0B83GHDTFG:1789877283.026359")
            .await
            .unwrap()
            .map(|w| w.id),
        Some(w.id.clone())
    );

    // An unwatched source is free: another project takes it over without `move`.
    store::unwatch(&db, &w.id).await.unwrap();
    let taken = store::watch(&db, req("p2", &s, false)).await.unwrap();
    assert_eq!(taken.watch.id, w.id);
    assert_eq!(taken.watch.project_id, "p2");
    assert!(taken.watch.active);
}

#[tokio::test]
async fn move_watch_and_project_cascade() {
    let db = mem_db().await;
    let s = src(ISSUE);
    let w = store::watch(&db, req("p1", &s, false)).await.unwrap().watch;
    let m = store::move_watch(&db, &w.id, "p2").await.unwrap();
    assert_eq!(
        (m.project_id.as_str(), m.resolve_status.as_str()),
        ("p2", "pending")
    );
    assert_eq!(
        store::move_watch(&db, "nope", "p2").await.unwrap_err().code,
        "not_found"
    );

    sqlx::query("DELETE FROM projects WHERE id = 'p2'")
        .execute(&db)
        .await
        .unwrap();
    assert!(store::get(&db, &w.id).await.unwrap().is_none());
}

#[tokio::test]
async fn resolve_project_by_id_name_and_fallback() {
    let db = mem_db().await;
    let ok = |r: Result<(String, String), CodedError>| r.unwrap().0;
    assert_eq!(
        ok(store::resolve_project(&db, Some("p1"), None).await),
        "p1"
    );
    assert_eq!(
        ok(store::resolve_project(&db, Some(" ALPHA "), Some("p2")).await),
        "p1"
    );
    assert_eq!(
        ok(store::resolve_project(&db, None, Some("p2")).await),
        "p2"
    );
    assert_eq!(
        ok(store::resolve_project(&db, Some(""), Some("p2")).await),
        "p2"
    );
    let code = |r: Result<(String, String), CodedError>| r.unwrap_err().code;
    assert_eq!(
        code(store::resolve_project(&db, Some("beta"), None).await),
        "project_ambiguous"
    );
    assert_eq!(
        code(store::resolve_project(&db, Some("Gamma"), None).await),
        "project_not_found"
    );
    assert_eq!(
        code(store::resolve_project(&db, None, Some("gone")).await),
        "project_not_found"
    );
    assert_eq!(
        code(store::resolve_project(&db, None, None).await),
        "project_required"
    );
}

#[tokio::test]
async fn normalize_uses_project_repos_and_slack_thread_ids() {
    let db = mem_db().await;
    let code = |r: Result<Source, CodedError>| r.unwrap_err().code;
    assert_eq!(
        code(store::normalize(&db, "#5", "p1").await),
        "repo_ambiguous"
    );
    sqlx::query(
        "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES \
         ('r1', 'p1', 'app', '/tmp/a', 'Org', 'App', '2026-01-01'), \
         ('r2', 'p2', 'x', '/tmp/x', 'org', 'x', '2026-01-01'), \
         ('r3', 'p2', 'y', '/tmp/y', 'org', 'y', '2026-01-01')",
    )
    .execute(&db)
    .await
    .unwrap();
    let s = store::normalize(&db, "#5", "p1").await.unwrap();
    assert_eq!(s.source_ref, "org/app#5");
    assert_eq!(
        code(store::normalize(&db, "#5", "p2").await),
        "repo_ambiguous"
    );

    sqlx::query(
        "INSERT INTO slack_threads (id, title, created_at, updated_at, channel_id, thread_ts, thread_url) \
         VALUES ('5f0c7a52-0000-4000-8000-000000000001', 't', '2026-01-01', '2026-01-01', \
         'C0B83GHDTFG', '1789877283.026359', ?)",
    )
    .bind(SLACK)
    .execute(&db)
    .await
    .unwrap();
    let s = store::normalize(&db, "5f0c7a52-0000-4000-8000-000000000001", "p1")
        .await
        .unwrap();
    assert_eq!(s.source_ref, "C0B83GHDTFG:1789877283.026359");
    assert_eq!(s.source_url, SLACK);
    assert_eq!(
        code(store::normalize(&db, "unknown-id", "p1").await),
        "invalid_source"
    );
    assert_eq!(
        code(store::normalize(&db, "https://github.com/org/app/pull/3", "p1").await),
        "invalid_source"
    );
}

#[tokio::test]
async fn overview_groups_active_watches_with_latest_digest() {
    let db = mem_db().await;
    let a = store::watch(&db, req("p1", &src(ISSUE), false))
        .await
        .unwrap()
        .watch;
    let b = store::watch(&db, req("p1", &src(SLACK), false))
        .await
        .unwrap()
        .watch;
    let c = store::watch(&db, req("p2", &src("org/app#13"), false))
        .await
        .unwrap()
        .watch;
    store::unwatch(&db, &c.id).await.unwrap();
    sqlx::query(
        "INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, summary, deadlines, created_at) VALUES \
         ('d-old', ?1, 'h1', 'on_track', 'none', 'old', '[]', '2026-01-01T00:00:00+00:00'), \
         ('d-new', ?1, 'h2', 'at_risk', 'me', 'new', '[{\"title\":\"Demo\"}]', '2026-01-02T00:00:00+00:00'), \
         ('d-c', ?2, 'h3', 'done', 'none', 'gone', 'oops', '2026-01-03T00:00:00+00:00')",
    )
    .bind(&a.id)
    .bind(&c.id)
    .execute(&db)
    .await
    .unwrap();

    let all = store::overview(&db, None).await.unwrap();
    assert_eq!(all.len(), 1, "inactive-only project hidden");
    let p = &all[0];
    assert_eq!(
        (p.project_id.as_str(), p.project_name.as_str()),
        ("p1", "Alpha")
    );
    assert_eq!(p.health.health, "yellow"); // b: never summarised → pending; a: at_risk
    let ids: Vec<&str> = p.watches.iter().map(|w| w.watch.id.as_str()).collect();
    assert_eq!(ids, vec![a.id.as_str(), b.id.as_str()]);
    let d = p.watches[0].digest.as_ref().unwrap();
    assert_eq!(
        (d.id.as_str(), d.status.as_str(), d.summary.as_str()),
        ("d-new", "at_risk", "new")
    );
    assert_eq!(d.deadlines, serde_json::json!([{ "title": "Demo" }]));
    assert!(p.watches[1].digest.is_none());

    let only = store::overview(&db, Some(&["p2".to_string(), "nope".to_string()]))
        .await
        .unwrap();
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].project_id, "p2");
    assert!(only[0].watches.is_empty());

    let v = serde_json::to_value(&all[0].watches[0]).unwrap();
    assert_eq!(v["sourceRef"], "org/app#12");
    assert_eq!(v["digest"]["openQuestions"], serde_json::json!([]));
}

#[tokio::test]
async fn slack_capture_adds_missing_threads_and_moves_existing_ones() {
    use crate::slack_threads::tests::{memory_db, offline_state, Tokens};
    let db = memory_db().await;
    let dir = std::env::temp_dir().join(format!("devdy-pulse-{}", uuid::Uuid::new_v4()));
    let state = offline_state(&db, &dir, &Tokens::default());

    // Missing thread, no Slack token: captured as an `error` row in the project.
    let t = state.ensure_thread(SLACK, "p1").await.unwrap();
    assert_eq!(t.error_code.as_deref(), Some("no_token"));
    let row: (Option<String>, String) =
        sqlx::query_as("SELECT project_id, fetch_status FROM slack_threads WHERE id = ?")
            .bind(&t.id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(row, (Some("p1".to_string()), "error".to_string()));

    // Existing thread of another project: same row, moved to the watch's project (BR-03).
    sqlx::query("UPDATE slack_threads SET fetch_status = 'ready', title = 'Deploy' WHERE id = ?")
        .bind(&t.id)
        .execute(&db)
        .await
        .unwrap();
    let reply = "https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359";
    let again = state.ensure_thread(reply, "p2").await.unwrap();
    assert_eq!(again.id, t.id);
    assert_eq!(
        (again.title.as_deref(), again.error_code),
        (Some("Deploy"), None)
    );
    let project: Option<String> =
        sqlx::query_scalar("SELECT project_id FROM slack_threads WHERE id = ?")
            .bind(&t.id)
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(project.as_deref(), Some("p2"));
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM slack_threads")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(n, 1);
    let _ = std::fs::remove_dir_all(dir);
}

/// Two pools on one file DB stand in for the app and the MCP sidecar.
async fn file_db_pair() -> (Db, Db, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!("devdy-pulse-race-{}.db", Uuid::new_v4()));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let a = SqlitePoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&a).await.unwrap();
    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at) VALUES \
         ('p1', 'Alpha', '/tmp/p1', '2026-01-01'), ('p2', 'Beta', '/tmp/p2', '2026-01-01')",
    )
    .execute(&a)
    .await
    .unwrap();
    let b = SqlitePoolOptions::new()
        .max_connections(4)
        .connect(&url)
        .await
        .unwrap();
    (a, b, path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_watches_from_two_connections_are_serialised() {
    let (a, b, path) = file_db_pair().await;
    for n in 1..=20u64 {
        let s = parse_source(&format!("org/app#{n}"), &[]).unwrap();
        let (sa, sb) = (s.clone(), s.clone());
        let (a2, b2) = (a.clone(), b.clone());
        // Different projects, no move: exactly one wins, the other is refused.
        let ta = tokio::spawn(async move { store::watch(&a2, req("p1", &sa, false)).await });
        let tb = tokio::spawn(async move { store::watch(&b2, req("p2", &sb, false)).await });
        let (ra, rb) = (ta.await.unwrap(), tb.await.unwrap());
        let ok = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
        assert_eq!(ok, 1, "#{n}: {ra:?} / {rb:?}");
        let refused = [&ra, &rb]
            .into_iter()
            .find_map(|r| r.as_ref().err())
            .unwrap();
        assert_eq!(refused.code, "watched_in_other_project");

        // Same project from both sides: one row, one `already_watching`.
        let s = parse_source(&format!("org/api#{n}"), &[]).unwrap();
        let (sa, sb) = (s.clone(), s.clone());
        let (a2, b2) = (a.clone(), b.clone());
        let ta = tokio::spawn(async move { store::watch(&a2, req("p1", &sa, false)).await });
        let tb = tokio::spawn(async move { store::watch(&b2, req("p1", &sb, false)).await });
        let (ra, rb) = (ta.await.unwrap().unwrap(), tb.await.unwrap().unwrap());
        assert_eq!(ra.watch.id, rb.watch.id);
        assert!(ra.already_watching ^ rb.already_watching, "#{n}");
    }
    let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pulse_watches")
        .fetch_one(&a)
        .await
        .unwrap();
    assert_eq!(rows, 40);

    // Concurrent movers: both serialise; the row ends in one of the targets, active.
    let s = parse_source("org/app#1", &[]).unwrap();
    let w = store::find_by_source(&a, "github_issue", &s.source_ref)
        .await
        .unwrap()
        .unwrap();
    let (a2, b2, id1, id2) = (a.clone(), b.clone(), w.id.clone(), w.id.clone());
    let ta = tokio::spawn(async move { store::move_watch(&a2, &id1, "p1").await });
    let tb = tokio::spawn(async move { store::move_watch(&b2, &id2, "p2").await });
    let (ra, rb) = (ta.await.unwrap().unwrap(), tb.await.unwrap().unwrap());
    let end = store::get(&a, &w.id).await.unwrap().unwrap();
    assert!(end.project_id == ra.project_id || end.project_id == rb.project_id);
    assert!(end.active);
    a.close().await;
    b.close().await;
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn poller_tick_respects_the_gate_and_recovers_leftovers() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set(ISSUE, Ok(issue("Login", "body")));
    let state = h.state.clone();
    // Left over from before a restart: a pending watch and an open request.
    let w = store::watch(&db, req("p1", &src(ISSUE), false))
        .await
        .unwrap()
        .watch;
    sqlx::query(
        "INSERT INTO pulse_requests (id, kind, watch_id, created_at) \
         VALUES ('q1', 'resolve', ?, '2026-01-01')",
    )
    .bind(&w.id)
    .execute(&db)
    .await
    .unwrap();

    // Gate closed (no running session, screen closed): nothing happens.
    assert!(!state.tick(false).await);
    assert_eq!(
        store::get(&db, &w.id)
            .await
            .unwrap()
            .unwrap()
            .resolve_status,
        "pending"
    );

    // A session starts running → the next tick processes the leftovers.
    sqlx::query(
        "INSERT INTO runs (id, project_id, type, status, engine, created_at) \
         VALUES ('r1', 'p1', 'session', 'running', 'claude', '2026-01-01')",
    )
    .execute(&db)
    .await
    .unwrap();
    assert!(state.tick(false).await);
    state.idle().await;
    assert_eq!(
        store::get(&db, &w.id)
            .await
            .unwrap()
            .unwrap()
            .resolve_status,
        "ready"
    );
    // The resolve request closes on the pass after its watch is resolved.
    assert!(state.tick(false).await);
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pulse_requests WHERE done_at IS NULL")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(open, 0);

    // Screen open or a kick also opens the gate.
    sqlx::query("UPDATE runs SET status = 'done'")
        .execute(&db)
        .await
        .unwrap();
    assert!(!state.tick(false).await);
    state.set_screen_open(true);
    assert!(state.tick(false).await);
    state.set_screen_open(false);
    assert!(state.tick(true).await);
}

// ---- Refresh pipeline (T3) ----------------------------------------------------

async fn open_requests(db: &Db) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM pulse_requests WHERE done_at IS NULL")
        .fetch_one(db)
        .await
        .unwrap()
}

async fn add_request(db: &Db, id: &str, kind: &str, watch_id: Option<&str>, project: Option<&str>) {
    sqlx::query(
        "INSERT INTO pulse_requests (id, kind, watch_id, project_id, created_at) \
         VALUES (?, ?, ?, ?, '2026-01-01')",
    )
    .bind(id)
    .bind(kind)
    .bind(watch_id)
    .bind(project)
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn github_watch_is_validated_and_digested_on_resolve() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github
        .set(ISSUE, Ok(issue("Login fails", "Steps to reproduce")));
    let w = h.watch(ISSUE).await;
    add_request(&db, "q1", "resolve", Some(&w.id), None).await;

    assert_eq!(h.state.process_pending().await, vec![w.id.clone()]);
    h.state.idle().await;
    let got = h.get(&w.id).await;
    assert_eq!(got.resolve_status, "ready");
    assert_eq!(got.title.as_deref(), Some("Login fails"));
    assert_eq!(got.last_error, None);
    assert!(got.checked_at.is_some());
    let d = h.digests(&w.id).await;
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].1, "on_track");
    let model: Option<String> = sqlx::query_scalar("SELECT model FROM pulse_digests")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(model.as_deref(), Some("claude:haiku"));
    let prompt = h.engine.prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("The user is: GitHub user thai"));
    assert!(prompt.contains("Title: Login fails"));
    assert!(prompt.contains("Steps to reproduce"));

    // The resolve request closes once its watch left `pending`.
    assert_eq!(open_requests(&db).await, 1);
    h.state.process_pending().await;
    assert_eq!(open_requests(&db).await, 0);
    // Events: progress 0/1 → 1/1, then the watch changed.
    assert_eq!(*h.events.progress.lock().unwrap(), vec![(0, 1), (1, 1)]);
    assert!(h
        .events
        .changed
        .lock()
        .unwrap()
        .contains(&vec![w.id.clone()]));
}

#[tokio::test]
async fn github_404_and_401_are_recorded_and_keep_old_digests() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set("org/app#1", Err(404));
    h.github.set("org/app#2", Err(401));
    let missing = h.watch("org/app#1").await;
    let denied = h.watch("org/app#2").await;
    h.run().await;
    let m = h.get(&missing.id).await;
    assert_eq!(
        (m.resolve_status.as_str(), m.last_error.as_deref()),
        ("error", Some("github_not_found"))
    );
    let d = h.get(&denied.id).await;
    assert_eq!(
        (d.resolve_status.as_str(), d.last_error.as_deref()),
        ("error", Some("github_auth"))
    );
    assert_eq!(h.engine.calls(), 0);
    assert!(h.digests(&missing.id).await.is_empty());

    // "Retry": the issue is reachable now → ready + digest.
    h.github.set("org/app#1", Ok(issue("Back", "x")));
    h.refresh(&missing.id).await;
    let m = h.get(&missing.id).await;
    assert_eq!((m.resolve_status.as_str(), m.last_error), ("ready", None));
    assert_eq!(h.digests(&missing.id).await.len(), 1);

    // A later 404 on a ready watch: error recorded, still ready, digest kept.
    h.github.set("org/app#1", Err(404));
    h.refresh(&missing.id).await;
    let m = h.get(&missing.id).await;
    assert_eq!(
        (m.resolve_status.as_str(), m.last_error.as_deref()),
        ("ready", Some("github_not_found"))
    );
    assert_eq!(h.digests(&missing.id).await.len(), 1);
}

#[tokio::test]
async fn unchanged_content_skips_the_ai_but_updates_checked_at() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set(ISSUE, Ok(issue("T", "same")));
    let w = h.watch(ISSUE).await;
    h.run().await;
    assert_eq!(h.engine.calls(), 1);
    sqlx::query(
        "UPDATE pulse_watches SET checked_at = '2000-01-01', last_error = 'x' WHERE id = ?",
    )
    .bind(&w.id)
    .execute(&db)
    .await
    .unwrap();
    h.refresh(&w.id).await;
    assert_eq!(h.engine.calls(), 1, "no AI call for the same hash");
    let got = h.get(&w.id).await;
    assert_ne!(got.checked_at.as_deref(), Some("2000-01-01"));
    assert_eq!(got.last_error, None);
    assert_eq!(h.digests(&w.id).await.len(), 1);

    h.github.set(ISSUE, Ok(issue("T", "changed")));
    h.refresh(&w.id).await;
    assert_eq!(h.engine.calls(), 2);
    assert_eq!(h.digests(&w.id).await.len(), 2);
}

#[tokio::test]
async fn invalid_answers_are_retried_once_then_fail() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set(ISSUE, Ok(issue("T", "v1")));
    let w = h.watch(ISSUE).await;
    h.engine.push(Ok("not json at all"));
    h.run().await;
    let prompts = h.engine.prompts.lock().unwrap().clone();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("Your previous answer was rejected: no JSON object"));
    assert_eq!(h.digests(&w.id).await.len(), 1);
    assert_eq!(h.get(&w.id).await.last_error, None);

    // Two bad answers → summarize_failed, the old digest stays.
    h.github.set(ISSUE, Ok(issue("T", "v2")));
    h.engine
        .push(Ok(r#"{"status":"late","ball":"me","summary":"x"}"#));
    h.engine
        .push(Ok(r#"{"status":"late","ball":"me","summary":"x"}"#));
    h.refresh(&w.id).await;
    let got = h.get(&w.id).await;
    assert_eq!(
        (got.resolve_status.as_str(), got.last_error.as_deref()),
        ("ready", Some("summarize_failed"))
    );
    assert_eq!(h.digests(&w.id).await.len(), 1);

    // Engine unavailable also → summarize_failed (no retry needed).
    h.github.set(ISSUE, Ok(issue("T", "v3")));
    h.engine.push(Err("sidecar down"));
    let before = h.engine.calls();
    h.refresh(&w.id).await;
    assert_eq!(h.engine.calls(), before + 1);
    assert_eq!(
        h.get(&w.id).await.last_error.as_deref(),
        Some("summarize_failed")
    );
}

#[tokio::test]
async fn closed_issue_and_board_deadline_override_the_ai() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut i = issue("Ship it", "x");
    i.state = "closed".into();
    i.board = Some(content::BoardFields {
        start: None,
        deadline: Some("2026-11-05".into()),
        status: Some("Done".into()),
    });
    h.github.set(ISSUE, Ok(i));
    let w = h.watch(ISSUE).await;
    h.run().await;
    let d = h.digests(&w.id).await;
    assert_eq!(d[0].1, "done");
    let deadlines: serde_json::Value = serde_json::from_str(&d[0].2).unwrap();
    assert_eq!(deadlines.as_array().unwrap().len(), 1);
    assert_eq!(deadlines[0]["title"], "Deadline (board)");
    assert!(deadlines[0]["due_at"]
        .as_str()
        .unwrap()
        .starts_with("2026-11-05T23:59:59"));
}

#[tokio::test]
async fn only_five_digests_are_kept() {
    let db = mem_db().await;
    let h = harness(&db);
    let w = h.watch(ISSUE).await;
    for n in 0..7 {
        h.github.set(ISSUE, Ok(issue("T", &format!("v{n}"))));
        if n == 0 {
            h.run().await;
        } else {
            h.refresh(&w.id).await;
        }
    }
    assert_eq!(h.engine.calls(), 7);
    let d = h.digests(&w.id).await;
    assert_eq!(d.len(), 5);
    let newest = content::github_content(&content::IssueData {
        number: 12,
        ..issue("T", "v6")
    })
    .hash;
    assert_eq!(d[0].0, newest);
}

#[tokio::test]
async fn refresh_request_is_done_only_after_its_scope_finished() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set("org/app#1", Ok(issue("A", "a")));
    h.github.set("org/app#2", Err(404));
    let a = h.watch("org/app#1").await;
    let b = h.watch("org/app#2").await;
    let other = store::watch(&db, req("p2", &src("org/app#3"), false))
        .await
        .unwrap()
        .watch;
    h.run().await; // resolve all
    let calls = h.engine.calls();
    h.github.set("org/app#1", Ok(issue("A", "a2")));

    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    *h.engine.gate.lock().unwrap() = Some(gate.clone());
    add_request(&db, "r1", "refresh", None, Some("p1")).await;
    h.state.process_pending().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // The AI call of watch a is blocked: the request must still be open.
    assert_eq!(open_requests(&db).await, 1);
    // A second pass does not start a duplicate refresh.
    h.state.process_pending().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.engine.calls(), calls + 1);

    gate.add_permits(10);
    h.state.idle().await;
    assert_eq!(open_requests(&db).await, 0);
    // Failures are on the watch; the other project's watch was not touched.
    assert_eq!(
        h.get(&b.id).await.last_error.as_deref(),
        Some("github_not_found")
    );
    assert_eq!(h.digests(&a.id).await.len(), 2);
    let checked_other = h.get(&other.id).await.checked_at;
    add_request(&db, "r2", "refresh", Some(&a.id), None).await;
    h.run().await;
    assert_eq!(h.get(&other.id).await.checked_at, checked_other);
    assert_eq!(open_requests(&db).await, 0);
}

#[tokio::test]
async fn at_most_three_refreshes_run_at_once_and_duplicates_wait() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut ids = Vec::new();
    for n in 1..=6 {
        let r = format!("org/app#{n}");
        h.github.set(&r, Ok(issue("T", &format!("body {n}"))));
        ids.push(h.watch(&r).await.id);
    }
    h.engine.delay_ms.store(80, Ordering::SeqCst);
    let watches = store::scope_watches(&db, Some("p1"), None).await.unwrap();
    let (started, _) = h.state.schedule(watches.clone());
    assert_eq!(started, 6);
    // Asking again while they run starts nothing new.
    let (again, waits) = h.state.schedule(watches);
    assert_eq!(again, 0);
    PulseState::wait_all(waits).await;
    h.state.idle().await;
    assert_eq!(h.engine.max.load(Ordering::SeqCst), 3);
    assert_eq!(h.engine.calls(), 6);
    for id in &ids {
        assert_eq!(h.get(id).await.resolve_status, "ready");
    }
    let p = h.events.progress.lock().unwrap().clone();
    assert_eq!(p.first(), Some(&(0, 6)));
    assert_eq!(p.last(), Some(&(6, 6)));
}

async fn seed_slack_thread(db: &Db, msgs: &[(&str, &str, &str)]) {
    sqlx::query(
        "INSERT INTO slack_threads (id, title, created_at, updated_at, team_id, channel_id, \
         thread_ts, thread_url, fetch_status) VALUES ('thread-1', 'Deploy', '2026-01-01', \
         '2026-01-01', 'T1', 'C0B83GHDTFG', '1789877283.026359', ?, 'ready')",
    )
    .bind(SLACK)
    .execute(db)
    .await
    .unwrap();
    sqlx::query("INSERT INTO slack_identities (team_id, user_id, fetched_at) VALUES ('T1', 'UME', '2026-01-01')")
        .execute(db)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO slack_users (team_id, user_id, display_name, fetched_at) VALUES \
         ('T1', 'UME', 'Thai', '2026-01-01'), ('T1', 'UCUST', 'Khach', '2026-01-01')",
    )
    .execute(db)
    .await
    .unwrap();
    for (seq, (ts, user, text)) in msgs.iter().enumerate() {
        sqlx::query(
            "INSERT INTO slack_messages (thread_id, ts, seq, user_id, text_raw, src_hash) \
             VALUES ('thread-1', ?, ?, ?, ?, 'h')",
        )
        .bind(ts)
        .bind(seq as i64)
        .bind(user)
        .bind(text)
        .execute(db)
        .await
        .unwrap();
    }
}

#[tokio::test]
async fn slack_watch_uses_the_ball_rule_over_the_ai() {
    let db = mem_db().await;
    seed_slack_thread(
        &db,
        &[
            ("1789877283.026359", "UME", "Báo giá đây"),
            ("1789877300.000100", "UCUST", "Khi nào demo được?"),
        ],
    )
    .await;
    let h = harness(&db);
    let w = h.watch(SLACK).await;
    h.run().await;
    assert_eq!(
        *h.slack.refreshed.lock().unwrap(),
        vec!["thread-1".to_string()]
    );
    let got = h.get(&w.id).await;
    assert_eq!(got.resolve_status, "ready");
    assert_eq!(got.slack_thread_id.as_deref(), Some("thread-1"));
    assert_eq!(got.title.as_deref(), Some("Deploy"));
    let (ball, since): (String, Option<String>) =
        sqlx::query_as("SELECT ball, ball_since FROM pulse_digests WHERE watch_id = ?")
            .bind(&w.id)
            .fetch_one(&db)
            .await
            .unwrap();
    // The AI said "customer"; the last message is the customer's → mine.
    assert_eq!(ball, "me");
    assert_eq!(since.as_deref(), Some("2026-09-20T04:08:20.000Z"));
    let prompt = h.engine.prompts.lock().unwrap()[0].clone();
    assert!(prompt.contains("The user is: Thai (their lines are marked (me))"));
    assert!(prompt.contains("Thai(me): Báo giá đây"));
    assert!(prompt.contains("Khach(other): Khi nào demo được?"));

    // Slack fetch failure → last_error with the Slack code, digest kept.
    *h.slack.refresh_fail.lock().unwrap() = Some("no_token".into());
    h.refresh(&w.id).await;
    let got = h.get(&w.id).await;
    assert_eq!(
        (got.resolve_status.as_str(), got.last_error.as_deref()),
        ("ready", Some("no_token"))
    );
    assert_eq!(h.digests(&w.id).await.len(), 1);
}

#[tokio::test]
async fn slack_capture_failure_on_resolve_is_an_error() {
    let db = mem_db().await;
    let h = harness(&db);
    *h.slack.fail.lock().unwrap() = Some("no_token".into());
    let w = h.watch(SLACK).await;
    h.run().await;
    let got = h.get(&w.id).await;
    assert_eq!(
        (got.resolve_status.as_str(), got.last_error.as_deref()),
        ("error", Some("no_token"))
    );
    assert!(h.slack.refreshed.lock().unwrap().is_empty());
}

#[tokio::test]
async fn transferred_issue_follows_its_new_repo_and_number() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut i = issue("Moved", "x");
    i.owner = "NewOrg".into();
    i.repo = "New-App".into();
    i.number = 77;
    h.github.set(ISSUE, Ok(i));
    let w = h.watch(ISSUE).await;
    h.run().await;
    let got = h.get(&w.id).await;
    assert_eq!(got.source_ref, "neworg/new-app#77");
    assert_eq!(
        got.source_url,
        "https://github.com/neworg/new-app/issues/77"
    );
    // The refresh went on under the new version of the watch.
    assert_eq!(got.resolve_status, "ready");
    assert_eq!(h.digests(&w.id).await.len(), 1);
    assert!(h.engine.prompts.lock().unwrap()[0].contains("Issue: NewOrg/New-App#77"));
}

#[tokio::test]
async fn transfer_onto_an_already_watched_issue_is_an_error() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut i = issue("Moved", "x");
    i.owner = "neworg".into();
    i.repo = "new-app".into();
    i.number = 77;
    h.github.set(ISSUE, Ok(i));
    let mut target = issue("Target", "y");
    target.owner = "neworg".into();
    target.repo = "new-app".into();
    h.github.set("neworg/new-app#77", Ok(target));
    let other = h.watch("neworg/new-app#77").await;
    let w = h.watch(ISSUE).await;
    h.run().await;
    let got = h.get(&w.id).await;
    assert_eq!(got.source_ref, ISSUE);
    assert_eq!(got.last_error.as_deref(), Some("duplicate_source"));
    assert_eq!(got.resolve_status, "error");
    assert_eq!(h.get(&other.id).await.resolve_status, "ready");
}

#[tokio::test]
async fn a_failed_board_query_does_not_count_as_a_change() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut i = issue("T", "x");
    i.board = Some(content::BoardFields {
        start: None,
        deadline: Some("2026-11-05".into()),
        status: Some("Todo".into()),
    });
    h.github.set(ISSUE, Ok(i.clone()));
    let w = h.watch(ISSUE).await;
    h.run().await;
    assert_eq!(h.engine.calls(), 1);
    // Board query fails this time: no board fields → different text, but no
    // new digest and no AI call.
    let mut flaky = i.clone();
    flaky.board = None;
    flaky.board_unavailable = true;
    h.github.set(ISSUE, Ok(flaky));
    h.refresh(&w.id).await;
    assert_eq!(h.engine.calls(), 1);
    assert_eq!(h.digests(&w.id).await.len(), 1);
    // Back to normal: same hash as before, still no call.
    h.github.set(ISSUE, Ok(i));
    h.refresh(&w.id).await;
    assert_eq!(h.engine.calls(), 1);
}

#[tokio::test]
async fn a_refresh_overtaken_by_a_move_or_rewatch_writes_nothing() {
    for change in ["move", "rewatch"] {
        let db = mem_db().await;
        let h = harness(&db);
        h.github.set(ISSUE, Ok(issue("Title", "x")));
        let w = h.watch(ISSUE).await;
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *h.engine.gate.lock().unwrap() = Some(gate.clone());
        let (_, waits) = h.state.schedule(vec![w.clone()]);
        // Wait until the AI call is in flight, then change the watch.
        while h.engine.calls() == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        if change == "move" {
            store::move_watch(&db, &w.id, "p2").await.unwrap();
        } else {
            store::unwatch(&db, &w.id).await.unwrap();
            store::watch(&db, req("p1", &src(ISSUE), false))
                .await
                .unwrap();
        }
        gate.add_permits(10);
        PulseState::wait_all(waits).await;
        h.state.idle().await;
        let got = h.get(&w.id).await;
        assert!(h.digests(&w.id).await.is_empty(), "{change}");
        assert_eq!((got.title, got.checked_at), (None, None), "{change}");
        assert_eq!(got.resolve_status, "pending", "{change}");
    }
}

#[tokio::test]
async fn pull_request_number_is_refused_on_resolve() {
    let db = mem_db().await;
    let h = harness(&db);
    let mut i = issue("A PR", "x");
    i.is_pull_request = true;
    h.github.set(ISSUE, Ok(i));
    let w = h.watch(ISSUE).await;
    h.run().await;
    let got = h.get(&w.id).await;
    assert_eq!(
        (got.resolve_status.as_str(), got.last_error.as_deref()),
        ("error", Some("invalid_source"))
    );
}

#[tokio::test]
async fn moved_while_resolving_keeps_the_watch_pending_and_unwatched_is_skipped() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set(ISSUE, Ok(issue("T", "x")));
    let w = h.watch(ISSUE).await;
    // A result computed for the old project does not resolve the moved watch.
    let moved = store::move_watch(&db, &w.id, "p2").await.unwrap();
    let stale = store::Watch {
        project_id: "p1".into(),
        ..moved.clone()
    };
    store::finish_refresh(
        &db,
        &stale,
        store::Finish {
            resolve_status: Some("ready"),
            resolving: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(h.get(&w.id).await.resolve_status, "pending");

    // An unwatched watch is not refreshed.
    store::unwatch(&db, &w.id).await.unwrap();
    h.refresh(&w.id).await;
    assert_eq!(h.github.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn external_watch_edits_emit_a_reload_event() {
    let db = mem_db().await;
    let h = harness(&db);
    h.github.set(ISSUE, Ok(issue("T", "x")));
    let w = h.watch(ISSUE).await;
    h.run().await;
    let before = h.events.changed.lock().unwrap().len();
    h.state.process_pending().await;
    assert_eq!(
        h.events.changed.lock().unwrap().len(),
        before,
        "nothing new"
    );
    sqlx::query("UPDATE pulse_watches SET active = 0, updated_at = '2099-01-01T00:00:00+00:00' WHERE id = ?")
        .bind(&w.id)
        .execute(&db)
        .await
        .unwrap();
    h.state.process_pending().await;
    assert_eq!(h.events.changed.lock().unwrap().last(), Some(&Vec::new()));
}

// ---- Health in the overview (stage C) -------------------------------------------

async fn digest_row(
    db: &Db,
    id: &str,
    watch: &str,
    status: &str,
    ball: &str,
    since: Option<&str>,
    closed: bool,
    created: &str,
) {
    sqlx::query(
        "INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, ball_since, summary, \
         source_closed, created_at) VALUES (?, ?, 'h', ?, ?, ?, 's', ?, ?)",
    )
    .bind(id)
    .bind(watch)
    .bind(status)
    .bind(ball)
    .bind(since)
    .bind(closed)
    .bind(created)
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn overview_orders_cards_by_health_and_groups_finished_watches() {
    let db = mem_db().await;
    let now = chrono::DateTime::parse_from_rfc3339("2026-10-22T09:00:00+07:00").unwrap();
    // Alpha: one green watch. Beta: my ball for 30h (red) + a closed issue.
    let a = store::watch(&db, req("p1", &src("org/app#1"), false))
        .await
        .unwrap()
        .watch;
    let b1 = store::watch(&db, req("p2", &src("org/app#2"), false))
        .await
        .unwrap()
        .watch;
    let b2 = store::watch(&db, req("p2", &src("org/app#3"), false))
        .await
        .unwrap()
        .watch;
    digest_row(
        &db,
        "da",
        &a.id,
        "on_track",
        "none",
        None,
        false,
        "2026-10-22T08:00:00+07:00",
    )
    .await;
    digest_row(
        &db,
        "db1",
        &b1.id,
        "waiting",
        "me",
        Some("2026-10-21T03:00:00+07:00"),
        false,
        "2026-10-22T08:00:00+07:00",
    )
    .await;
    digest_row(
        &db,
        "db2",
        &b2.id,
        "done",
        "none",
        None,
        true,
        "2026-10-22T08:00:00+07:00",
    )
    .await;

    let out = store::overview_at(&db, None, now).await.unwrap();
    let names: Vec<&str> = out.iter().map(|p| p.project_name.as_str()).collect();
    assert_eq!(names, vec!["Beta", "Alpha"], "red before green");
    let beta = &out[0];
    assert_eq!(beta.health.health, "red");
    assert_eq!(
        (beta.health.counts.needs_me, beta.health.counts.watching),
        (1, 2)
    );
    let rows: Vec<(&str, &str, bool)> = beta
        .watches
        .iter()
        .map(|w| {
            (
                w.watch.id.as_str(),
                w.health.health.as_str(),
                w.health.finished,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (b1.id.as_str(), "red", false),
            (b2.id.as_str(), "done", true)
        ]
    );
    let v = serde_json::to_value(&out[0]).unwrap();
    assert_eq!(v["health"], "red");
    assert_eq!(v["counts"]["needsMe"], 1);
    assert_eq!(v["watches"][1]["finished"], true);
    assert_eq!(v["watches"][1]["digest"]["sourceClosed"], true);
    assert_eq!(out[1].health.health, "green");
}

#[tokio::test]
async fn shutdown_stops_the_digest_engine() {
    let db = mem_db().await;
    let h = harness(&db);
    h.state.shutdown().await;
    assert_eq!(h.engine.shut.load(Ordering::SeqCst), 1);
    // A translate state without a warm sidecar shuts down as a no-op.
    crate::commands::translate::TranslateState::default()
        .shutdown()
        .await;
}
