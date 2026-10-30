//! Core <-> client boundary (design §9.3, on-demand-sync.md §P2). The sync
//! engine and the change feed only talk to `SyncStore`; `SqliteSyncStore` is
//! the real implementation over Devdy's DB.
//!
//! Local changes land in `sync_outbox` (capture triggers, file and Keychain
//! scans) and are *stamped* before the feed is read: each changed row gets
//! a fresh HLC no older than the change in `sync_rows`, which moves it to the
//! end of the change feed (`sync_feed`, kept by triggers on `sync_rows`, so
//! rows applied from the other device move there too). The outbox is just
//! "changes not stamped yet" now; nothing is pushed anywhere.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, MutexGuard};

use super::clock::{wall_ms, Hlc, HlcClock};
use super::codec::{encode_row, read_row, PathCtx};
use super::events::{Notice, Notifier};
use super::feed::{ChangesPage, ChangesReq, FileReadReq, FileScope};
use super::files::FileSource;
use super::op::{Op, OpKind, OP_VERSION};
use super::{apply, files, prefs, schema, secrets, SCHEMA_VERSION};
use crate::db::Db;

/// `sync_state` key prefix of the cursor in another device's change feed
/// (`peer_cursor:<device id>`).
pub const CURSOR_PREFIX: &str = "peer_cursor:";
/// A pulled file/secret op that keeps failing is dropped after this many tries.
const MAX_EXTRA_ATTEMPTS: i64 = 50;
/// Outbox rows stamped per transaction.
const STAMP_BATCH: usize = 500;
/// Largest op (JSON) a page carries: a sealed reply must stay below the
/// relay's frame cap (4 MiB + 64 as base64).
pub const MAX_OP_BYTES: usize = 2_560 * 1024;
/// A `changes` request continuing the previous page of the same asker within
/// this long does not rescan the files.
const CONTINUE_WITHIN: Duration = Duration::from_secs(600);

fn cursor_key(source: &str) -> String {
    format!("{CURSOR_PREFIX}{source}")
}

/// Ops whose `tbl` is not a DB table (§9b): routed to `files` / `secrets`,
/// never to `apply`.
pub fn is_extra_tbl(tbl: &str) -> bool {
    tbl == files::TBL || tbl == secrets::TBL
}

// `#[async_trait]` marks the boxed futures `#[must_use]` a second time.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SyncStore: Send + Sync {
    /// Serve `changes` to device `from`: one page of this device's change
    /// feed (non-DB sources rescanned first, unless the request continues the
    /// previous page).
    async fn changes(&self, from: &str, req: &ChangesReq) -> Result<ChangesPage, String>;
    /// Serve `file_read`: a range of a tracked file (`feed::FILE_CHANGED` /
    /// `feed::FILE_GONE` when that version is not here).
    async fn read_file(&self, req: &FileReadReq) -> Result<Vec<u8>, String>;
    /// Apply DB row ops from the other device (LWW, selective filters).
    async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String>;
    /// File `file` / `secret` ops to fetch / apply later (the newest per
    /// item wins). Returns how many were filed.
    async fn park_extra(&self, ops: Vec<Op>) -> Result<usize, String>;
    /// `(content bytes, items)` of the filed ops `scope` would fetch.
    async fn extra_to_fetch(&self, scope: FileScope) -> Result<(u64, usize), String>;
    /// Fetch / apply the filed ops in `scope` (contents read from `src`);
    /// `on_item` runs after each one. Failures stay filed for the next try.
    async fn fetch_extra(
        &self,
        src: &dyn FileSource,
        scope: FileScope,
        on_item: &(dyn Fn() + Send + Sync),
    ) -> Result<ApplyReport, String>;
    /// This device's position in `source`'s change feed.
    async fn cursor(&self, source: &str) -> Result<u64, String>;
    /// A no-op while a cursor reset is pending (see [`SyncStore::cursor_start`]).
    async fn set_cursor(&self, source: &str, seq: u64) -> Result<(), String>;
    /// The cursor a pull from `source` starts from; ends a pending reset
    /// (selective sync turned something on), which kept pulls already
    /// running from writing over it.
    async fn cursor_start(&self, source: &str) -> Result<u64, String>;
    /// `(skip_categories, skip_projects)` this device asks with.
    async fn ask_filter(&self) -> Result<(Vec<String>, Vec<String>), String>;
    /// The relay named the group's creator (welcome, or the role moved).
    async fn set_group_creator(&self, _device_id: &str) -> Result<(), String> {
        Ok(())
    }
    /// Rows too large to ever be served.
    async fn parked_count(&self) -> Result<u64, String> {
        Ok(0)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub applied: usize,
    /// Older than what we have, beaten by a newer unstamped local change, or
    /// a machine-local setting.
    pub skipped_older: usize,
    pub conflicts: usize,
    /// Ops parked in `sync_deferred` after this call (retried on the next one).
    pub deferred: usize,
}

pub struct SqliteSyncStore {
    db: Db,
    device_id: String,
    paths: PathCtx,
    /// Loaded lazily from `sync_state`; the lock also serialises stamping and
    /// applying.
    clock: Mutex<Option<HlcClock>>,
    /// Keychain access for `secret` ops; `None` = secrets are not synced.
    secrets: Option<secrets::Backend>,
    scan: Mutex<files::ScanState>,
    /// Tells the UI what was applied (`None` in tests / bootstrap).
    notify: Option<Notifier>,
    /// Per asking device: the `next` of the last page served with `more`.
    serve_marks: std::sync::Mutex<HashMap<String, (u64, Instant)>>,
    /// Tests: bytes served by `read_file`; reads from this offset on fail
    /// (a transfer cut midway).
    #[cfg(test)]
    pub served_bytes: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub fail_reads_from: std::sync::Mutex<Option<u64>>,
}

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

impl SqliteSyncStore {
    pub fn new(db: Db, device_id: String) -> Self {
        SqliteSyncStore {
            db,
            device_id,
            paths: PathCtx::from_env(),
            clock: Mutex::new(None),
            secrets: None,
            scan: Mutex::new(files::ScanState::default()),
            notify: None,
            serve_marks: Default::default(),
            #[cfg(test)]
            served_bytes: Default::default(),
            #[cfg(test)]
            fail_reads_from: Default::default(),
        }
    }

