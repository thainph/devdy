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
//! Lease expiry is measured on this machine's clock: a remote lease counts as
//! live until [`LEASE_SECS`] passed locally since the last renewal arrived
//! here ([`LeaseReceipts`]), so clock skew between devices does not matter.
//! When a synced row says another device owns a run whose sidecar is still
//! live here, that sidecar is stopped (the session continued over there).
//! On release the owner records the final log/transcript sizes; a take-over
//! waits until the synced copies here have caught up with them.
//!
//! Timestamps are written as RFC 3339 UTC with millisecond precision (`...Z`).

use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use sqlx::Row;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex as StdMutex, OnceLock};
use std::time::{Duration, Instant};

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

/// Local receive times of remote lease renewals: `run_id -> (lease_until
/// value, when it was first seen here)`. Machine-local and in memory: after a
/// restart a lease gets a full [`LEASE_SECS`] from the moment it is first seen.
#[derive(Default)]
pub struct LeaseReceipts(StdMutex<HashMap<String, (String, Instant)>>);

impl LeaseReceipts {
    /// Record the lease value a synced row carries. A new value is a renewal
    /// received `now`; None (released) forgets the run.
    pub fn observe(&self, run_id: &str, lease_until: Option<&str>, now: Instant) {
        let Ok(mut map) = self.0.lock() else { return };
        match lease_until.filter(|l| !l.is_empty()) {
            None => {
                map.remove(run_id);
            }
            Some(l) => {
                if map.get(run_id).is_none_or(|(seen, _)| seen != l) {
                    map.insert(run_id.to_string(), (l.to_string(), now));
                }
            }
        }
    }

    /// How long the remote lease stays live, measured locally; None = expired
    /// or released.
    pub fn remaining(&self, run_id: &str, lease_until: Option<&str>, now: Instant) -> Option<Duration> {
        self.observe(run_id, lease_until, now);
        let map = self.0.lock().ok()?;
        let (_, at) = map.get(run_id)?;
        let left = Duration::from_secs(LEASE_SECS as u64).checked_sub(now.saturating_duration_since(*at))?;
        (!left.is_zero()).then_some(left)
    }

    pub fn live(&self, run_id: &str, lease_until: Option<&str>, now: Instant) -> bool {
        self.remaining(run_id, lease_until, now).is_some()
    }

    pub fn forget(&self, run_id: &str) {
        if let Ok(mut map) = self.0.lock() {
            map.remove(run_id);
        }
    }
}

pub fn receipts() -> &'static LeaseReceipts {
    static R: LazyLock<LeaseReceipts> = LazyLock::new(LeaseReceipts::default);
    &R
}

fn is_remote(owner: Option<&str>, local: &str) -> bool {
    owner.is_some_and(|o| !o.is_empty() && o != local)
}

/// Another device owns the run and its lease is live (see [`LeaseReceipts`]):
/// this device must not start, resume, message or cancel it.
pub fn held_elsewhere(owner: Option<&str>, local: &str, lease_live: bool) -> bool {
    is_remote(owner, local) && lease_live
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
    if owner.is_none() || lease.is_none() {
        return Ok(());
    }
    let local = super::ensure_device(db).await?;
    let live = is_remote(owner.as_deref(), &local) && receipts().live(run_id, lease.as_deref(), Instant::now());
    if held_elsewhere(owner.as_deref(), &local, live) {
        return Err(running_elsewhere_message(row.get::<Option<String>, _>("device_name").as_deref()));
    }
    Ok(())
}

