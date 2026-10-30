//! Bootstrap (design §5): the full upload of a device's state, the first-join
//! merge of a device that already has data, and the project path mapping.
//!
//! - **Full upload** ([`enqueue_all`]): every synced row goes into
//!   `sync_outbox`; used when this device creates a group and by "Re-sync
//!   everything".
//! - **First join** ([`preview`] / [`commit`]): the whole group state is
//!   pulled into memory (never applied blindly), every remote row is matched
//!   to a local one by pk, then by natural key (`TableSpec::natural_key`;
//!   projects by path → git remote → name), and a matched local row with a
//!   different id is **re-keyed** to the remote id together with every row
//!   pointing at it ([`REFS`]), in one guarded transaction. Then the remote
//!   rows are applied with the normal LWW engine (a local row only wins when
//!   it has an unpushed change newer than the remote op) and the local-only
//!   rows are queued for push. `preview` runs the same merge and rolls back.
//! - **Project mapping**: a project that lives somewhere else on this machine
//!   is mapped from its synced (portable) root to the local root, kept as JSON
//!   in `sync_state.project_path_map`; `codec::map_project_path` /
//!   `codec::portable_path` rewrite every path under it both ways.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection};

use super::clock::wall_ms;
use super::codec::{bind_value, decode_row, expand_portable, map_project_path, pk_of_row, portable_path, table_columns, PathCtx};
use super::crypto::SyncKeys;
use super::op::{canonical_json, Op, OpKind};
use super::schema::{self, lit, pk_sql, q, TableSpec, SYNCED_TABLES};
use super::store::{ApplyReport, SqliteSyncStore, SyncStore};
use super::transport::Connection;
use crate::db::Db;

/// `sync_state` key: JSON object `{ "<synced portable root>": "<local root>" }`.
pub const PROJECT_MAP_KEY: &str = "project_path_map";
/// `sync_state` key: "1" between joining a group and committing the merge;
/// the engine does not start meanwhile (it would push rows with local ids).
pub const PENDING_KEY: &str = "bootstrap_pending";
const PULL_LIMIT: u64 = 500;

/// Every column holding the id of a row of a table that can be re-keyed
/// (natural key != pk), declared FK or not, synced or machine-local:
/// `(parent table, child table, child column)`. Checked against the declared
/// FKs by `refs_cover_declared_foreign_keys`.
pub static REFS: &[(&str, &str, &str)] = &[
    ("github_accounts", "projects", "github_account_id"),
    ("github_accounts", "projects", "github_reviewer_account_id"),
    ("github_accounts", "runs", "github_account_id"),
    ("gitlab_accounts", "projects", "gitlab_account_id"),
    ("aws_accounts", "projects", "aws_account_id"),
    ("azure_accounts", "projects", "azure_account_id"),
    ("claude_accounts", "projects", "claude_account_id"),
    ("claude_accounts", "runs", "claude_account_id"),
    ("claude_accounts", "run_usage", "claude_account_id"),
    ("servers", "project_servers", "server_id"),
    ("servers", "runs", "server_id"),
    ("mcp_servers", "project_mcp_servers", "server_id"),
    ("skills", "project_skills", "skill_id"),
    ("skills", "skill_group_members", "skill_id"),
    ("skills", "sync_conflicts", "skill_id"),
    ("rules", "project_rules", "rule_id"),
    ("rules", "rule_group_members", "rule_id"),
    ("rules", "rule_sync_conflicts", "rule_id"),
    ("skill_groups", "skill_group_members", "group_id"),
    ("skill_groups", "project_skill_groups", "group_id"),
    ("rule_groups", "rule_group_members", "group_id"),
    ("rule_groups", "project_rule_groups", "group_id"),
    ("projects", "repos", "project_id"),
    ("projects", "runs", "project_id"),
    ("projects", "run_usage", "project_id"),
    ("projects", "deleted_sessions", "project_id"),
    ("projects", "notes", "project_id"),
    ("projects", "todos", "project_id"),
    ("projects", "note_runs", "project_id"),
    ("projects", "todo_runs", "project_id"),
    ("projects", "slack_threads", "project_id"),
    ("projects", "slack_thread_runs", "project_id"),
    ("projects", "pulse_watches", "project_id"),
    ("projects", "pulse_requests", "project_id"),
    ("projects", "project_skills", "project_id"),
    ("projects", "project_rules", "project_id"),
    ("projects", "project_skill_groups", "project_id"),
    ("projects", "project_rule_groups", "project_id"),
    ("projects", "project_mcp_servers", "project_id"),
    ("projects", "project_servers", "project_id"),
    ("projects", "project_secrets", "project_id"),
    ("projects", "sync_conflicts", "project_id"),
    ("projects", "rule_sync_conflicts", "project_id"),
    ("projects", "conductor_sessions", "project_id"),
    ("repos", "runs", "repo_id"),
    ("runs", "run_usage", "run_id"),
    ("runs", "run_usage_baseline", "run_id"),
    ("runs", "note_runs", "run_id"),
    ("runs", "todo_runs", "run_id"),
    ("runs", "slack_thread_runs", "run_id"),
    ("runs", "notes", "run_id"),
    ("runs", "todos", "run_id"),
    ("runs", "pulse_watches", "added_run_id"),
    ("runs", "runs", "conductor_run_id"),
    ("runs", "remote_audit", "run_id"),
    ("slack_threads", "slack_messages", "thread_id"),
    ("slack_threads", "slack_files", "thread_id"),
    ("slack_threads", "slack_thread_runs", "thread_id"),
    ("slack_threads", "pulse_watches", "slack_thread_id"),
    ("pulse_watches", "pulse_digests", "watch_id"),
    ("pulse_watches", "pulse_requests", "watch_id"),
];

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

// ---- sync_state helpers --------------------------------------------------------

async fn get_state(conn: &mut SqliteConnection, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)
}

async fn set_state(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO sync_state (key, value) VALUES (?, ?) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
    )
    .bind(key)
    .bind(value)
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok(())
}

fn parse_map(json: Option<String>) -> Vec<(String, String)> {
    let Some(Value::Object(m)) = json.and_then(|s| serde_json::from_str(&s).ok()) else {
        return Vec::new();
    };
    m.into_iter().filter_map(|(k, v)| Some((k, v.as_str()?.to_string()))).collect()
}

/// The project path map (synced portable root → local root).
pub async fn load_project_map(db: &Db) -> Result<Vec<(String, String)>, String> {
    let mut conn = db.acquire().await.map_err(err)?;
    Ok(parse_map(get_state(&mut conn, PROJECT_MAP_KEY).await?))
}

async fn save_project_map(conn: &mut SqliteConnection, map: &[(String, String)]) -> Result<(), String> {
    let obj: Map<String, Value> = map.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect();
    set_state(conn, PROJECT_MAP_KEY, &Value::Object(obj).to_string()).await
}

pub async fn is_pending(db: &Db) -> Result<bool, String> {
    let mut conn = db.acquire().await.map_err(err)?;
    Ok(get_state(&mut conn, PENDING_KEY).await?.as_deref() == Some("1"))
}

/// Just joined a group: hold the engine until the merge is committed.
pub async fn mark_joined(db: &Db) -> Result<(), String> {
    let mut conn = db.acquire().await.map_err(err)?;
    set_state(&mut conn, PENDING_KEY, "1").await
}

/// This device just created a group: it is the reference state, so upload
/// everything from the start of the (empty) group's log.
pub async fn prepare_new_group(db: &Db) -> Result<usize, String> {
    let device = super::ensure_device(db).await?;
    SqliteSyncStore::new(db.clone(), device).set_cursor(0).await?;
    let mut conn = db.acquire().await.map_err(err)?;
    set_state(&mut conn, PENDING_KEY, "0").await?;
    drop(conn);
    enqueue_all(db).await
}

// ---- full upload ---------------------------------------------------------------

fn filter_where(spec: &TableSpec) -> String {
    match spec.filter {
        None => String::new(),
        Some(f) => {
            let values: Vec<String> = f.allowed.iter().map(|v| lit(v)).collect();
            format!(" WHERE {} IN ({})", q(f.col), values.join(", "))
        }
    }
}

