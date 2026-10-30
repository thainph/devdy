//! Applying remote ops: one transaction, capture triggers muted by the guard,
//! last-writer-wins by HLC, tombstones for deletes, FK-safe out-of-order apply.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection};

use super::clock::{wall_ms, Hlc, HlcClock};
use super::codec::{decode_row, delete_row, pk_values, read_row, upsert_row, PathCtx};
use super::op::{canonical_json, Op, OpKind};
use super::schema::{self, pk_sql, q, TableSpec};
use super::store::ApplyReport;
use super::SCHEMA_VERSION;
use crate::db::Db;

type RowKey = (String, String);

#[derive(Debug, PartialEq)]
enum Outcome {
    Applied,
    Skipped,
    /// Applied, and the losing local version was kept as a copy.
    Conflict,
}

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

/// Whether a filtered table (settings) accepts this op's row.
fn accepted(spec: &TableSpec, op: &Op) -> bool {
    let Some(f) = spec.filter else { return true };
    let from_row = op.row.as_ref().and_then(|r| r.get(f.col)).and_then(Value::as_str);
    let from_pk = match (spec.pk, &op.pk) {
        ([only], Value::String(s)) if *only == f.col => Some(s.as_str()),
        (_, Value::Object(m)) => m.get(f.col).and_then(Value::as_str),
        _ => None,
    };
    spec.accepts(from_row.or(from_pk))
}

pub async fn apply_ops(
    db: &Db,
    paths: &PathCtx,
    clock: &mut HlcClock,
    ops: Vec<Op>,
) -> Result<ApplyReport, String> {
    // Ops deferred by earlier calls are retried first, so a newer incoming op on
    // the same row still wins by HLC.
    let mut all: Vec<Op> = sqlx::query_scalar::<_, String>("SELECT op FROM sync_deferred")
        .fetch_all(db)
        .await
        .map_err(err)?
        .into_iter()
        .filter_map(|s| serde_json::from_str(&s).ok())
        .collect();
    all.extend(ops);

    let mut base = ApplyReport::default();
    let mut to_defer: Vec<(Op, String)> = Vec::new();
    let mut candidates: Vec<Op> = Vec::new();
    for op in all {
        clock.observe(&op.hlc);
        if op.schema > SCHEMA_VERSION {
            to_defer.push((op, format!("schema newer than local ({SCHEMA_VERSION})")));
            continue;
        }
        let Some(spec) = schema::spec(&op.tbl) else {
            to_defer.push((op, "unknown table".into()));
            continue;
        };
        if !accepted(spec, &op) {
            // A machine-local setting: never applied.
            base.skipped_older += 1;
            continue;
        }
        candidates.push(op);
    }
    // Parents first for upserts, children first for deletes.
    candidates.sort_by_key(|op| {
        let level = schema::spec(&op.tbl).map(|s| s.level()).unwrap_or(usize::MAX);
        match op.kind {
            OpKind::Upsert => (0, level),
            OpKind::Delete => (1, usize::MAX - level),
        }
    });

    let mut fk_blocked: HashSet<RowKey> = HashSet::new();
    let mut preexisting: Option<HashSet<RowKey>> = None;
    loop {
        let mut tx = db.begin().await.map_err(err)?;
        sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await.map_err(err)?;
        sqlx::query("UPDATE sync_apply_guard SET active = 1 WHERE id = 1")
            .execute(&mut *tx)
            .await
            .map_err(err)?;

        let mut report = base.clone();
        let mut failed: Vec<(Op, String)> = Vec::new();
        let mut applied: HashSet<RowKey> = HashSet::new();
        let mut touched: BTreeSet<&'static str> = BTreeSet::new();
        for op in &candidates {
            let key = (op.tbl.clone(), op.pk_key());
            if fk_blocked.contains(&key) {
                continue;
            }
            sqlx::query("SAVEPOINT sync_op").execute(&mut *tx).await.map_err(err)?;
            match apply_one(&mut tx, paths, op).await {
                Ok(outcome) => {
                    sqlx::query("RELEASE sync_op").execute(&mut *tx).await.map_err(err)?;
                    match outcome {
                        Outcome::Skipped => report.skipped_older += 1,
                        Outcome::Applied | Outcome::Conflict => {
                            report.applied += 1;
                            if outcome == Outcome::Conflict {
                                report.conflicts += 1;
                            }
                            if let Some(spec) = schema::spec(&op.tbl) {
                                touched.insert(spec.name);
                            }
                            applied.insert(key);
                        }
                    }
                }
                Err(e) => {
                    sqlx::query("ROLLBACK TO sync_op").execute(&mut *tx).await.map_err(err)?;
                    sqlx::query("RELEASE sync_op").execute(&mut *tx).await.map_err(err)?;
                    failed.push((op.clone(), e));
                }
            }
        }

        // Rows whose parent is missing would fail the commit: drop those ops
        // (whole transaction rolled back, so an update leaves the old row in
        // place) and keep them for a later retry.
        let violating = fk_violations(&mut tx, &touched).await?;
        let mut blocked: Vec<RowKey> = violating.intersection(&applied).cloned().collect();
        if let Some(pre) = &preexisting {
            blocked.retain(|k| !pre.contains(k));
        }
        if !blocked.is_empty() {
            tx.rollback().await.map_err(err)?;
            if preexisting.is_none() {
                // Violations that were already there (sidecar-mcp writes without
                // FK enforcement) are not this batch's fault.
                let mut conn = db.acquire().await.map_err(err)?;
                let pre = fk_violations(&mut conn, &touched).await?;
                blocked.retain(|k| !pre.contains(k));
                preexisting = Some(pre);
            }
            fk_blocked.extend(blocked);
            continue;
        }

        for op in candidates.iter().filter(|op| fk_blocked.contains(&(op.tbl.clone(), op.pk_key()))) {
            to_defer.push((op.clone(), "missing parent row".into()));
        }
        to_defer.extend(failed);
        report.deferred = write_deferred(&mut tx, std::mem::take(&mut to_defer)).await?;

        sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1")
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        clock.save(&mut tx).await?;
        tx.commit().await.map_err(err)?;
        return Ok(report);
    }
}