/// The run (re)started here: take ownership and a fresh lease now (a run
/// another device owned moves to the next ownership epoch), then record
/// the working tree's git state in the background (git can be slow).
///
/// The run's log is this device's drain output from now on: the transcript
/// size last mirrored (machine-local; set when sync wrote the transcript) is
/// dropped, or the session importer would take the resumed turn's growth for
/// work done outside Devdy and rebuild the merged log from the transcript
/// (review-3 M2).
pub async fn claim(db: &Db, run_id: &str, cwd: &str) -> Result<(), String> {
    let me = local_device(db).await?;
    sqlx::query(
        "UPDATE runs SET owner_epoch = owner_epoch + (COALESCE(owner_device, '') NOT IN ('', ?)), \
         owner_device = ?, device_name = ?, lease_until = ?, \
         final_log_size = NULL, final_transcript_size = NULL, transcript_synced_size = NULL WHERE id = ?",
    )
        .bind(&me.id)
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

/// Review-3 L6: this device is about to write a run another device owns but
/// no longer drives (its lease expired here, checked by the caller): take the
/// ownership first, in the next epoch, like a take-over. A bare status write
/// at the old epoch could be undone by the old owner's next renewal (newer
/// HLC, same epoch) or undo a take-over made elsewhere meanwhile. Returns
/// whether the run moved here.
pub async fn adopt_orphan(db: &Db, run_id: &str) -> Result<bool, String> {
    let me = local_device(db).await?;
    let moved = sqlx::query(
        "UPDATE runs SET owner_epoch = owner_epoch + 1, owner_device = ?, device_name = ?, lease_until = NULL \
         WHERE id = ? AND COALESCE(owner_device, '') NOT IN ('', ?)",
    )
    .bind(&me.id)
    .bind(&me.name)
    .bind(run_id)
    .bind(&me.id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?
    .rows_affected();
    if moved > 0 {
        receipts().forget(run_id);
    }
    Ok(moved > 0)
}

/// Best-effort [`claim`] for the run pipeline: a failure only loses the
/// cross-device view of this run, never the run itself.
pub async fn claim_logged(db: &Db, run_id: &str, cwd: &str) {
    if let Err(e) = claim(db, run_id, cwd).await {
        tracing::warn!(event = "sync_run_claim_failed", run_id, error = %e);
    }
}

/// Renew the lease of runs this device owns. The owner is re-checked in the
/// UPDATE itself: once a take-over from another device landed here, a still
/// running sidecar must not write the row (and its owner) back.
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
/// other devices may continue it, record the final size of its log and
/// transcript (a device taking over waits for its synced copies to catch up)
/// and kick both files so their last bytes sync right away. A run another
/// device owns is left alone.
pub async fn release(db: &Db, run_id: &str) {
    let Ok(local) = super::ensure_device(db).await else { return };
    let Ok(Some(row)) = sqlx::query(
        "SELECT r.owner_device, r.output_path, r.engine, r.session_id, r.transcript_path, p.path AS project_path
         FROM runs r LEFT JOIN projects p ON p.id = r.project_id WHERE r.id = ?",
    )
    .bind(run_id)
    .fetch_optional(db)
    .await
    else {
        return;
    };
    if is_remote(row.get::<Option<String>, _>("owner_device").as_deref(), &local) {
        return;
    }
    let log = row.get::<Option<String>, _>("output_path").map(PathBuf::from);
    let transcript = transcript_file(
        db,
        &row.get::<String, _>("engine"),
        row.get::<Option<String>, _>("session_id").as_deref(),
        row.get::<Option<String>, _>("transcript_path").as_deref(),
        row.get::<Option<String>, _>("project_path").as_deref(),
    )
    .await;
    // Sizes as the other devices get the files (a transcript sync rewrote
    // here and nothing appended to since never travels again, so its size on
    // disk would never be reached there).
    let size = |p: Option<PathBuf>| async move {
        match p {
            Some(p) => super::files::wire_size(db, &p).await.map(|n| n as i64),
            None => None,
        }
    };
    let (log_size, transcript_size) = (size(log.clone()).await, size(transcript.clone()).await);
    let _ = sqlx::query(
        "UPDATE runs SET lease_until = NULL, final_log_size = ?, final_transcript_size = ? \
         WHERE id = ? AND (owner_device IS NULL OR owner_device = ?) \
         AND (lease_until IS NOT NULL OR final_log_size IS NOT ? OR final_transcript_size IS NOT ?)",
    )
    .bind(log_size)
    .bind(transcript_size)
    .bind(run_id)
    .bind(&local)
    .bind(log_size)
    .bind(transcript_size)
    .execute(db)
    .await;
    for p in [log, transcript].into_iter().flatten() {
        super::files::kick(p);
    }
}

/// SQL condition on `runs`: the run is this device's (or never owned, from
/// before sync). Anything counting "running" runs to decide what THIS device
/// does must use it, or a session another device runs would count here.
pub const OWNED_HERE: &str =
    "(owner_device IS NULL OR owner_device = '' OR owner_device IN (SELECT id FROM sync_device))";

/// App start, before anything runs: nothing from the previous session drives
/// this device's runs any more. Leftover `running` rows become `cancelled` and
/// their leases are dropped — but only for runs owned here (or never owned, from
/// before sync): a run another device is running must not be touched, or the
/// change would sync back and clobber it. Dropping a lease goes through
/// [`release`], so the final log / transcript sizes a device taking the run
/// over waits for are recorded as for any ended run (review-3 L4).
pub async fn reconcile_startup(db: &Db) -> Result<(), sqlx::Error> {
    const MINE: &str = OWNED_HERE;
    let leased: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT id FROM runs WHERE (status = 'running' OR lease_until IS NOT NULL) AND {MINE}"
    ))
    .fetch_all(db)
    .await?;
    sqlx::query(&format!(
        "UPDATE runs SET status = 'cancelled', finished_at = ? \
         WHERE status = 'running' AND {MINE}"
    ))
    .bind(Utc::now().to_rfc3339())
    .execute(db)
    .await?;
    for id in &leased {
        release(db, id).await;
    }
    // Whatever release left (a never-owned run with a stray lease…).
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

// ---- hand-off (another device took a run live here over) -------------------

/// Runs whose local sidecar was stopped because another device took them
/// over: their drain must not flush the log or write the row.
static HANDED_OFF: LazyLock<StdMutex<HashSet<String>>> = LazyLock::new(Default::default);

pub fn is_handed_off(run_id: &str) -> bool {
    HANDED_OFF.lock().is_ok_and(|s| s.contains(run_id))
}

pub fn clear_handed_off(run_id: &str) {
    if let Ok(mut s) = HANDED_OFF.lock() {
        s.remove(run_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandOff {
    pub run_id: String,
    pub device_name: Option<String>,
}

/// Synced `runs` rows were applied: note the lease renewals of remote-owned
/// runs (local receive time) and return the runs live here (`is_live`) that
/// another device owns now.
pub async fn check_applied(
    db: &Db,
    run_ids: &[String],
    is_live: impl Fn(&str) -> bool,
) -> Result<Vec<HandOff>, String> {
    if run_ids.is_empty() {
        return Ok(Vec::new());
    }
    let local = super::ensure_device(db).await?;
    let now = Instant::now();
    let mut out = Vec::new();
    for id in run_ids {
        let row = sqlx::query("SELECT owner_device, device_name, lease_until FROM runs WHERE id = ?")
            .bind(id)
            .fetch_optional(db)
            .await
            .map_err(|e| e.to_string())?;
        let Some(row) = row else {
            receipts().forget(id);
            continue;
        };
        let owner: Option<String> = row.get("owner_device");
        if !is_remote(owner.as_deref(), &local) {
            receipts().forget(id);
            continue;
        }
        receipts().observe(id, row.get::<Option<String>, _>("lease_until").as_deref(), now);
        if is_live(id) {
            out.push(HandOff { run_id: id.clone(), device_name: row.get("device_name") });
        }
    }
    Ok(out)
}

/// [`check_applied`], then stop the local sidecar of every run another device
/// continued. Nothing is written for those runs: status, lease and log are
/// the new owner's now.
pub async fn runs_applied(db: &Db, registry: &RunRegistry, run_ids: &[String]) -> Vec<HandOff> {
    let live: HashSet<String> = registry.lock().await.keys().cloned().collect();
    let handoffs = match check_applied(db, run_ids, |id| live.contains(id)).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!(event = "sync_runs_applied_failed", error = %e);
            return Vec::new();
        }
    };
    let mut stopped = Vec::new();
    for h in handoffs {
        if let Ok(mut s) = HANDED_OFF.lock() {
            s.insert(h.run_id.clone());
        }
        if crate::commands::runs::stop_local_sidecar(registry, &h.run_id).await {
            tracing::info!(event = "sync_run_handed_off", run_id = %h.run_id);
            stopped.push(h);
        } else {
            clear_handed_off(&h.run_id);
        }
    }
    stopped
}

// ---- take over ------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitMismatch {
    pub recorded: GitState,
    pub local: GitState,
}

/// The synced copies of a run's final log/transcript are still behind the
/// sizes the owner recorded on release (bytes, summed over both files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SyncGap {
    pub have: u64,
    pub want: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub enum TakeOverDecision {
    /// The run is live here already: nothing to do.
    AlreadyHere,
    /// Another device still holds the lease.
    Refused,
    /// The owner's final output has not fully arrived yet.
    Syncing(SyncGap),
    /// The checkouts differ; the user has to confirm first.
    Confirm(GitMismatch),
    Proceed,
}

pub struct TakeOverInput<'a> {
    pub owner: Option<&'a str>,
    pub local_id: &'a str,
    /// The owner's lease is live (measured locally).
    pub lease_live: bool,
    /// The run's sidecar is in this device's registry.
    pub live_here: bool,
    pub gap: Option<SyncGap>,
    pub recorded: &'a GitState,
    pub local: &'a GitState,
    /// The user confirmed the git mismatch.
    pub force: bool,
    /// The user chose to continue before the final output synced.
    pub skip_sync: bool,
}

