//! Cross-device sessions (design §6): run ownership and leases.
//!
//! The device that starts or resumes a run becomes its owner
//! (`runs.owner_device`, `runs.device_name`) and holds a lease
//! (`runs.lease_until`) renewed by a heartbeat while the sidecar is alive and
//! cleared when it ends. All of it syncs with the run row, so other devices
//! show the session read-only ("running on <device>") while the lease is valid
//! and may take it over ("Continue here") once it is idle or the lease expired
//! (the owner crashed or went offline).
//!
//! Timestamps are written as RFC 3339 UTC with millisecond precision (`...Z`).

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use crate::db::Db;
use crate::runs::RunRegistry;

/// How long a lease stays valid without a heartbeat.
pub const LEASE_SECS: i64 = 90;
/// Heartbeat period while a run's sidecar is alive.
pub const HEARTBEAT_SECS: u64 = 30;

fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn lease_deadline(now: DateTime<Utc>) -> String {
    ts(now + chrono::Duration::seconds(LEASE_SECS))
}

/// Whether a lease timestamp is still in the future. Unparseable = expired.
pub fn lease_active(lease_until: Option<&str>, now: DateTime<Utc>) -> bool {
    lease_until
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .is_some_and(|t| t.with_timezone(&Utc) > now)
}

/// Another device owns the run and its lease is valid: this device must not
/// start, resume, message or cancel it.
pub fn held_elsewhere(
    owner: Option<&str>,
    local: &str,
    lease_until: Option<&str>,
    now: DateTime<Utc>,
) -> bool {
    owner.is_some_and(|o| !o.is_empty() && o != local) && lease_active(lease_until, now)
}

pub fn running_elsewhere_message(device_name: Option<&str>) -> String {
    let name = device_name.map(str::trim).filter(|s| !s.is_empty()).unwrap_or("another device");
    format!("This session is running on {name}")
}

// ---- device identity ------------------------------------------------------

#[derive(Debug, Clone)]
pub struct LocalDevice {
    pub id: String,
    pub name: String,
}

/// macOS computer name (`scutil --get ComputerName`), else the host name.
fn computer_name() -> String {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        let run = |cmd: &str, args: &[&str]| {
            std::process::Command::new(cmd)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        };
        run("scutil", &["--get", "ComputerName"])
            .or_else(|| run("hostname", &["-s"]))
            .unwrap_or_else(|| "Unknown device".to_string())
    })
    .clone()
}

/// This machine's sync device id and display name (`sync_device.name`,
/// defaulting to the computer name).
pub async fn local_device(db: &Db) -> Result<LocalDevice, String> {
    let id = super::ensure_device(db).await?;
    let name: Option<String> = sqlx::query_scalar("SELECT name FROM sync_device WHERE id = ?")
        .bind(&id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let name = match name.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
        Some(n) => n,
        None => tokio::task::spawn_blocking(computer_name).await.unwrap_or_default(),
    };
    Ok(LocalDevice { id, name })
}

// ---- git state of the working tree ----------------------------------------

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct GitState {
    pub head: Option<String>,
    pub dirty: Option<bool>,
}

async fn git(cwd: &str, args: &[&str]) -> Option<String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(cwd).args(args).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await.ok()?.ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// HEAD + dirty flag of `cwd` (both None when it is not a git work tree).
/// Untracked files don't count: `.devdy/` scratch files would always flag it.
pub async fn git_state(cwd: &str) -> GitState {
    if !Path::new(cwd).is_dir() {
        return GitState::default();
    }
    let Some(head) = git(cwd, &["rev-parse", "HEAD"]).await.map(|s| s.trim().to_string()) else {
        return GitState::default();
    };
    let dirty = git(cwd, &["status", "--porcelain", "--untracked-files=no"])
        .await
        .map(|s| !s.trim().is_empty());
    GitState { head: Some(head).filter(|h| !h.is_empty()), dirty }
}

