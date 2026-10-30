//! Applying remote ops: one transaction, capture triggers muted by the guard,
//! last-writer-wins by HLC, tombstones for deletes, FK-safe out-of-order apply.
//!
//! Two devices can create "the same thing" independently after pairing (a
//! skill with the same name, the same imported CLI session, the same Pulse
//! watch): an incoming row whose pk is new here but that clashes with a local
//! row on another UNIQUE key is reconciled the same way on every device,
//! whatever the arrival order: the row with the lower id survives. A local
//! loser is re-keyed to the incoming id (children, outbox, Keychain items
//! follow; the moved rows are queued for push so the relay log drops the old
//! id); an incoming loser is merged into the local row. Either way the content
//! is settled by last-writer-wins, and the decision is kept as an alias
//! (`sync_state` `alias:<tbl>:<old pk>`) that redirects later upserts and
//! references carrying the old id. The same rule settles a row that already
//! exists here and is renamed into a clash: the loser is folded into the
//! survivor (children move, the loser row goes).
//!
//! The losing id is pushed as an alias op: a Delete whose row is
//! `{"_alias_of": <surviving pk>}` (same relay key as the old id's ops, so it
//! supersedes them). A receiver re-keys its own row under the old id to the
//! survivor (or folds it into the survivor it already has) instead of
//! deleting it, so children are never cascaded away, whatever order or pull
//! page the re-keyed rows arrive in. A v1 receiver reads it as a plain Delete.

use std::collections::{BTreeSet, HashMap, HashSet};

use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection};

use super::bootstrap::{rekey, rekey_outbox, RekeyOpts, REFS};
use super::clock::{wall_ms, Hlc, HlcClock};
use super::codec::{bind_value, decode_row, delete_row, pk_values, read_row, upsert_row, PathCtx};
use super::op::{canonical_json, Op, OpKind};
use super::schema::{self, pk_sql, q, TableSpec};
use super::store::ApplyReport;
use super::SCHEMA_VERSION;
use crate::db::Db;

type RowKey = (String, String);

/// `(table, canonical old pk)` -> surviving pk.
type Aliases = HashMap<RowKey, Value>;

const ALIAS_PREFIX: &str = "alias:";

/// Row field of an alias op (see the module docs).
pub const ALIAS_FIELD: &str = "_alias_of";

/// The surviving pk an alias op points at.
pub fn alias_of(op: &Op) -> Option<&Value> {
    if op.kind != OpKind::Delete {
        return None;
    }
    op.row.as_ref()?.get(ALIAS_FIELD).filter(|v| !v.is_null())
}

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
    let (mut report, new_alias) = apply_batch(db, paths, clock, ops).await?;
    if new_alias && report.deferred > 0 {
        // A row held back for a parent under an id this batch just re-keyed
        // can go now; on a quiet group nothing else would retry it soon.
        let (again, _) = apply_batch(db, paths, clock, Vec::new()).await?;
        report.applied += again.applied;
        report.skipped_older += again.skipped_older;
        report.conflicts += again.conflicts;
        report.deferred = again.deferred;
    }
    Ok(report)
}