/// "Continue here" rules: allowed when the run is owned here (or by nobody) or
/// the owner's lease expired. Taking a remote run over first waits for its
/// final output to sync (`skip_sync` overrides), then needs a confirmation
/// (`force`) when this checkout differs from the one the run last ran on.
pub fn decide_take_over(i: &TakeOverInput) -> TakeOverDecision {
    if i.live_here {
        return TakeOverDecision::AlreadyHere;
    }
    if held_elsewhere(i.owner, i.local_id, i.lease_live) {
        return TakeOverDecision::Refused;
    }
    if !is_remote(i.owner, i.local_id) {
        return TakeOverDecision::Proceed;
    }
    if let Some(gap) = i.gap.filter(|_| !i.skip_sync) {
        return TakeOverDecision::Syncing(gap);
    }
    if !i.force && git_differs(i.recorded, i.local) {
        return TakeOverDecision::Confirm(GitMismatch { recorded: i.recorded.clone(), local: i.local.clone() });
    }
    TakeOverDecision::Proceed
}

#[derive(Debug, Default, Serialize)]
pub struct TakeOverResult {
    /// Ownership moved here (or the run is live here already); the next
    /// message resumes the session locally.
    pub taken: bool,
    /// Set (with `taken = false`) when the user must confirm a git mismatch.
    pub mismatch: Option<GitMismatch>,
    /// Set (with `taken = false`) while the owner's final log/transcript bytes
    /// are still arriving: retry, or continue anyway with `skip_sync`.
    pub syncing: Option<SyncGap>,
    /// The synced CLI transcript is present, so the resume keeps the context.
    pub transcript_available: bool,
}

/// This device's copy of the run's CLI transcript (Claude session file or
/// Codex rollout), if it can be located.
async fn transcript_file(
    db: &Db,
    engine: &str,
    session_id: Option<&str>,
    transcript_path: Option<&str>,
    project_path: Option<&str>,
) -> Option<PathBuf> {
    let sid = session_id.filter(|s| !s.is_empty())?;
    if engine == "codex" {
        return crate::commands::codex_sessions::codex_session_file(sid);
    }
    if let Some(p) = transcript_path.filter(|p| Path::new(p).is_file()) {
        return Some(PathBuf::from(p));
    }
    crate::commands::sessions::claude_sessions_dirs(db, project_path?)
        .await
        .into_iter()
        .map(|d| d.join(format!("{sid}.jsonl")))
        .find(|p| p.is_file())
}

/// Bytes of `path` this device has: the synced content's size when sync wrote
/// it (the file on disk may carry a rewritten cwd), else the file's size.
async fn synced_size(db: &Db, path: Option<&Path>) -> u64 {
    let Some(path) = path else { return 0 };
    let recorded: Option<i64> =
        sqlx::query_scalar("SELECT size FROM sync_files WHERE local_path = ? AND deleted = 0")
            .bind(path.to_string_lossy().as_ref())
            .fetch_optional(db)
            .await
            .ok()
            .flatten();
    match recorded {
        Some(n) => n.max(0) as u64,
        None => std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
    }
}

/// Compare the final sizes recorded on release with what arrived here.
pub fn sync_gap(files: &[(Option<i64>, u64)]) -> Option<SyncGap> {
    let mut gap = SyncGap { have: 0, want: 0 };
    let mut behind = false;
    for &(want, have) in files {
        let Some(want) = want.filter(|w| *w > 0).map(|w| w as u64) else { continue };
        behind |= have < want;
        gap.have += have.min(want);
        gap.want += want;
    }
    behind.then_some(gap)
}