    /// Unstamped changes coalesced per row, oldest first (of every row, or
    /// only of rows in `only`), each with a fresh HLC no older than the
    /// change, recorded in `sync_rows` (which moves the row to the end of the
    /// change feed). The op carries the CURRENT row, or a Delete when it is
    /// gone. [`SqliteSyncStore::mark_pushed`] drops the entries.
    async fn pending_filtered(&self, limit: usize, only: Option<&[&str]>) -> Result<Vec<(i64, Op)>, String> {
        if let Some(t) = only {
            // Polled while pushes back off: stay cheap when nothing waits.
            let sql = format!("SELECT EXISTS (SELECT 1 FROM sync_outbox WHERE tbl IN ({}))", vec!["?"; t.len()].join(", "));
            let mut q = sqlx::query_scalar::<_, bool>(&sql);
            for tbl in t {
                q = q.bind(*tbl);
            }
            if t.is_empty() || !q.fetch_one(&self.db).await.map_err(err)? {
                return Ok(Vec::new());
            }
        }
        let paths = self.paths_now().await?;
        let with_secrets = self.secrets_on().await?;
        // The Keychain is read before the transaction (it may prompt), and
        // only when a secret op is waiting.
        let secret_waiting: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_outbox WHERE tbl = ? LIMIT 1")
            .bind(secrets::TBL)
            .fetch_optional(&self.db)
            .await
            .map_err(err)?;
        let keychain = match (&self.secrets, with_secrets && secret_waiting.is_some()) {
            (Some(backend), true) => {
                let b = backend.clone();
                Some(tokio::task::spawn_blocking(move || b.list()).await.map_err(|e| e.to_string())??)
            }
            _ => None,
        };
        let mut guard = self.clock().await?;
        let mut clock = guard.clone().ok_or("clock not loaded")?;
        let mut tx = self.db.begin().await.map_err(err)?;
        let only_sql = only.map_or(String::new(), |t| format!("WHERE tbl IN ({})", vec!["?"; t.len()].join(", ")));
        let sql = format!(
            "SELECT tbl, pk, MAX(seq) AS s, MAX(changed_at) FROM sync_outbox {only_sql} \
             GROUP BY tbl, pk ORDER BY s LIMIT ?"
        );
        let mut q = sqlx::query_as::<_, (String, String, i64, i64)>(&sql);
        for t in only.unwrap_or_default() {
            q = q.bind(*t);
        }
        let groups = q.bind(limit as i64).fetch_all(&mut *tx).await.map_err(err)?;

        let mut out = Vec::with_capacity(groups.len());
        for (tbl, pk_text, seq, changed_at) in groups {
            let pk: Option<Value> = serde_json::from_str(&pk_text).ok();
            let entry = match (tbl.as_str(), pk) {
                (files::TBL, Some(pk)) => files::pending_row(&mut tx, &pk, with_secrets).await?.map(|e| (pk, e)),
                (secrets::TBL, Some(pk)) if with_secrets => match &keychain {
                    Some(items) => secrets::pending_row(items, &pk).map(|e| (pk, e)),
                    // Queued after the Keychain was read: next time.
                    None => continue,
                },
                (_, Some(pk)) => match schema::spec(&tbl) {
                    Some(spec) => self.db_entry(&mut tx, &paths, spec, &pk_text, pk).await?,
                    None => None,
                },
                (_, None) => None,
            };
            let Some((pk, (kind, row))) = entry else {
                drop_entries(&mut tx, &tbl, &pk_text, seq).await?;
                continue;
            };
            let hlc = clock.tick_at(changed_at.max(0) as u64);
            sqlx::query(
                "INSERT INTO sync_rows (tbl, pk, hlc, deleted) VALUES (?, ?, ?, ?) \
                 ON CONFLICT(tbl, pk) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted",
            )
            .bind(&tbl)
            .bind(&pk_text)
            .bind(hlc.to_string())
            .bind((kind == OpKind::Delete) as i64)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
            out.push((seq, Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc, tbl, pk, kind, row }));
        }
        clock.save(&mut tx).await?;
        tx.commit().await.map_err(err)?;
        *guard = Some(clock);
        Ok(out)
    }

    /// [`SqliteSyncStore::pending_filtered`] of every row.
    pub async fn pending(&self, limit: usize) -> Result<Vec<(i64, Op)>, String> {
        self.pending_filtered(limit, None).await
    }

    /// Whether anything waits to be stamped.
    pub async fn has_pending(&self) -> Result<bool, String> {
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sync_outbox)").fetch_one(&self.db).await.map_err(err)
    }

    /// Those ops (from [`SqliteSyncStore::pending`]) are stamped: drop the
    /// outbox entries up to `up_to_outbox_seq`.
    pub async fn mark_pushed(&self, up_to_outbox_seq: i64) -> Result<(), String> {
        sqlx::query("DELETE FROM sync_outbox WHERE seq <= ?")
            .bind(up_to_outbox_seq)
            .execute(&self.db)
            .await
            .map_err(err)?;
        Ok(())
    }

    /// Stamp every local change waiting in the outbox. Returns how many rows.
    pub async fn stamp(&self) -> Result<usize, String> {
        let mut n = 0;
        let mut idle = 0;
        loop {
            let ops = self.pending(STAMP_BATCH).await?;
            if let Some((last, _)) = ops.last() {
                self.mark_pushed(*last).await?;
                n += ops.len();
                continue;
            }
            // Whole batches of filtered rows come back empty; a secret
            // queued while the Keychain was read waits for the next round.
            idle += 1;
            if idle > 3 || !self.has_pending().await? {
                return Ok(n);
            }
        }
    }

    /// Rescan the non-DB sources in `scope` (files, Keychain).
    async fn scan(&self, scope: FileScope) -> Result<(), String> {
        if scope == FileScope::Nothing {
            return Ok(());
        }
        let with_secrets = self.secrets_on().await?;
        let paths = self.paths_now().await?;
        let mut state = self.scan.lock().await;
        state.skip_sessions = scope == FileScope::NoSessions;
        files::scan(&self.db, &paths, &mut state, with_secrets).await?;
        drop(state);
        // Keychain moves left over by an earlier re-key (Keychain locked...).
        self.move_secrets().await;
        if let (Some(backend), true) = (&self.secrets, with_secrets) {
            if let Err(e) = secrets::scan(&self.db, backend.clone()).await {
                tracing::warn!(event = "sync_secret_scan_failed", error = %e);
            }
        }
        Ok(())
    }

    /// The op of one feed entry as it is now; `None` = not served (filtered
    /// row, secrets off, unknown table).
    #[allow(clippy::too_many_arguments)]
    async fn feed_op(
        &self,
        conn: &mut sqlx::SqliteConnection,
        paths: &PathCtx,
        keychain: Option<&std::collections::BTreeMap<secrets::SecretId, String>>,
        with_secrets: bool,
        tbl: &str,
        pk_text: &str,
        hlc: Hlc,
        deleted: bool,
    ) -> Result<Option<Op>, String> {
        let Ok(pk) = serde_json::from_str::<Value>(pk_text) else { return Ok(None) };
        let entry = match tbl {
            files::TBL => files::pending_row(conn, &pk, with_secrets).await?,
            secrets::TBL if !with_secrets => None,
            secrets::TBL if deleted => Some((OpKind::Delete, None)),
            secrets::TBL => keychain.and_then(|items| secrets::pending_row(items, &pk)),
            _ => match schema::spec(tbl) {
                Some(spec) => self.db_entry(conn, paths, spec, pk_text, pk.clone()).await?.map(|(_, e)| e),
                None => None,
            },
        };
        Ok(entry.map(|(kind, row)| Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc, tbl: tbl.to_string(), pk, kind, row }))
    }

    /// One page of the change feed after `req.since` for `from`.
    async fn page(&self, from: &str, req: &ChangesReq) -> Result<ChangesPage, String> {
        let paths = self.paths_now().await?;
        let skip = prefs::ServeFilter::new(&req.skip_categories, &req.skip_projects);
        let with_secrets = self.secrets_on().await? && !skip.category_off(prefs::SECRETS);
        let since = req.since.min(i64::MAX as u64) as i64;
        let secret_rows: bool = with_secrets
            && sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sync_feed WHERE tbl = ? AND seq > ?)")
                .bind(secrets::TBL)
                .bind(since)
                .fetch_one(&self.db)
                .await
                .map_err(err)?;
        // The Keychain is read outside any transaction (it may prompt).
        let keychain = match (&self.secrets, secret_rows) {
            (Some(backend), true) => {
                let b = backend.clone();
                Some(tokio::task::spawn_blocking(move || b.list()).await.map_err(|e| e.to_string())??)
            }
            _ => None,
        };
        let mut conn = self.db.acquire().await.map_err(err)?;
        let mut page = ChangesPage { ops: Vec::new(), next: req.since, more: false };
        let mut bytes = 0usize;
        let mut served: Vec<(String, String)> = Vec::new();
        let mut too_large: Vec<(String, String)> = Vec::new();
        'feed: loop {
            let rows: Vec<(i64, String, String, String, i64)> = sqlx::query_as(
                "SELECT f.seq, f.tbl, f.pk, r.hlc, r.deleted FROM sync_feed f \
                 JOIN sync_rows r ON r.tbl = f.tbl AND r.pk = f.pk WHERE f.seq > ? ORDER BY f.seq LIMIT 200",
            )
            .bind(page.next.min(i64::MAX as u64) as i64)
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?;
            if rows.is_empty() {
                break;
            }
            for (seq, tbl, pk_text, hlc, deleted) in rows {
                let seq = seq.max(0) as u64;
                let Ok(hlc) = hlc.parse::<Hlc>() else {
                    page.next = seq;
                    continue;
                };
                // The asker's own version: it has it (or something newer).
                if hlc.device == from {
                    page.next = seq;
                    continue;
                }
                let op = self.feed_op(&mut conn, &paths, keychain.as_ref(), with_secrets, &tbl, &pk_text, hlc, deleted != 0).await?;
                let Some(op) = op else {
                    page.next = seq;
                    continue;
                };
                if skip.skips(&mut conn, &self.db, &paths, &op).await? {
                    page.next = seq;
                    continue;
                }
                let size = serde_json::to_vec(&op).map(|v| v.len()).unwrap_or(usize::MAX);
                if size > MAX_OP_BYTES {
                    tracing::warn!(event = "sync_op_too_large", tbl = %tbl);
                    too_large.push((tbl, pk_text));
                    page.next = seq;
                    continue;
                }
                if !page.ops.is_empty() && bytes + size > req.max_bytes {
                    break 'feed;
                }
                bytes += size;
                page.ops.push(op);
                served.push((tbl, pk_text));
                page.next = seq;
                if page.ops.len() >= req.max_ops {
                    break 'feed;
                }
            }
        }
        page.more = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sync_feed WHERE seq > ?)")
            .bind(page.next.min(i64::MAX as u64) as i64)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
        if !too_large.is_empty() || !served.is_empty() {
            let mut tx = sqlx::Connection::begin(&mut *conn).await.map_err(err)?;
            for (tbl, pk) in &too_large {
                sqlx::query("INSERT OR REPLACE INTO sync_parked (tbl, pk, reason, parked_at) VALUES (?, ?, 'too_large', ?)")
                    .bind(tbl)
                    .bind(pk)
                    .bind(wall_ms() as i64)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
            }
            for (tbl, pk) in &served {
                sqlx::query("DELETE FROM sync_parked WHERE tbl = ? AND pk = ?")
                    .bind(tbl)
                    .bind(pk)
                    .execute(&mut *tx)
                    .await
                    .map_err(err)?;
            }
            tx.commit().await.map_err(err)?;
        }
        Ok(page)
    }

    /// Filed extra ops (newest first per item is already guaranteed) in
    /// `scope`, with their attempt counts.
    async fn filed_extra(&self, scope: FileScope) -> Result<Vec<(Op, i64)>, String> {
        let rows: Vec<(String, i64)> = sqlx::query_as("SELECT op, attempts FROM sync_extra_deferred ORDER BY deferred_at")
            .fetch_all(&self.db)
            .await
            .map_err(err)?;
        Ok(rows
            .into_iter()
            .filter_map(|(s, n)| Some((serde_json::from_str::<Op>(&s).ok()?, n)))
            .filter(|(op, _)| in_scope(op, scope))
            .collect())
    }

    pub fn with_notifier(mut self, notify: Notifier) -> Self {
        self.notify = Some(notify);
        self
    }

    fn notice(&self, n: Notice) {
        if let Some(f) = &self.notify {
            f(n);
        }
    }

    pub fn with_secrets(mut self, backend: secrets::Backend) -> Self {
        self.secrets = Some(backend);
        self
    }

    pub fn with_paths(mut self, paths: PathCtx) -> Self {
        self.paths = paths;
        self
    }

    /// `paths` plus the current project path map (it changes at bootstrap and
    /// when the user maps a project by hand) and Claude config dirs.
    async fn paths_now(&self) -> Result<PathCtx, String> {
        let map = super::bootstrap::load_project_map(&self.db).await?;
        let dirs: Vec<String> = sqlx::query_scalar("SELECT config_dir FROM claude_accounts WHERE config_dir != ''")
            .fetch_all(&self.db)
            .await
            .map_err(err)?;
        Ok(self.paths.clone().with_project_map(map).with_claude_dirs(dirs.into_iter().map(std::path::PathBuf::from)))
    }

    /// Whether `secret` ops (and SSH key files) travel: a Keychain backend is
    /// wired and the machine-local setting is on.
    async fn secrets_on(&self) -> Result<bool, String> {
        if self.secrets.is_none() {
            return Ok(false);
        }
        let mut conn = self.db.acquire().await.map_err(err)?;
        secrets::enabled(&mut conn).await
    }

    /// Move the Keychain items of rows re-keyed by apply or by the bootstrap
    /// merge (which applies through `apply_remote` too).
    async fn move_secrets(&self) {
        if let Err(e) = super::bootstrap::run_dir_moves(&self.db, &self.paths).await {
            tracing::warn!(event = "sync_dir_move_failed", error = %e);
        }
        let Some(backend) = &self.secrets else { return };
        if let Err(e) = secrets::run_key_moves(&self.db, backend.clone()).await {
            tracing::warn!(event = "sync_secret_move_failed", error = %e);
        }
    }

    async fn clock(&self) -> Result<MutexGuard<'_, Option<HlcClock>>, String> {
        let mut guard = self.clock.lock().await;
        if guard.is_none() {
            let mut conn = self.db.acquire().await.map_err(err)?;
            *guard = Some(HlcClock::load(&mut conn, &self.device_id).await?);
        }
        Ok(guard)
    }
}

