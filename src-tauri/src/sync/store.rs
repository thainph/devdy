//! Core <-> client boundary (design §9.3). The sync engine only talks to
//! `SyncStore`; `SqliteSyncStore` is the real implementation over Devdy's DB.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::{Mutex, MutexGuard};

use super::clock::{wall_ms, HlcClock};
use super::codec::{encode_row, read_row, PathCtx};
use super::op::{Op, OpKind, OP_VERSION};
use super::events::{Notice, Notifier};
use super::{apply, files, schema, secrets, SCHEMA_VERSION};
use crate::db::Db;

const CURSOR_KEY: &str = "pull_cursor";
/// A pulled file/secret op that keeps failing is dropped after this many tries.
const MAX_EXTRA_ATTEMPTS: i64 = 50;

/// Ops whose `tbl` is not a DB table (§9b): routed to `files` / `secrets`,
/// never to `apply`.
pub fn is_extra_tbl(tbl: &str) -> bool {
    tbl == files::TBL || tbl == secrets::TBL
}

/// The relay's sealed blob store as file sync uses it. The engine implements
/// it over its connection (sealing with the group key); tests use a fake.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait BlobIo: Send + Sync {
    /// Seal and upload one plaintext chunk; returns the sealed blob's sha.
    async fn put(&self, plaintext: &[u8]) -> Result<String, String>;
    /// Download, verify and open one blob.
    async fn get(&self, blob_sha: &str) -> Result<Vec<u8>, String>;
}

// async_trait's generated futures trip clippy::double_must_use (as elsewhere).
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait SyncStore: Send + Sync {
    async fn pending(&self, limit: usize) -> Result<Vec<(i64 /*outbox seq*/, Op)>, String>;
    async fn mark_pushed(&self, up_to_outbox_seq: i64) -> Result<(), String>;
    async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String>;
    async fn cursor(&self) -> Result<u64, String>; // last pulled relay seq
    async fn set_cursor(&self, seq: u64) -> Result<(), String>;

    /// Scan the non-DB sources that are due (files, Keychain), upload file
    /// chunks and queue `file` / `secret` ops into the outbox. Called before
    /// every push and on the outbox poll; cheap when nothing is due.
    async fn stage(&self, _blobs: &dyn BlobIo) -> Result<(), String> {
        Ok(())
    }

    /// Relay blobs this device stopped using whose release is due.
    async fn releasable_blobs(&self, _limit: usize) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }

    /// The relay took the release of `shas` (or never will): forget them.
    async fn blobs_released(&self, _shas: &[String]) -> Result<(), String> {
        Ok(())
    }

    /// Apply pulled `file` / `secret` ops (and retry earlier failed ones).
    async fn apply_remote_extra(&self, ops: Vec<Op>, _blobs: &dyn BlobIo) -> Result<ApplyReport, String> {
        Ok(ApplyReport { skipped_older: ops.len(), ..Default::default() })
    }

    /// The relay will never take the pending op at `outbox_seq` (too large,
    /// malformed): drop that row's outbox entries up to it and record why, so
    /// it stops blocking the push queue.
    async fn park(&self, _outbox_seq: i64, _reason: &str) -> Result<(), String> {
        Err("this store cannot park ops".into())
    }

    /// Rows whose latest change was parked.
    async fn parked_count(&self) -> Result<u64, String> {
        Ok(0)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    pub applied: usize,
    /// Older than what we have, beaten by a newer unpushed local change, or a
    /// machine-local setting.
    pub skipped_older: usize,
    pub conflicts: usize,
    /// Ops parked in `sync_deferred` after this call (retried on the next one).
    pub deferred: usize,
}

