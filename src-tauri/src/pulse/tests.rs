//! Watch rules, project lookup, overview and the BR-11 queue on an in-memory
//! DB with a fake Slack capture. No network.

use super::source::{parse_source, Source};
use super::store::{self, WatchRequest};
use super::*;
use sqlx::sqlite::SqlitePoolOptions;
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
}

#[derive(Default)]
struct RecEvents(Mutex<Vec<Vec<String>>>);

impl PulseEvents for RecEvents {
    fn changed(&self, watch_ids: &[String]) {
        self.0.lock().unwrap().push(watch_ids.to_vec());
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
    assert_eq!(p.health, None);
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

async fn insert_thread_1(db: &Db) {
    sqlx::query(
        "INSERT INTO slack_threads (id, title, created_at, updated_at) \
         VALUES ('thread-1', 't', '2026-01-01', '2026-01-01')",
    )
    .execute(db)
    .await
    .unwrap();
}

#[tokio::test]
async fn process_pending_resolves_watches_and_closes_requests() {
    let db = mem_db().await;
    insert_thread_1(&db).await;
    let slack = Arc::new(FakeSlack::default());
    let events = Arc::new(RecEvents::default());
    let state = PulseState::new(db.clone(), slack.clone(), events.clone());

    let ws = store::watch(&db, req("p1", &src(SLACK), false))
        .await
        .unwrap()
        .watch;
    let wg = store::watch(&db, req("p1", &src(ISSUE), false))
        .await
        .unwrap()
        .watch;
    let wi = store::watch(&db, req("p1", &src("org/app#99"), false))
        .await
        .unwrap()
        .watch;
    store::unwatch(&db, &wi.id).await.unwrap(); // inactive: left alone
    sqlx::query(
        "INSERT INTO pulse_requests (id, kind, watch_id, created_at) VALUES \
         ('q1', 'resolve', ?, '2026-01-01'), ('q2', 'refresh', NULL, '2026-01-01')",
    )
    .bind(&ws.id)
    .execute(&db)
    .await
    .unwrap();

    let changed = state.process_pending().await;
    assert_eq!(changed, vec![ws.id.clone(), wg.id.clone()]);
    assert_eq!(
        *slack.calls.lock().unwrap(),
        vec![(SLACK.to_string(), "p1".to_string())]
    );
    let s = store::get(&db, &ws.id).await.unwrap().unwrap();
    assert_eq!(
        (
            s.resolve_status.as_str(),
            s.slack_thread_id.as_deref(),
            s.title.as_deref()
        ),
        ("ready", Some("thread-1"), Some("Deploy thread"))
    );
    let g = store::get(&db, &wg.id).await.unwrap().unwrap();
    assert_eq!(g.resolve_status, "ready");
    assert_eq!(
        store::get(&db, &wi.id)
            .await
            .unwrap()
            .unwrap()
            .resolve_status,
        "pending"
    );
    let open: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM pulse_requests WHERE done_at IS NULL")
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(open, 0);
    assert_eq!(
        *events.0.lock().unwrap(),
        vec![vec![ws.id.clone(), wg.id.clone()]]
    );

    // Nothing pending → no event.
    assert!(state.process_pending().await.is_empty());
    assert_eq!(events.0.lock().unwrap().len(), 1);

    // A write the app did not make (an MCP unwatch) → reload-all event.
    sqlx::query("UPDATE pulse_watches SET active = 0, updated_at = '2099-01-01T00:00:00+00:00' WHERE id = ?")
        .bind(&wg.id)
        .execute(&db)
        .await
        .unwrap();
    assert!(state.process_pending().await.is_empty());
    assert_eq!(events.0.lock().unwrap().last(), Some(&Vec::new()));
    state.process_pending().await;
    assert_eq!(events.0.lock().unwrap().len(), 2);

    // Capture failure → error with the Slack code.
    *slack.fail.lock().unwrap() = Some("no_token".into());
    store::move_watch(&db, &ws.id, "p2").await.unwrap();
    state.process_pending().await;
    let s = store::get(&db, &ws.id).await.unwrap().unwrap();
    assert_eq!(
        (
            s.resolve_status.as_str(),
            s.last_error.as_deref(),
            s.project_id.as_str()
        ),
        ("error", Some("no_token"), "p2")
    );
    assert_eq!(slack.calls.lock().unwrap()[1].1, "p2");
}

#[tokio::test]
async fn resolve_links_slack_thread_and_ignores_stale_results() {
    let db = mem_db().await;
    insert_thread_1(&db).await;
    let slack = Arc::new(FakeSlack::default());
    let state = PulseState::new(db.clone(), slack, Arc::new(RecEvents::default()));
    let w = store::watch(&db, req("p1", &src(SLACK), false))
        .await
        .unwrap()
        .watch;
    state.process_pending().await;
    let s = store::get(&db, &w.id).await.unwrap().unwrap();
    assert_eq!(s.slack_thread_id.as_deref(), Some("thread-1"));

    // Deleting the thread keeps the watch (ON DELETE SET NULL).
    sqlx::query("DELETE FROM slack_threads WHERE id = 'thread-1'")
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(
        store::get(&db, &w.id)
            .await
            .unwrap()
            .unwrap()
            .slack_thread_id,
        None
    );

    // A result computed for the old project is dropped after a move.
    let w = store::move_watch(&db, &w.id, "p2").await.unwrap();
    let stale = store::Watch {
        project_id: "p1".into(),
        ..w.clone()
    };
    assert!(!store::set_resolved(&db, &stale, None, None, None)
        .await
        .unwrap());
    assert_eq!(
        store::get(&db, &w.id)
            .await
            .unwrap()
            .unwrap()
            .resolve_status,
        "pending"
    );
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
    let state = PulseState::new(
        db.clone(),
        Arc::new(FakeSlack::default()),
        Arc::new(RecEvents::default()),
    );
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
    assert_eq!(
        store::get(&db, &w.id)
            .await
            .unwrap()
            .unwrap()
            .resolve_status,
        "ready"
    );
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