/// Queue every synced row (settings: portable keys only) for push. Returns
/// the number of rows queued. The push gives each a fresh HLC, so this
/// device's version of every row wins on the others.
pub async fn enqueue_all(db: &Db) -> Result<usize, String> {
    let now = wall_ms() as i64;
    let mut tx = db.begin().await.map_err(err)?;
    let mut n = 0u64;
    for spec in SYNCED_TABLES {
        let sql = format!(
            "INSERT INTO sync_outbox (tbl, pk, op, changed_at) SELECT {}, {}, 'U', ? FROM {}{}",
            lit(spec.name),
            pk_sql(spec, &q(spec.name)),
            q(spec.name),
            filter_where(spec),
        );
        n += sqlx::query(&sql).bind(now).execute(&mut *tx).await.map_err(err)?.rows_affected();
    }
    tx.commit().await.map_err(err)?;
    Ok(n as usize)
}

/// Every row of a synced table (raw local form), filter applied.
async fn read_all(conn: &mut SqliteConnection, spec: &TableSpec) -> Result<Vec<Map<String, Value>>, String> {
    let cols = table_columns(conn, spec.name).await?;
    let args: Vec<String> = cols.iter().map(|c| format!("{}, {}", lit(c), q(c))).collect();
    let sql = format!("SELECT json_object({}) FROM {}{}", args.join(", "), q(spec.name), filter_where(spec));
    let rows: Vec<String> = sqlx::query_scalar(&sql).fetch_all(&mut *conn).await.map_err(err)?;
    rows.iter()
        .map(|s| match serde_json::from_str::<Value>(s) {
            Ok(Value::Object(m)) => Ok(m),
            _ => Err(format!("{}: unreadable row", spec.name)),
        })
        .collect()
}

// ---- group state ----------------------------------------------------------------

/// The whole group log, newest op per row, held in memory.
#[derive(Debug, Clone, Default)]
pub struct RemoteState {
    pub ops: Vec<Op>,
    /// Pull cursor to continue from once the merge is committed.
    pub last_seq: u64,
    pub undecryptable: usize,
}

impl RemoteState {
    pub fn from_ops(ops: Vec<Op>, last_seq: u64) -> Self {
        let mut latest: HashMap<(String, String), Op> = HashMap::new();
        for op in ops {
            let key = (op.tbl.clone(), op.pk_key());
            match latest.get(&key) {
                Some(kept) if kept.hlc >= op.hlc => {}
                _ => {
                    latest.insert(key, op);
                }
            }
        }
        let mut ops: Vec<Op> = latest.into_values().collect();
        ops.sort_by(|a, b| a.hlc.cmp(&b.hlc));
        RemoteState { ops, last_seq, undecryptable: 0 }
    }
}

/// Pull the group log from the start (own ops included: a re-joining device
/// gets its old rows back).
pub async fn fetch_group_state(conn: &Connection, keys: &SyncKeys) -> Result<RemoteState, String> {
    let mut cursor = 0u64;
    let mut ops = Vec::new();
    let mut undecryptable = 0;
    loop {
        let (items, last_seq, more) = conn.pull(cursor, PULL_LIMIT).await.map_err(|e| e.to_string())?;
        let mut next = cursor;
        for item in items {
            next = next.max(item.seq.unwrap_or(0));
            match keys.open_op(&item.blob, item.key.as_deref().unwrap_or_default()) {
                Ok(op) => ops.push(op),
                Err(_) => undecryptable += 1,
            }
        }
        next = next.max(last_seq);
        let stalled = next == cursor;
        cursor = next;
        if !more || stalled {
            break;
        }
    }
    let mut state = RemoteState::from_ops(ops, cursor);
    state.undecryptable = undecryptable;
    Ok(state)
}

// ---- git remote identity --------------------------------------------------------

/// `host/owner/repo` (lowercase, no `.git`) for the usual remote URL forms:
/// `git@host:owner/repo.git`, `ssh://git@host:22/owner/repo`,
/// `https://user@host/owner/repo.git`.
pub fn normalize_git_url(url: &str) -> Option<String> {
    let u = url.trim().trim_end_matches('/');
    let u = u.strip_suffix(".git").unwrap_or(u);
    let (host, path) = match u.split_once("://") {
        Some((_, rest)) => {
            let (authority, path) = rest.split_once('/')?;
            let host = authority.rsplit('@').next()?;
            (host.split(':').next()?, path)
        }
        None => {
            let (left, path) = u.split_once(':')?;
            if left.contains('/') {
                return None; // a local path
            }
            (left.rsplit('@').next()?, path)
        }
    };
    let path = path.trim_matches('/');
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("{}/{}", host.to_lowercase(), path.to_lowercase()))
}

/// `owner/repo` part of a normalized URL (the remote rows only carry that).
fn repo_path(normalized: &str) -> Option<String> {
    normalized.split_once('/').map(|(_, p)| p.to_string())
}

/// `git -C <path> remote get-url origin`, if `path` is a checkout.
pub fn git_origin(path: &str) -> Option<String> {
    if !Path::new(path).is_dir() {
        return None;
    }
    let out = std::process::Command::new("git")
        .args(["-C", path, "remote", "get-url", "origin"])
        .output()
        .ok()?;
    let url = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !url.is_empty()).then_some(url)
}

/// Origin URLs of every local project and repo checkout (blocking git calls,
/// run off the async runtime before the merge transaction).
pub async fn local_git_remotes(db: &Db) -> Result<HashMap<String, String>, String> {
    let paths: Vec<String> = sqlx::query_scalar("SELECT path FROM projects UNION SELECT path FROM repos WHERE path IS NOT NULL")
        .fetch_all(db)
        .await
        .map_err(err)?;
    tokio::task::spawn_blocking(move || paths.into_iter().filter_map(|p| Some((p.clone(), git_origin(&p)?))).collect())
        .await
        .map_err(|e| e.to_string())
}

fn str_of<'a>(row: &'a Map<String, Value>, col: &str) -> Option<&'a str> {
    row.get(col).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// `owner/repo` identities a project row or repo row carries itself.
fn row_identities(row: &Map<String, Value>, out: &mut HashSet<String>) {
    if let (Some(o), Some(r)) = (str_of(row, "github_owner"), str_of(row, "github_repo")) {
        out.insert(format!("{o}/{r}").to_lowercase());
    }
    if let Some(p) = str_of(row, "gitlab_project_path") {
        out.insert(p.trim_matches('/').to_lowercase());
    }
}

// ---- merge -----------------------------------------------------------------------