#[async_trait]
impl SyncStore for SqliteSyncStore {
    async fn changes(&self, from: &str, req: &ChangesReq) -> Result<ChangesPage, String> {
        let continuing = self
            .serve_marks
            .lock()
            .ok()
            .and_then(|m| m.get(from).copied())
            .is_some_and(|(next, at)| next == req.since && at.elapsed() < CONTINUE_WITHIN);
        if !continuing {
            self.scan(req.files).await?;
        }
        self.stamp().await?;
        let page = self.page(from, req).await?;
        if let Ok(mut m) = self.serve_marks.lock() {
            if page.more {
                m.insert(from.to_string(), (page.next, Instant::now()));
            } else {
                m.remove(from);
            }
        }
        Ok(page)
    }

    async fn read_file(&self, req: &FileReadReq) -> Result<Vec<u8>, String> {
        #[cfg(test)]
        if self.fail_reads_from.lock().unwrap().is_some_and(|from| req.offset >= from) {
            return Err("connection lost".into());
        }
        let paths = self.paths_now().await?;
        let with_keys = self.secrets_on().await?;
        let bytes = files::read_range(&self.db, &paths, req, with_keys).await?;
        #[cfg(test)]
        self.served_bytes.fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
        Ok(bytes)
    }

    async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String> {
        let mut guard = self.clock().await?;
        let mut clock = guard.clone().ok_or("clock not loaded")?;
        let paths = self.paths_now().await?;
        let tables: std::collections::BTreeSet<String> = ops.iter().map(|o| o.tbl.clone()).collect();
        let runs: Vec<String> =
            ops.iter().filter(|o| o.tbl == "runs").filter_map(|o| o.pk.as_str().map(str::to_string)).collect();
        let report = apply::apply_ops(&self.db, &paths, &mut clock, ops).await?;
        // Cut references whose parent just arrived.
        prefs::settle_stripped(&self.db).await?;
        *guard = Some(clock);
        drop(guard);
        self.move_secrets().await;
        if report.applied > 0 {
            self.notice(Notice::RowsApplied(tables.into_iter().collect()));
            if !runs.is_empty() {
                self.notice(Notice::RunsApplied(runs));
            }
        }
        Ok(report)
    }

    async fn park_extra(&self, ops: Vec<Op>) -> Result<usize, String> {
        self.file_extra(ops).await
    }

    async fn extra_to_fetch(&self, scope: FileScope) -> Result<(u64, usize), String> {
        let ops = self.filed_extra(scope).await?;
        let bytes = ops.iter().map(|(op, _)| super::feed::file_op_size(op)).sum();
        Ok((bytes, ops.len()))
    }

    async fn fetch_extra(
        &self,
        src: &dyn FileSource,
        scope: FileScope,
        on_item: &(dyn Fn() + Send + Sync),
    ) -> Result<ApplyReport, String> {
        let with_keys = self.secrets_on().await?;
        let paths = self.paths_now().await?;
        let filter = {
            let mut conn = self.db.acquire().await.map_err(err)?;
            prefs::Filter::load(&mut conn).await?
        };
        let mut report = ApplyReport::default();
        let mut grown_runs = std::collections::BTreeSet::new();
        for (op, attempts) in self.filed_extra(scope).await? {
            {
                let mut guard = self.clock().await?;
                if let Some(c) = guard.as_mut() {
                    c.observe(&op.hlc);
                }
            }
            let result = if op.schema > SCHEMA_VERSION {
                Err(format!("schema newer than local ({SCHEMA_VERSION})"))
            } else if op.tbl == files::TBL && prefs::skip_file(&self.db, &filter, &paths, &op).await? {
                // Selective sync: dropped, not deferred (re-pulled once enabled).
                Ok((0, 1, 0))
            } else {
                match op.tbl.as_str() {
                    files::TBL => files::apply_op(&self.db, &paths, src, &op, with_keys).await.map(|o| match o {
                        files::Outcome::Applied => (1, 0, 0),
                        files::Outcome::Skipped => (0, 1, 0),
                        files::Outcome::Conflict => (1, 0, 1),
                    }),
                    secrets::TBL => match &self.secrets {
                        Some(b) => secrets::apply_op(&self.db, b.clone(), &op).await.map(|o| match o {
                            secrets::Outcome::Applied => (1, 0, 0),
                            secrets::Outcome::Skipped => (0, 1, 0),
                        }),
                        None => Ok((0, 1, 0)),
                    },
                    _ => Ok((0, 1, 0)),
                }
            };
            if matches!(result, Ok((1, _, 0))) && self.notify.is_some() && op.tbl == files::TBL {
                if let Some(run) = files::run_of_op(&self.db, &paths, &op).await {
                    grown_runs.insert(run);
                }
            }
            let key = op.pk_key();
            match result {
                Ok((a, s, c)) => {
                    report.applied += a;
                    report.skipped_older += s;
                    report.conflicts += c;
                    self.unfile(&op.tbl, &key, &op.hlc).await?;
                }
                // That version is gone on the source: the newer op comes
                // with the next Sync.
                Err(e) if files::is_superseded(&e) => {
                    report.skipped_older += 1;
                    self.unfile(&op.tbl, &key, &op.hlc).await?;
                }
                Err(e) if attempts + 1 > MAX_EXTRA_ATTEMPTS => {
                    tracing::warn!(event = "sync_extra_op_dropped", tbl = %op.tbl, reason = %e);
                    self.unfile(&op.tbl, &key, &op.hlc).await?;
                }
                Err(e) => {
                    tracing::debug!(event = "sync_extra_op_deferred", tbl = %op.tbl, reason = %e);
                    sqlx::query("UPDATE sync_extra_deferred SET attempts = ?, reason = ?, deferred_at = ? WHERE tbl = ? AND pk = ?")
                        .bind(attempts + 1)
                        .bind(&e)
                        .bind(wall_ms() as i64)
                        .bind(&op.tbl)
                        .bind(&key)
                        .execute(&self.db)
                        .await
                        .map_err(err)?;
                    report.deferred += 1;
                }
            }
            on_item();
        }
        {
            let mut guard = self.clock().await?;
            if let Some(c) = guard.as_mut() {
                let mut conn = self.db.acquire().await.map_err(err)?;
                c.save(&mut conn).await?;
            }
        }
        for run in grown_runs {
            self.notice(Notice::RunLogUpdated(run));
        }
        Ok(report)
    }

    async fn cursor(&self, source: &str) -> Result<u64, String> {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
            .bind(cursor_key(source))
            .fetch_optional(&self.db)
            .await
            .map_err(err)?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    async fn cursor_start(&self, source: &str) -> Result<u64, String> {
        sqlx::query("DELETE FROM sync_state WHERE key = ?")
            .bind(prefs::CURSOR_RESET_KEY)
            .execute(&self.db)
            .await
            .map_err(err)?;
        self.cursor(source).await
    }

    /// A no-op while a reset is pending (`prefs`): a pull that started before
    /// it must not move the cursor past ops it filtered out.
    async fn set_cursor(&self, source: &str, seq: u64) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO sync_state (key, value) SELECT ?1, ?2 \
             WHERE NOT EXISTS (SELECT 1 FROM sync_state WHERE key = ?3) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(cursor_key(source))
        .bind(seq.to_string())
        .bind(prefs::CURSOR_RESET_KEY)
        .execute(&self.db)
        .await
        .map_err(err)?;
        Ok(())
    }

    async fn ask_filter(&self) -> Result<(Vec<String>, Vec<String>), String> {
        let mut conn = self.db.acquire().await.map_err(err)?;
        Ok(prefs::Filter::load(&mut conn).await?.lists())
    }

    async fn set_group_creator(&self, device_id: &str) -> Result<(), String> {
        super::leader::set_creator(&self.db, device_id).await
    }

    async fn parked_count(&self) -> Result<u64, String> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_parked")
            .fetch_one(&self.db)
            .await
            .map_err(err)?;
        Ok(n as u64)
    }
}

/// Whether a filed op is fetched by a pull with `scope`.
fn in_scope(op: &Op, scope: FileScope) -> bool {
    if op.tbl != files::TBL {
        return true;
    }
    let kind = op.pk.get("kind").and_then(Value::as_str).and_then(files::FileKind::parse);
    match scope {
        FileScope::All => true,
        FileScope::NoSessions => !kind.is_some_and(files::FileKind::is_session),
        FileScope::Nothing => false,
    }
}

impl SqliteSyncStore {
    /// File pulled `file` / `secret` ops in `sync_extra_deferred` without
    /// applying them (the newest per item wins); a pull fetches them after
    /// its pages.
    async fn file_extra(&self, ops: Vec<Op>) -> Result<usize, String> {
        let mut tx = self.db.begin().await.map_err(err)?;
        let mut n = 0;
        for op in ops {
            let json = serde_json::to_string(&op).map_err(|e| e.to_string())?;
            n += sqlx::query(
                "INSERT INTO sync_extra_deferred (tbl, pk, op, reason, attempts, deferred_at) VALUES (?, ?, ?, 'not fetched yet', 0, ?) \
                 ON CONFLICT(tbl, pk) DO UPDATE SET op = excluded.op, reason = excluded.reason, attempts = 0, deferred_at = excluded.deferred_at \
                 WHERE json_extract(excluded.op, '$.hlc') > json_extract(sync_extra_deferred.op, '$.hlc')",
            )
            .bind(&op.tbl)
            .bind(op.pk_key())
            .bind(json)
            .bind(wall_ms() as i64)
            .execute(&mut *tx)
            .await
            .map_err(err)?
            .rows_affected() as usize;
        }
        tx.commit().await.map_err(err)?;
        Ok(n)
    }