/// Replaces `sync_deferred` with `ops` (one per row, the newest wins).
async fn write_deferred(conn: &mut SqliteConnection, ops: Vec<(Op, String)>) -> Result<usize, String> {
    let mut by_row: HashMap<RowKey, (Op, String)> = HashMap::new();
    for (op, reason) in ops {
        let key = (op.tbl.clone(), op.pk_key());
        match by_row.get(&key) {
            Some((kept, _)) if kept.hlc >= op.hlc => {}
            _ => {
                by_row.insert(key, (op, reason));
            }
        }
    }
    sqlx::query("DELETE FROM sync_deferred").execute(&mut *conn).await.map_err(err)?;
    let now = wall_ms() as i64;
    for ((tbl, pk), (op, reason)) in &by_row {
        let json = serde_json::to_string(op).map_err(|e| e.to_string())?;
        sqlx::query(
            "INSERT INTO sync_deferred (tbl, pk, hlc, op, reason, deferred_at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(tbl)
        .bind(pk)
        .bind(op.hlc.to_string())
        .bind(json)
        .bind(reason)
        .bind(now)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }
    Ok(by_row.len())
}

/// `(table, canonical pk)` of every row of `tables` whose FK parent is missing.
async fn fk_violations(
    conn: &mut SqliteConnection,
    tables: &BTreeSet<&'static str>,
) -> Result<HashSet<RowKey>, String> {
    let mut out = HashSet::new();
    for tbl in tables {
        let Some(spec) = schema::spec(tbl) else { continue };
        let rows = sqlx::query(&format!("PRAGMA foreign_key_check({})", q(tbl)))
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?;
        let rowids: BTreeSet<i64> = rows.iter().filter_map(|r| r.try_get::<Option<i64>, _>(1).ok().flatten()).collect();
        let sql = format!("SELECT {} FROM {} WHERE rowid = ?", pk_sql(spec, &q(tbl)), q(tbl));
        for rowid in rowids {
            let pk: Option<String> = sqlx::query_scalar(&sql)
                .bind(rowid)
                .fetch_optional(&mut *conn)
                .await
                .map_err(err)?;
            if let Some(pk) = pk {
                out.insert((tbl.to_string(), pk));
            }
        }
    }
    Ok(out)
}

async fn apply_one(conn: &mut SqliteConnection, paths: &PathCtx, op: &Op) -> Result<Outcome, String> {
    let spec = schema::spec(&op.tbl).ok_or("unknown table")?;
    let key = op.pk_key();
    let known: Option<String> = sqlx::query_scalar("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(&key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if let Some(h) = known.and_then(|s| s.parse::<Hlc>().ok()) {
        if h >= op.hlc {
            return Ok(Outcome::Skipped);
        }
    }

    let mut outcome = Outcome::Applied;
    let local_changed: Option<i64> =
        sqlx::query_scalar("SELECT MAX(changed_at) FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(&key)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
    if let Some(changed) = local_changed {
        if spec.name == "notes" && op.kind == OpKind::Upsert && keep_note_conflict_copy(conn, op).await? {
            outcome = Outcome::Conflict;
        } else if changed.max(0) as u64 >= op.hlc.millis {
            // The unpushed local change is newer; it will be pushed with an HLC
            // above this one and win on the other side too.
            return Ok(Outcome::Skipped);
        }
        // The local change lost (or survives as a copy): never push it.
        sqlx::query("DELETE FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(&key)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }

    match op.kind {
        OpKind::Upsert => {
            let row = op.row.as_ref().ok_or("upsert without row")?;
            let mut local = decode_row(paths, spec, row);
            // The op's pk is authoritative.
            for (col, v) in spec.pk.iter().zip(pk_values(spec, &op.pk)?) {
                local.insert((*col).to_string(), v);
            }
            upsert_row(conn, spec, &local).await?;
        }
        OpKind::Delete => delete_row(conn, spec, &op.pk).await?,
    }
    sqlx::query(
        "INSERT INTO sync_rows (tbl, pk, hlc, deleted) VALUES (?, ?, ?, ?) \
         ON CONFLICT(tbl, pk) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted",
    )
    .bind(spec.name)
    .bind(&key)
    .bind(op.hlc.to_string())
    .bind((op.kind == OpKind::Delete) as i64)
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok(outcome)
}

/// Both sides edited this note since the last sync: the remote version becomes
/// the row, the local one is kept as a new note "<title> (conflict copy)"
/// (queued for push explicitly, since the guard mutes the triggers). Returns
/// false when there is nothing to keep (no local row, same content).
async fn keep_note_conflict_copy(conn: &mut SqliteConnection, op: &Op) -> Result<bool, String> {
    let spec = schema::spec("notes").ok_or("notes not synced")?;
    let Some(local) = read_row(conn, spec, &op.pk).await? else { return Ok(false) };
    let remote_content = op.row.as_ref().and_then(|r| r.get("content"));
    if remote_content == local.get("content") {
        return Ok(false);
    }
    let title = local.get("title").and_then(Value::as_str).unwrap_or("");
    let id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let mut copy: Map<String, Value> = local.clone();
    copy.insert("id".into(), Value::String(id.clone()));
    copy.insert("title".into(), Value::String(format!("{title} (conflict copy)")));
    copy.insert("created_at".into(), Value::String(now.clone()));
    copy.insert("updated_at".into(), Value::String(now));
    upsert_row(conn, spec, &copy).await?;
    sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES ('notes', ?, 'I', ?)")
        .bind(canonical_json(&Value::String(id)))
        .bind(wall_ms() as i64)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(true)
}