/// The recorded and the local working tree differ in HEAD or dirty flag.
/// Unknown recorded values never count as a difference.
pub fn git_differs(recorded: &GitState, local: &GitState) -> bool {
    let head = recorded.head.is_some() && recorded.head != local.head;
    let dirty = matches!((recorded.dirty, local.dirty), (Some(a), Some(b)) if a != b);
    head || dirty
}

// ---- lease lifecycle (called from the run pipeline) -----------------------

/// Refuse to drive a run whose lease another device holds.
pub async fn ensure_not_held_elsewhere(db: &Db, run_id: &str) -> Result<(), String> {
    let row = sqlx::query("SELECT owner_device, device_name, lease_until FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let Some(row) = row else { return Ok(()) };
    let owner: Option<String> = row.get("owner_device");
    let lease: Option<String> = row.get("lease_until");
    // Only look the local id up when the run is leased at all (the common
    // local case returns without touching sync_device).
    if owner.is_none() || !lease_active(lease.as_deref(), Utc::now()) {
        return Ok(());
    }
    let local = super::ensure_device(db).await?;
    if held_elsewhere(owner.as_deref(), &local, lease.as_deref(), Utc::now()) {
        return Err(running_elsewhere_message(row.get::<Option<String>, _>("device_name").as_deref()));
    }
    Ok(())
}

/// The run (re)started here: take ownership and a fresh lease now, then record
/// the working tree's git state in the background (git can be slow).
pub async fn claim(db: &Db, run_id: &str, cwd: &str) -> Result<(), String> {
    let me = local_device(db).await?;
    sqlx::query("UPDATE runs SET owner_device = ?, device_name = ?, lease_until = ? WHERE id = ?")
        .bind(&me.id)
        .bind(&me.name)
        .bind(lease_deadline(Utc::now()))
        .bind(run_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    let (db, run_id, cwd) = (db.clone(), run_id.to_string(), cwd.to_string());
    tokio::spawn(async move {
        let g = git_state(&cwd).await;
        let _ = sqlx::query("UPDATE runs SET git_head = ?, git_dirty = ? WHERE id = ? AND owner_device = ?")
            .bind(&g.head)
            .bind(g.dirty.map(i64::from))
            .bind(&run_id)
            .bind(&me.id)
            .execute(&db)
            .await;
    });
    Ok(())
}

/// Best-effort [`claim`] for the run pipeline: a failure only loses the
/// cross-device view of this run, never the run itself.
pub async fn claim_logged(db: &Db, run_id: &str, cwd: &str) {
    if let Err(e) = claim(db, run_id, cwd).await {
        tracing::warn!(event = "sync_run_claim_failed", run_id, error = %e);
    }
}

/// Renew the lease of runs this device owns.
pub async fn renew(db: &Db, run_ids: &[String]) -> Result<(), String> {
    if run_ids.is_empty() {
        return Ok(());
    }
    let local = super::ensure_device(db).await?;
    let until = lease_deadline(Utc::now());
    for id in run_ids {
        sqlx::query("UPDATE runs SET lease_until = ? WHERE id = ? AND owner_device = ?")
            .bind(&until)
            .bind(id)
            .bind(&local)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// The run's sidecar ended (finished, cancelled, crashed): drop the lease so
/// other devices may continue it. A lease held by another device is left alone.
pub async fn release(db: &Db, run_id: &str) {
    let Ok(local) = super::ensure_device(db).await else { return };
    let _ = sqlx::query(
        "UPDATE runs SET lease_until = NULL \
         WHERE id = ? AND lease_until IS NOT NULL AND (owner_device IS NULL OR owner_device = ?)",
    )
    .bind(run_id)
    .bind(&local)
    .execute(db)
    .await;
}

/// App start, before anything runs: nothing from the previous session drives
/// this device's runs any more. Leftover `running` rows become `cancelled` and
/// their leases are dropped — but only for runs owned here (or never owned, from
/// before sync): a run another device is running must not be touched, or the
/// change would sync back and clobber it.
pub async fn reconcile_startup(db: &Db) -> Result<(), sqlx::Error> {
    const MINE: &str = "(owner_device IS NULL OR owner_device IN (SELECT id FROM sync_device))";
    sqlx::query(&format!(
        "UPDATE runs SET status = 'cancelled', finished_at = ?, lease_until = NULL \
         WHERE status = 'running' AND {MINE}"
    ))
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    sqlx::query(&format!("UPDATE runs SET lease_until = NULL WHERE lease_until IS NOT NULL AND {MINE}"))
        .execute(db)
        .await?;
    Ok(())
}

/// Renew the leases of every run whose sidecar is alive, every
/// [`HEARTBEAT_SECS`].
pub fn spawn_heartbeat(db: Db, registry: RunRegistry) {
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(HEARTBEAT_SECS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let ids: Vec<String> = registry.lock().await.keys().cloned().collect();
            if let Err(e) = renew(&db, &ids).await {
                tracing::warn!(event = "sync_lease_renew_failed", error = %e);
            }
        }
    });
}

// ---- take over ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitMismatch {
    pub recorded: GitState,
    pub local: GitState,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TakeOverDecision {
    /// Another device still holds the lease.
    Refused,
    /// The checkouts differ; the user has to confirm first.
    Confirm(GitMismatch),
    Proceed,
}

/// "Continue here" rules: allowed when the run is owned here (or by nobody) or
/// the owner's lease expired. Taking a remote run over needs a confirmation
/// (`force`) when this checkout differs from the one the run last ran on.
pub fn decide_take_over(
    owner: Option<&str>,
    local_id: &str,
    lease_until: Option<&str>,
    now: DateTime<Utc>,
    recorded: &GitState,
    local: &GitState,
    force: bool,
) -> TakeOverDecision {
    if held_elsewhere(owner, local_id, lease_until, now) {
        return TakeOverDecision::Refused;
    }
    let remote = owner.is_some_and(|o| !o.is_empty() && o != local_id);
    if remote && !force && git_differs(recorded, local) {
        return TakeOverDecision::Confirm(GitMismatch { recorded: recorded.clone(), local: local.clone() });
    }
    TakeOverDecision::Proceed
}

#[derive(Debug, Serialize)]
pub struct TakeOverResult {
    /// Ownership moved here; the next message resumes the session locally.
    pub taken: bool,
    /// Set (with `taken = false`) when the user must confirm a git mismatch.
    pub mismatch: Option<GitMismatch>,
    /// The synced CLI transcript is present, so the resume keeps the context.
    pub transcript_available: bool,
}

async fn transcript_available(
    db: &Db,
    engine: &str,
    session_id: Option<&str>,
    transcript_path: Option<&str>,
    project_path: &str,
) -> bool {
    let Some(sid) = session_id.filter(|s| !s.is_empty()) else { return false };
    if engine == "codex" {
        return crate::commands::codex_sessions::codex_session_file(sid).is_some();
    }
    if transcript_path.is_some_and(|p| Path::new(p).is_file()) {
        return true;
    }
    crate::commands::sessions::claude_sessions_dirs(db, project_path)
        .await
        .iter()
        .any(|d| d.join(format!("{sid}.jsonl")).is_file())
}

pub async fn take_over(db: &Db, run_id: &str, force: bool) -> Result<TakeOverResult, String> {
    let row = sqlx::query(
        "SELECT r.owner_device, r.device_name, r.lease_until, r.git_head, r.git_dirty, r.status,
                r.engine, r.session_id, r.transcript_path, p.path AS project_path
         FROM runs r JOIN projects p ON p.id = r.project_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    let owner: Option<String> = row.get("owner_device");
    let lease: Option<String> = row.get("lease_until");
    let project_path: String = row.get("project_path");
    let recorded = GitState {
        head: row.get("git_head"),
        dirty: row.get::<Option<i64>, _>("git_dirty").map(|d| d != 0),
    };
    let me = local_device(db).await?;
    let remote = owner.as_deref().is_some_and(|o| !o.is_empty() && o != me.id);
    let local_git = if remote { git_state(&project_path).await } else { GitState::default() };
    let available = transcript_available(
        db,
        &row.get::<String, _>("engine"),
        row.get::<Option<String>, _>("session_id").as_deref(),
        row.get::<Option<String>, _>("transcript_path").as_deref(),
        &project_path,
    )
    .await;

    match decide_take_over(owner.as_deref(), &me.id, lease.as_deref(), Utc::now(), &recorded, &local_git, force) {
        TakeOverDecision::Refused => {
            Err(running_elsewhere_message(row.get::<Option<String>, _>("device_name").as_deref()))
        }
        TakeOverDecision::Confirm(m) => {
            Ok(TakeOverResult { taken: false, mismatch: Some(m), transcript_available: available })
        }
        TakeOverDecision::Proceed => {
            // A run left `running` by an owner whose lease expired is dead.
            let now = Utc::now().to_rfc3339();
            sqlx::query(
                "UPDATE runs SET owner_device = ?, device_name = ?, lease_until = NULL, \
                 finished_at = CASE WHEN status = 'running' THEN ? ELSE finished_at END, \
                 status = CASE WHEN status = 'running' THEN 'cancelled' ELSE status END \
                 WHERE id = ?",
            )
            .bind(&me.id)
            .bind(&me.name)
            .bind(&now)
            .bind(run_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            Ok(TakeOverResult { taken: true, mismatch: None, transcript_available: available })
        }
    }
}

// ---- commands ---------------------------------------------------------------

/// A run of the project owned by another device (for the device badge and the
/// read-only view).
#[derive(Debug, Serialize)]
pub struct RemoteRunOwner {
    pub run_id: String,
    pub owner_device: String,
    pub device_name: Option<String>,
    pub lease_until: Option<String>,
    /// The owner's lease is valid right now: the run is live over there.
    pub running: bool,
    pub git_head: Option<String>,
    pub git_dirty: Option<bool>,
}

pub async fn remote_owners(db: &Db, project_id: &str) -> Result<Vec<RemoteRunOwner>, String> {
    let local = super::ensure_device(db).await?;
    let rows = sqlx::query(
        "SELECT id, owner_device, device_name, lease_until, git_head, git_dirty FROM runs \
         WHERE project_id = ? AND owner_device IS NOT NULL AND owner_device != '' AND owner_device != ?",
    )
    .bind(project_id)
    .bind(&local)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let now = Utc::now();
    Ok(rows
        .iter()
        .map(|r| {
            let lease_until: Option<String> = r.get("lease_until");
            RemoteRunOwner {
                run_id: r.get("id"),
                owner_device: r.get("owner_device"),
                device_name: r.get("device_name"),
                running: lease_active(lease_until.as_deref(), now),
                lease_until,
                git_head: r.get("git_head"),
                git_dirty: r.get::<Option<i64>, _>("git_dirty").map(|d| d != 0),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn sync_run_owners(
    db: tauri::State<'_, Db>,
    project_id: String,
) -> Result<Vec<RemoteRunOwner>, String> {
    remote_owners(db.inner(), &project_id).await
}

/// "Continue here": move a session owned by another device to this one. With
/// `force = false` a git mismatch is returned for confirmation instead.
#[tauri::command]
pub async fn sync_take_over_run(
    db: tauri::State<'_, Db>,
    run_id: String,
    force: Option<bool>,
) -> Result<TakeOverResult, String> {
    take_over(db.inner(), &run_id, force.unwrap_or(false)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }
    const NOW: &str = "2026-10-30T12:00:00.000Z";
    const LATER: &str = "2026-10-30T12:01:00.000Z";
    const EARLIER: &str = "2026-10-30T11:59:00.000Z";

    fn git(head: &str, dirty: bool) -> GitState {
        GitState { head: Some(head.into()), dirty: Some(dirty) }
    }

    #[test]
    fn lease_validity() {
        let now = at(NOW);
        assert!(lease_active(Some(LATER), now));
        assert!(!lease_active(Some(EARLIER), now));
        assert!(!lease_active(Some(NOW), now));
        assert!(!lease_active(None, now));
        assert!(!lease_active(Some("garbage"), now));
        // Other offsets compare by instant.
        assert!(lease_active(Some("2026-10-30T21:00:30+09:00"), now));
        assert_eq!(lease_deadline(now), "2026-10-30T12:01:30.000Z");
    }

    #[test]
    fn guard_only_blocks_a_valid_remote_lease() {
        let now = at(NOW);
        assert!(held_elsewhere(Some("other"), "me", Some(LATER), now));
        assert!(!held_elsewhere(Some("other"), "me", Some(EARLIER), now), "expired lease");
        assert!(!held_elsewhere(Some("other"), "me", None, now), "idle");
        assert!(!held_elsewhere(Some("me"), "me", Some(LATER), now), "own lease");
        assert!(!held_elsewhere(None, "me", Some(LATER), now), "never owned");
        assert_eq!(running_elsewhere_message(Some("Mac mini")), "This session is running on Mac mini");
        assert_eq!(running_elsewhere_message(Some(" ")), "This session is running on another device");
    }

    #[test]
    fn git_comparison() {
        assert!(!git_differs(&git("a", false), &git("a", false)));
        assert!(git_differs(&git("a", false), &git("b", false)));
        assert!(git_differs(&git("a", false), &git("a", true)));
        assert!(git_differs(&git("a", true), &git("a", false)));
        assert!(git_differs(&git("a", false), &GitState::default()), "not a repo here");
        assert!(!git_differs(&GitState::default(), &git("a", true)), "nothing recorded");
    }

    #[test]
    fn take_over_rules() {
        let now = at(NOW);
        let same = git("a", false);
        let other = git("b", false);
        use TakeOverDecision::*;
        assert_eq!(decide_take_over(Some("x"), "me", Some(LATER), now, &same, &same, true), Refused);
        assert_eq!(decide_take_over(Some("x"), "me", None, now, &same, &same, false), Proceed);
        assert_eq!(decide_take_over(Some("x"), "me", Some(EARLIER), now, &same, &same, false), Proceed);
        assert!(matches!(decide_take_over(Some("x"), "me", None, now, &same, &other, false), Confirm(_)));
        assert_eq!(decide_take_over(Some("x"), "me", None, now, &same, &other, true), Proceed);
        // Own runs never need a confirmation.
        assert_eq!(decide_take_over(Some("me"), "me", Some(LATER), now, &same, &other, false), Proceed);
        assert_eq!(decide_take_over(None, "me", None, now, &same, &other, false), Proceed);
    }

    async fn mem_db() -> Db {
        let db = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/nonexistent/p1', '2026-01-01')")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("INSERT INTO sync_device (id, name, created_at) VALUES ('me', 'MacBook', '2026-01-01')")
            .execute(&db)
            .await
            .unwrap();
        db
    }

    async fn add_run(db: &Db, id: &str, status: &str, owner: Option<&str>, lease: Option<&str>) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, created_at, owner_device, device_name, lease_until) \
             VALUES (?, 'p1', 'session', ?, 'claude', '2026-01-01', ?, ?, ?)",
        )
        .bind(id)
        .bind(status)
        .bind(owner)
        .bind(owner.map(|o| format!("{o}-name")))
        .bind(lease)
        .execute(db)
        .await
        .unwrap();
    }

    async fn col(db: &Db, id: &str, c: &str) -> Option<String> {
        sqlx::query_scalar(&format!("SELECT CAST({c} AS TEXT) FROM runs WHERE id = ?"))
            .bind(id)
            .fetch_one(db)
            .await
            .unwrap()
    }

    fn future() -> String {
        ts(Utc::now() + chrono::Duration::seconds(60))
    }

    #[tokio::test]
    async fn claim_renew_release() {
        let db = mem_db().await;
        add_run(&db, "r", "running", None, None).await;
        claim(&db, "r", "/nonexistent/p1").await.unwrap();
        assert_eq!(col(&db, "r", "owner_device").await.as_deref(), Some("me"));
        assert_eq!(col(&db, "r", "device_name").await.as_deref(), Some("MacBook"));
        assert!(lease_active(col(&db, "r", "lease_until").await.as_deref(), Utc::now()));
        ensure_not_held_elsewhere(&db, "r").await.unwrap();

        renew(&db, &["r".into()]).await.unwrap();
        assert!(lease_active(col(&db, "r", "lease_until").await.as_deref(), Utc::now()));
        release(&db, "r").await;
        assert_eq!(col(&db, "r", "lease_until").await, None);

        // A remote lease is neither renewed nor released from here.
        let lease = future();
        add_run(&db, "x", "running", Some("mini"), Some(&lease)).await;
        renew(&db, &["x".into()]).await.unwrap();
        release(&db, "x").await;
        assert_eq!(col(&db, "x", "lease_until").await, Some(lease));
        let err = ensure_not_held_elsewhere(&db, "x").await.unwrap_err();
        assert_eq!(err, "This session is running on mini-name");
    }

    #[tokio::test]
    async fn startup_reconcile_leaves_remote_runs_alone() {
        let db = mem_db().await;
        let lease = future();
        add_run(&db, "legacy", "running", None, None).await;
        add_run(&db, "mine", "running", Some("me"), Some(&lease)).await;
        add_run(&db, "idle", "completed", Some("me"), Some(&lease)).await;
        add_run(&db, "theirs", "running", Some("mini"), Some(&lease)).await;
        reconcile_startup(&db).await.unwrap();
        for id in ["legacy", "mine"] {
            assert_eq!(col(&db, id, "status").await.as_deref(), Some("cancelled"), "{id}");
            assert_eq!(col(&db, id, "lease_until").await, None, "{id}");
        }
        assert_eq!(col(&db, "idle", "lease_until").await, None);
        assert_eq!(col(&db, "idle", "status").await.as_deref(), Some("completed"));
        assert_eq!(col(&db, "theirs", "status").await.as_deref(), Some("running"));
        assert_eq!(col(&db, "theirs", "lease_until").await, Some(lease));
    }

    #[tokio::test]
    async fn take_over_moves_ownership() {
        let db = mem_db().await;
        let lease = future();
        add_run(&db, "live", "running", Some("mini"), Some(&lease)).await;
        assert!(take_over(&db, "live", true).await.unwrap_err().contains("mini-name"));

        // Owner crashed: lease expired while the row still says running.
        add_run(&db, "dead", "running", Some("mini"), Some("2020-01-01T00:00:00.000Z")).await;
        let r = take_over(&db, "dead", false).await.unwrap();
        assert!(r.taken && r.mismatch.is_none() && !r.transcript_available);
        assert_eq!(col(&db, "dead", "owner_device").await.as_deref(), Some("me"));
        assert_eq!(col(&db, "dead", "device_name").await.as_deref(), Some("MacBook"));
        assert_eq!(col(&db, "dead", "status").await.as_deref(), Some("cancelled"));

        // A recorded HEAD that this (non-git) checkout can't match asks first.
        add_run(&db, "idle", "completed", Some("mini"), None).await;
        sqlx::query("UPDATE runs SET git_head = 'abc', git_dirty = 0 WHERE id = 'idle'")
            .execute(&db)
            .await
            .unwrap();
        let r = take_over(&db, "idle", false).await.unwrap();
        assert!(!r.taken);
        assert_eq!(r.mismatch.unwrap().recorded.head.as_deref(), Some("abc"));
        assert_eq!(col(&db, "idle", "owner_device").await.as_deref(), Some("mini"));
        assert!(take_over(&db, "idle", true).await.unwrap().taken);
        assert_eq!(col(&db, "idle", "status").await.as_deref(), Some("completed"));

        let owners = remote_owners(&db, "p1").await.unwrap();
        assert_eq!(owners.iter().map(|o| o.run_id.as_str()).collect::<Vec<_>>(), vec!["live"]);
        assert!(owners[0].running);
    }
}