    /// Forget a filed op (done, or dropped) unless a newer one replaced it.
    async fn unfile(&self, tbl: &str, pk: &str, hlc: &Hlc) -> Result<(), String> {
        sqlx::query("DELETE FROM sync_extra_deferred WHERE tbl = ? AND pk = ? AND json_extract(op, '$.hlc') <= ?")
            .bind(tbl)
            .bind(pk)
            .bind(hlc.to_string())
            .execute(&self.db)
            .await
            .map_err(err)?;
        Ok(())
    }

    /// The op for a DB row change; `None` = a filtered row (never sent).
    async fn db_entry(
        &self,
        conn: &mut sqlx::SqliteConnection,
        paths: &PathCtx,
        spec: &'static schema::TableSpec,
        pk_text: &str,
        pk: Value,
    ) -> Result<Option<(Value, (OpKind, Option<serde_json::Map<String, Value>>))>, String> {
        let row = read_row(conn, spec, &pk).await?;
        if let Some(f) = spec.filter {
            let value = row
                .as_ref()
                .and_then(|r| r.get(f.col))
                .or_else(|| (spec.pk == [f.col]).then_some(&pk))
                .and_then(Value::as_str);
            if !spec.accepts(value) {
                return Ok(None);
            }
        }
        let entry = match row {
            Some(r) => {
                let mut wire = encode_row(paths, spec, r);
                prefs::restore_refs(conn, spec.name, pk_text, &mut wire).await?;
                if spec.name == "notes" {
                    apply::add_note_base(conn, pk_text, &mut wire).await?;
                }
                (OpKind::Upsert, Some(wire))
            }
            // A re-keyed / merged id goes out as an alias op (apply.rs).
            None => match apply::alias_target(conn, spec.name, pk_text).await? {
                Some(to) => (OpKind::Delete, Some(serde_json::Map::from_iter([(apply::ALIAS_FIELD.to_string(), to)]))),
                None => (OpKind::Delete, None),
            },
        };
        Ok(Some((pk, entry)))
    }
}

/// Outbox entries that will never be sent (table left the registry, filtered
/// row): dropped so they don't linger.
async fn drop_entries(conn: &mut sqlx::SqliteConnection, tbl: &str, pk: &str, up_to: i64) -> Result<(), String> {
    sqlx::query("DELETE FROM sync_outbox WHERE tbl = ? AND pk = ? AND seq <= ?")
        .bind(tbl)
        .bind(pk)
        .bind(up_to)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

/// One parked row (`sync_parked`), for Settings → Sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ParkedRow {
    pub tbl: String,
    /// The primary key, readable: the id, or `k=v, k=v` for composite keys.
    pub pk: String,
    /// Why the relay refused it (`too_large`, `bad_request`).
    pub reason: String,
    /// Unix millis.
    pub parked_at: i64,
}

/// Canonical pk JSON (`"id"` or `{"a":1,"b":"x"}`) as a short label.
fn pk_summary(pk: &str) -> String {
    let plain = |v: &Value| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    match serde_json::from_str::<Value>(pk) {
        Ok(Value::Object(m)) => m.iter().map(|(k, v)| format!("{k}={}", plain(v))).collect::<Vec<_>>().join(", "),
        Ok(v) => plain(&v),
        Err(_) => pk.to_string(),
    }
}

/// The newest parked rows (at most `limit`).
pub async fn parked_list(db: &Db, limit: usize) -> Result<Vec<ParkedRow>, String> {
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT tbl, pk, reason, parked_at FROM sync_parked ORDER BY parked_at DESC, tbl, pk LIMIT ?",
    )
    .bind(limit as i64)
    .fetch_all(db)
    .await
    .map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|(tbl, pk, reason, parked_at)| ParkedRow { tbl, pk: pk_summary(&pk), reason, parked_at })
        .collect())
}