#[derive(Debug, Default, Clone, Serialize, PartialEq, Eq)]
pub struct TableCounts {
    pub tbl: String,
    /// Remote rows that already exist here (same id, or re-keyed).
    pub matched: usize,
    /// Of `matched`: local rows that got the remote id.
    pub rekeyed: usize,
    /// Remote rows that will be created here.
    pub new_remote: usize,
    /// Local rows the group does not have; pushed after the merge.
    pub new_local: usize,
    /// Of `matched`: rows whose content differs (the remote version wins
    /// unless the local row has a newer unpushed change; notes keep a copy).
    pub conflicts: usize,
    /// Local rows the group deleted; removed here unless changed later.
    pub deleted_remote: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectMatch {
    pub remote_id: String,
    pub name: String,
    /// Path as synced (portable form).
    pub synced_path: String,
    /// The local project it was matched with (id before re-keying).
    pub local_id: Option<String>,
    /// `id` | `path` | `git_remote` | `name`; `None` = new on this machine.
    pub matched_by: Option<String>,
    /// Where the project lives on this machine after the merge.
    pub local_path: String,
    pub on_this_machine: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct BootstrapPreview {
    pub matched: usize,
    pub rekeyed: usize,
    pub new_remote: usize,
    pub new_local: usize,
    pub conflicts: usize,
    pub deleted_remote: usize,
    /// Tables with anything to report, in registry order.
    pub tables: Vec<TableCounts>,
    pub projects: Vec<ProjectMatch>,
    /// Ops from the relay nobody can open (skipped).
    pub undecryptable: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct BootstrapReport {
    pub preview: BootstrapPreview,
    pub applied: ApplyReport,
    /// Local rows queued for push.
    pub queued: usize,
}

struct Merge {
    preview: BootstrapPreview,
    /// `(tbl, canonical pk)` of local rows the group does not have.
    local_only: Vec<(String, String)>,
    map: Vec<(String, String)>,
}

fn natural_value(spec: &TableSpec, row: &Map<String, Value>) -> Option<String> {
    if spec.natural_key == spec.pk {
        return None;
    }
    let mut vals = Vec::with_capacity(spec.natural_key.len());
    for c in spec.natural_key {
        match row.get(*c) {
            None | Some(Value::Null) => return None,
            Some(v) => vals.push(v.clone()),
        }
    }
    Some(canonical_json(&Value::Array(vals)))
}

fn differs(spec: &TableSpec, local: &Map<String, Value>, remote: &Map<String, Value>) -> bool {
    remote
        .iter()
        .filter(|(c, _)| !spec.is_skipped(c))
        .any(|(c, v)| local.get(c).is_some_and(|l| l != v))
}

/// Project-matching inputs gathered before the merge.
struct ProjectIndex {
    /// Local project id → `owner/repo` identities (git remotes + row columns).
    local_ids: HashMap<String, HashSet<String>>,
    /// Remote project id → identities from its row and its repos.
    remote_ids: HashMap<String, HashSet<String>>,
}

async fn project_index(
    conn: &mut SqliteConnection,
    remote: &HashMap<&str, Vec<&Op>>,
    git: &HashMap<String, String>,
) -> Result<ProjectIndex, String> {
    let projects = read_all(conn, schema::spec("projects").ok_or("projects not synced")?).await?;
    let repos = read_all(conn, schema::spec("repos").ok_or("repos not synced")?).await?;
    let mut local_ids: HashMap<String, HashSet<String>> = HashMap::new();
    let add_git = |path: Option<&str>, set: &mut HashSet<String>| {
        if let Some(id) = path.and_then(|p| git.get(p)).and_then(|u| normalize_git_url(u)).and_then(|n| repo_path(&n)) {
            set.insert(id);
        }
    };
    for p in &projects {
        let Some(id) = str_of(p, "id") else { continue };
        let set = local_ids.entry(id.to_string()).or_default();
        row_identities(p, set);
        add_git(str_of(p, "path"), set);
    }
    for r in &repos {
        let Some(pid) = str_of(r, "project_id") else { continue };
        let set = local_ids.entry(pid.to_string()).or_default();
        row_identities(r, set);
        add_git(str_of(r, "path"), set);
    }
    let mut remote_ids: HashMap<String, HashSet<String>> = HashMap::new();
    for op in remote.get("projects").into_iter().flatten() {
        if let (Some(id), Some(row)) = (op.pk.as_str(), op.row.as_ref()) {
            row_identities(row, remote_ids.entry(id.to_string()).or_default());
        }
    }
    for op in remote.get("repos").into_iter().flatten() {
        let Some(row) = op.row.as_ref() else { continue };
        if let Some(pid) = str_of(row, "project_id") {
            row_identities(row, remote_ids.entry(pid.to_string()).or_default());
        }
    }
    Ok(ProjectIndex { local_ids, remote_ids })
}

/// Match remote rows to local ones table by table (parents first, so a
/// child's natural key already carries re-keyed parent ids) and re-key the
/// local rows on `conn`. The caller owns the transaction (guard on, FKs
/// deferred) and decides to commit or roll back.
async fn merge(
    conn: &mut SqliteConnection,
    home: &PathCtx,
    ops: &[Op],
    git: &HashMap<String, String>,
) -> Result<Merge, String> {
    let mut upserts: HashMap<&str, Vec<&Op>> = HashMap::new();
    let mut deletes: HashMap<&str, HashSet<String>> = HashMap::new();
    for op in ops {
        let Some(spec) = schema::spec(&op.tbl) else { continue };
        match op.kind {
            OpKind::Upsert if op.row.is_some() => upserts.entry(spec.name).or_default().push(op),
            OpKind::Upsert => {}
            OpKind::Delete => {
                deletes.entry(spec.name).or_default().insert(op.pk_key());
            }
        }
    }
    let projects_index = project_index(conn, &upserts, git).await?;
    let mut map = parse_map(get_state(conn, PROJECT_MAP_KEY).await?);
    let mut ctx = home.clone().with_project_map(map.clone());
    let mut preview = BootstrapPreview::default();
    let mut local_only = Vec::new();
    let mut project_matches: Vec<ProjectMatch> = Vec::new();

    for spec in SYNCED_TABLES {
        let mut counts = TableCounts { tbl: spec.name.to_string(), ..Default::default() };
        let locals = read_all(conn, spec).await?;
        let mut local_by_pk: HashMap<String, Map<String, Value>> = HashMap::new();
        for row in locals {
            if let Some(pk) = pk_of_row(spec, &row) {
                local_by_pk.insert(canonical_json(&pk), row);
            }
        }
        let remote_rows = upserts.get(spec.name).cloned().unwrap_or_default();
        let remote_pks: HashSet<String> = remote_rows.iter().map(|op| op.pk_key()).collect();
        // Local rows already paired with a remote row (by their local pk).
        let mut claimed: HashSet<String> = HashSet::new();
        let free = |pk: &String, claimed: &HashSet<String>| !remote_pks.contains(pk) && !claimed.contains(pk);
        let mut by_natural: HashMap<String, String> = HashMap::new();
        for (pk, row) in &local_by_pk {
            if let Some(nk) = natural_value(spec, row) {
                by_natural.insert(nk, pk.clone());
            }
        }

        for op in remote_rows {
            let Some(row) = op.row.as_ref() else { continue };
            let rpk = op.pk_key();
            let mut decoded = decode_row(&ctx, spec, row);
            let mut matched_by: Option<&str> = None;
            let mut local_pk: Option<String> = None;
            if local_by_pk.contains_key(&rpk) {
                matched_by = Some("id");
                local_pk = Some(rpk.clone());
            } else if spec.name == "projects" {
                if let Some((pk, by)) = match_project(&local_by_pk, &decoded, op, &projects_index, &|pk| free(pk, &claimed)) {
                    matched_by = Some(by);
                    local_pk = Some(pk);
                }
            } else if let Some(pk) = natural_value(spec, &decoded).and_then(|nk| by_natural.get(&nk)).filter(|pk| free(pk, &claimed)) {
                matched_by = Some("natural");
                local_pk = Some(pk.clone());
            }

            if spec.name == "projects" {
                let synced_path = str_of(row, "path").unwrap_or_default().to_string();
                let local_path = local_pk
                    .as_ref()
                    .and_then(|pk| local_by_pk.get(pk))
                    .and_then(|l| str_of(l, "path"))
                    .map(str::to_string);
                // A matched project keeps its local checkout path.
                if let Some(lp) = &local_path {
                    if *lp != map_project_path(&ctx, &synced_path) && !synced_path.is_empty() {
                        map.retain(|(k, _)| *k != synced_path);
                        map.push((synced_path.clone(), lp.clone()));
                        ctx = home.clone().with_project_map(map.clone());
                        decoded = decode_row(&ctx, spec, row);
                    }
                }
                project_matches.push(ProjectMatch {
                    remote_id: op.pk.as_str().unwrap_or_default().to_string(),
                    name: str_of(row, "name").unwrap_or_default().to_string(),
                    synced_path,
                    local_id: local_pk.as_ref().and_then(|pk| local_by_pk.get(pk)).and_then(|l| str_of(l, "id")).map(str::to_string),
                    matched_by: matched_by.map(str::to_string),
                    local_path: String::new(),
                    on_this_machine: false,
                });
            }

            let Some(lpk) = local_pk else {
                counts.new_remote += 1;
                continue;
            };
            claimed.insert(lpk.clone());
            counts.matched += 1;
            let mut local = local_by_pk.get(&lpk).cloned().unwrap_or_default();
            if lpk != rpk {
                let old: Value = serde_json::from_str(&lpk).map_err(|e| e.to_string())?;
                rekey(conn, spec, &old, &op.pk).await?;
                counts.rekeyed += 1;
                for (c, v) in spec.pk.iter().zip(super::codec::pk_values(spec, &op.pk)?) {
                    local.insert((*c).to_string(), v);
                }
            }
            if differs(spec, &local, &decoded) {
                counts.conflicts += 1;
            }
        }

        let gone = deletes.get(spec.name);
        for pk in local_by_pk.keys() {
            if claimed.contains(pk) || remote_pks.contains(pk) {
                continue;
            }
            if gone.is_some_and(|g| g.contains(pk)) {
                counts.deleted_remote += 1;
            } else {
                counts.new_local += 1;
                local_only.push((spec.name.to_string(), pk.clone()));
            }
        }
        preview.matched += counts.matched;
        preview.rekeyed += counts.rekeyed;
        preview.new_remote += counts.new_remote;
        preview.new_local += counts.new_local;
        preview.conflicts += counts.conflicts;
        preview.deleted_remote += counts.deleted_remote;
        if counts != (TableCounts { tbl: spec.name.to_string(), ..Default::default() }) {
            preview.tables.push(counts);
        }
    }

    for m in &mut project_matches {
        m.local_path = map_project_path(&ctx, &m.synced_path);
        m.on_this_machine = Path::new(&m.local_path).is_dir();
    }
    project_matches.sort_by(|a, b| a.name.cmp(&b.name));
    preview.projects = project_matches;
    Ok(Merge { preview, local_only, map })
}

/// A local project for a remote one not present by id: same path here, then
/// same git remote (`owner/repo`), then same name.
fn match_project(
    locals: &HashMap<String, Map<String, Value>>,
    decoded: &Map<String, Value>,
    op: &Op,
    index: &ProjectIndex,
    free: &dyn Fn(&String) -> bool,
) -> Option<(String, &'static str)> {
    let mut candidates: Vec<(&String, &Map<String, Value>)> = locals.iter().filter(|(pk, _)| free(pk)).collect();
    candidates.sort_by(|a, b| a.0.cmp(b.0));
    let path = str_of(decoded, "path");
    if let Some((pk, _)) = candidates.iter().find(|(_, l)| path.is_some() && str_of(l, "path") == path) {
        return Some(((*pk).clone(), "path"));
    }
    let remote_ids = op.pk.as_str().and_then(|id| index.remote_ids.get(id));
    if let Some(rids) = remote_ids.filter(|s| !s.is_empty()) {
        let hit = candidates.iter().find(|(_, l)| {
            str_of(l, "id").and_then(|id| index.local_ids.get(id)).is_some_and(|lids| !lids.is_disjoint(rids))
        });
        if let Some((pk, _)) = hit {
            return Some(((*pk).clone(), "git_remote"));
        }
    }
    let name = str_of(decoded, "name");
    candidates
        .iter()
        .find(|(_, l)| name.is_some() && str_of(l, "name") == name)
        .map(|(pk, _)| ((*pk).clone(), "name"))
}

/// Rewrites outbox entries of `tbl` whose pk (or pk column `col`) is `old`.
async fn rekey_outbox(conn: &mut SqliteConnection, tbl: &str, col: Option<&str>, old: &Value, new: &Value) -> Result<(), String> {
    match col {
        None => {
            sqlx::query("UPDATE sync_outbox SET pk = ? WHERE tbl = ? AND pk = ?")
                .bind(canonical_json(new))
                .bind(tbl)
                .bind(canonical_json(old))
                .execute(&mut *conn)
                .await
                .map_err(err)?;
        }
        Some(col) => {
            let rows = sqlx::query("SELECT seq, pk FROM sync_outbox WHERE tbl = ?")
                .bind(tbl)
                .fetch_all(&mut *conn)
                .await
                .map_err(err)?;
            for r in rows {
                let seq: i64 = r.get(0);
                let pk: String = r.get(1);
                let Ok(Value::Object(mut m)) = serde_json::from_str::<Value>(&pk) else { continue };
                if m.get(col) != Some(old) {
                    continue;
                }
                m.insert(col.to_string(), new.clone());
                sqlx::query("UPDATE sync_outbox SET pk = ? WHERE seq = ?")
                    .bind(canonical_json(&Value::Object(m)))
                    .bind(seq)
                    .execute(&mut *conn)
                    .await
                    .map_err(err)?;
            }
        }
    }
    Ok(())
}

/// Give a local row the remote id, and every row pointing at it (REFS) too.
async fn rekey(conn: &mut SqliteConnection, spec: &TableSpec, old: &Value, new: &Value) -> Result<(), String> {
    let [pk_col] = spec.pk else {
        return Err(format!("{}: cannot re-key a composite key", spec.name));
    };
    let sql = format!("UPDATE {} SET {1} = ? WHERE {1} = ?", q(spec.name), q(pk_col));
    bind_value(bind_value(sqlx::query(&sql), new), old).execute(&mut *conn).await.map_err(err)?;
    rekey_outbox(conn, spec.name, None, old, new).await?;
    sqlx::query("DELETE FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(canonical_json(old))
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    for (_, child, col) in REFS.iter().filter(|(p, _, _)| *p == spec.name) {
        let sql = format!("UPDATE {} SET {1} = ? WHERE {1} = ?", q(child), q(col));
        bind_value(bind_value(sqlx::query(&sql), new), old).execute(&mut *conn).await.map_err(err)?;
        if let Some(child_spec) = schema::spec(child) {
            if child_spec.pk.contains(col) {
                let col = if child_spec.pk.len() == 1 { None } else { Some(*col) };
                rekey_outbox(conn, child, col, old, new).await?;
            }
        }
    }
    Ok(())
}

async fn begin_guarded(db: &Db) -> Result<sqlx::Transaction<'static, sqlx::Sqlite>, String> {
    let mut tx = db.begin().await.map_err(err)?;
    sqlx::query("PRAGMA defer_foreign_keys = ON").execute(&mut *tx).await.map_err(err)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 1 WHERE id = 1").execute(&mut *tx).await.map_err(err)?;
    Ok(tx)
}

/// What the first-join merge would do. Nothing is written.
pub async fn preview(db: &Db, home: &PathCtx, state: &RemoteState, git: &HashMap<String, String>) -> Result<BootstrapPreview, String> {
    let mut tx = begin_guarded(db).await?;
    let merged = merge(&mut tx, home, &state.ops, git).await;
    tx.rollback().await.map_err(err)?;
    let mut preview = merged?.preview;
    preview.undecryptable = state.undecryptable;
    Ok(preview)
}

/// Run the first-join merge for real: re-key + project map (one guarded
/// transaction), LWW-apply the group state, queue the local-only rows, and
/// continue pulling after it. The engine must not be running.
pub async fn commit(
    db: &Db,
    store: &SqliteSyncStore,
    home: &PathCtx,
    state: RemoteState,
    git: &HashMap<String, String>,
) -> Result<BootstrapReport, String> {
    let mut tx = begin_guarded(db).await?;
    let merged = merge(&mut tx, home, &state.ops, git).await?;
    save_project_map(&mut tx, &merged.map).await?;
    let now = wall_ms() as i64;
    for (tbl, pk) in &merged.local_only {
        sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES (?, ?, 'U', ?)")
            .bind(tbl)
            .bind(pk)
            .bind(now)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    }
    sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1").execute(&mut *tx).await.map_err(err)?;
    tx.commit().await.map_err(err)?;

    let applied = store.apply_remote(state.ops).await?;
    store.set_cursor(state.last_seq).await?;
    let mut conn = db.acquire().await.map_err(err)?;
    set_state(&mut conn, PENDING_KEY, "0").await?;
    let mut preview = merged.preview;
    preview.undecryptable = state.undecryptable;
    Ok(BootstrapReport { preview, applied, queued: merged.local_only.len() })
}

// ---- project mapping ---------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectMapping {
    pub project_id: String,
    pub name: String,
    /// Local checkout root.
    pub path: String,
    /// The root as synced to the other devices.
    pub synced_path: String,
    /// Set by the bootstrap match or by hand (else: `~` expansion only).
    pub mapped: bool,
    /// `false` = "not on this machine" (the checkout does not exist here).
    pub on_this_machine: bool,
}

pub async fn project_mappings(db: &Db, home: &PathCtx) -> Result<Vec<ProjectMapping>, String> {
    let map = load_project_map(db).await?;
    let ctx = home.clone().with_project_map(map.clone());
    let rows: Vec<(String, String, String)> = sqlx::query_as("SELECT id, name, path FROM projects ORDER BY name")
        .fetch_all(db)
        .await
        .map_err(err)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, path)| {
            let synced_path = portable_path(&ctx, &path);
            ProjectMapping {
                project_id: id,
                name,
                mapped: map.iter().any(|(k, v)| *k == synced_path && *v == path),
                on_this_machine: Path::new(&path).is_dir(),
                synced_path,
                path,
            }
        })
        .collect())
}