pub struct SqliteSyncStore {
    db: Db,
    device_id: String,
    paths: PathCtx,
    /// Loaded lazily from `sync_state`; the lock also serialises pending/apply.
    clock: Mutex<Option<HlcClock>>,
    /// Keychain access for `secret` ops; `None` = secrets are not synced.
    secrets: Option<secrets::Backend>,
    file_timing: files::Timing,
    scan: Mutex<files::ScanState>,
    /// Tells the UI what was applied (`None` in tests / bootstrap).
    notify: Option<Notifier>,
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
            file_timing: files::Timing::default(),
            scan: Mutex::new(files::ScanState::default()),
            notify: None,
        }
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

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_file_timing(mut self, timing: files::Timing) -> Self {
        self.file_timing = timing;
        self
    }

    pub fn with_paths(mut self, paths: PathCtx) -> Self {
        self.paths = paths;
        self
    }

    /// `paths` plus the current project path map (it changes at bootstrap and
    /// when the user maps a project by hand).
    async fn paths_now(&self) -> Result<PathCtx, String> {
        let map = super::bootstrap::load_project_map(&self.db).await?;
        Ok(self.paths.clone().with_project_map(map))
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
    /// Outbox entries coalesced per row (the op carries the CURRENT row, or a
    /// Delete when it is gone), each with a fresh HLC no older than the change.
    async fn pending(&self, limit: usize) -> Result<Vec<(i64, Op)>, String> {
        let paths = self.paths_now().await?;
        let mut guard = self.clock().await?;
        let mut clock = guard.clone().ok_or("clock not loaded")?;
        let mut tx = self.db.begin().await.map_err(err)?;
        let groups: Vec<(String, String, i64, i64)> = sqlx::query_as(
            "SELECT tbl, pk, MAX(seq) AS s, MAX(changed_at) FROM sync_outbox \
             GROUP BY tbl, pk ORDER BY s LIMIT ?",
        )
        .bind(limit as i64)
        .fetch_all(&mut *tx)
        .await
        .map_err(err)?;

        let mut out = Vec::with_capacity(groups.len());
        let with_secrets = self.secrets.is_some() && secrets::enabled(&mut tx).await?;
        // Read once per call, and only when a secret op is waiting.
        let mut keychain: Option<std::collections::BTreeMap<secrets::SecretId, String>> = None;
        for (tbl, pk_text, seq, changed_at) in groups {
            let pk: Option<Value> = serde_json::from_str(&pk_text).ok();
            let entry = match (tbl.as_str(), pk) {
                (files::TBL, Some(pk)) => files::pending_row(&mut tx, &pk, with_secrets).await?.map(|e| (pk, e)),
                (secrets::TBL, Some(pk)) if with_secrets => {
                    if keychain.is_none() {
                        let backend = self.secrets.as_ref().ok_or("no keychain")?;
                        keychain = Some(backend.list()?);
                    }
                    keychain.as_ref().and_then(|items| secrets::pending_row(items, &pk)).map(|e| (pk, e))
                }
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

    async fn stage(&self, blobs: &dyn BlobIo) -> Result<(), String> {
        let mut state = self.scan.lock().await;
        let Some(mode) = state.due(&self.file_timing, files::has_kicks()) else { return Ok(()) };
        let with_secrets = match &self.secrets {
            Some(_) => {
                let mut conn = self.db.acquire().await.map_err(err)?;
                secrets::enabled(&mut conn).await?
            }
            None => false,
        };
        let paths = self.paths_now().await?;
        files::stage(&self.db, &paths, blobs, &mut state, mode, with_secrets).await?;
        drop(state);
        if mode == files::ScanMode::Full {
            if let (Some(backend), true) = (&self.secrets, with_secrets) {
                if let Err(e) = secrets::scan(&self.db, backend.as_ref()).await {
                    tracing::warn!(event = "sync_secret_scan_failed", error = %e);
                }
            }
            // Retry pulled ops that failed earlier even when nothing new arrives.
            self.apply_remote_extra(Vec::new(), blobs).await?;
        }
        Ok(())
    }

    async fn apply_remote_extra(&self, ops: Vec<Op>, blobs: &dyn BlobIo) -> Result<ApplyReport, String> {
        let mut guard = self.clock().await?;
        let mut clock = guard.clone().ok_or("clock not loaded")?;
        let paths = self.paths_now().await?;
        let parked: Vec<(String, i64)> = sqlx::query_as("SELECT op, attempts FROM sync_extra_deferred")
            .fetch_all(&self.db)
            .await
            .map_err(err)?;
        // Newest op per (tbl, pk); a parked op keeps its attempt count.
        let mut latest: std::collections::BTreeMap<(String, String), (Op, i64)> = Default::default();
        let incoming = ops.into_iter().map(|o| (o, 0));
        let parked = parked.into_iter().filter_map(|(s, n)| Some((serde_json::from_str::<Op>(&s).ok()?, n)));
        for (op, attempts) in parked.chain(incoming) {
            let key = (op.tbl.clone(), op.pk_key());
            match latest.get(&key) {
                Some((kept, _)) if kept.hlc >= op.hlc => {}
                _ => {
                    latest.insert(key, (op, attempts));
                }
            }
        }
        let mut report = ApplyReport::default();
        let mut failed: Vec<(Op, i64, String)> = Vec::new();
        let mut grown_runs = std::collections::BTreeSet::new();
        for (_, (op, attempts)) in latest {
            clock.observe(&op.hlc);
            if op.schema > SCHEMA_VERSION {
                failed.push((op, attempts, format!("schema newer than local ({SCHEMA_VERSION})")));
                continue;
            }
            let result = match op.tbl.as_str() {
                files::TBL => files::apply_op(&self.db, &paths, blobs, &op).await.map(|o| match o {
                    files::Outcome::Applied => (1, 0, 0),
                    files::Outcome::Skipped => (0, 1, 0),
                    files::Outcome::Conflict => (1, 0, 1),
                }),
                secrets::TBL => match &self.secrets {
                    Some(b) => secrets::apply_op(&self.db, b.as_ref(), &op).await.map(|o| match o {
                        secrets::Outcome::Applied => (1, 0, 0),
                        secrets::Outcome::Skipped => (0, 1, 0),
                    }),
                    None => Ok((0, 1, 0)),
                },
                _ => Ok((0, 1, 0)),
            };
            if matches!(result, Ok((1, _, 0))) && self.notify.is_some() && op.tbl == files::TBL {
                if let Some(run) = files::run_of_op(&self.db, &paths, &op).await {
                    grown_runs.insert(run);
                }
            }
            match result {
                Ok((a, s, c)) => {
                    report.applied += a;
                    report.skipped_older += s;
                    report.conflicts += c;
                }
                Err(e) => failed.push((op, attempts + 1, e)),
            }
        }
        let mut tx = self.db.begin().await.map_err(err)?;
        sqlx::query("DELETE FROM sync_extra_deferred").execute(&mut *tx).await.map_err(err)?;
        for (op, attempts, reason) in failed {
            if attempts > MAX_EXTRA_ATTEMPTS {
                tracing::warn!(event = "sync_extra_op_dropped", tbl = %op.tbl, reason = %reason);
                continue;
            }
            sqlx::query(
                "INSERT INTO sync_extra_deferred (tbl, pk, op, reason, attempts, deferred_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&op.tbl)
            .bind(op.pk_key())
            .bind(serde_json::to_string(&op).map_err(|e| e.to_string())?)
            .bind(reason)
            .bind(attempts)
            .bind(wall_ms() as i64)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
            report.deferred += 1;
        }
        clock.save(&mut tx).await?;
        tx.commit().await.map_err(err)?;
        *guard = Some(clock);
        for run in grown_runs {
            self.notice(Notice::RunLogUpdated(run));
        }
        Ok(report)
    }

    async fn releasable_blobs(&self, limit: usize) -> Result<Vec<String>, String> {
        files::releasable_blobs(&self.db, limit).await
    }

    async fn blobs_released(&self, shas: &[String]) -> Result<(), String> {
        files::forget_released(&self.db, shas).await
    }

    async fn mark_pushed(&self, up_to_outbox_seq: i64) -> Result<(), String> {
        let mut tx = self.db.begin().await.map_err(err)?;
        // A newer change of a parked row went out: it is no longer stuck.
        sqlx::query(
            "DELETE FROM sync_parked WHERE EXISTS (SELECT 1 FROM sync_outbox o \
             WHERE o.tbl = sync_parked.tbl AND o.pk = sync_parked.pk AND o.seq <= ?)",
        )
        .bind(up_to_outbox_seq)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
        sqlx::query("DELETE FROM sync_outbox WHERE seq <= ?")
            .bind(up_to_outbox_seq)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        tx.commit().await.map_err(err)
    }

    async fn park(&self, outbox_seq: i64, reason: &str) -> Result<(), String> {
        let mut tx = self.db.begin().await.map_err(err)?;
        let row: Option<(String, String)> = sqlx::query_as("SELECT tbl, pk FROM sync_outbox WHERE seq = ?")
            .bind(outbox_seq)
            .fetch_optional(&mut *tx)
            .await
            .map_err(err)?;
        let Some((tbl, pk)) = row else { return Ok(()) };
        sqlx::query("INSERT OR REPLACE INTO sync_parked (tbl, pk, reason, parked_at) VALUES (?, ?, ?, ?)")
            .bind(&tbl)
            .bind(&pk)
            .bind(reason)
            .bind(wall_ms() as i64)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        sqlx::query("DELETE FROM sync_outbox WHERE tbl = ? AND pk = ? AND seq <= ?")
            .bind(&tbl)
            .bind(&pk)
            .bind(outbox_seq)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        tx.commit().await.map_err(err)
    }

    async fn parked_count(&self) -> Result<u64, String> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_parked")
            .fetch_one(&self.db)
            .await
            .map_err(err)?;
        Ok(n as u64)
    }

    async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String> {
        let mut guard = self.clock().await?;
        let mut clock = guard.clone().ok_or("clock not loaded")?;
        let paths = self.paths_now().await?;
        let tables: std::collections::BTreeSet<String> = ops.iter().map(|o| o.tbl.clone()).collect();
        let report = apply::apply_ops(&self.db, &paths, &mut clock, ops).await?;
        *guard = Some(clock);
        if report.applied > 0 {
            self.notice(Notice::RowsApplied(tables.into_iter().collect()));
        }
        Ok(report)
    }

    async fn cursor(&self) -> Result<u64, String> {
        let v: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
            .bind(CURSOR_KEY)
            .fetch_optional(&self.db)
            .await
            .map_err(err)?;
        Ok(v.and_then(|s| s.parse().ok()).unwrap_or(0))
    }

    async fn set_cursor(&self, seq: u64) -> Result<(), String> {
        sqlx::query(
            "INSERT INTO sync_state (key, value) VALUES (?, ?) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        )
        .bind(CURSOR_KEY)
        .bind(seq.to_string())
        .execute(&self.db)
        .await
        .map_err(err)?;
        Ok(())
    }
}

impl SqliteSyncStore {
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
                if spec.name == "notes" {
                    apply::add_note_base(conn, pk_text, &mut wire).await?;
                }
                (OpKind::Upsert, Some(wire))
            }
            None => (OpKind::Delete, None),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::clock::Hlc;
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
        assert_eq!(reopened.cursor().await.unwrap(), 0);
        reopened.set_cursor(42).await.unwrap();
        assert_eq!(store.cursor().await.unwrap(), 42);
    }
}