/// Queue every parked row for push again (the current row decides what goes
/// out) and forget the records. Returns how many were queued. A row the relay
/// still refuses is parked again on the next push.
pub async fn parked_retry(db: &Db) -> Result<usize, String> {
    let mut tx = db.begin().await.map_err(err)?;
    let n = sqlx::query(
        "INSERT INTO sync_outbox (tbl, pk, op, changed_at) SELECT tbl, pk, 'U', ? FROM sync_parked ORDER BY parked_at",
    )
    .bind(wall_ms() as i64)
    .execute(&mut *tx)
    .await
    .map_err(err)?
    .rows_affected();
    sqlx::query("DELETE FROM sync_parked").execute(&mut *tx).await.map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(n as usize)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    const T0: &str = "2026-01-01T00:00:00Z";

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    async fn device(name: &str, home: &str) -> (Db, SqliteSyncStore) {
        let db = mem_db().await;
        let store = SqliteSyncStore::new(db.clone(), name.into()).with_paths(PathCtx::with_home(home));
        (db, store)
    }

    async fn exec(db: &Db, sql: &str) {
        sqlx::query(sql).execute(db).await.unwrap();
    }

    async fn outbox(db: &Db) -> Vec<(String, String, String)> {
        sqlx::query_as("SELECT tbl, pk, op FROM sync_outbox ORDER BY seq").fetch_all(db).await.unwrap()
    }

    async fn scalar(db: &Db, sql: &str) -> Option<String> {
        sqlx::query_scalar(sql).fetch_optional(db).await.unwrap()
    }

    /// Push everything pending from `from` into `to`.
    async fn sync(from: &SqliteSyncStore, to: &SqliteSyncStore) -> ApplyReport {
        let ops = from.pending(1000).await.unwrap();
        if let Some((seq, _)) = ops.last() {
            from.mark_pushed(*seq).await.unwrap();
        }
        to.apply_remote(ops.into_iter().map(|(_, op)| op).collect()).await.unwrap()
    }

    async fn add_project(db: &Db, id: &str, path: &str) {
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(id)
            .bind(path)
            .bind(T0)
            .execute(db)
            .await
            .unwrap();
    }

    fn upsert(tbl: &str, pk: Value, hlc: Hlc, row: Value) -> Op {
        Op {
            v: OP_VERSION,
            schema: SCHEMA_VERSION,
            hlc,
            tbl: tbl.into(),
            pk,
            kind: OpKind::Upsert,
            row: Some(row.as_object().unwrap().clone()),
        }
    }

    fn note_row(id: &str, title: &str, content: &str) -> Value {
        json!({"id": id, "title": title, "content": content, "project_id": null,
               "position": 0, "created_at": T0, "updated_at": T0, "run_id": null})
    }

    #[tokio::test]
    async fn every_table_is_classified() {
        let db = mem_db().await;
        let tables: Vec<String> = sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(&db)
            .await
            .unwrap();
        for t in tables {
            let known = schema::spec(&t).is_some() || schema::EXCLUDED_TABLES.contains(&t.as_str()) || t.starts_with("sync_");
            assert!(known, "table {t} is neither synced nor excluded in sync/schema.rs");
        }
        // And every spec matches a real table with those pk columns.
        let mut conn = db.acquire().await.unwrap();
        for spec in schema::SYNCED_TABLES {
            let cols = crate::sync::codec::table_columns(&mut conn, spec.name).await.unwrap();
            assert!(!cols.is_empty(), "{} missing", spec.name);
            for c in spec.pk.iter().chain(spec.skip_cols).chain(spec.path_cols).chain(spec.natural_key) {
                assert!(cols.iter().any(|x| x == c), "{}.{c} missing", spec.name);
            }
        }
    }

    #[tokio::test]
    async fn triggers_capture_insert_update_delete() {
        let db = mem_db().await;
        exec(&db, "INSERT INTO notes (id, title, position, created_at, updated_at) VALUES ('n1', 'a', 0, 'x', 'x')").await;
        exec(&db, "UPDATE notes SET title = 'b' WHERE id = 'n1'").await;
        exec(&db, "DELETE FROM notes WHERE id = 'n1'").await;
        let ob = outbox(&db).await;
        let n1 = r#""n1""#.to_string();
        assert_eq!(
            ob,
            vec![
                ("notes".into(), n1.clone(), "I".into()),
                ("notes".into(), n1.clone(), "U".into()),
                ("notes".into(), n1, "D".into()),
            ]
        );
        let changed: i64 = sqlx::query_scalar("SELECT MIN(changed_at) FROM sync_outbox").fetch_one(&db).await.unwrap();
        assert!((changed - chrono::Utc::now().timestamp_millis()).abs() < 60_000);
        // Reinstalling with an unchanged registry is a no-op.
        assert!(!crate::sync::capture::install_triggers(&db).await.unwrap());
        exec(&db, "DROP TRIGGER sync_cap_notes_ins").await;
        assert!(crate::sync::capture::install_triggers(&db).await.unwrap());
    }

    #[tokio::test]
    async fn pending_coalesces_and_emits_deletes() {
        let (db, store) = device("a", "/Users/a").await;
        exec(&db, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'x', 0, 'x')").await;
        exec(&db, "UPDATE todos SET text = 'y' WHERE id = 't1'").await;
        exec(&db, "INSERT INTO todos (id, text, position, created_at) VALUES ('t2', 'z', 1, 'x')").await;
        exec(&db, "DELETE FROM todos WHERE id = 't2'").await;
        let ops = store.pending(10).await.unwrap();
        assert_eq!(ops.len(), 2);
        let (_, t1) = &ops[0];
        assert_eq!((t1.pk.clone(), t1.kind), (json!("t1"), OpKind::Upsert));
        assert_eq!(t1.row.as_ref().unwrap()["text"], "y");
        let (seq, t2) = &ops[1];
        assert_eq!((t2.pk.clone(), t2.kind, t2.row.clone()), (json!("t2"), OpKind::Delete, None));
        assert!(t1.hlc < t2.hlc);
        assert_eq!(t1.hlc.device, "a");
        store.mark_pushed(*seq).await.unwrap();
        assert!(outbox(&db).await.is_empty());
        let deleted: i64 = sqlx::query_scalar(r#"SELECT deleted FROM sync_rows WHERE pk = '"t2"'"#).fetch_one(&db).await.unwrap();
        assert_eq!(deleted, 1);
    }

    #[tokio::test]
    async fn applied_ops_are_not_echoed() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n1', 'T', 'body', 0, 'x', 'x')").await;
        let r = sync(&a, &b).await;
        assert_eq!(r, ApplyReport { applied: 1, ..Default::default() });
        assert_eq!(scalar(&db_b, "SELECT content FROM notes WHERE id = 'n1'").await.unwrap(), "body");
        assert!(outbox(&db_b).await.is_empty(), "guard must mute the triggers");
        assert!(b.pending(10).await.unwrap().is_empty());
        let active: i64 = sqlx::query_scalar("SELECT active FROM sync_apply_guard").fetch_one(&db_b).await.unwrap();
        assert_eq!(active, 0);
        // Re-applying the same op is skipped.
        exec(&db_a, "UPDATE notes SET content = 'body' WHERE id = 'n1'").await;
        let ops: Vec<Op> = a.pending(10).await.unwrap().into_iter().map(|(_, o)| o).collect();
        let r1 = b.apply_remote(ops.clone()).await.unwrap();
        let r2 = b.apply_remote(ops).await.unwrap();
        assert_eq!((r1.applied, r2.skipped_older), (1, 1));
    }

    #[tokio::test]
    async fn lww_both_directions() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'base', 0, 'x')").await;
        sync(&a, &b).await;

        let far = 9_000_000_000_000;
        let row = |text: &str| json!({"id": "t1", "text": text, "done": 0, "position": 0, "created_at": "x"});

        // Older remote op loses against the row's known HLC.
        let old = upsert("todos", json!("t1"), Hlc::new(1, 0, "z"), row("ancient"));
        assert_eq!(b.apply_remote(vec![old]).await.unwrap().skipped_older, 1);

        // A newer remote op wins over an older unpushed local change...
        exec(&db_b, "UPDATE todos SET text = 'local-b' WHERE id = 't1'").await;
        let newer = upsert("todos", json!("t1"), Hlc::new(far, 0, "a"), row("remote-a"));
        assert_eq!(b.apply_remote(vec![newer]).await.unwrap().applied, 1);
        assert_eq!(scalar(&db_b, "SELECT text FROM todos WHERE id = 't1'").await.unwrap(), "remote-a");
        assert!(outbox(&db_b).await.is_empty(), "the losing local change is not pushed");

        // ...and an unpushed local change newer than the remote op wins.
        exec(&db_b, "UPDATE todos SET text = 'local-b2' WHERE id = 't1'").await;
        exec(&db_b, &format!("UPDATE sync_outbox SET changed_at = {}", far + 10)).await;
        let stale = upsert("todos", json!("t1"), Hlc::new(far + 1, 0, "a"), row("remote-a2"));
        assert_eq!(b.apply_remote(vec![stale]).await.unwrap().skipped_older, 1);
        assert_eq!(scalar(&db_b, "SELECT text FROM todos WHERE id = 't1'").await.unwrap(), "local-b2");
        // b's change is pushed with an HLC above the one it beat, so a takes it.
        let r = sync(&b, &a).await;
        assert_eq!(r.applied, 1);
        assert_eq!(scalar(&db_a, "SELECT text FROM todos WHERE id = 't1'").await.unwrap(), "local-b2");
    }

    #[tokio::test]
    async fn retired_and_unknown_tables_and_columns_are_tolerated() {
        let (db_b, b) = device("b", "/Users/b").await;
        let far = 9_000_000_000_000;
        // An older peer still sends Pulse rows: skipped, never parked.
        let watch = upsert(
            "pulse_watches",
            json!("pw-1"),
            Hlc::new(far, 0, "a"),
            json!({"id": "pw-1", "project_id": "p1", "source_type": "github_issue", "source_ref": "o/r#1"}),
        );
        let r = b.apply_remote(vec![watch]).await.unwrap();
        assert_eq!((r.applied, r.skipped_older, r.deferred), (0, 1, 0));
        // A table this build does not know yet is parked for a later upgrade.
        let future = upsert("focus_future", json!("x"), Hlc::new(far, 1, "a"), json!({"id": "x"}));
        assert_eq!(b.apply_remote(vec![future]).await.unwrap().deferred, 1);
        assert_eq!(scalar(&db_b, "SELECT reason FROM sync_deferred").await.unwrap(), "unknown table");
        // Columns this build does not have are dropped; an old peer's todo
        // (no Focus columns) gets the column defaults.
        let todo = upsert(
            "todos",
            json!("t1"),
            Hlc::new(far, 2, "a"),
            json!({"id": "t1", "text": "x", "done": 0, "position": 0, "created_at": "x", "colour": "red"}),
        );
        assert_eq!(b.apply_remote(vec![todo]).await.unwrap().applied, 1);
        assert_eq!(scalar(&db_b, "SELECT status || '/' || priority FROM todos WHERE id = 't1'").await.unwrap(), "todo/2");
        // Focus columns and day plans travel.
        let todo = upsert(
            "todos",
            json!("t2"),
            Hlc::new(far, 3, "a"),
            json!({"id": "t2", "text": "y", "done": 0, "position": 0, "created_at": "x",
                   "due_time": "15:00", "priority": 1, "estimate_min": 90, "status": "doing"}),
        );
        let plan = upsert(
            "focus_day_plans",
            json!("2026-10-30"),
            Hlc::new(far, 4, "a"),
            json!({"date": "2026-10-30", "plan_json": "{}", "created_at": "x"}),
        );
        assert_eq!(b.apply_remote(vec![todo, plan]).await.unwrap().applied, 2);
        assert_eq!(
            scalar(&db_b, "SELECT due_time || '/' || priority || '/' || estimate_min || '/' || status FROM todos WHERE id = 't2'").await.unwrap(),
            "15:00/1/90/doing"
        );
        assert_eq!(scalar(&db_b, "SELECT plan_json FROM focus_day_plans").await.unwrap(), "{}");
    }

    #[tokio::test]
    async fn delete_vs_update_race() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'x', 0, 'x'), ('t2', 'x', 1, 'x')").await;
        sync(&a, &b).await;
        let row = |id: &str, text: &str| json!({"id": id, "text": text, "done": 0, "position": 0, "created_at": "x"});
        let del = |id: &str, hlc: Hlc| Op {
            v: OP_VERSION, schema: SCHEMA_VERSION, hlc, tbl: "todos".into(), pk: json!(id), kind: OpKind::Delete, row: None,
        };
        let far = 9_000_000_000_000;
        // t1: the delete is newer than the update -> deleted.
        b.apply_remote(vec![del("t1", Hlc::new(far + 2, 0, "a")), upsert("todos", json!("t1"), Hlc::new(far + 1, 0, "c"), row("t1", "upd"))]).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT id FROM todos WHERE id = 't1'").await, None);
        let tomb: i64 = sqlx::query_scalar(r#"SELECT deleted FROM sync_rows WHERE tbl = 'todos' AND pk = '"t1"'"#).fetch_one(&db_b).await.unwrap();
        assert_eq!(tomb, 1);
        // t2: the update is newer than the delete -> the row comes back.
        let r = b.apply_remote(vec![del("t2", Hlc::new(far + 1, 0, "a")), upsert("todos", json!("t2"), Hlc::new(far + 2, 0, "c"), row("t2", "revived"))]).await.unwrap();
        assert_eq!((r.applied, r.skipped_older), (1, 1));
        assert_eq!(scalar(&db_b, "SELECT text FROM todos WHERE id = 't2'").await.unwrap(), "revived");
        let tomb: i64 = sqlx::query_scalar(r#"SELECT deleted FROM sync_rows WHERE tbl = 'todos' AND pk = '"t2"'"#).fetch_one(&db_b).await.unwrap();
        assert_eq!(tomb, 0);
        // A stale update after the tombstone stays dead.
        let r = b.apply_remote(vec![upsert("todos", json!("t1"), Hlc::new(far, 5, "c"), row("t1", "zombie"))]).await.unwrap();
        assert_eq!(r.skipped_older, 1);
        assert_eq!(scalar(&db_b, "SELECT id FROM todos WHERE id = 't1'").await, None);
    }

    #[tokio::test]
    async fn composite_pk_link_tables() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        add_project(&db_a, "p1", "/Users/a/w/p1").await;
        exec(&db_a, "INSERT INTO servers (id, label, host, username, auth_method, created_at) VALUES ('s1', 'vps', 'h', 'u', 'agent', 'x')").await;
        exec(&db_a, "INSERT INTO project_servers (project_id, server_id, role, created_at) VALUES ('p1', 's1', 'staging', 'x'), ('p1', 's1', 'production', 'x')").await;
        let ob = outbox(&db_a).await;
        assert!(ob.iter().any(|(t, pk, _)| t == "project_servers" && pk == r#"{"project_id":"p1","role":"staging","server_id":"s1"}"#), "{ob:?}");
        assert_eq!(sync(&a, &b).await.applied, 4);
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_servers").fetch_one(&db_b).await.unwrap();
        assert_eq!(n, 2);
        exec(&db_a, "DELETE FROM project_servers WHERE role = 'staging'").await;
        let ops = a.pending(10).await.unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].1.pk, json!({"project_id": "p1", "role": "staging", "server_id": "s1"}));
        a.mark_pushed(ops[0].0).await.unwrap();
        b.apply_remote(vec![ops[0].1.clone()]).await.unwrap();
        let roles: Vec<String> = sqlx::query_scalar("SELECT role FROM project_servers").fetch_all(&db_b).await.unwrap();
        assert_eq!(roles, vec!["production"]);
    }

    #[tokio::test]
    async fn settings_sync_per_portable_key() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'dark'), ('claude_path', '/opt/claude'), ('remote_enabled', '1')").await;
        exec(&db_a, "INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'light')").await;
        let ob = outbox(&db_a).await;
        assert!(ob.iter().all(|(_, pk, _)| pk == r#""theme""#), "{ob:?}");
        assert_eq!(sync(&a, &b).await.applied, 1);
        assert_eq!(scalar(&db_b, "SELECT value FROM settings WHERE key = 'theme'").await.unwrap(), "light");
        // claude_path keeps b's own (seeded) value.
        assert_eq!(scalar(&db_b, "SELECT value FROM settings WHERE key = 'claude_path'").await.unwrap(), "claude");
        // A non-portable key from a (buggy/older) peer is never applied.
        let bad = upsert("settings", json!("claude_path"), Hlc::new(9_000_000_000_000, 0, "a"), json!({"key": "claude_path", "value": "/x"}));
        assert_eq!(b.apply_remote(vec![bad]).await.unwrap().skipped_older, 1);
        assert_eq!(scalar(&db_b, "SELECT value FROM settings WHERE key = 'claude_path'").await.unwrap(), "claude");
    }

    #[tokio::test]
    async fn paths_are_portable_between_homes() {
        let (db_a, a) = device("a", "/Users/alice").await;
        let (db_b, b) = device("b", "/Users/bob").await;
        add_project(&db_a, "p1", "/Users/alice/Workspace/devdy").await;
        exec(&db_a, "INSERT INTO runs (id, project_id, type, created_at, output_path, transcript_path, transcript_synced_size) \
                     VALUES ('r1', 'p1', 'session', 'x', '/Users/alice/Workspace/devdy/.devdy/runs/r1.md', '/opt/t.jsonl', 99)").await;
        let ops = a.pending(10).await.unwrap();
        let run = &ops.iter().find(|(_, o)| o.tbl == "runs").unwrap().1;
        let row = run.row.as_ref().unwrap();
        assert_eq!(row["output_path"], "~/Workspace/devdy/.devdy/runs/r1.md");
        assert!(!row.contains_key("transcript_synced_size"));
        b.apply_remote(ops.into_iter().map(|(_, o)| o).collect()).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT path FROM projects WHERE id = 'p1'").await.unwrap(), "/Users/bob/Workspace/devdy");
        assert_eq!(scalar(&db_b, "SELECT output_path FROM runs").await.unwrap(), "/Users/bob/Workspace/devdy/.devdy/runs/r1.md");
        assert_eq!(scalar(&db_b, "SELECT transcript_path FROM runs").await.unwrap(), "/opt/t.jsonl");
        let synced: Option<i64> = sqlx::query_scalar("SELECT transcript_synced_size FROM runs").fetch_one(&db_b).await.unwrap();
        assert_eq!(synced, None, "machine-local column is not applied");
    }

    #[tokio::test]
    async fn fk_out_of_order_apply_and_deferral() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        add_project(&db_a, "p1", "/srv/p1").await;
        exec(&db_a, "INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('ru1', 'r', '', '/x', 'x')").await;
        exec(&db_a, "INSERT INTO project_rules (project_id, rule_id, target, applied_at) VALUES ('p1', 'ru1', 'both', 'x')").await;
        let mut ops: Vec<Op> = a.pending(10).await.unwrap().into_iter().map(|(_, o)| o).collect();
        // Children first, parents last: still applies in one batch.
        ops.reverse();
        assert_eq!(ops[0].tbl, "project_rules");
        let r = b.apply_remote(ops).await.unwrap();
        assert_eq!((r.applied, r.deferred), (3, 0));
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_rules").fetch_one(&db_b).await.unwrap();
        assert_eq!(n, 1);

        // A link whose parent never arrives is parked, the rest still applies.
        let link = upsert("project_rules", json!({"project_id": "p1", "rule_id": "ru-missing"}), Hlc::new(9_000_000_000_000, 0, "a"),
            json!({"project_id": "p1", "rule_id": "ru-missing", "target": "both", "applied_at": "x", "manual": 1}));
        let note = upsert("notes", json!("n1"), Hlc::new(9_000_000_000_000, 1, "a"), note_row("n1", "t", "c"));
        let r = b.apply_remote(vec![link, note]).await.unwrap();
        assert_eq!((r.applied, r.deferred), (1, 1));
        assert_eq!(scalar(&db_b, "SELECT reason FROM sync_deferred").await.unwrap(), "missing parent row");
        assert!(scalar(&db_b, "SELECT id FROM notes WHERE id = 'n1'").await.is_some());

        // Unknown table / newer schema are deferred too, not errors.
        let mut future = upsert("notes", json!("n2"), Hlc::new(9_000_000_000_000, 2, "a"), note_row("n2", "t", "c"));
        future.schema = SCHEMA_VERSION + 1;
        let mut file = upsert("file", json!("f"), Hlc::new(9_000_000_000_000, 3, "a"), json!({}));
        file.kind = OpKind::Upsert;
        let r = b.apply_remote(vec![future, file]).await.unwrap();
        assert_eq!((r.applied, r.deferred), (0, 3));

        // When the parent shows up, the parked link is retried and applied.
        let rule = upsert("rules", json!("ru-missing"), Hlc::new(9_000_000_000_000, 4, "a"),
            json!({"id": "ru-missing", "name": "late", "description": "", "target": "both", "source_path": "/y", "updated_at": "x"}));
        let r = b.apply_remote(vec![rule]).await.unwrap();
        assert_eq!((r.applied, r.deferred), (2, 2));
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM project_rules").fetch_one(&db_b).await.unwrap();
        assert_eq!(n, 2);
        assert!(outbox(&db_b).await.is_empty());
    }

    #[tokio::test]
    async fn note_conflict_keeps_a_copy() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n1', 'Plan', 'v0', 0, 'x', 'x')").await;
        sync(&a, &b).await;
        // Both edit offline; b's edit is even newer, yet the remote row wins and
        // b's text survives as a copy.
        exec(&db_a, "UPDATE notes SET content = 'from a' WHERE id = 'n1'").await;
        exec(&db_b, "UPDATE notes SET content = 'from b' WHERE id = 'n1'").await;
        let r = sync(&a, &b).await;
        assert_eq!((r.applied, r.conflicts), (1, 1));
        assert_eq!(scalar(&db_b, "SELECT content FROM notes WHERE id = 'n1'").await.unwrap(), "from a");
        let copy: (String, String) = sqlx::query_as("SELECT title, content FROM notes WHERE id != 'n1'").fetch_one(&db_b).await.unwrap();
        assert_eq!(copy, ("Plan (conflict copy)".into(), "from b".into()));
        // b pushes only the copy; a ends with both versions too.
        let ops = b.pending(10).await.unwrap();
        assert_eq!(ops.len(), 1);
        assert_ne!(ops[0].1.pk, json!("n1"));
        b.mark_pushed(ops[0].0).await.unwrap();
        a.apply_remote(vec![ops[0].1.clone()]).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM notes").fetch_one(&db_a).await.unwrap();
        assert_eq!(n, 2);
        assert_eq!(scalar(&db_a, "SELECT content FROM notes WHERE id = 'n1'").await.unwrap(), "from a");
    }

    #[tokio::test]
    async fn hlc_survives_restart_and_cursor_persists() {
        let (db, store) = device("a", "/Users/a").await;
        let far = Hlc::new(9_000_000_000_000, 7, "z");
        store.apply_remote(vec![upsert("todos", json!("t1"), far.clone(),
            json!({"id": "t1", "text": "x", "done": 0, "position": 0, "created_at": "x"}))]).await.unwrap();
        let reopened = SqliteSyncStore::new(db.clone(), "a".into());
        exec(&db, "UPDATE todos SET text = 'y'").await;
        let ops = reopened.pending(10).await.unwrap();
        assert!(ops[0].1.hlc > far, "persisted clock orders local edits after observed ones");
        assert_eq!(reopened.cursor("peer").await.unwrap(), 0);
        reopened.set_cursor("peer", 42).await.unwrap();
        assert_eq!(store.cursor("peer").await.unwrap(), 42);
        assert_eq!(store.cursor("other").await.unwrap(), 0, "one cursor per source device");
    }
    /// Exchange ops both ways until neither side has anything left to push.
    async fn converge(a: &SqliteSyncStore, b: &SqliteSyncStore) {
        for _ in 0..8 {
            sync(a, b).await;
            sync(b, a).await;
            if outbox(&a.db).await.is_empty() && outbox(&b.db).await.is_empty() {
                for db in [&a.db, &b.db] {
                    let parked: Vec<String> = sqlx::query_scalar("SELECT reason FROM sync_deferred").fetch_all(db).await.unwrap();
                    assert!(parked.is_empty(), "deferred: {parked:?}");
                }
                return;
            }
        }
        panic!("no convergence");
    }

    async fn rows(db: &Db, sql: &str) -> Vec<String> {
        sqlx::query_scalar(sql).fetch_all(db).await.unwrap()
    }

    /// Two paired devices with the same project (synced) and nothing else.
    async fn paired() -> (Db, SqliteSyncStore, Db, SqliteSyncStore) {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        add_project(&db_a, "p1", "/Users/a/w/p1").await;
        exec(&db_a, "INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('ru', 'r', '', '/x', 'x')").await;
        converge(&a, &b).await;
        (db_a, a, db_b, b)
    }

    #[tokio::test]
    async fn same_skill_created_on_both_converges_on_the_lower_id() {
        for a_first in [true, false] {
            let (db_a, a, db_b, b) = paired().await;
            // Created independently after pairing; b's copy has the higher id
            // and a child pointing at it.
            exec(&db_a, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-1', 'review', 'from a', '/s', 'x')").await;
            exec(&db_b, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-2', 'review', 'from b', '/s', 'x')").await;
            exec(&db_b, "INSERT INTO project_skills (project_id, skill_id, applied_at) VALUES ('p1', 'sk-2', 'x')").await;
            exec(&db_b, "UPDATE sync_outbox SET changed_at = changed_at + 5000").await; // b's edit is the newer one
            if a_first {
                sync(&a, &b).await;
            } else {
                sync(&b, &a).await;
            }
            converge(&a, &b).await;
            for db in [&db_a, &db_b] {
                assert_eq!(rows(db, "SELECT id || ':' || description FROM skills").await, vec!["sk-1:from b"], "a_first={a_first}");
                assert_eq!(rows(db, "SELECT project_id || '/' || skill_id FROM project_skills").await, vec!["p1/sk-1"]);
            }
        }
    }

    #[tokio::test]
    async fn same_cli_session_converges() {
        for a_first in [true, false] {
            let (db_a, a, db_b, b) = paired().await;
            // The same external session imported on both machines; b's row
            // has the lower id here and a's has a linked note.
            exec(&db_a, "INSERT INTO runs (id, project_id, type, engine, session_id, created_at) VALUES ('run-z', 'p1', 'session', 'claude', 'sess', 'a')").await;
            exec(&db_a, "INSERT INTO notes (id, title, position, created_at, updated_at) VALUES ('n1', 'n', 0, 'x', 'x')").await;
            exec(&db_a, "INSERT INTO note_runs (note_id, run_id, project_id, source, linked_at) VALUES ('n1', 'run-z', 'p1', 'manual', 'x')").await;
            exec(&db_b, "INSERT INTO runs (id, project_id, type, engine, session_id, created_at) VALUES ('run-a', 'p1', 'session', 'claude', 'sess', 'b')").await;
            if a_first {
                sync(&a, &b).await;
            } else {
                sync(&b, &a).await;
            }
            converge(&a, &b).await;
            for db in [&db_a, &db_b] {
                assert_eq!(rows(db, "SELECT id FROM runs").await, vec!["run-a"], "a_first={a_first}");
                assert_eq!(rows(db, "SELECT note_id || '/' || run_id FROM note_runs").await, vec!["n1/run-a"]);
            }
        }
    }

    #[tokio::test]
    async fn rekeyed_rows_take_their_keychain_items_along() {
        use crate::sync::secrets::tests::FakeKeychain;
        let (db_a, a, db_b, b) = paired().await;
        let (kc_a, kc_b) = (std::sync::Arc::new(FakeKeychain::default()), std::sync::Arc::new(FakeKeychain::default()));
        let a = a.with_secrets(kc_a.clone());
        let b = b.with_secrets(kc_b.clone());
        exec(&db_a, "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m-1', 'fs', 'stdio', 'x')").await;
        exec(&db_b, "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m-2', 'fs', 'stdio', 'x')").await;
        // Created on b first this time.
        exec(&db_b, "INSERT INTO google_accounts (id, label, email, created_at) VALUES ('go-2', 'work', 'a@b', 'x')").await;
        exec(&db_a, "INSERT INTO google_accounts (id, label, email, created_at) VALUES ('go-1', 'work', 'a@b', 'x')").await;
        kc_b.put("google_accounts", "go-2", "refresh_b");
        kc_b.put("mcp", "m-2", r#"{"env":{"K":"b"},"headers":{}}"#);
        converge(&a, &b).await;
        assert_eq!(rows(&db_b, "SELECT id FROM mcp_servers").await, vec!["m-1"]);
        // b's item moved to the surviving id; an item already there is kept.
        assert_eq!(kc_b.get("mcp", "m-1").as_deref(), Some(r#"{"env":{"K":"b"},"headers":{}}"#));
        assert_eq!(kc_b.get("mcp", "m-2"), None);
        assert_eq!(rows(&db_b, "SELECT id FROM google_accounts").await, vec!["go-1"]);
        assert_eq!((kc_b.get("google_accounts", "go-1").as_deref(), kc_b.get("google_accounts", "go-2")), (Some("refresh_b"), None));
        assert!(kc_a.items.lock().unwrap().is_empty(), "a had nothing to move");
        kc_b.put("github", "gh-1", "kept");
        kc_b.put("github", "gh-9", "orphan");
        let mut conn = db_b.acquire().await.unwrap();
        crate::sync::secrets::queue_key_move(&mut conn, "github_accounts", &json!("gh-9"), &json!("gh-1")).await.unwrap();
        crate::sync::secrets::queue_key_move(&mut conn, "notes", &json!("x"), &json!("y")).await.unwrap();
        drop(conn);
        assert_eq!(crate::sync::secrets::run_key_moves(&db_b, kc_b.clone()).await.unwrap(), 1);
        assert_eq!((kc_b.get("github", "gh-1").as_deref(), kc_b.get("github", "gh-9")), (Some("kept"), None));
        // A locked Keychain keeps the move queued.
        let mut conn = db_b.acquire().await.unwrap();
        crate::sync::secrets::queue_key_move(&mut conn, "github_accounts", &json!("gh-1"), &json!("gh-0")).await.unwrap();
        drop(conn);
        *kc_b.broken.lock().unwrap() = true;
        assert!(crate::sync::secrets::run_key_moves(&db_b, kc_b.clone()).await.is_err());
        *kc_b.broken.lock().unwrap() = false;
        assert_eq!(crate::sync::secrets::run_key_moves(&db_b, kc_b.clone()).await.unwrap(), 1);
        assert_eq!(kc_b.get("github", "gh-0").as_deref(), Some("kept"));
    }

    async fn skill(db: &Db, id: &str, name: &str) {
        exec(db, &format!("INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('{id}', '{name}', '', '/s', 'x')")).await;
    }

    #[tokio::test]
    async fn merged_then_deleted_row_is_gone_everywhere() {
        let (db_a, a, db_b, b) = paired().await;
        skill(&db_a, "sk-1", "review").await;
        skill(&db_b, "sk-2", "review").await;
        // a merges b's copy into its own (lower) id...
        sync(&b, &a).await;
        assert_eq!(rows(&db_a, "SELECT id FROM skills").await, vec!["sk-1"]);
        // ...and deletes it before b ever hears of sk-1.
        exec(&db_a, "DELETE FROM skills WHERE id = 'sk-1'").await;
        let ops = a.pending(100).await.unwrap();
        let alias = ops.iter().find(|(_, o)| o.pk == json!("sk-2")).map(|(_, o)| o.clone()).expect("loser id pushed");
        assert_eq!((alias.kind, apply::alias_of(&alias)), (OpKind::Delete, Some(&json!("sk-1"))));
        a.mark_pushed(ops.last().unwrap().0).await.unwrap();
        b.apply_remote(ops.into_iter().map(|(_, o)| o).collect()).await.unwrap();
        converge(&a, &b).await;
        for db in [&db_a, &db_b] {
            assert!(rows(db, "SELECT id FROM skills").await.is_empty());
        }
        // A late copy of the loser's old upsert is redirected, not resurrected.
        let late = upsert("skills", json!("sk-2"), "0000000000001000-b".parse().unwrap(),
            json!({"id": "sk-2", "name": "review", "description": "", "source_path": "/s", "updated_at": "x"}));
        a.apply_remote(vec![late]).await.unwrap();
        assert!(rows(&db_a, "SELECT id FROM skills").await.is_empty());
    }

    fn alias_op(tbl: &str, old: &str, to: &str, hlc: Hlc) -> Op {
        Op {
            v: OP_VERSION, schema: SCHEMA_VERSION, hlc, tbl: tbl.into(), pk: json!(old), kind: OpKind::Delete,
            row: Some(serde_json::Map::from_iter([(apply::ALIAS_FIELD.to_string(), json!(to))])),
        }
    }

    /// M4: the loser of a fold changed here after the sender folded it: its
    /// newer content lands on the survivor and is pushed, instead of being
    /// dropped with the loser row.
    #[tokio::test]
    async fn alias_fold_keeps_a_newer_loser_edit() {
        let (db_a, a, db_b, b) = paired().await;
        skill(&db_a, "sk-1", "x").await;
        skill(&db_a, "sk-2", "y").await;
        exec(&db_a, "INSERT INTO project_skills (project_id, skill_id, applied_at) VALUES ('p1', 'sk-2', 'x')").await;
        converge(&a, &b).await;
        // Folded on another device a moment ago...
        let folded_at = Hlc::new(wall_ms(), 0, "c");
        // ...while b edited the loser (not pushed yet).
        exec(&db_b, "UPDATE skills SET description = 'late edit' WHERE id = 'sk-2'").await;
        exec(&db_b, "UPDATE sync_outbox SET changed_at = changed_at + 5000").await;
        b.apply_remote(vec![alias_op("skills", "sk-2", "sk-1", folded_at.clone())]).await.unwrap();
        assert_eq!(rows(&db_b, "SELECT id || ':' || description FROM skills").await, vec!["sk-1:late edit"]);
        assert_eq!(rows(&db_b, "SELECT skill_id FROM project_skills").await, vec!["sk-1"]);
        let out: Vec<Op> = b.pending(100).await.unwrap().into_iter().map(|(_, o)| o).collect();
        let up = out.iter().find(|o| o.pk == json!("sk-1")).expect("survivor pushed");
        assert_eq!(up.row.as_ref().unwrap()["description"], json!("late edit"));

        // An older loser is simply folded: the survivor keeps its content.
        let (_, _, db_c, c) = paired().await;
        skill(&db_c, "sk-1", "x").await;
        skill(&db_c, "sk-2", "y").await;
        exec(&db_c, "UPDATE skills SET description = 'survivor' WHERE id = 'sk-1'").await;
        exec(&db_c, "UPDATE sync_outbox SET changed_at = changed_at + 5000 WHERE pk = '\"sk-1\"'").await;
        c.apply_remote(vec![alias_op("skills", "sk-2", "sk-1", folded_at)]).await.unwrap();
        assert_eq!(rows(&db_c, "SELECT id || ':' || description FROM skills").await, vec!["sk-1:survivor"]);
    }

    /// L1: a row whose parent arrives under an id the same batch re-keys is
    /// applied in that batch, not left deferred until some later op.
    #[tokio::test]
    async fn child_of_an_aliased_parent_applies_in_the_same_batch() {
        let (db_b, b) = device("b", "/Users/b").await;
        add_project(&db_b, "p-1", "/Users/b/w/x").await;
        let at = Hlc::new(wall_ms(), 0, "a");
        let repo = upsert("repos", json!("r-1"), at.clone(),
            json!({"id": "r-1", "project_id": "p-2", "name": "x", "path": "/Users/b/w/x", "created_at": "x"}));
        let r = b.apply_remote(vec![repo, alias_op("projects", "p-2", "p-1", at)]).await.unwrap();
        assert_eq!(r.deferred, 0, "{r:?}");
        assert_eq!(rows(&db_b, "SELECT id || '>' || project_id FROM repos").await, vec!["r-1>p-1"]);
    }

    #[tokio::test]
    async fn rekey_split_across_pull_pages_keeps_every_child() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        let (db_c, c) = device("c", "/Users/c").await;
        // b's project reached c, which added a repo of its own (not pushed yet).
        add_project(&db_b, "p-2", "/Users/b/w/x").await;
        exec(&db_b, "INSERT INTO repos (id, project_id, name, path, created_at) VALUES ('r-b', 'p-2', 'x', '/Users/b/w/x', 'x')").await;
        sync(&b, &c).await;
        exec(&db_c, "INSERT INTO repos (id, project_id, name, path, created_at) VALUES ('r-c', 'p-2', 'y', '/Users/c/w/x/y', 'x')").await;
        // a made the same project independently with a lower id: b re-keys.
        add_project(&db_a, "p-1", "/Users/a/w/x").await;
        sync(&a, &b).await;
        assert_eq!(rows(&db_b, "SELECT id FROM projects").await, vec!["p-1"]);
        // c pulls b's re-key one op per page, old id first.
        let ops = b.pending(100).await.unwrap();
        assert_eq!(ops[0].1.pk, json!("p-2"));
        b.mark_pushed(ops.last().unwrap().0).await.unwrap();
        for (_, op) in ops {
            c.apply_remote(vec![op]).await.unwrap();
        }
        assert_eq!(rows(&db_c, "SELECT id FROM projects").await, vec!["p-1"]);
        assert_eq!(rows(&db_c, "SELECT id || '>' || project_id FROM repos ORDER BY id").await, vec!["r-b>p-1", "r-c>p-1"]);
        // c's own repo still goes out, under the surviving id.
        let out: Vec<Op> = c.pending(100).await.unwrap().into_iter().map(|(_, o)| o).collect();
        let rc = out.iter().find(|o| o.pk == json!("r-c")).expect("r-c pushed");
        assert_eq!(rc.row.as_ref().unwrap()["project_id"], json!("p-1"));
        assert!(out.iter().all(|o| o.pk != json!("p-2")), "c does not push the re-key again");
    }

    #[tokio::test]
    async fn rename_into_a_unique_clash_converges() {
        let (db_a, a, db_b, b) = paired().await;
        skill(&db_a, "sk-1", "x").await;
        skill(&db_a, "sk-2", "y").await;
        exec(&db_a, "INSERT INTO project_skills (project_id, skill_id, applied_at) VALUES ('p1', 'sk-2', 'x')").await;
        converge(&a, &b).await;
        // a frees the name and gives it to sk-2; b edits sk-1 meanwhile (newer).
        exec(&db_a, "UPDATE skills SET name = 'w' WHERE id = 'sk-1'").await;
        exec(&db_a, "UPDATE skills SET name = 'x' WHERE id = 'sk-2'").await;
        exec(&db_b, "UPDATE skills SET description = 'from b' WHERE id = 'sk-1'").await;
        exec(&db_b, "UPDATE sync_outbox SET changed_at = changed_at + 5000").await;
        converge(&a, &b).await;
        for db in [&db_a, &db_b] {
            assert_eq!(rows(db, "SELECT id || ':' || name || ':' || description FROM skills").await, vec!["sk-1:x:from b"]);
            assert_eq!(rows(db, "SELECT project_id || '/' || skill_id FROM project_skills").await, vec!["p1/sk-1"]);
        }
    }

    #[tokio::test]
    async fn nothing_is_captured_outside_a_group() {
        let db = mem_db().await;
        crate::sync::capture::set_member(&db, false).await.unwrap();
        exec(&db, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'a', 0, 'x')").await;
        assert!(outbox(&db).await.is_empty());
        let store = SqliteSyncStore::new(db.clone(), "d".into());
        assert!(!store.has_pending().await.unwrap());
        crate::sync::capture::set_member(&db, true).await.unwrap();
        exec(&db, "UPDATE todos SET text = 'b' WHERE id = 't1'").await;
        assert_eq!(outbox(&db).await.len(), 1);
        assert!(store.has_pending().await.unwrap());
        // Leaving drops what was not pushed.
        crate::sync::capture::set_member(&db, false).await.unwrap();
        assert!(outbox(&db).await.is_empty());

        // Startup: entries queued by older builds outside a group are pruned...
        exec(&db, "DELETE FROM sync_state WHERE key = 'member'").await;
        exec(&db, "INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES ('todos', '\"t1\"', 'U', 1)").await;
        crate::sync::capture::settle_membership(&db).await.unwrap();
        assert!(outbox(&db).await.is_empty());
        assert_eq!(scalar(&db, "SELECT value FROM sync_state WHERE key = 'member'").await, None);
        // ...while a device that already synced keeps capturing.
        store.set_cursor("peer", 7).await.unwrap();
        exec(&db, "INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES ('todos', '\"t1\"', 'U', 1)").await;
        crate::sync::capture::settle_membership(&db).await.unwrap();
        assert_eq!(outbox(&db).await.len(), 1);
        assert_eq!(scalar(&db, "SELECT value FROM sync_state WHERE key = 'member'").await.as_deref(), Some("1"));
    }

    #[test]
    fn pk_summary_reads_plain_and_composite_keys() {
        assert_eq!(pk_summary(r#""t1""#), "t1");
        assert_eq!(pk_summary(r#"{"a":1,"b":"x"}"#), "a=1, b=x");
        assert_eq!(pk_summary("not json"), "not json");
    }

    #[tokio::test]
    async fn parked_rows_are_listed_and_retried() {
        let db = mem_db().await;
        let store = SqliteSyncStore::new(db.clone(), "dev-a".into());
        let huge = "x".repeat(MAX_OP_BYTES + 1);
        sqlx::query("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', ?, 0, 'x'), ('t2', 'small', 1, 'x')")
            .bind(&huge)
            .execute(&db)
            .await
            .unwrap();
        let page = store.changes("peer", &ChangesReq::new(0)).await.unwrap();
        let served: Vec<Value> = page.ops.iter().map(|o| o.pk.clone()).collect();
        assert_eq!(served, vec![json!("t2")], "too large for a reply: left out, the rest goes");
        assert!(!page.more);
        assert_eq!(store.parked_count().await.unwrap(), 1);

        let list = parked_list(&db, 50).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!((list[0].tbl.as_str(), list[0].pk.as_str(), list[0].reason.as_str()), ("todos", "t1", "too_large"));

        // Retried once it is small enough: served and no longer parked.
        exec(&db, "UPDATE todos SET text = 'trimmed' WHERE id = 't1'").await;
        exec(&db, "DELETE FROM sync_outbox").await;
        assert_eq!(parked_retry(&db).await.unwrap(), 1);
        let again = store.changes("peer", &ChangesReq::new(page.next)).await.unwrap();
        assert_eq!(again.ops.iter().map(|o| o.pk.clone()).collect::<Vec<_>>(), vec![json!("t1")]);
        assert_eq!(store.parked_count().await.unwrap(), 0);
        assert_eq!(parked_retry(&db).await.unwrap(), 0);
    }

    /// The change feed: one entry per row with its latest state (deletes as
    /// tombstones), in change order, local edits and applied rows alike;
    /// paged by count and bytes.
    #[tokio::test]
    async fn the_change_feed_keeps_the_latest_state_per_row() {
        let (db_a, a) = device("a", "/Users/a").await;
        exec(&db_a, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'x', 0, 'x'), ('t2', 'z', 1, 'x')").await;
        exec(&db_a, "UPDATE todos SET text = 'y' WHERE id = 't1'").await;
        exec(&db_a, "DELETE FROM todos WHERE id = 't2'").await;
        let page = a.changes("b", &ChangesReq::new(0)).await.unwrap();
        let got: Vec<(Value, OpKind)> = page.ops.iter().map(|o| (o.pk.clone(), o.kind)).collect();
        assert_eq!(got, vec![(json!("t1"), OpKind::Upsert), (json!("t2"), OpKind::Delete)]);
        assert_eq!(page.ops[0].row.as_ref().unwrap()["text"], "y");
        assert!(outbox(&db_a).await.is_empty(), "stamped");
        assert!(!page.more);
        // Nothing new after `next`.
        let empty = a.changes("b", &ChangesReq::new(page.next)).await.unwrap();
        assert!(empty.ops.is_empty() && empty.next == page.next);

        // A row applied from the other device enters the feed too (for the
        // other device's asks it is its own version: not served back).
        let far = Hlc::new(9_000_000_000_000, 0, "b");
        a.apply_remote(vec![upsert("todos", json!("t3"), far, json!({"id": "t3", "text": "from b", "done": 0, "position": 2, "created_at": "x"}))])
            .await
            .unwrap();
        let to_b = a.changes("b", &ChangesReq::new(page.next)).await.unwrap();
        assert!(to_b.ops.is_empty() && to_b.next > page.next, "skipped, but the cursor moves on");
        let to_c = a.changes("c", &ChangesReq::new(page.next)).await.unwrap();
        assert_eq!(to_c.ops.iter().map(|o| o.pk.clone()).collect::<Vec<_>>(), vec![json!("t3")]);

        // Paging: by count, then by bytes (at least one op per page).
        for i in 0..5 {
            exec(&db_a, &format!("INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n{i}', 't', '{}', {i}, 'x', 'x')", "c".repeat(400))).await;
        }
        let small = ChangesReq { max_ops: 2, ..ChangesReq::new(to_c.next) };
        let p1 = a.changes("c", &small.clone().clamped()).await.unwrap();
        assert_eq!((p1.ops.len(), p1.more), (2, true));
        let tight = ChangesReq { max_bytes: 10, ..ChangesReq::new(p1.next) };
        let p2 = a.changes("c", &tight.clamped()).await.unwrap();
        assert_eq!((p2.ops.len(), p2.more), (1, true));
        let rest = a.changes("c", &ChangesReq::new(p2.next)).await.unwrap();
        assert_eq!((rest.ops.len(), rest.more), (2, false));
    }

    /// Cursors per source device; a selective-sync reset clears them all and
    /// keeps a pull that started before it from writing its cursor back.
    #[tokio::test]
    async fn cursor_resets_hold_until_the_next_pull_starts() {
        let (db, store) = device("b", "/Users/b").await;
        store.set_cursor("a", 42).await.unwrap();
        assert_eq!(store.cursor_start("a").await.unwrap(), 42);
        crate::sync::prefs::set_categories_off(&db, &["notes".to_string()]).await.unwrap();
        assert_eq!(store.cursor("a").await.unwrap(), 42, "turning something off keeps the cursor");
        crate::sync::prefs::set_categories_off(&db, &[]).await.unwrap();
        assert_eq!(store.cursor("a").await.unwrap(), 0);
        store.set_cursor("a", 99).await.unwrap();
        assert_eq!(store.cursor("a").await.unwrap(), 0, "a pull from before the reset does not move it");
        assert_eq!(store.cursor_start("a").await.unwrap(), 0);
        store.set_cursor("a", 7).await.unwrap();
        assert_eq!(store.cursor("a").await.unwrap(), 7);
        let (off, projects) = store.ask_filter().await.unwrap();
        assert!(off.is_empty() && projects.is_empty());
    }
}