/// Point a synced project at its checkout on this machine: records the
/// mapping and moves every local path under the old root (projects, repos,
/// runs, ...) to the new one, without pushing anything (the synced form of
/// those paths does not change).
pub async fn set_project_mapping(db: &Db, home: &PathCtx, project_id: &str, local_path: &str) -> Result<ProjectMapping, String> {
    let local_path = local_path.trim_end_matches('/');
    if !Path::new(local_path).is_absolute() || !Path::new(local_path).is_dir() {
        return Err(format!("not a folder on this machine: {local_path}"));
    }
    let mut map = load_project_map(db).await?;
    let ctx = home.clone().with_project_map(map.clone());
    let old: String = sqlx::query_scalar("SELECT path FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(err)?
        .ok_or("project not found")?;
    let synced = portable_path(&ctx, &old);
    map.retain(|(k, _)| *k != synced);
    if expand_portable(home, &synced) != local_path {
        map.push((synced.clone(), local_path.to_string()));
    }

    let mut tx = begin_guarded(db).await?;
    if old != local_path {
        for spec in SYNCED_TABLES {
            for col in spec.path_cols {
                let sql = format!(
                    "UPDATE {t} SET {c} = ? || substr({c}, ?) WHERE {c} = ? OR substr({c}, 1, ?) = ?",
                    t = q(spec.name),
                    c = q(col),
                );
                sqlx::query(&sql)
                    .bind(local_path)
                    .bind(old.chars().count() as i64 + 1)
                    .bind(&old)
                    .bind(old.chars().count() as i64 + 1)
                    .bind(format!("{old}/"))
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| e.to_string())?;
            }
        }
    }
    save_project_map(&mut tx, &map).await?;
    sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1").execute(&mut *tx).await.map_err(err)?;
    tx.commit().await.map_err(err)?;
    project_mappings(db, home)
        .await?
        .into_iter()
        .find(|m| m.project_id == project_id)
        .ok_or_else(|| "project not found".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::clock::Hlc;
    use crate::sync::op::OP_VERSION;
    use crate::sync::SCHEMA_VERSION;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    async fn device(name: &str, home: &str) -> (Db, SqliteSyncStore, PathCtx) {
        let db = mem_db().await;
        let ctx = PathCtx::with_home(home);
        let store = SqliteSyncStore::new(db.clone(), name.into()).with_paths(ctx.clone());
        (db, store, ctx)
    }

    async fn exec(db: &Db, sql: &str) {
        sqlx::query(sql).execute(db).await.unwrap_or_else(|e| panic!("{e}: {sql}"));
    }

    async fn scalar(db: &Db, sql: &str) -> Option<String> {
        sqlx::query_scalar(sql).fetch_optional(db).await.unwrap()
    }

    async fn count(db: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
    }

    /// Everything `store` has to push, as the group state a joiner downloads.
    async fn group_state(store: &SqliteSyncStore) -> RemoteState {
        let ops = store.pending(100_000).await.unwrap();
        if let Some((seq, _)) = ops.last() {
            store.mark_pushed(*seq).await.unwrap();
        }
        RemoteState::from_ops(ops.into_iter().map(|(_, o)| o).collect(), 42)
    }

    fn table<'a>(p: &'a BootstrapPreview, tbl: &str) -> &'a TableCounts {
        p.tables.iter().find(|t| t.tbl == tbl).unwrap_or_else(|| panic!("no counts for {tbl}: {:?}", p.tables))
    }

    /// One row in every synced table, all wired together.
    async fn seed_everything(db: &Db, p: &str, home: &str) {
        let stmts = [
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'dark'), ('claude_path', '/x/claude')",
            "INSERT INTO github_accounts (id, label, username, created_at) VALUES ('gh-P', 'work', 'me', 'x')",
            "INSERT INTO gitlab_accounts (id, label, username, created_at) VALUES ('gl-P', 'gl', 'me', 'x')",
            "INSERT INTO aws_accounts (id, label, auth_method, region, created_at) VALUES ('aws-P', 'aws', 'sso', 'ap', 'x')",
            "INSERT INTO azure_accounts (id, label, created_at) VALUES ('az-P', 'az', 'x')",
            "INSERT INTO google_accounts (id, label, email, created_at) VALUES ('go-P', 'goo', 'a@b', 'x')",
            "INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('ca-P', 'max', '{home}/.claude-max', 'x')",
            "INSERT INTO servers (id, label, host, username, auth_method, created_at) VALUES ('sv-P', 'vps', 'h', 'u', 'agent', 'x')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('mcp-P', 'fs', 'stdio', 'x')",
            "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-P', 'skill', '', '{home}/s', 'x')",
            "INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('ru-P', 'rule', '', '{home}/r', 'x')",
            "INSERT INTO skill_groups (id, name, created_at, updated_at) VALUES ('sg-P', 'sgrp', 'x', 'x')",
            "INSERT INTO rule_groups (id, name, created_at, updated_at) VALUES ('rg-P', 'rgrp', 'x', 'x')",
            "INSERT INTO skill_group_members (group_id, skill_id) VALUES ('sg-P', 'sk-P')",
            "INSERT INTO rule_group_members (group_id, rule_id) VALUES ('rg-P', 'ru-P')",
            "INSERT INTO projects (id, name, path, created_at, github_account_id, github_reviewer_account_id, gitlab_account_id, aws_account_id, azure_account_id, claude_account_id) \
             VALUES ('pr-P', 'devdy', '{home}/w/devdy', 'x', 'gh-P', 'gh-P', 'gl-P', 'aws-P', 'az-P', 'ca-P')",
            "INSERT INTO repos (id, project_id, name, path, created_at) VALUES ('re-P', 'pr-P', 'app', '{home}/w/devdy/app', 'x')",
            "INSERT INTO project_skills (project_id, skill_id, applied_at) VALUES ('pr-P', 'sk-P', 'x')",
            "INSERT INTO project_rules (project_id, rule_id, target, applied_at) VALUES ('pr-P', 'ru-P', 'both', 'x')",
            "INSERT INTO project_skill_groups (project_id, group_id, enabled_at) VALUES ('pr-P', 'sg-P', 'x')",
            "INSERT INTO project_rule_groups (project_id, group_id, enabled_at) VALUES ('pr-P', 'rg-P', 'x')",
            "INSERT INTO project_mcp_servers (project_id, server_id, enabled_at) VALUES ('pr-P', 'mcp-P', 'x')",
            "INSERT INTO project_servers (project_id, server_id, role, created_at) VALUES ('pr-P', 'sv-P', 'staging', 'x')",
            "INSERT INTO runs (id, project_id, repo_id, type, created_at, claude_account_id, github_account_id, server_id, engine, session_id, output_path) \
             VALUES ('rn-P', 'pr-P', 're-P', 'session', 'x', 'ca-P', 'gh-P', 'sv-P', 'claude', 'sess-P', '{home}/w/devdy/.devdy/runs/rn-P.md')",
            "INSERT INTO run_usage (id, run_id, project_id, claude_account_id, engine, created_at) VALUES ('ru1-P', 'rn-P', 'pr-P', 'ca-P', 'claude', 'x')",
            "INSERT INTO deleted_sessions (project_id, session_id, deleted_at) VALUES ('pr-P', 'gone-P', 'x')",
            "INSERT INTO notes (id, title, content, project_id, run_id, position, created_at, updated_at) VALUES ('no-P', 'n', 'body', 'pr-P', 'rn-P', 0, 'x', 'x')",
            "INSERT INTO todos (id, text, project_id, run_id, position, created_at) VALUES ('to-P', 't', 'pr-P', 'rn-P', 0, 'x')",
            "INSERT INTO note_runs (note_id, run_id, project_id, source, linked_at) VALUES ('no-P', 'rn-P', 'pr-P', 'manual', 'x')",
            "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('to-P', 'rn-P', 'pr-P', 'manual', 'x')",
            "INSERT INTO slack_identities (team_id, user_id, fetched_at) VALUES ('T-P', 'U', 'x')",
            "INSERT INTO slack_users (team_id, user_id, fetched_at) VALUES ('T-P', 'U', 'x')",
            "INSERT INTO slack_channels (team_id, channel_id, fetched_at) VALUES ('T-P', 'C', 'x')",
            "INSERT INTO slack_threads (id, project_id, channel_id, thread_ts, created_at, updated_at) VALUES ('st-P', 'pr-P', 'C-P', '1.0', 'x', 'x')",
            "INSERT INTO slack_messages (thread_id, ts, seq, src_hash) VALUES ('st-P', '1.0', 0, 'h')",
            "INSERT INTO slack_files (id, thread_id, slack_file_id, name, created_at) VALUES ('sf-P', 'st-P', 'F-P', 'f', 'x')",
            "INSERT INTO slack_thread_runs (thread_id, run_id, project_id, source, linked_at) VALUES ('st-P', 'rn-P', 'pr-P', 'manual', 'x')",
            "INSERT INTO pulse_watches (id, project_id, source_type, source_ref, source_url, added_via, slack_thread_id, added_run_id, created_at, updated_at) \
             VALUES ('pw-P', 'pr-P', 'github_issue', 'o/r#P', 'u', 'button', 'st-P', 'rn-P', 'x', 'x')",
            "INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, created_at) VALUES ('pd-P', 'pw-P', 'h', 'on_track', 'me', 'x')",
        ];
        for s in stmts {
            exec(db, &s.replace("{home}", home).replace("-P", &format!("-{p}")).replace("#P", &format!("#{p}"))).await;
        }
    }

    #[tokio::test]
    async fn enqueue_all_covers_every_synced_table() {
        let (db, store, _) = device("a", "/Users/a").await;
        seed_everything(&db, "a", "/Users/a").await;
        exec(&db, "DELETE FROM sync_outbox").await;
        let n = enqueue_all(&db).await.unwrap();
        assert!(n > 0);
        let ops = store.pending(100_000).await.unwrap();
        let tables: HashSet<&str> = ops.iter().map(|(_, o)| o.tbl.as_str()).collect();
        for spec in SYNCED_TABLES {
            assert!(tables.contains(spec.name), "enqueue_all missed {}", spec.name);
        }
        // Settings: portable keys only.
        assert!(ops.iter().filter(|(_, o)| o.tbl == "settings").all(|(_, o)| schema::spec("settings").unwrap().accepts(o.pk.as_str())));
        assert!(!ops.iter().any(|(_, o)| o.pk == json!("claude_path")));
    }

    #[tokio::test]
    async fn rekeys_local_rows_across_every_fk_chain() {
        let (db_a, a, _) = device("a", "/Users/alice").await;
        let (db_b, b, home_b) = device("b", "/Users/bob").await;
        seed_everything(&db_a, "A", "/Users/alice").await;
        // Same things on b, created independently (different ids), plus
        // machine-local rows that point at them.
        seed_everything(&db_b, "B", "/Users/bob").await;
        exec(&db_b, "INSERT INTO project_secrets (project_id, pat_keyring_key) VALUES ('pr-B', 'k')").await;
        exec(&db_b, "INSERT INTO sync_conflicts (id, project_id, skill_id, detected_at, local_hash, source_hash) VALUES ('c1', 'pr-B', 'sk-B', 'x', 'l', 's')").await;
        exec(&db_b, "INSERT INTO conductor_sessions (id, project_id, created_at) VALUES ('cs', 'pr-B', 'x')").await;
        exec(&db_b, "INSERT INTO pulse_requests (id, kind, watch_id, project_id, created_at) VALUES ('rq', 'refresh', 'pw-B', 'pr-B', 'x')").await;
        // b's data predates a's: the group's version of matched rows wins.
        exec(&db_b, "UPDATE sync_outbox SET changed_at = 1").await;
        let state = group_state(&a).await;
        let git = HashMap::new();

        let p = preview(&db_b, &home_b, &state, &git).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT id FROM projects").await.unwrap(), "pr-B", "preview writes nothing");
        for tbl in ["github_accounts", "gitlab_accounts", "aws_accounts", "azure_accounts", "google_accounts", "claude_accounts",
                    "servers", "mcp_servers", "skills", "rules", "skill_groups", "rule_groups", "projects", "repos"] {
            let t = table(&p, tbl);
            assert_eq!((t.matched, t.rekeyed, t.new_remote, t.new_local), (1, 1, 0, 0), "{tbl}");
        }
        for tbl in ["skill_group_members", "rule_group_members", "project_skills", "project_rules",
                    "project_skill_groups", "project_rule_groups", "project_mcp_servers", "project_servers"] {
            let t = table(&p, tbl);
            assert_eq!((t.matched, t.rekeyed, t.new_remote, t.new_local), (1, 0, 0, 0), "{tbl} matches by pk after its parents");
        }
        for tbl in ["runs", "notes", "todos", "slack_threads", "pulse_watches"] {
            let t = table(&p, tbl);
            assert_eq!((t.matched, t.new_remote, t.new_local), (0, 1, 1), "{tbl}");
        }
        assert_eq!(table(&p, "settings").matched, 1);

        let report = commit(&db_b, &b, &home_b, state, &git).await.unwrap();
        assert_eq!(report.applied.deferred, 0, "{report:?}");
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM pragma_foreign_key_check").await, 0);
        // Every parent got a's id...
        for (tbl, id) in [("projects", "pr-A"), ("repos", "re-A"), ("skills", "sk-A"), ("rules", "ru-A"), ("github_accounts", "gh-A"),
                          ("claude_accounts", "ca-A"), ("servers", "sv-A"), ("mcp_servers", "mcp-A"), ("skill_groups", "sg-A")] {
            assert_eq!(count(&db_b, &format!("SELECT COUNT(*) FROM {tbl}")).await, 1, "{tbl}");
            assert_eq!(scalar(&db_b, &format!("SELECT id FROM {tbl}")).await.unwrap(), id);
        }
        // ...and b's own rows follow it, synced or machine-local.
        let one = |sql: &'static str| async { count(&db_b, sql).await };
        assert_eq!(one("SELECT COUNT(*) FROM runs WHERE id = 'rn-B' AND project_id = 'pr-A' AND repo_id = 're-A' AND claude_account_id = 'ca-A' AND github_account_id = 'gh-A' AND server_id = 'sv-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM run_usage WHERE id = 'ru1-B' AND project_id = 'pr-A' AND claude_account_id = 'ca-A' AND run_id = 'rn-B'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM notes WHERE id = 'no-B' AND project_id = 'pr-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM todos WHERE id = 'to-B' AND project_id = 'pr-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM note_runs WHERE project_id = 'pr-A'").await, 2);
        assert_eq!(one("SELECT COUNT(*) FROM slack_threads WHERE project_id = 'pr-A'").await, 2);
        assert_eq!(one("SELECT COUNT(*) FROM pulse_watches WHERE project_id = 'pr-A'").await, 2);
        assert_eq!(one("SELECT COUNT(*) FROM deleted_sessions WHERE project_id = 'pr-A'").await, 2);
        assert_eq!(one("SELECT COUNT(*) FROM projects WHERE github_account_id = 'gh-A' AND github_reviewer_account_id = 'gh-A' AND gitlab_account_id = 'gl-A' AND aws_account_id = 'aws-A' AND azure_account_id = 'az-A' AND claude_account_id = 'ca-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM project_skills WHERE project_id = 'pr-A' AND skill_id = 'sk-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM project_servers WHERE project_id = 'pr-A' AND server_id = 'sv-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM skill_group_members WHERE group_id = 'sg-A' AND skill_id = 'sk-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM rule_group_members WHERE group_id = 'rg-A' AND rule_id = 'ru-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM project_secrets WHERE project_id = 'pr-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM sync_conflicts WHERE project_id = 'pr-A' AND skill_id = 'sk-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM conductor_sessions WHERE project_id = 'pr-A'").await, 1);
        assert_eq!(one("SELECT COUNT(*) FROM pulse_requests WHERE project_id = 'pr-A' AND watch_id = 'pw-B'").await, 1);

        // b pushes only what the group lacks, already under the group's ids.
        let ops = b.pending(100_000).await.unwrap();
        assert!(!ops.is_empty());
        for (_, op) in &ops {
            let s = canonical_json(&op.pk) + &op.row.as_ref().map(|r| Value::Object(r.clone()).to_string()).unwrap_or_default();
            for old in ["pr-B", "sk-B", "ru-B", "gh-B", "ca-B", "sv-B", "re-B", "mcp-B", "sg-B", "rg-B"] {
                assert!(!s.contains(&format!("\"{old}\"")), "{} {s} still carries {old}", op.tbl);
            }
            assert_eq!(op.kind, OpKind::Upsert);
        }
        let pushed: HashSet<&str> = ops.iter().map(|(_, o)| o.tbl.as_str()).collect();
        assert!(pushed.contains("runs") && pushed.contains("notes") && !pushed.contains("projects"), "{pushed:?}");
        assert!(!is_pending(&db_b).await.unwrap());
        assert_eq!(b.cursor().await.unwrap(), 42);
    }

    #[tokio::test]
    async fn preview_counts_new_conflicting_and_deleted_rows() {
        let (db_a, a, _) = device("a", "/Users/a").await;
        let (db_b, _b, home_b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'remote', 0, 'x'), ('t2', 'only-a', 1, 'x')").await;
        exec(&db_a, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('s-a', 'same', 'A', '/s', 'x')").await;
        exec(&db_b, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'local', 0, 'x'), ('t3', 'only-b', 2, 'x'), ('t4', 'gone', 3, 'x')").await;
        exec(&db_b, "INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('s-b', 'same', 'A', '/s', 'x')").await;
        let mut state = group_state(&a).await;
        state.ops.push(Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc: Hlc::new(1, 0, "a"), tbl: "todos".into(), pk: json!("t4"), kind: OpKind::Delete, row: None });
        let p = preview(&db_b, &home_b, &state, &HashMap::new()).await.unwrap();
        assert_eq!(*table(&p, "todos"), TableCounts { tbl: "todos".into(), matched: 1, rekeyed: 0, new_remote: 1, new_local: 1, conflicts: 1, deleted_remote: 1 });
        assert_eq!(*table(&p, "skills"), TableCounts { tbl: "skills".into(), matched: 1, rekeyed: 1, ..Default::default() });
        let settings = table(&p, "settings");
        assert_eq!(settings.new_local, p.new_local - 1, "only b's settings rows besides t3");
        assert_eq!((p.matched, p.rekeyed, p.conflicts, p.deleted_remote), (2, 1, 1, 1));
    }

    #[test]
    fn git_urls_normalize_across_forms() {
        let n = |u: &str| normalize_git_url(u);
        let want = Some("github.com/thainph/devdy".to_string());
        assert_eq!(n("git@github.com:thainph/devdy.git"), want);
        assert_eq!(n("https://github.com/ThainPH/devdy"), want);
        assert_eq!(n("https://user@github.com/thainph/devdy.git/"), want);
        assert_eq!(n("ssh://git@github.com:22/thainph/devdy.git"), want);
        assert_eq!(n("/srv/git/devdy"), None);
        assert_eq!(n("./x:y/z"), None);
    }

    async fn project_op(a_db: &Db, a: &SqliteSyncStore) -> RemoteState {
        let _ = a_db;
        group_state(a).await
    }

    #[tokio::test]
    async fn projects_map_by_path_then_git_remote_then_name() {
        let (db_a, a, _) = device("a", "/Users/alice").await;
        let (db_b, b, home_b) = device("b", "/Users/bob").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p-path', 'same-path', '/Users/alice/w/one', 'x')").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at, github_owner, github_repo) VALUES ('p-git', 'devdy', '/Users/alice/w/devdy', 'x', 'thainph', 'devdy')").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p-name', 'notes', '/Users/alice/w/notes', 'x')").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p-away', 'elsewhere', '/Users/alice/w/away', 'x')").await;
        exec(&db_a, "INSERT INTO runs (id, project_id, type, created_at, output_path) VALUES ('r1', 'p-git', 'session', 'x', '/Users/alice/w/devdy/.devdy/runs/r1.md')").await;

        exec(&db_b, "INSERT INTO projects (id, name, path, created_at) VALUES ('b1', 'other-name', '/Users/bob/w/one', 'x')").await;
        exec(&db_b, "INSERT INTO projects (id, name, path, created_at) VALUES ('b2', 'my-devdy', '/Volumes/code/devdy', 'x')").await;
        exec(&db_b, "INSERT INTO projects (id, name, path, created_at) VALUES ('b3', 'notes', '/Users/bob/elsewhere/notes', 'x')").await;
        let git: HashMap<String, String> = [("/Volumes/code/devdy".to_string(), "git@github.com:ThainPH/devdy.git".to_string())].into();

        exec(&db_b, "DELETE FROM sync_outbox").await; // b's rows predate sync
        let state = project_op(&db_a, &a).await;
        let p = preview(&db_b, &home_b, &state, &git).await.unwrap();
        let by: HashMap<&str, (Option<&str>, &str)> =
            p.projects.iter().map(|m| (m.remote_id.as_str(), (m.matched_by.as_deref(), m.local_path.as_str()))).collect();
        assert_eq!(by["p-path"], (Some("path"), "/Users/bob/w/one"));
        assert_eq!(by["p-git"], (Some("git_remote"), "/Volumes/code/devdy"));
        assert_eq!(by["p-name"], (Some("name"), "/Users/bob/elsewhere/notes"));
        assert_eq!(by["p-away"], (None, "/Users/bob/w/away"));
        assert!(!p.projects.iter().find(|m| m.remote_id == "p-away").unwrap().on_this_machine);

        commit(&db_b, &b, &home_b, state, &git).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT path FROM projects WHERE id = 'p-git'").await.unwrap(), "/Volumes/code/devdy");
        assert_eq!(scalar(&db_b, "SELECT name FROM projects WHERE id = 'p-path'").await.unwrap(), "same-path", "remote row wins");
        assert_eq!(scalar(&db_b, "SELECT path FROM projects WHERE id = 'p-name'").await.unwrap(), "/Users/bob/elsewhere/notes");
        assert_eq!(scalar(&db_b, "SELECT path FROM projects WHERE id = 'p-away'").await.unwrap(), "/Users/bob/w/away");
        assert_eq!(scalar(&db_b, "SELECT output_path FROM runs WHERE id = 'r1'").await.unwrap(), "/Volumes/code/devdy/.devdy/runs/r1.md");
        let map: HashMap<String, String> = load_project_map(&db_b).await.unwrap().into_iter().collect();
        assert_eq!(map.get("~/w/devdy").map(String::as_str), Some("/Volumes/code/devdy"));
        assert!(!map.contains_key("~/w/one"), "same path needs no mapping");

        // Later ops under a mapped project land under the local root, and b
        // sends its paths in the group's form.
        exec(&db_a, "INSERT INTO runs (id, project_id, type, created_at, output_path) VALUES ('r2', 'p-git', 'session', 'x', '/Users/alice/w/devdy/.devdy/runs/r2.md')").await;
        let ops: Vec<Op> = a.pending(10).await.unwrap().into_iter().map(|(_, o)| o).collect();
        b.apply_remote(ops).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT output_path FROM runs WHERE id = 'r2'").await.unwrap(), "/Volumes/code/devdy/.devdy/runs/r2.md");
        exec(&db_b, "UPDATE projects SET name = 'devdy!' WHERE id = 'p-git'").await;
        let ops = b.pending(10).await.unwrap();
        let row = ops.iter().find(|(_, o)| o.tbl == "projects").unwrap().1.row.clone().unwrap();
        assert_eq!(row["path"], "~/w/devdy");
    }

    #[tokio::test]
    async fn project_mapping_set_moves_paths_without_pushing() {
        let home_dir = std::env::temp_dir().join(format!("devdy-bootstrap-{}", uuid::Uuid::new_v4().simple()));
        let checkout = home_dir.join("code/devdy");
        std::fs::create_dir_all(&checkout).unwrap();
        let home = home_dir.to_string_lossy().to_string();
        let (db, store, ctx) = device("b", &home).await;
        exec(&db, &format!("INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'devdy', '{home}/w/devdy', 'x')")).await;
        exec(&db, &format!("INSERT INTO runs (id, project_id, type, created_at, output_path, input_path) VALUES ('r', 'p', 'session', 'x', '{home}/w/devdy/.devdy/runs/r.md', '{home}/w/devdy2/in')")).await;
        exec(&db, "DELETE FROM sync_outbox").await;
        let list = project_mappings(&db, &ctx).await.unwrap();
        assert_eq!((list[0].mapped, list[0].on_this_machine, list[0].synced_path.as_str()), (false, false, "~/w/devdy"));

        let m = set_project_mapping(&db, &ctx, "p", &checkout.to_string_lossy()).await.unwrap();
        assert_eq!((m.mapped, m.on_this_machine, m.synced_path.as_str()), (true, true, "~/w/devdy"));
        assert_eq!(scalar(&db, "SELECT output_path FROM runs").await.unwrap(), format!("{home}/code/devdy/.devdy/runs/r.md"));
        assert_eq!(scalar(&db, "SELECT input_path FROM runs").await.unwrap(), format!("{home}/w/devdy2/in"), "sibling prefix untouched");
        assert!(store.pending(10).await.unwrap().is_empty());
        assert!(set_project_mapping(&db, &ctx, "p", "/definitely/not/here").await.is_err());
        std::fs::remove_dir_all(&home_dir).ok();
    }

    #[tokio::test]
    async fn git_origin_reads_a_real_checkout() {
        let dir = std::env::temp_dir().join(format!("devdy-git-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.to_string_lossy().to_string();
        let git = |args: &[&str]| std::process::Command::new("git").args(["-C", &path]).args(args).output().unwrap();
        if !git(&["init", "-q"]).status.success() {
            return; // no git on this machine
        }
        git(&["remote", "add", "origin", "git@github.com:o/r.git"]);
        assert_eq!(git_origin(&path).as_deref(), Some("git@github.com:o/r.git"));
        assert_eq!(git_origin("/definitely/not/here"), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    async fn note(db: &Db, id: &str) -> String {
        scalar(db, &format!("SELECT content FROM notes WHERE id = '{id}'")).await.unwrap()
    }

    /// Push `from`'s pending ops into `to`.
    async fn sync(from: &SqliteSyncStore, to: &SqliteSyncStore) -> ApplyReport {
        let ops = from.pending(1000).await.unwrap();
        if let Some((seq, _)) = ops.last() {
            from.mark_pushed(*seq).await.unwrap();
        }
        to.apply_remote(ops.into_iter().map(|(_, o)| o).collect()).await.unwrap()
    }

    #[tokio::test]
    async fn note_base_hash_decides_conflicts() {
        let (db_a, a, _) = device("a", "/Users/a").await;
        let (db_b, b, _) = device("b", "/Users/b").await;
        let far = 9_000_000_000_000i64;
        exec(&db_a, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n', 'Plan', 'v0', 0, 'x', 'x')").await;
        sync(&a, &b).await;
        let title = |db: &Db| { let db = db.clone(); async move { scalar(&db, "SELECT title FROM notes WHERE id = 'n'").await.unwrap() } };

        // a edits the body, b (later) only the title: no copy, both edits kept.
        exec(&db_a, "UPDATE notes SET content = 'v1' WHERE id = 'n'").await;
        exec(&db_b, "UPDATE notes SET title = 'Plan B' WHERE id = 'n'").await;
        exec(&db_b, &format!("UPDATE sync_outbox SET changed_at = {far}")).await;
        let r = sync(&a, &b).await;
        assert_eq!((r.applied, r.conflicts), (1, 0));
        assert_eq!((note(&db_b, "n").await, title(&db_b).await), ("v1".into(), "Plan B".into()));
        sync(&b, &a).await;
        assert_eq!((note(&db_a, "n").await, title(&db_a).await), ("v1".into(), "Plan B".into()));

        // b edits the body, a (later) only the title: b's body survives a's newer row.
        exec(&db_b, "UPDATE notes SET content = 'v2-b' WHERE id = 'n'").await;
        exec(&db_b, "UPDATE sync_outbox SET changed_at = 1").await;
        exec(&db_a, "UPDATE notes SET title = 'Plan C' WHERE id = 'n'").await;
        exec(&db_a, &format!("UPDATE sync_outbox SET changed_at = {}", far + 1000)).await;
        let r = sync(&a, &b).await;
        assert_eq!((r.applied, r.conflicts), (1, 0));
        assert_eq!((note(&db_b, "n").await, title(&db_b).await), ("v2-b".into(), "Plan C".into()));
        sync(&b, &a).await;
        assert_eq!((note(&db_a, "n").await, title(&db_a).await), ("v2-b".into(), "Plan C".into()));
        for db in [&db_a, &db_b] {
            assert_eq!(count(db, "SELECT COUNT(*) FROM notes").await, 1);
        }

        // Both push a new body before seeing the other's: nothing is unpushed
        // any more, yet the side receiving the newer op keeps its own as a copy
        // (the op's base is not its body), the other just skips the older op.
        exec(&db_a, "UPDATE notes SET content = 'v3-a' WHERE id = 'n'").await;
        exec(&db_b, "UPDATE notes SET content = 'v3-b' WHERE id = 'n'").await;
        exec(&db_b, &format!("UPDATE sync_outbox SET changed_at = {}", far + 2000)).await;
        let ops_a = a.pending(10).await.unwrap();
        let ops_b = b.pending(10).await.unwrap();
        a.mark_pushed(ops_a.last().unwrap().0).await.unwrap();
        b.mark_pushed(ops_b.last().unwrap().0).await.unwrap();
        assert!(ops_b[0].1.hlc > ops_a[0].1.hlc);
        let on_a = a.apply_remote(ops_b.into_iter().map(|(_, o)| o).collect()).await.unwrap();
        let on_b = b.apply_remote(ops_a.into_iter().map(|(_, o)| o).collect()).await.unwrap();
        assert_eq!((on_a.applied, on_a.conflicts, on_b.skipped_older), (1, 1, 1));
        sync(&a, &b).await; // the copy
        for db in [&db_a, &db_b] {
            assert_eq!(note(db, "n").await, "v3-b");
            assert_eq!(scalar(db, "SELECT content FROM notes WHERE id != 'n'").await.unwrap(), "v3-a");
            assert_eq!(count(db, "SELECT COUNT(*) FROM notes").await, 2);
        }

        // An edit made on top of the received version is never a conflict.
        exec(&db_b, "UPDATE notes SET content = 'v4-b' WHERE id = 'n'").await;
        let r = sync(&b, &a).await;
        assert_eq!((r.applied, r.conflicts), (1, 0));
        exec(&db_a, "UPDATE notes SET content = 'v5-a' WHERE id = 'n'").await;
        let r = sync(&a, &b).await;
        assert_eq!((r.applied, r.conflicts), (1, 0));
        assert_eq!(note(&db_b, "n").await, "v5-a");
    }

    #[tokio::test]
    async fn first_join_note_with_other_body_keeps_a_copy() {
        let (db_a, a, _) = device("a", "/Users/a").await;
        let (db_b, b, home_b) = device("b", "/Users/b").await;
        // The same note (same id, e.g. from a backup) edited on both machines.
        exec(&db_a, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n', 'T', 'a-body', 0, 'x', 'x')").await;
        exec(&db_b, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n', 'T', 'b-body', 0, 'x', 'x')").await;
        exec(&db_b, "DELETE FROM sync_outbox").await; // edited before sync existed
        let state = group_state(&a).await;
        let r = commit(&db_b, &b, &home_b, state, &HashMap::new()).await.unwrap();
        assert_eq!(r.applied.conflicts, 1);
        assert_eq!(note(&db_b, "n").await, "a-body");
        assert_eq!(scalar(&db_b, "SELECT content FROM notes WHERE id != 'n'").await.unwrap(), "b-body");
    }

    #[tokio::test]
    async fn refs_cover_declared_foreign_keys() {
        let db = mem_db().await;
        let rekeyable: HashSet<&str> = SYNCED_TABLES.iter().filter(|s| s.natural_key != s.pk).map(|s| s.name).collect();
        let fks: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT m.name, f.\"from\", f.\"table\" FROM sqlite_master m, pragma_foreign_key_list(m.name) f WHERE m.type = 'table'",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        for (child, col, parent) in fks {
            if rekeyable.contains(parent.as_str()) {
                assert!(REFS.contains(&(parent.as_str(), child.as_str(), col.as_str())), "REFS misses {child}.{col} -> {parent}");
            }
        }
        let mut conn = db.acquire().await.unwrap();
        for (parent, child, col) in REFS {
            let cols = table_columns(&mut conn, child).await.unwrap();
            assert!(cols.iter().any(|c| c == col), "{child}.{col} does not exist");
            assert!(schema::spec(parent).is_some(), "{parent} is not synced");
        }
    }
}