/// One pass over `ops` and the deferred ones; also tells whether it recorded
/// a new alias.
async fn apply_batch(
    db: &Db,
    paths: &PathCtx,
    clock: &mut HlcClock,
    ops: Vec<Op>,
) -> Result<(ApplyReport, bool), String> {
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
    // Parents first for upserts, then aliases (parents first, so their
    // children move before any delete), children first for deletes.
    candidates.sort_by_key(|op| {
        let level = schema::spec(&op.tbl).map(|s| s.level()).unwrap_or(usize::MAX);
        match op.kind {
            OpKind::Upsert => (0, level),
            OpKind::Delete if alias_of(op).is_some() => (1, level),
            OpKind::Delete => (2, usize::MAX - level),
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
        // Row actually written (an op may be redirected) -> the op's own key.
        let mut applied: HashMap<RowKey, RowKey> = HashMap::new();
        let mut touched: BTreeSet<&'static str> = BTreeSet::new();
        let mut aliases = load_aliases(&mut tx).await?;
        let aliases_before = aliases.len();
        for op in &candidates {
            let key = (op.tbl.clone(), op.pk_key());
            if fk_blocked.contains(&key) {
                continue;
            }
            sqlx::query("SAVEPOINT sync_op").execute(&mut *tx).await.map_err(err)?;
            match apply_one(&mut tx, paths, op, &mut aliases).await {
                Ok((outcome, row_key)) => {
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
                            applied.insert(row_key, key);
                        }
                    }
                }
                Err(e) => {
                    sqlx::query("ROLLBACK TO sync_op").execute(&mut *tx).await.map_err(err)?;
                    sqlx::query("RELEASE sync_op").execute(&mut *tx).await.map_err(err)?;
                    // Aliases the failed op recorded were rolled back too.
                    aliases = load_aliases(&mut tx).await?;
                    failed.push((op.clone(), e));
                }
            }
        }

        // Rows whose parent is missing would fail the commit: drop those ops
        // (whole transaction rolled back, so an update leaves the old row in
        // place) and keep them for a later retry.
        let violating = fk_violations(&mut tx, &touched).await?;
        let mut blocked: Vec<RowKey> = violating.into_iter().filter(|k| applied.contains_key(k)).collect();
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
            fk_blocked.extend(blocked.iter().filter_map(|k| applied.get(k).cloned()));
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
        let new_alias = aliases.len() > aliases_before;
        tx.commit().await.map_err(err)?;
        return Ok((report, new_alias));
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

/// Apply one op; returns the outcome and the row it wrote (an upsert may be
/// redirected to another id, see the module docs).
async fn apply_one(
    conn: &mut SqliteConnection,
    paths: &PathCtx,
    op: &Op,
    aliases: &mut Aliases,
) -> Result<(Outcome, RowKey), String> {
    let spec = schema::spec(&op.tbl).ok_or("unknown table")?;
    if op.kind == OpKind::Delete && spec.pk.len() == 1 {
        // A plain Delete of an id we know was re-keyed (from a v1 sender) is
        // the same thing as its alias op.
        let target = alias_of(op).cloned().or_else(|| aliases.get(&(spec.name.to_string(), op.pk_key())).cloned());
        if let Some(target) = target {
            return apply_alias(conn, spec, op, target, aliases).await;
        }
    }
    let mut op = op.clone();
    let mut merged = false;
    let mut redirected = false;
    if op.kind == OpKind::Upsert {
        let sent_as = op.pk_key();
        follow_aliases(spec, &mut op, aliases);
        redirected = op.pk_key() != sent_as;
        merged = reconcile_unique(conn, paths, spec, &mut op, aliases).await?;
    }
    let key = op.pk_key();
    let outcome = apply_row(conn, paths, spec, &op, &key).await?;
    // The sender still has its row under the losing id: push ours (the merged
    // result) so it re-keys too, whichever version won here. An upsert of a
    // losing id that wins here also replaced whatever the survivor had queued
    // (the merge's own push included): the relay only learns the result if
    // it is queued again.
    if merged || (redirected && outcome != Outcome::Skipped) {
        sqlx::query(
            "INSERT INTO sync_outbox (tbl, pk, op, changed_at) SELECT ?, ?, 'U', 0 \
             WHERE NOT EXISTS (SELECT 1 FROM sync_outbox WHERE tbl = ? AND pk = ?)",
        )
        .bind(spec.name)
        .bind(&key)
        .bind(spec.name)
        .bind(&key)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    }
    Ok((outcome, (spec.name.to_string(), key)))
}

async fn apply_row(
    conn: &mut SqliteConnection,
    paths: &PathCtx,
    spec: &'static TableSpec,
    op: &Op,
    key: &str,
) -> Result<Outcome, String> {
    let key = key.to_string();
    // A run's ownership follows its epoch, not the HLC.
    let owner = super::sessions::owner_merge(conn, spec, op).await?;
    let known: Option<String> = sqlx::query_scalar("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(&key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if let Some(h) = known.and_then(|s| s.parse::<Hlc>().ok()) {
        if h >= op.hlc {
            return Ok(if owner.apply_skipped(conn, op).await? { Outcome::Applied } else { Outcome::Skipped });
        }
    }

    let local_changed: Option<i64> =
        sqlx::query_scalar("SELECT MAX(changed_at) FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(&key)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
    // An unpushed local change newer than the op: it will be pushed with an
    // HLC above this one and win on the other side too.
    let local_newer = local_changed.is_some_and(|c| c.max(0) as u64 >= op.hlc.millis);
    let merge = if spec.name == "notes" && op.kind == OpKind::Upsert {
        note_merge(conn, op).await?
    } else {
        NoteMerge::Lww
    };
    let mut outcome = Outcome::Applied;
    // The remote row is applied but the local note body is kept (and pushed
    // again), because only this side changed it.
    let mut local_body: Option<Value> = None;
    match merge {
        NoteMerge::Copied => outcome = Outcome::Conflict,
        NoteMerge::Lww | NoteMerge::KeepLocal(_) if local_newer => {
            return Ok(if owner.apply_skipped(conn, op).await? { Outcome::Applied } else { Outcome::Skipped });
        }
        NoteMerge::TakeRemote if local_newer => {
            // The local row wins except for the body only the remote changed.
            take_remote_body(conn, op).await?;
            return Ok(Outcome::Applied);
        }
        NoteMerge::KeepLocal(body) => {
            local_body = Some(body);
            sqlx::query(
                "INSERT INTO sync_outbox (tbl, pk, op, changed_at) SELECT 'notes', ?, 'U', ? \
                 WHERE NOT EXISTS (SELECT 1 FROM sync_outbox WHERE tbl = 'notes' AND pk = ?)",
            )
            .bind(&key)
            .bind(wall_ms() as i64)
            .bind(&key)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
        }
        NoteMerge::Lww | NoteMerge::TakeRemote => {}
    }
    if local_changed.is_some() && local_body.is_none() && !owner.keeps_local() {
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
            if let Some(body) = local_body {
                local.insert("content".into(), body);
            }
            owner.overlay(&mut local);
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
    if spec.name == "notes" && op.kind == OpKind::Upsert {
        set_note_base(conn, &key, op.row.as_ref().and_then(|r| r.get("content"))).await?;
    }
    Ok(outcome)
}

// ---- Unique-key reconcile ---------------------------------------------------------

async fn load_aliases(conn: &mut SqliteConnection) -> Result<Aliases, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT key, value FROM sync_state WHERE substr(key, 1, ?) = ?",
    )
    .bind(ALIAS_PREFIX.len() as i64)
    .bind(ALIAS_PREFIX)
    .fetch_all(&mut *conn)
    .await
    .map_err(err)?;
    Ok(rows
        .into_iter()
        .filter_map(|(k, v)| {
            let (tbl, pk) = k.strip_prefix(ALIAS_PREFIX)?.split_once(':')?;
            Some(((tbl.to_string(), pk.to_string()), serde_json::from_str(&v).ok()?))
        })
        .collect())
}

async fn add_alias(conn: &mut SqliteConnection, aliases: &mut Aliases, tbl: &str, old: &str, new: &Value) -> Result<(), String> {
    sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
        .bind(format!("{ALIAS_PREFIX}{tbl}:{old}"))
        .bind(canonical_json(new))
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    aliases.insert((tbl.to_string(), old.to_string()), new.clone());
    Ok(())
}

/// Where a re-keyed / merged id of `tbl` lives now (chained), from `sync_state`.
pub async fn alias_target(conn: &mut SqliteConnection, tbl: &str, pk: &str) -> Result<Option<Value>, String> {
    let mut cur: Option<Value> = None;
    let mut key = pk.to_string();
    for _ in 0..8 {
        let next: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
            .bind(format!("{ALIAS_PREFIX}{tbl}:{key}"))
            .fetch_optional(&mut *conn)
            .await
            .map_err(err)?;
        let Some(v) = next.and_then(|s| serde_json::from_str::<Value>(&s).ok()) else { break };
        key = canonical_json(&v);
        if key == pk {
            break;
        }
        cur = Some(v);
    }
    Ok(cur)
}

/// Where an id of `tbl` lives now (following re-key decisions, chained).
fn resolve(aliases: &Aliases, tbl: &str, id: &Value) -> Option<Value> {
    let mut cur = aliases.get(&(tbl.to_string(), canonical_json(id)))?.clone();
    for _ in 0..8 {
        match aliases.get(&(tbl.to_string(), canonical_json(&cur))) {
            Some(next) if *next != cur => cur = next.clone(),
            _ => break,
        }
    }
    Some(cur)
}

/// Rewrite an upsert that still carries ids an earlier reconcile replaced:
/// its own pk, and every column referencing a re-keyed row.
fn follow_aliases(spec: &TableSpec, op: &mut Op, aliases: &Aliases) {
    if aliases.is_empty() {
        return;
    }
    if spec.pk.len() == 1 {
        if let Some(to) = resolve(aliases, spec.name, &op.pk) {
            op.pk = to;
        }
    }
    for (parent, _, col) in REFS.iter().filter(|(_, child, _)| *child == spec.name) {
        let Some(to) = op.row.as_ref().and_then(|r| r.get(*col)).and_then(|v| resolve(aliases, parent, v)) else { continue };
        if let Some(row) = op.row.as_mut() {
            row.insert((*col).to_string(), to.clone());
        }
        if let Value::Object(pk) = &mut op.pk {
            if pk.contains_key(*col) {
                pk.insert((*col).to_string(), to);
            }
        }
    }
}

/// The pk of a local row (other than `pk`) holding the same values as `row`
/// in one of the table's UNIQUE indexes (NULLs never clash).
async fn clashing_row(
    conn: &mut SqliteConnection,
    spec: &TableSpec,
    row: &Map<String, Value>,
    pk: &Value,
) -> Result<Option<Value>, String> {
    let indexes: Vec<String> =
        sqlx::query_scalar("SELECT name FROM pragma_index_list(?) WHERE \"unique\" = 1 AND origin != 'pk'")
            .bind(spec.name)
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?;
    let [pk_col] = spec.pk else { return Ok(None) };
    for index in indexes {
        let cols: Vec<Option<String>> = sqlx::query_scalar("SELECT name FROM pragma_index_info(?) ORDER BY seqno")
            .bind(&index)
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?;
        let Some(cols) = cols.into_iter().collect::<Option<Vec<String>>>() else { continue };
        let Some(values) = cols.iter().map(|c| row.get(c).filter(|v| !v.is_null())).collect::<Option<Vec<&Value>>>() else {
            continue;
        };
        let cond: Vec<String> = cols.iter().map(|c| format!("{} = ?", q(c))).collect();
        let sql = format!(
            "SELECT {} FROM {} WHERE {} AND {} != ? LIMIT 1",
            pk_sql(spec, &q(spec.name)),
            q(spec.name),
            cond.join(" AND "),
            q(pk_col),
        );
        let mut query = sqlx::query(&sql);
        for v in values {
            query = bind_value(query, v);
        }
        let found = bind_value(query, pk).fetch_optional(&mut *conn).await.map_err(err)?;
        if let Some(r) = found {
            let pk: String = r.get(0);
            return Ok(Some(serde_json::from_str(&pk).map_err(|e| e.to_string())?));
        }
    }
    Ok(None)
}

/// An incoming row that clashes with another local row on a UNIQUE key (new
/// here, or renamed into the clash): the lower id survives on every device
/// (see the module docs). Returns whether the op was redirected into a local
/// row.
async fn reconcile_unique(
    conn: &mut SqliteConnection,
    paths: &PathCtx,
    spec: &'static TableSpec,
    op: &mut Op,
    aliases: &mut Aliases,
) -> Result<bool, String> {
    if spec.pk.len() != 1 {
        return Ok(false);
    }
    let exists = read_row(conn, spec, &op.pk).await?.is_some();
    for _ in 0..4 {
        let Some(wire) = op.row.as_ref() else { return Ok(false) };
        let mut row = decode_row(paths, spec, wire);
        for (col, v) in spec.pk.iter().zip(pk_values(spec, &op.pk)?) {
            row.insert((*col).to_string(), v);
        }
        let Some(local) = clashing_row(conn, spec, &row, &op.pk).await? else { return Ok(false) };
        let incoming = op.pk.clone();
        if canonical_json(&local) < canonical_json(&incoming) {
            // Ours survives: the incoming row is merged into it.
            if exists {
                fold_into(conn, spec, &incoming, &local, true).await?;
            } else {
                super::secrets::queue_key_move(conn, spec.name, &incoming, &local).await?;
            }
            add_alias(conn, aliases, spec.name, &canonical_json(&incoming), &local).await?;
            // The losing id goes out as an alias op: it supersedes the
            // sender's ops on the relay and re-keys the row wherever it lives.
            queue_outbox(conn, spec.name, &canonical_json(&incoming), "D", wall_ms() as i64).await?;
            op.pk = local;
            return Ok(true);
        }
        // Theirs survives: ours takes its id (or folds into it).
        if exists {
            fold_into(conn, spec, &local, &incoming, true).await?;
            queue_outbox(conn, spec.name, &canonical_json(&local), "D", wall_ms() as i64).await?;
            // Not a local change: the op being applied decides the content.
            queue_outbox(conn, spec.name, &canonical_json(&incoming), "U", 0).await?;
        } else {
            rekey(conn, spec, &local, &incoming, RekeyOpts::APPLY).await?;
        }
        add_alias(conn, aliases, spec.name, &canonical_json(&local), &incoming).await?;
        tracing::info!(event = "sync_unique_rekey", tbl = spec.name);
    }
    Err(format!("{}: unique key clash did not settle", spec.name))
}

async fn queue_outbox(conn: &mut SqliteConnection, tbl: &str, pk: &str, op: &str, changed_at: i64) -> Result<(), String> {
    sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES (?, ?, ?, ?)")
        .bind(tbl)
        .bind(pk)
        .bind(op)
        .bind(changed_at)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

/// Two local rows are one: every row pointing at `loser` moves to `survivor`
/// (a row the survivor already has a twin of is dropped), the loser's
/// unpushed changes go out as the survivor's, and the loser row is deleted.
/// With `repush` the moved synced rows are queued for push, like `rekey` does.
async fn fold_into(
    conn: &mut SqliteConnection,
    spec: &TableSpec,
    loser: &Value,
    survivor: &Value,
    repush: bool,
) -> Result<(), String> {
    let now = wall_ms() as i64;
    let (loser_key, survivor_key) = (canonical_json(loser), canonical_json(survivor));
    for (_, child, col) in REFS.iter().filter(|(p, _, _)| *p == spec.name) {
        let child_spec = schema::spec(child);
        let moved: Vec<String> = match child_spec.filter(|_| repush) {
            Some(cs) => {
                let sql = format!("SELECT {} FROM {} WHERE {} = ?", pk_sql(cs, &q(child)), q(child), q(col));
                let rows = bind_value(sqlx::query(&sql), loser).fetch_all(&mut *conn).await.map_err(err)?;
                rows.iter().map(|r| r.get::<String, _>(0)).collect()
            }
            None => Vec::new(),
        };
        let sql = format!("UPDATE OR IGNORE {} SET {1} = ? WHERE {1} = ?", q(child), q(col));
        bind_value(bind_value(sqlx::query(&sql), survivor), loser).execute(&mut *conn).await.map_err(err)?;
        // What is left clashed with a twin under the survivor.
        let sql = format!("DELETE FROM {} WHERE {} = ?", q(child), q(col));
        bind_value(sqlx::query(&sql), loser).execute(&mut *conn).await.map_err(err)?;
        let Some(cs) = child_spec else { continue };
        if cs.pk.contains(col) {
            rekey_outbox(conn, child, (cs.pk.len() > 1).then_some(*col), loser, survivor).await?;
        }
        for pk in moved {
            match serde_json::from_str::<Value>(&pk) {
                Ok(Value::Object(mut m)) if cs.pk.contains(col) => {
                    queue_outbox(conn, child, &pk, "D", now).await?;
                    m.insert((*col).to_string(), survivor.clone());
                    queue_outbox(conn, child, &canonical_json(&Value::Object(m)), "U", now).await?;
                }
                Ok(_) if cs.pk == [*col] => {
                    queue_outbox(conn, child, &pk, "D", now).await?;
                    queue_outbox(conn, child, &survivor_key, "U", now).await?;
                }
                Ok(_) => queue_outbox(conn, child, &pk, "U", now).await?,
                Err(_) => {}
            }
        }
    }
    rekey_outbox(conn, spec.name, None, loser, survivor).await?;
    delete_row(conn, spec, loser).await?;
    sqlx::query("DELETE FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(&loser_key)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    super::secrets::queue_key_move(conn, spec.name, loser, survivor).await?;
    Ok(())
}

/// An alias op (or a Delete of an id we know was re-keyed): the row under the
/// old id becomes the survivor here too — re-keyed when the survivor is not
/// here yet, folded into it when it is, deleted only when the survivor itself
/// was deleted. Never pushed again (the sender queued the moved rows).
async fn apply_alias(
    conn: &mut SqliteConnection,
    spec: &'static TableSpec,
    op: &Op,
    target: Value,
    aliases: &mut Aliases,
) -> Result<(Outcome, RowKey), String> {
    let old_key = op.pk_key();
    let new = resolve(aliases, spec.name, &target).unwrap_or(target);
    let new_key = canonical_json(&new);
    if new_key == old_key {
        return Err(format!("{}: alias of an id to itself", spec.name));
    }
    add_alias(conn, aliases, spec.name, &old_key, &new).await?;
    if read_row(conn, spec, &op.pk).await?.is_some() {
        let survivor_deleted: Option<i64> = sqlx::query_scalar("SELECT deleted FROM sync_rows WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(&new_key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(err)?;
        if read_row(conn, spec, &new).await?.is_some() {
            // The loser may have changed here after the sender folded it (a
            // later edit, local or pulled): the newer of the two rows is the
            // content, as it is wherever that edit lands on the survivor.
            let carry = if loser_is_newer(conn, spec, &old_key, &new_key).await? {
                read_row(conn, spec, &op.pk).await?
            } else {
                None
            };
            fold_into(conn, spec, &op.pk, &new, false).await?;
            if let Some(mut row) = carry {
                for (col, v) in spec.pk.iter().zip(pk_values(spec, &new)?) {
                    row.insert((*col).to_string(), v);
                }
                upsert_row(conn, spec, &row).await?;
                // Pushed with a fresh HLC, so the sender adopts it too.
                queue_outbox(conn, spec.name, &new_key, "U", wall_ms() as i64).await?;
                tracing::info!(event = "sync_alias_carry", tbl = spec.name);
            }
        } else if survivor_deleted == Some(1) {
            delete_row(conn, spec, &op.pk).await?;
        } else {
            rekey(conn, spec, &op.pk, &new, RekeyOpts { keep_hlc: true, repush: false }).await?;
        }
        tracing::info!(event = "sync_alias_rekey", tbl = spec.name);
    }
    let known: Option<String> = sqlx::query_scalar("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(&old_key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    let hlc = match known.and_then(|s| s.parse::<Hlc>().ok()) {
        Some(h) if h > op.hlc => h,
        _ => op.hlc.clone(),
    };
    sqlx::query(
        "INSERT INTO sync_rows (tbl, pk, hlc, deleted) VALUES (?, ?, ?, 1) \
         ON CONFLICT(tbl, pk) DO UPDATE SET hlc = excluded.hlc, deleted = 1",
    )
    .bind(spec.name)
    .bind(&old_key)
    .bind(hlc.to_string())
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok((Outcome::Applied, (spec.name.to_string(), old_key)))
}

/// Whether the row under `loser` changed after the one under `survivor` (by
/// last known HLC, an unpushed local change counting as of its time).
async fn loser_is_newer(conn: &mut SqliteConnection, spec: &TableSpec, loser: &str, survivor: &str) -> Result<bool, String> {
    let mut stamps = Vec::with_capacity(2);
    for key in [loser, survivor] {
        let synced: Option<Hlc> = sqlx::query_scalar::<_, String>("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(key)
            .fetch_optional(&mut *conn)
            .await
            .map_err(err)?
            .and_then(|s| s.parse().ok());
        let local: Option<i64> = sqlx::query_scalar("SELECT MAX(changed_at) FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(spec.name)
            .bind(key)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
        // A local change goes out with an HLC above its time and above
        // anything seen so far.
        let local = local.filter(|c| *c > 0).map(|c| Hlc::new(c as u64, u16::MAX, ""));
        stamps.push(synced.max(local));
    }
    Ok(stamps[0] > stamps[1])
}

/// sha256 of a note body (JSON-encoded, so NULL and "" differ).
pub fn content_hash(content: Option<&Value>) -> String {
    use sha2::{Digest, Sha256};
    let json = canonical_json(content.unwrap_or(&Value::Null));
    hex::encode(Sha256::digest(json.as_bytes()))
}

/// Pseudo-column of a pushed note row: `base_hash` of the sender, i.e. the
/// body its edit was made on top of. Not a real column, so `upsert_row` never
/// writes it.
pub const NOTE_BASE_FIELD: &str = "_sync_base";

/// Remember the note body received from another device.
async fn set_note_base(conn: &mut SqliteConnection, pk: &str, content: Option<&Value>) -> Result<(), String> {
    sqlx::query("UPDATE sync_rows SET base_hash = ? WHERE tbl = 'notes' AND pk = ?")
        .bind(content_hash(content))
        .bind(pk)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

/// Adds `NOTE_BASE_FIELD` to a note row about to be pushed (`SyncStore::pending`).
pub async fn add_note_base(conn: &mut SqliteConnection, pk: &str, row: &mut Map<String, Value>) -> Result<(), String> {
    let base: Option<String> = sqlx::query_scalar("SELECT base_hash FROM sync_rows WHERE tbl = 'notes' AND pk = ?")
        .bind(pk)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?
        .flatten();
    row.insert(NOTE_BASE_FIELD.into(), base.map(Value::String).unwrap_or(Value::Null));
    Ok(())
}

/// How an incoming note meets the local version of the same note, judged on
/// the body: the local base (`sync_rows.base_hash`, last body received) tells
/// whether this side edited it, the op's base (`NOTE_BASE_FIELD`) whether the
/// sender built on the local body.
#[derive(Debug)]
enum NoteMerge {
    /// Same body (or nothing to compare): plain row-level LWW.
    Lww,
    /// Both sides edited the body concurrently: the local one was kept as a copy.
    Copied,
    /// Only this side changed the body: keep it whatever wins the rest of the row.
    KeepLocal(Value),
    /// The remote body supersedes the local one, whatever wins the rest of the row.
    TakeRemote,
}

async fn note_merge(conn: &mut SqliteConnection, op: &Op) -> Result<NoteMerge, String> {
    let spec = schema::spec("notes").ok_or("notes not synced")?;
    let Some(local) = read_row(conn, spec, &op.pk).await? else { return Ok(NoteMerge::Lww) };
    let Some(remote_body) = op.row.as_ref().and_then(|r| r.get("content")) else {
        return Ok(NoteMerge::Lww);
    };
    let local_body = local.get("content").cloned().unwrap_or(Value::Null);
    if *remote_body == local_body {
        return Ok(NoteMerge::Lww);
    }
    let received: Option<String> = sqlx::query_scalar("SELECT base_hash FROM sync_rows WHERE tbl = 'notes' AND pk = ?")
        .bind(op.pk_key())
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?
        .flatten();
    let remote_base = op.row.as_ref().and_then(|r| r.get(NOTE_BASE_FIELD)).and_then(Value::as_str);
    let local_hash = content_hash(Some(&local_body));
    // The sender edited on top of our body, or we never edited what we got.
    if remote_base == Some(local_hash.as_str()) || received.as_deref() == Some(local_hash.as_str()) {
        return Ok(NoteMerge::TakeRemote);
    }
    // The sender did not touch the body it had (only other columns changed).
    if remote_base == Some(content_hash(Some(remote_body)).as_str()) {
        return Ok(NoteMerge::KeepLocal(local_body));
    }
    keep_note_conflict_copy(conn, &local).await?;
    Ok(NoteMerge::Copied)
}

/// The local note row stays; only its body becomes the remote one (the
/// outbox entry is kept, so the merged row is pushed).
async fn take_remote_body(conn: &mut SqliteConnection, op: &Op) -> Result<(), String> {
    let body = op.row.as_ref().and_then(|r| r.get("content")).cloned().unwrap_or(Value::Null);
    let id = op.pk.as_str().ok_or("notes: malformed pk")?;
    let query = sqlx::query("UPDATE notes SET content = ? WHERE id = ?");
    bind_value(query, &body).bind(id).execute(&mut *conn).await.map_err(err)?;
    set_note_base(conn, &op.pk_key(), Some(&body)).await
}

/// Both sides edited this note's body since the last agreed version: the
/// remote version becomes the row, the local one is kept as a new note
/// "<title> (conflict copy)" (queued for push explicitly, since the guard
/// mutes the triggers).
async fn keep_note_conflict_copy(conn: &mut SqliteConnection, local: &Map<String, Value>) -> Result<(), String> {
    let spec = schema::spec("notes").ok_or("notes not synced")?;
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
    Ok(())
}