pub async fn take_over(
    db: &Db,
    run_id: &str,
    live_here: bool,
    force: bool,
    skip_sync: bool,
) -> Result<TakeOverResult, String> {
    let row = sqlx::query(
        "SELECT r.owner_device, r.device_name, r.lease_until, r.git_head, r.git_dirty, r.status,
                r.engine, r.session_id, r.transcript_path, r.output_path,
                r.final_log_size, r.final_transcript_size, p.path AS project_path
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
    let remote = is_remote(owner.as_deref(), &me.id);
    let lease_live = remote && receipts().live(run_id, lease.as_deref(), Instant::now());
    let transcript = transcript_file(
        db,
        &row.get::<String, _>("engine"),
        row.get::<Option<String>, _>("session_id").as_deref(),
        row.get::<Option<String>, _>("transcript_path").as_deref(),
        Some(&project_path),
    )
    .await;
    let available = transcript.is_some();
    let (local_git, gap) = if remote && !live_here && !lease_live {
        let log = row.get::<Option<String>, _>("output_path").map(PathBuf::from);
        let files = [
            (row.get::<Option<i64>, _>("final_log_size"), synced_size(db, log.as_deref()).await),
            (row.get::<Option<i64>, _>("final_transcript_size"), synced_size(db, transcript.as_deref()).await),
        ];
        (git_state(&project_path).await, sync_gap(&files))
    } else {
        (GitState::default(), None)
    };

    let decision = decide_take_over(&TakeOverInput {
        owner: owner.as_deref(),
        local_id: &me.id,
        lease_live,
        live_here,
        gap,
        recorded: &recorded,
        local: &local_git,
        force,
        skip_sync,
    });
    let result = TakeOverResult { transcript_available: available, ..Default::default() };
    match decision {
        TakeOverDecision::AlreadyHere => Ok(TakeOverResult { taken: true, ..result }),
        TakeOverDecision::Refused => {
            Err(running_elsewhere_message(row.get::<Option<String>, _>("device_name").as_deref()))
        }
        TakeOverDecision::Syncing(g) => Ok(TakeOverResult { syncing: Some(g), ..result }),
        TakeOverDecision::Confirm(m) => Ok(TakeOverResult { mismatch: Some(m), ..result }),
        TakeOverDecision::Proceed => {
            // A run left `running` by an owner whose lease expired is dead.
            // Ownership moving here opens a new epoch: whatever the old owner
            // still pushes from before it learns of this never takes it back.
            let now = Utc::now().to_rfc3339();
            sqlx::query(
                "UPDATE runs SET owner_epoch = owner_epoch + (COALESCE(owner_device, '') NOT IN ('', ?)), \
                 owner_device = ?, device_name = ?, lease_until = NULL, transcript_synced_size = NULL, \
                 finished_at = CASE WHEN status = 'running' THEN ? ELSE finished_at END, \
                 status = CASE WHEN status = 'running' THEN 'cancelled' ELSE status END \
                 WHERE id = ?",
            )
            .bind(&me.id)
            .bind(&me.id)
            .bind(&me.name)
            .bind(&now)
            .bind(run_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            receipts().forget(run_id);
            Ok(TakeOverResult { taken: true, ..result })
        }
    }
}

// ---- ownership epochs (applying synced `runs` rows) -----------------------

/// Columns of `runs` owned by the device running it. They move together and
/// follow `owner_epoch`, not the row's HLC: an old owner cut off from the
/// relay keeps renewing its lease (fresh HLCs) while another device takes the
/// run over (a higher epoch); its renewals must not win the run back.
pub const OWNERSHIP_COLUMNS: &[&str] = &[
    "owner_epoch",
    "owner_device",
    "device_name",
    "lease_until",
    "status",
    "finished_at",
    "final_log_size",
    "final_transcript_size",
    "git_head",
    "git_dirty",
];

/// How an incoming `runs` upsert meets the local row's ownership.
#[derive(Debug, PartialEq)]
pub enum OwnerMerge {
    /// Same epoch (or not a run / epoch unknown): plain last-writer-wins.
    Lww,
    /// The local epoch is higher: whatever wins the row, its ownership
    /// columns keep these local values (and an unpushed local change stays
    /// queued).
    KeepLocal(serde_json::Map<String, serde_json::Value>),
    /// The incoming epoch is higher: its ownership columns apply even if the
    /// rest of the op loses by HLC.
    TakeIncoming,
}

fn epoch_of(row: &serde_json::Map<String, serde_json::Value>) -> Option<i64> {
    row.get("owner_epoch").and_then(serde_json::Value::as_i64)
}

/// Compare the ownership epoch of an incoming op with the local row's
/// (called by `apply` inside its transaction, before the HLC checks).
pub async fn owner_merge(
    conn: &mut sqlx::SqliteConnection,
    spec: &super::schema::TableSpec,
    op: &super::op::Op,
) -> Result<OwnerMerge, String> {
    if spec.name != "runs" || op.kind != super::op::OpKind::Upsert {
        return Ok(OwnerMerge::Lww);
    }
    let Some(incoming) = op.row.as_ref().and_then(epoch_of) else { return Ok(OwnerMerge::Lww) };
    let Some(local) = super::codec::read_row(conn, spec, &op.pk).await? else { return Ok(OwnerMerge::Lww) };
    let local_epoch = epoch_of(&local).unwrap_or(0);
    Ok(match local_epoch.cmp(&incoming) {
        std::cmp::Ordering::Equal => OwnerMerge::Lww,
        std::cmp::Ordering::Less => OwnerMerge::TakeIncoming,
        std::cmp::Ordering::Greater => OwnerMerge::KeepLocal(
            OWNERSHIP_COLUMNS
                .iter()
                .filter_map(|c| local.get(*c).map(|v| ((*c).to_string(), v.clone())))
                .collect(),
        ),
    })
}

impl OwnerMerge {
    /// The op lost by HLC: still move a newer ownership onto the local row.
    /// Returns whether anything was written.
    pub async fn apply_skipped(&self, conn: &mut sqlx::SqliteConnection, op: &super::op::Op) -> Result<bool, String> {
        if *self != OwnerMerge::TakeIncoming {
            return Ok(false);
        }
        let (Some(row), Some(id)) = (op.row.as_ref(), op.pk.as_str()) else { return Ok(false) };
        let cols: Vec<&str> = OWNERSHIP_COLUMNS.iter().copied().filter(|c| row.contains_key(*c)).collect();
        let sets: Vec<String> = cols.iter().map(|c| format!("{} = ?", super::schema::q(c))).collect();
        let sql = format!("UPDATE runs SET {} WHERE id = ?", sets.join(", "));
        let mut query = sqlx::query(&sql);
        for c in &cols {
            query = super::codec::bind_value(query, &row[*c]);
        }
        query.bind(id).execute(&mut *conn).await.map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// The op wins by HLC: keep a higher local ownership in the written row.
    pub fn overlay(&self, row: &mut serde_json::Map<String, serde_json::Value>) {
        if let OwnerMerge::KeepLocal(own) = self {
            row.extend(own.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
    }

    /// A pending local change carries the higher ownership: it must still be
    /// pushed even though the rest of the row lost.
    pub fn keeps_local(&self) -> bool {
        matches!(self, OwnerMerge::KeepLocal(_))
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
    /// The owner's lease is live right now (measured on this machine's clock):
    /// the run is live over there.
    pub running: bool,
    /// Milliseconds the lease stays live from the time of this answer unless a
    /// renewal arrives (None when not running). The UI counts down from it.
    pub lease_remaining_ms: Option<u64>,
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
    let now = Instant::now();
    Ok(rows
        .iter()
        .map(|r| {
            let run_id: String = r.get("id");
            let lease_until: Option<String> = r.get("lease_until");
            let remaining = receipts().remaining(&run_id, lease_until.as_deref(), now);
            RemoteRunOwner {
                run_id,
                owner_device: r.get("owner_device"),
                device_name: r.get("device_name"),
                running: remaining.is_some(),
                lease_remaining_ms: remaining.map(|d| d.as_millis() as u64),
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

/// "Continue here": move a session owned by another device to this one. A
/// run live here already is a no-op success. While the owner's final output
/// is still syncing the gap is returned (`skip_sync` continues anyway); with
/// `force = false` a git mismatch is returned for confirmation instead.
#[tauri::command]
pub async fn sync_take_over_run(
    db: tauri::State<'_, Db>,
    registry: tauri::State<'_, RunRegistry>,
    run_id: String,
    force: Option<bool>,
    skip_sync: Option<bool>,
) -> Result<TakeOverResult, String> {
    let live_here = registry.lock().await.contains_key(&run_id);
    take_over(db.inner(), &run_id, live_here, force.unwrap_or(false), skip_sync.unwrap_or(false)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::store::{SqliteSyncStore, SyncStore};
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Arc;

    fn git(head: &str, dirty: bool) -> GitState {
        GitState { head: Some(head.into()), dirty: Some(dirty) }
    }

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    #[test]
    fn lease_expiry_is_measured_from_local_receipt() {
        let r = LeaseReceipts::default();
        let t0 = Instant::now();
        // The wall-clock value itself is never compared: a lease "in the past"
        // by this machine's clock is live once received.
        r.observe("a", Some("2000-01-01T00:00:00.000Z"), t0);
        assert!(r.live("a", Some("2000-01-01T00:00:00.000Z"), t0 + secs(89)));
        assert!(!r.live("a", Some("2000-01-01T00:00:00.000Z"), t0 + secs(91)));
        assert_eq!(r.remaining("a", Some("2000-01-01T00:00:00.000Z"), t0 + secs(30)), Some(secs(60)));

        // A renewal (new value) restarts the local window; the same value doesn't.
        r.observe("b", Some("L1"), t0);
        r.observe("b", Some("L1"), t0 + secs(60));
        assert!(!r.live("b", Some("L1"), t0 + secs(95)));
        r.observe("b", Some("L2"), t0 + secs(60));
        assert!(r.live("b", Some("L2"), t0 + secs(120)));
        assert!(!r.live("b", Some("L2"), t0 + secs(151)));

        // First sight (e.g. after a restart) counts as a fresh receipt.
        assert!(r.live("c", Some("L"), t0));
        // Released / no lease: not live, and forgotten.
        assert!(!r.live("c", None, t0));
        assert!(r.live("c", Some("L"), t0 + secs(200)), "seen anew after the release");
    }

    #[test]
    fn guard_only_blocks_a_live_remote_lease() {
        assert!(held_elsewhere(Some("other"), "me", true));
        assert!(!held_elsewhere(Some("other"), "me", false), "expired / idle");
        assert!(!held_elsewhere(Some("me"), "me", true), "own lease");
        assert!(!held_elsewhere(Some(""), "me", true), "blank owner");
        assert!(!held_elsewhere(None, "me", true), "never owned");
        assert_eq!(running_elsewhere_message(Some("Mac mini")), "This session is running on Mac mini");
        assert_eq!(running_elsewhere_message(Some(" ")), "This session is running on another device");
        assert_eq!(lease_deadline(DateTime::parse_from_rfc3339("2026-10-30T12:00:00Z").unwrap().into()), "2026-10-30T12:01:30.000Z");
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
        let same = git("a", false);
        let other = git("b", false);
        let gap = Some(SyncGap { have: 1, want: 2 });
        let base = TakeOverInput {
            owner: Some("x"),
            local_id: "me",
            lease_live: false,
            live_here: false,
            gap: None,
            recorded: &same,
            local: &same,
            force: false,
            skip_sync: false,
        };
        use TakeOverDecision::*;
        assert_eq!(decide_take_over(&TakeOverInput { lease_live: true, force: true, ..base }), Refused);
        assert_eq!(decide_take_over(&TakeOverInput { ..base }), Proceed);
        assert_eq!(decide_take_over(&TakeOverInput { local: &other, ..base }), Confirm(GitMismatch { recorded: same.clone(), local: other.clone() }));
        assert_eq!(decide_take_over(&TakeOverInput { local: &other, force: true, ..base }), Proceed);
        // The owner's final output first, then the git check.
        assert_eq!(decide_take_over(&TakeOverInput { gap, local: &other, force: true, ..base }), Syncing(gap.unwrap()));
        assert!(matches!(decide_take_over(&TakeOverInput { gap, local: &other, skip_sync: true, ..base }), Confirm(_)));
        // Live here: a no-op, whatever the row says.
        assert_eq!(decide_take_over(&TakeOverInput { live_here: true, lease_live: true, gap, ..base }), AlreadyHere);
        // Own runs never wait or ask.
        assert_eq!(decide_take_over(&TakeOverInput { owner: Some("me"), gap, local: &other, ..base }), Proceed);
        assert_eq!(decide_take_over(&TakeOverInput { owner: None, gap, local: &other, ..base }), Proceed);
    }

    #[test]
    fn sync_gap_sums_both_files() {
        assert_eq!(sync_gap(&[(None, 0), (None, 5)]), None, "nothing recorded");
        assert_eq!(sync_gap(&[(Some(10), 10), (Some(0), 0)]), None, "caught up");
        assert_eq!(sync_gap(&[(Some(10), 12), (None, 0)]), None, "grew past it");
        assert_eq!(sync_gap(&[(Some(10), 4), (Some(20), 20)]), Some(SyncGap { have: 24, want: 30 }));
        assert_eq!(sync_gap(&[(Some(10), 15), (Some(20), 5)]), Some(SyncGap { have: 15, want: 30 }));
    }

    async fn db_for(device: &str, name: &str) -> Db {
        let db = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&db).await.unwrap();
        sqlx::query("INSERT INTO sync_device (id, name, created_at) VALUES (?, ?, '2026-01-01')")
            .bind(device)
            .bind(name)
            .execute(&db)
            .await
            .unwrap();
        crate::sync::install(&db).await.unwrap();
        db
    }

    async fn mem_db() -> Db {
        let db = db_for("me", "MacBook").await;
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/nonexistent/p1', '2026-01-01')")
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

    async fn exec(db: &Db, sql: &str) {
        sqlx::query(sql).execute(db).await.unwrap();
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

    /// Pretend the lease value `lease` of `run_id` arrived here long ago.
    fn received_long_ago(run_id: &str, lease: &str) {
        receipts().forget(run_id);
        receipts().observe(run_id, Some(lease), Instant::now() - secs(LEASE_SECS as u64 + 5));
    }

    fn temp_file(name: &str, len: usize) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-sessions-test-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, vec![b'x'; len]).unwrap();
        p
    }

    #[tokio::test]
    async fn claim_renew_release() {
        let db = mem_db().await;
        add_run(&db, "crr", "running", None, None).await;
        claim(&db, "crr", "/nonexistent/p1").await.unwrap();
        assert_eq!(col(&db, "crr", "owner_device").await.as_deref(), Some("me"));
        assert_eq!(col(&db, "crr", "device_name").await.as_deref(), Some("MacBook"));
        assert!(col(&db, "crr", "lease_until").await.is_some());
        ensure_not_held_elsewhere(&db, "crr").await.unwrap();

        renew(&db, &["crr".into()]).await.unwrap();
        assert!(col(&db, "crr", "lease_until").await.is_some());
        release(&db, "crr").await;
        assert_eq!(col(&db, "crr", "lease_until").await, None);

        // A remote lease is neither renewed nor released from here.
        let lease = future();
        add_run(&db, "crr-x", "running", Some("mini"), Some(&lease)).await;
        renew(&db, &["crr-x".into()]).await.unwrap();
        release(&db, "crr-x").await;
        assert_eq!(col(&db, "crr-x", "lease_until").await, Some(lease.clone()));
        let err = ensure_not_held_elsewhere(&db, "crr-x").await.unwrap_err();
        assert_eq!(err, "This session is running on mini-name");
        // No renewal arrived here for a full lease: the owner is gone, whatever
        // the wall-clock value says.
        received_long_ago("crr-x", &lease);
        ensure_not_held_elsewhere(&db, "crr-x").await.unwrap();
    }

    /// Review-3 M2: a run continued here no longer counts its transcript as
    /// mirrored by sync, so the importer leaves the drain's log alone.
    #[tokio::test]
    async fn claiming_drops_the_mirrored_transcript_size() {
        let db = mem_db().await;
        add_run(&db, "m2", "done", Some("mini"), None).await;
        exec(&db, "UPDATE runs SET transcript_synced_size = 100 WHERE id = 'm2'").await;
        claim(&db, "m2", "/nonexistent/p1").await.unwrap();
        assert_eq!(col(&db, "m2", "transcript_synced_size").await, None);

        add_run(&db, "m2-t", "done", Some("mini"), None).await;
        exec(&db, "UPDATE runs SET transcript_synced_size = 100 WHERE id = 'm2-t'").await;
        assert!(take_over(&db, "m2-t", false, true, true).await.unwrap().taken);
        assert_eq!(col(&db, "m2-t", "transcript_synced_size").await, None);
    }

    /// Review-3 L4: a run left running by a crash is released at startup like
    /// any ended run: final sizes recorded, lease dropped.
    #[tokio::test]
    async fn startup_reconcile_records_final_sizes() {
        let db = mem_db().await;
        let log = temp_file("l4.log", 7);
        add_run(&db, "l4", "running", Some("me"), Some(&future())).await;
        sqlx::query("UPDATE runs SET output_path = ? WHERE id = 'l4'")
            .bind(log.to_string_lossy().as_ref())
            .execute(&db)
            .await
            .unwrap();
        // A remote-owned running run is not touched.
        let lease = future();
        add_run(&db, "l4-x", "running", Some("mini"), Some(&lease)).await;
        reconcile_startup(&db).await.unwrap();
        assert_eq!(col(&db, "l4", "status").await.as_deref(), Some("cancelled"));
        assert_eq!(col(&db, "l4", "lease_until").await, None);
        assert_eq!(col(&db, "l4", "final_log_size").await.as_deref(), Some("7"));
        assert_eq!(col(&db, "l4-x", "status").await.as_deref(), Some("running"));
        assert_eq!(col(&db, "l4-x", "lease_until").await, Some(lease));
    }

    /// Review-3 L6: cancelling a run whose remote owner's lease expired here
    /// moves it to this device in the next epoch, so the old owner's later
    /// renewals (same epoch, newer HLC) cannot bring the run back.
    #[tokio::test]
    async fn cancelling_an_orphaned_remote_run_takes_it_over() {
        let db = mem_db().await;
        let lease = future();
        add_run(&db, "l6", "running", Some("mini"), Some(&lease)).await;
        received_long_ago("l6", &lease);
        let registry: RunRegistry = Default::default();
        crate::commands::runs::cancel_run_inner(&registry, &db, "l6", true).await.unwrap();
        assert_eq!(col(&db, "l6", "status").await.as_deref(), Some("cancelled"));
        assert_eq!(col(&db, "l6", "owner_device").await.as_deref(), Some("me"));
        assert_eq!(col(&db, "l6", "owner_epoch").await.as_deref(), Some("1"));
        assert_eq!(col(&db, "l6", "lease_until").await, None);
        // Not ours to adopt: own and never-owned runs stay as they are.
        add_run(&db, "l6-own", "done", Some("me"), None).await;
        assert!(!adopt_orphan(&db, "l6-own").await.unwrap());
        assert_eq!(col(&db, "l6-own", "owner_epoch").await.as_deref(), Some("0"));
    }

    #[tokio::test]
    async fn release_records_final_sizes_and_kicks_files() {
        let db = mem_db().await;
        let log = temp_file("rel.log", 10);
        add_run(&db, "rel", "running", Some("me"), Some(&future())).await;
        sqlx::query("UPDATE runs SET output_path = ? WHERE id = 'rel'")
            .bind(log.to_string_lossy().as_ref())
            .execute(&db)
            .await
            .unwrap();
        assert!(!crate::sync::files::has_kicks());
        release(&db, "rel").await;
        assert_eq!(col(&db, "rel", "lease_until").await, None);
        assert_eq!(col(&db, "rel", "final_log_size").await.as_deref(), Some("10"));
        assert_eq!(col(&db, "rel", "final_transcript_size").await, None, "no session yet");
        assert!(crate::sync::files::has_kicks());

        // The drain's later flush grew the log after a cancel released it.
        std::fs::write(&log, vec![b'x'; 25]).unwrap();
        release(&db, "rel").await;
        assert_eq!(col(&db, "rel", "final_log_size").await.as_deref(), Some("25"));

        // A new turn starts: the sizes are stale until the next release.
        claim(&db, "rel", "/nonexistent/p1").await.unwrap();
        assert_eq!(col(&db, "rel", "final_log_size").await, None);
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
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
    async fn remote_running_runs_do_not_make_this_device_busy() {
        let db = mem_db().await;
        let lease = future();
        let count = |db: Db| async move {
            sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM runs WHERE status = 'running' AND {OWNED_HERE}"))
                .fetch_one(&db)
                .await
                .unwrap()
        };
        add_run(&db, "theirs", "running", Some("mini"), Some(&lease)).await;
        assert_eq!(count(db.clone()).await, 0);
        add_run(&db, "legacy", "running", None, None).await;
        assert_eq!(count(db.clone()).await, 1);
        add_run(&db, "mine", "running", Some("me"), Some(&lease)).await;
        add_run(&db, "blank", "running", Some(""), None).await;
        assert_eq!(count(db.clone()).await, 3);
    }

    #[tokio::test]
    async fn take_over_moves_ownership() {
        let db = mem_db().await;
        let lease = future();
        add_run(&db, "to-live", "running", Some("mini"), Some(&lease)).await;
        assert!(take_over(&db, "to-live", false, true, true).await.unwrap_err().contains("mini-name"));
        let owners = remote_owners(&db, "p1").await.unwrap();
        let live = owners.iter().find(|o| o.run_id == "to-live").unwrap();
        assert!(live.running);
        assert!(live.lease_remaining_ms.is_some_and(|ms| ms > 80_000 && ms <= 90_000));

        // Owner crashed: no renewal for a full lease while the row says running.
        let stale = "2020-01-01T00:00:00.000Z";
        add_run(&db, "to-dead", "running", Some("mini"), Some(stale)).await;
        received_long_ago("to-dead", stale);
        let r = take_over(&db, "to-dead", false, false, false).await.unwrap();
        assert!(r.taken && r.mismatch.is_none() && r.syncing.is_none() && !r.transcript_available);
        assert_eq!(col(&db, "to-dead", "owner_device").await.as_deref(), Some("me"));
        assert_eq!(col(&db, "to-dead", "device_name").await.as_deref(), Some("MacBook"));
        assert_eq!(col(&db, "to-dead", "status").await.as_deref(), Some("cancelled"));

        // A recorded HEAD that this (non-git) checkout can't match asks first.
        add_run(&db, "to-idle", "completed", Some("mini"), None).await;
        exec(&db, "UPDATE runs SET git_head = 'abc', git_dirty = 0 WHERE id = 'to-idle'").await;
        let r = take_over(&db, "to-idle", false, false, false).await.unwrap();
        assert!(!r.taken);
        assert_eq!(r.mismatch.unwrap().recorded.head.as_deref(), Some("abc"));
        assert_eq!(col(&db, "to-idle", "owner_device").await.as_deref(), Some("mini"));
        assert!(take_over(&db, "to-idle", false, true, false).await.unwrap().taken);
        assert_eq!(col(&db, "to-idle", "status").await.as_deref(), Some("completed"));

        let owners = remote_owners(&db, "p1").await.unwrap();
        assert_eq!(owners.iter().map(|o| o.run_id.as_str()).collect::<Vec<_>>(), vec!["to-live"]);
    }

    #[tokio::test]
    async fn take_over_of_a_run_live_here_is_a_no_op() {
        let db = mem_db().await;
        add_run(&db, "noop", "running", Some("me"), Some(&future())).await;
        let r = take_over(&db, "noop", true, false, false).await.unwrap();
        assert!(r.taken && r.mismatch.is_none() && r.syncing.is_none());
        assert_eq!(col(&db, "noop", "status").await.as_deref(), Some("running"), "not cancelled");
        assert!(col(&db, "noop", "lease_until").await.is_some(), "lease kept");
    }

    /// Finding N13: B continued a session whose transcript sync wrote here
    /// with the cwd rewritten (longer on disk than the synced content), and
    /// the resume wrote nothing. B records the synced size, so A — whose copy
    /// is that synced content — can take the run back; once B's own bytes
    /// are on disk, their size is what counts.
    #[tokio::test]
    async fn final_sizes_are_the_synced_sizes_after_a_cwd_rewrite() {
        use sha2::{Digest, Sha256};
        let b = mem_db().await;
        let transcript = temp_file("s-n13.jsonl", 120);
        add_run(&b, "n13", "running", Some("me"), Some(&future())).await;
        sqlx::query("UPDATE runs SET session_id = 's-n13', transcript_path = ? WHERE id = 'n13'")
            .bind(transcript.to_string_lossy().as_ref())
            .execute(&b)
            .await
            .unwrap();
        let disk_sha = hex::encode(Sha256::digest(vec![b'x'; 120]));
        sqlx::query(
            "INSERT INTO sync_files (kind, path, local_path, size, sha256, local_size, local_sha256, origin) \
             VALUES ('claude_transcript', 't', ?, 100, 'synced', 120, ?, 'remote')",
        )
        .bind(transcript.to_string_lossy().as_ref())
        .bind(&disk_sha)
        .execute(&b)
        .await
        .unwrap();
        release(&b, "n13").await;
        assert_eq!(col(&b, "n13", "final_transcript_size").await.as_deref(), Some("100"));

        // A has the synced content (100 bytes): the gap is closed.
        let a = db_for("mini", "Mac mini").await;
        exec(&a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'P', '/nonexistent/p1', '2026-01-01')").await;
        add_run(&a, "n13", "completed", Some("me"), None).await;
        let a_copy = temp_file("s-n13.jsonl", 100);
        sqlx::query("UPDATE runs SET session_id = 's-n13', transcript_path = ?, final_transcript_size = 100 WHERE id = 'n13'")
            .bind(a_copy.to_string_lossy().as_ref())
            .execute(&a)
            .await
            .unwrap();
        let r = take_over(&a, "n13", false, true, false).await.unwrap();
        assert_eq!(r.syncing, None);
        assert!(r.taken);

        // Same size but other bytes than sync wrote (the resume rewrote a
        // line): the file on disk is what travels next.
        std::fs::write(&transcript, vec![b'y'; 120]).unwrap();
        exec(&b, "UPDATE runs SET lease_until = 'x' WHERE id = 'n13'").await;
        release(&b, "n13").await;
        assert_eq!(col(&b, "n13", "final_transcript_size").await.as_deref(), Some("120"));
        for f in [transcript, a_copy] {
            let _ = std::fs::remove_dir_all(f.parent().unwrap());
        }
    }

    #[tokio::test]
    async fn take_over_waits_for_the_final_output() {
        let db = mem_db().await;
        let log = temp_file("fin.log", 40);
        add_run(&db, "fin", "completed", Some("mini"), None).await;
        sqlx::query("UPDATE runs SET output_path = ?, final_log_size = 100 WHERE id = 'fin'")
            .bind(log.to_string_lossy().as_ref())
            .execute(&db)
            .await
            .unwrap();
        let r = take_over(&db, "fin", false, false, false).await.unwrap();
        assert!(!r.taken);
        assert_eq!(r.syncing, Some(SyncGap { have: 40, want: 100 }));
        assert_eq!(col(&db, "fin", "owner_device").await.as_deref(), Some("mini"));

        // Sync recorded the full synced content (the file on disk may differ
        // in size after the cwd rewrite).
        sqlx::query("INSERT INTO sync_files (kind, path, local_path, size) VALUES ('run_log', 'x', ?, 100)")
            .bind(log.to_string_lossy().as_ref())
            .execute(&db)
            .await
            .unwrap();
        assert!(take_over(&db, "fin", false, false, false).await.unwrap().taken);

        // Or the user continues anyway.
        add_run(&db, "fin2", "completed", Some("mini"), None).await;
        exec(&db, "UPDATE runs SET final_transcript_size = 5 WHERE id = 'fin2'").await;
        assert_eq!(
            take_over(&db, "fin2", false, false, false).await.unwrap().syncing,
            Some(SyncGap { have: 0, want: 5 }),
            "transcript not here at all"
        );
        assert!(take_over(&db, "fin2", false, false, true).await.unwrap().taken);
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }

    /// Two devices syncing the run row: B takes over a run A still runs; A
    /// sees the row, stops its sidecar and no longer writes the row.
    #[tokio::test]
    async fn take_over_hands_a_live_run_off() {
        let db_a = db_for("dev-a", "Alpha").await;
        let db_b = db_for("dev-b", "Beta").await;
        let notices = Arc::new(StdMutex::new(Vec::new()));
        let seen = notices.clone();
        let store_a = SqliteSyncStore::new(db_a.clone(), "dev-a".into())
            .with_paths(crate::sync::codec::PathCtx::with_home("/Users/u"))
            .with_notifier(Arc::new(move |n| seen.lock().unwrap().push(n)));
        let store_b = SqliteSyncStore::new(db_b.clone(), "dev-b".into())
            .with_paths(crate::sync::codec::PathCtx::with_home("/Users/u"));
        async fn sync(from: &SqliteSyncStore, to: &SqliteSyncStore) {
            let ops = from.pending(1000).await.unwrap();
            if let Some((seq, _)) = ops.last() {
                from.mark_pushed(*seq).await.unwrap();
            }
            to.apply_remote(ops.into_iter().map(|(_, op)| op).collect()).await.unwrap();
        }

        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('hp', 'P', '/Users/u/p', '2026-01-01')").await;
        exec(
            &db_a,
            "INSERT INTO runs (id, project_id, type, status, engine, created_at) \
             VALUES ('ho', 'hp', 'session', 'running', 'claude', '2026-01-01')",
        )
        .await;
        claim(&db_a, "ho", "/nonexistent").await.unwrap();
        sync(&store_a, &store_b).await;

        // B sees A's live lease.
        assert!(check_applied(&db_b, &["ho".into()], |_| false).await.unwrap().is_empty());
        assert!(take_over(&db_b, "ho", false, false, false).await.is_err());
        // A went quiet for a full lease (measured on B's clock): B continues.
        received_long_ago("ho", &col(&db_b, "ho", "lease_until").await.unwrap());
        assert!(take_over(&db_b, "ho", false, false, false).await.unwrap().taken);
        sync(&store_b, &store_a).await;

        assert!(notices.lock().unwrap().contains(&crate::sync::events::Notice::RunsApplied(vec!["ho".into()])));
        assert_eq!(col(&db_a, "ho", "owner_device").await.as_deref(), Some("dev-b"));
        let handoffs = check_applied(&db_a, &["ho".into()], |id| id == "ho").await.unwrap();
        assert_eq!(handoffs, vec![HandOff { run_id: "ho".into(), device_name: Some("Beta".into()) }]);
        assert!(check_applied(&db_a, &["ho".into()], |_| false).await.unwrap().is_empty(), "not live here");

        // A's heartbeat and release no longer touch the row: nothing to push.
        renew(&db_a, &["ho".into()]).await.unwrap();
        release(&db_a, "ho").await;
        assert!(store_a.pending(10).await.unwrap().is_empty());
        assert_eq!(col(&db_a, "ho", "lease_until").await, None);
        assert_eq!(col(&db_a, "ho", "status").await.as_deref(), Some("cancelled"));
    }
}
