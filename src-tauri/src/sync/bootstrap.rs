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
use super::store::{is_extra_tbl, ApplyReport, SqliteSyncStore, SyncStore};
use super::transport::Connection;
use crate::db::Db;

/// `sync_state` key: JSON object `{ "<synced portable root>": "<local root>" }`.
pub const PROJECT_MAP_KEY: &str = "project_path_map";
/// `sync_state` key: "1" between joining a group and committing the merge;
/// the engine does not start meanwhile (it would push rows with local ids).
pub const PENDING_KEY: &str = "bootstrap_pending";
const PULL_LIMIT: u64 = 500;
/// Remote ops applied per write transaction by [`commit`] (a long write
/// transaction would starve the other writers, e.g. `sidecar-mcp`).
const APPLY_CHUNK: usize = 500;
/// `sync_state` key: JSON list of per-id app-data folders still to rename
/// after a re-key (see [`DIR_KEYED`], [`run_dir_moves`]).
const DIR_MOVES_KEY: &str = "dir_moves";

/// Folders under app data named after a row id, which follow the row when it
/// is re-keyed: `(table, folder)`.
pub static DIR_KEYED: &[(&str, &str)] = &[
    ("claude_accounts", "claude-accounts"),
    ("azure_accounts", "azure-accounts"),
    ("servers", "server-keys"),
    ("projects", "aws-projects"),
];

/// Set while a first-join merge is being committed: run starts are refused
/// meanwhile (the merge re-keys the ids a new sidecar would be handed).
static COMMITTING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Error code of a run start refused during [`commit`].
pub const COMMITTING_CODE: &str = "sync_bootstrap_in_progress";

/// Held for the whole commit (live-run check included); dropping it lets runs
/// start again.
pub struct CommitGuard(());

impl Drop for CommitGuard {
    fn drop(&mut self) {
        COMMITTING.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Block run starts until the guard drops; fails when a commit is already on.
pub fn begin_commit() -> Result<CommitGuard, String> {
    COMMITTING
        .compare_exchange(false, true, std::sync::atomic::Ordering::SeqCst, std::sync::atomic::Ordering::SeqCst)
        .map(|_| CommitGuard(()))
        .map_err(|_| format!("{COMMITTING_CODE}: a sync merge is already running"))
}

/// Run-start hook (`start_run_inner`, resume): refused while a merge commits.
pub fn refuse_if_committing() -> Result<(), String> {
    if COMMITTING.load(std::sync::atomic::Ordering::SeqCst) {
        return Err(format!("{COMMITTING_CODE}: sync is merging this device's data; try again in a moment"));
    }
    Ok(())
}

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

/// Just joined a group (or about to): hold the engine until the merge is
/// committed. Also used to re-merge with a relay that lost ops (N15).
pub async fn mark_joined(db: &Db) -> Result<(), String> {
    let mut conn = db.acquire().await.map_err(err)?;
    set_state(&mut conn, PENDING_KEY, "1").await
}

pub async fn clear_pending(db: &Db) -> Result<(), String> {
    let mut conn = db.acquire().await.map_err(err)?;
    set_state(&mut conn, PENDING_KEY, "0").await
}

/// This device just created a group: it is the reference state, so upload
/// everything from the start of the (empty) group's log. The pending flag is
/// cleared last: a crash before that resumes at the merge step.
pub async fn prepare_new_group(db: &Db) -> Result<usize, String> {
    let device = super::ensure_device(db).await?;
    SqliteSyncStore::new(db.clone(), device).set_cursor(0).await?;
    super::set_member(db, true).await?;
    let n = enqueue_all(db).await?;
    clear_pending(db).await?;
    Ok(n)
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

        // Projects: exact path matches across all remote rows come first, so a
        // looser match (git remote, name) of an earlier row can't claim them.
        let mut by_path: HashMap<String, String> = HashMap::new();
        if spec.name == "projects" {
            let mut locals: Vec<(&String, &Map<String, Value>)> = local_by_pk.iter().filter(|(pk, _)| free(pk, &claimed)).collect();
            locals.sort_by(|a, b| a.0.cmp(b.0));
            for op in remote_rows.iter().filter(|op| !local_by_pk.contains_key(&op.pk_key())) {
                let Some(path) = op.row.as_ref().map(|r| decode_row(&ctx, spec, r)).and_then(|d| str_of(&d, "path").map(str::to_string)) else {
                    continue;
                };
                let taken: HashSet<&String> = by_path.values().collect();
                if let Some((pk, _)) = locals.iter().find(|(pk, l)| !taken.contains(pk) && str_of(l, "path") == Some(path.as_str())) {
                    by_path.insert(op.pk_key(), (*pk).clone());
                }
            }
        }
        let path_taken: HashSet<String> = by_path.values().cloned().collect();

        for op in remote_rows {
            let Some(row) = op.row.as_ref() else { continue };
            let rpk = op.pk_key();
            let mut decoded = decode_row(&ctx, spec, row);
            let mut matched_by: Option<&str> = None;
            let mut local_pk: Option<String> = None;
            if local_by_pk.contains_key(&rpk) {
                matched_by = Some("id");
                local_pk = Some(rpk.clone());
            } else if let Some(pk) = by_path.get(&rpk) {
                matched_by = Some("path");
                local_pk = Some(pk.clone());
            } else if spec.name == "projects" {
                let free = |pk: &String| free(pk, &claimed) && !path_taken.contains(pk);
                if let Some((pk, by)) = match_project(&local_by_pk, &decoded, op, &projects_index, &free) {
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
                rekey(conn, spec, &old, &op.pk, RekeyOpts::BOOTSTRAP).await?;
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
pub(super) async fn rekey_outbox(conn: &mut SqliteConnection, tbl: &str, col: Option<&str>, old: &Value, new: &Value) -> Result<(), String> {
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

/// How a re-key treats the sync bookkeeping.
#[derive(Debug, Clone, Copy)]
pub struct RekeyOpts {
    /// Keep the row's last known HLC under the new id, so the op that caused
    /// the re-key is still judged by last-writer-wins (apply). The first-join
    /// merge drops it instead.
    pub keep_hlc: bool,
    /// Queue the moved rows for push: the old id as a delete, the row and
    /// every child that now points at the new id as upserts, so the relay log
    /// stops referring to the old id (apply, after pairing).
    pub repush: bool,
}

impl RekeyOpts {
    pub const BOOTSTRAP: RekeyOpts = RekeyOpts { keep_hlc: false, repush: false };
    pub const APPLY: RekeyOpts = RekeyOpts { keep_hlc: true, repush: true };
}

async fn queue_push(conn: &mut SqliteConnection, tbl: &str, pk: &str, op: &str, changed_at: i64) -> Result<(), String> {
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

/// Give a local row the remote id, and every row pointing at it (REFS) too;
/// the Keychain items keyed by its id follow after the commit
/// (`sync::secrets::run_key_moves`).
pub async fn rekey(conn: &mut SqliteConnection, spec: &TableSpec, old: &Value, new: &Value, opts: RekeyOpts) -> Result<(), String> {
    let [pk_col] = spec.pk else {
        return Err(format!("{}: cannot re-key a composite key", spec.name));
    };
    let (old_key, new_key) = (canonical_json(old), canonical_json(new));
    let now = wall_ms() as i64;
    let sql = format!("UPDATE {} SET {1} = ? WHERE {1} = ?", q(spec.name), q(pk_col));
    bind_value(bind_value(sqlx::query(&sql), new), old).execute(&mut *conn).await.map_err(err)?;
    rekey_outbox(conn, spec.name, None, old, new).await?;
    if opts.keep_hlc {
        sqlx::query("UPDATE OR IGNORE sync_rows SET pk = ? WHERE tbl = ? AND pk = ?")
            .bind(&new_key)
            .bind(spec.name)
            .bind(&old_key)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    sqlx::query("DELETE FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(spec.name)
        .bind(&old_key)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    super::secrets::queue_key_move(conn, spec.name, old, new).await?;
    if let (Some(from), Some(to)) = (old.as_str(), new.as_str()) {
        queue_dir_move(conn, spec.name, from, to, opts.repush).await?;
    }
    if opts.repush {
        queue_push(conn, spec.name, &old_key, "D", now).await?;
        // Not a local change (dated 0): the kept HLC alone decides whether the
        // op that caused the re-key wins (which drops this entry).
        queue_push(conn, spec.name, &new_key, "U", 0).await?;
    }
    for (_, child, col) in REFS.iter().filter(|(p, _, _)| *p == spec.name) {
        let child_spec = schema::spec(child);
        let moved: Vec<String> = match child_spec.filter(|_| opts.repush) {
            Some(cs) => {
                let sql = format!("SELECT {} FROM {} WHERE {} = ?", pk_sql(cs, &q(child)), q(child), q(col));
                let rows = bind_value(sqlx::query(&sql), old).fetch_all(&mut *conn).await.map_err(err)?;
                rows.iter().map(|r| r.get::<String, _>(0)).collect()
            }
            None => Vec::new(),
        };
        let sql = format!("UPDATE {} SET {1} = ? WHERE {1} = ?", q(child), q(col));
        bind_value(bind_value(sqlx::query(&sql), new), old).execute(&mut *conn).await.map_err(err)?;
        let Some(cs) = child_spec else { continue };
        if cs.pk.contains(col) {
            let col_in_pk = if cs.pk.len() == 1 { None } else { Some(*col) };
            rekey_outbox(conn, child, col_in_pk, old, new).await?;
        }
        for pk in moved {
            match serde_json::from_str::<Value>(&pk) {
                Ok(Value::Object(mut m)) if cs.pk.contains(col) => {
                    queue_push(conn, child, &pk, "D", now).await?;
                    m.insert((*col).to_string(), new.clone());
                    queue_push(conn, child, &canonical_json(&Value::Object(m)), "U", now).await?;
                }
                Ok(_) if cs.pk == [*col] => {
                    queue_push(conn, child, &pk, "D", now).await?;
                    queue_push(conn, child, &new_key, "U", now).await?;
                }
                Ok(_) => queue_push(conn, child, &pk, "U", now).await?,
                Err(_) => {}
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
struct DirMove {
    dir: String,
    from: String,
    to: String,
}

async fn load_dir_moves(conn: &mut SqliteConnection) -> Result<Vec<DirMove>, String> {
    Ok(get_state(conn, DIR_MOVES_KEY).await?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default())
}

async fn save_dir_moves(conn: &mut SqliteConnection, moves: &[DirMove]) -> Result<(), String> {
    set_state(conn, DIR_MOVES_KEY, &serde_json::to_string(moves).map_err(|e| e.to_string())?).await
}

/// The row `from` of `tbl` (already re-keyed to `to`) owns a folder under app
/// data named after its id: point every path into that folder at the new
/// folder name (the row's own path column, run logs and transcripts, file
/// records...) and queue the folder rename for after the transaction
/// ([`run_dir_moves`]). With `repush` the rewritten synced rows are queued for
/// push, so the relay stops handing out the old paths.
async fn queue_dir_move(conn: &mut SqliteConnection, tbl: &str, from: &str, to: &str, repush: bool) -> Result<(), String> {
    let Some((_, dir)) = DIR_KEYED.iter().find(|(t, _)| *t == tbl) else { return Ok(()) };
    let (old_seg, new_seg) = (format!("/{dir}/{from}"), format!("/{dir}/{to}"));
    let now = wall_ms() as i64;
    for spec in SYNCED_TABLES {
        for col in spec.path_cols {
            let cond = under_dir_sql(col);
            let moved: Vec<String> = if repush {
                let sql = format!("SELECT {} FROM {} WHERE {cond}", pk_sql(spec, &q(spec.name)), q(spec.name));
                sqlx::query_scalar(&sql).bind(&old_seg).fetch_all(&mut *conn).await.map_err(err)?
            } else {
                Vec::new()
            };
            let sql = format!("UPDATE {} SET {} = {} WHERE {cond}", q(spec.name), q(col), moved_path_sql(col));
            sqlx::query(&sql).bind(&old_seg).bind(&new_seg).execute(&mut *conn).await.map_err(err)?;
            for pk in moved {
                queue_push(conn, spec.name, &pk, "U", now).await?;
            }
        }
    }
    super::files::move_dir_records(conn, &old_seg, &new_seg).await?;
    let mut moves = load_dir_moves(conn).await?;
    let m = DirMove { dir: dir.to_string(), from: from.into(), to: to.into() };
    if !moves.contains(&m) {
        moves.push(m);
        save_dir_moves(conn, &moves).await?;
    }
    Ok(())
}

/// SQL condition: column `col` is the folder `?1` (`/<dir>/<id>`) or a path
/// inside it.
pub(crate) fn under_dir_sql(col: &str) -> String {
    let c = q(col);
    format!("(instr({c}, ?1 || '/') > 0 OR substr({c}, -length(?1)) = ?1)")
}

/// SQL value: column `col` (matching [`under_dir_sql`]) with the folder `?1`
/// replaced by `?2`.
pub(crate) fn moved_path_sql(col: &str) -> String {
    let c = q(col);
    format!(
        "CASE WHEN instr({c}, ?1 || '/') > 0 \
         THEN substr({c}, 1, instr({c}, ?1 || '/') - 1) || ?2 || substr({c}, instr({c}, ?1 || '/') + length(?1)) \
         ELSE substr({c}, 1, length({c}) - length(?1)) || ?2 END"
    )
}

/// Rename the queued per-id folders under `paths.app_data` (a login, Azure
/// profile, managed SSH key... must follow its re-keyed row). A folder whose
/// new name is already taken is left alone (the existing one wins); a rename
/// that fails stays queued. Returns how many were moved.
pub async fn run_dir_moves(db: &Db, paths: &PathCtx) -> Result<usize, String> {
    let Some(root) = paths.app_data.clone() else { return Ok(0) };
    let mut conn = db.acquire().await.map_err(err)?;
    let moves = load_dir_moves(&mut conn).await?;
    drop(conn);
    if moves.is_empty() {
        return Ok(0);
    }
    let todo = moves.clone();
    let (done, moved) = tokio::task::spawn_blocking(move || {
        let mut done = Vec::new();
        let mut moved = 0;
        for m in todo {
            let safe = |id: &str| !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..";
            if !safe(&m.from) || !safe(&m.to) {
                done.push(m);
                continue;
            }
            let (from, to) = (root.join(&m.dir).join(&m.from), root.join(&m.dir).join(&m.to));
            if !from.exists() || to.exists() {
                done.push(m);
                continue;
            }
            match std::fs::rename(&from, &to) {
                Ok(()) => {
                    moved += 1;
                    done.push(m);
                }
                Err(e) => tracing::warn!(event = "sync_dir_move_failed", dir = %m.dir, error = %e),
            }
        }
        (done, moved)
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut tx = db.begin().await.map_err(err)?;
    let mut left = load_dir_moves(&mut tx).await?;
    left.retain(|m| !done.contains(m));
    save_dir_moves(&mut tx, &left).await?;
    tx.commit().await.map_err(err)?;
    Ok(moved)
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

fn add_report(total: &mut ApplyReport, r: &ApplyReport) {
    total.applied += r.applied;
    total.skipped_older += r.skipped_older;
    total.conflicts += r.conflicts;
    // Each call retries what is still parked: the last count is the total.
    total.deferred = r.deferred;
}

/// Row ops in the order chunks must land: upserts parents first, then
/// deletes children first (a chunk boundary never splits a parent from a
/// child that needs it).
fn apply_order(mut ops: Vec<Op>) -> Vec<Op> {
    let level = |op: &Op| schema::spec(&op.tbl).map(|s| s.level()).unwrap_or(usize::MAX);
    ops.sort_by(|a, b| {
        let key = |op: &Op| match op.kind {
            OpKind::Upsert => (0usize, level(op)),
            OpKind::Delete => (1, usize::MAX - level(op)),
        };
        key(a).cmp(&key(b)).then_with(|| a.hlc.cmp(&b.hlc))
    });
    ops
}

/// Run the first-join merge for real: re-key + project map (one guarded
/// transaction), LWW-apply the group's rows in chunks, park its files and
/// Keychain items for the engine to fetch, queue the local-only rows, and
/// continue pulling after it. The engine must not be running; the caller
/// holds [`begin_commit`]. Safe to run again after a crash: the pending flag
/// is cleared last and a second merge finds the rows already re-keyed.
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

    run_dir_moves(db, home).await?;

    let (extra, rows): (Vec<Op>, Vec<Op>) = state.ops.into_iter().partition(|o| is_extra_tbl(&o.tbl));
    let mut applied = ApplyReport::default();
    for chunk in apply_order(rows).chunks(APPLY_CHUNK) {
        add_report(&mut applied, &store.apply_remote(chunk.to_vec()).await?);
    }
    // Files and Keychain items are fetched by the engine once syncing runs
    // (its first full scan retries parked ops before scanning): downloading
    // every log and transcript here would hold run starts for minutes.
    store.park_extra(extra).await?;
    run_dir_moves(db, home).await?;
    store.set_cursor(state.last_seq).await?;
    clear_pending(db).await?;
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
/// those paths does not change). The folder may not exist yet (folder sync
/// creates it), but must not be a file.
pub async fn set_project_mapping(db: &Db, home: &PathCtx, project_id: &str, local_path: &str) -> Result<ProjectMapping, String> {
    let local_path = local_path.trim_end_matches('/');
    let p = Path::new(local_path);
    if !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) || (p.exists() && !p.is_dir()) {
        return Err(format!("not a folder path on this machine: {local_path}"));
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

        // Keychain items keyed by a re-keyed id follow it (this store has no
        // Keychain: the moves stay queued until one runs them).
        let kc = std::sync::Arc::new(crate::sync::secrets::tests::FakeKeychain::default());
        for (service, id) in [("github", "gh-B"), ("gitlab", "gl-B"), ("mcp", "mcp-B"), ("servers", "sv-B"), ("aws", "aws-B"), ("google_accounts", "go-B")] {
            kc.put(service, id, &format!("{service}-secret"));
        }
        kc.put("github", "gh-A", "already-there");
        assert_eq!(crate::sync::secrets::run_key_moves(&db_b, kc.clone()).await.unwrap(), 6);
        assert_eq!(kc.get("github", "gh-A").as_deref(), Some("already-there"), "never overwritten");
        for (service, id) in [("gitlab", "gl-A"), ("mcp", "mcp-A"), ("servers", "sv-A"), ("aws", "aws-A"), ("google_accounts", "go-A")] {
            assert_eq!(kc.get(service, id), Some(format!("{service}-secret")), "{service}");
        }
        assert!(kc.items.lock().unwrap().keys().all(|k| !k.account.ends_with("-B")));
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
    async fn exact_path_matches_win_over_an_earlier_git_remote_match() {
        let (db_a, a, _) = device("a", "/Users/alice").await;
        let (db_b, _b, home_b) = device("b", "/Users/bob").await;
        // Two checkouts of the same repo on a; the first one in the log only
        // matches b's project by git remote, the second one by exact path.
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at, github_owner, github_repo) VALUES ('p-mini', 'devdy-mini', '/Users/alice/w/mini', 'x', 'thainph', 'devdy')").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at, github_owner, github_repo) VALUES ('p-main', 'devdy', '/Users/alice/w/devdy', 'x', 'thainph', 'devdy')").await;
        exec(&db_b, "INSERT INTO projects (id, name, path, created_at) VALUES ('b1', 'devdy', '/Users/bob/w/devdy', 'x')").await;
        let git: HashMap<String, String> = [("/Users/bob/w/devdy".to_string(), "git@github.com:thainph/devdy.git".to_string())].into();
        exec(&db_b, "DELETE FROM sync_outbox").await;
        let state = project_op(&db_a, &a).await;
        assert_eq!(state.ops.iter().filter(|o| o.tbl == "projects").map(|o| o.pk.as_str().unwrap()).collect::<Vec<_>>(), vec!["p-mini", "p-main"]);
        let p = preview(&db_b, &home_b, &state, &git).await.unwrap();
        let by: HashMap<&str, Option<&str>> = p.projects.iter().map(|m| (m.remote_id.as_str(), m.matched_by.as_deref())).collect();
        assert_eq!(by["p-main"], Some("path"));
        assert_eq!(by["p-mini"], None);
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
        assert!(set_project_mapping(&db, &ctx, "p", "relative/here").await.is_err());
        assert!(set_project_mapping(&db, &ctx, "p", &format!("{home}/code/../x")).await.is_err());
        std::fs::write(home_dir.join("file"), "x").unwrap();
        assert!(set_project_mapping(&db, &ctx, "p", &format!("{home}/file")).await.is_err());
        // A folder that folder sync will create.
        let later = format!("{home}/later/devdy");
        let m = set_project_mapping(&db, &ctx, "p", &later).await.unwrap();
        assert_eq!((m.mapped, m.on_this_machine, m.path.as_str()), (true, false, later.as_str()));
        assert_eq!(scalar(&db, "SELECT output_path FROM runs").await.unwrap(), format!("{later}/.devdy/runs/r.md"));
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

    async fn eventually(what: &str, mut check: impl FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            if check().await {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Every string in every synced row of `db`.
    async fn all_synced_strings(db: &Db) -> Vec<(String, String)> {
        let mut conn = db.acquire().await.unwrap();
        let mut out = Vec::new();
        for spec in SYNCED_TABLES {
            for row in read_all(&mut conn, spec).await.unwrap() {
                for v in row.values() {
                    if let Some(s) = v.as_str() {
                        out.push((spec.name.to_string(), s.to_string()));
                    }
                }
            }
        }
        out
    }

    /// The real first run: the Mac mini (M) has everything and creates the
    /// group on the relay; a MacBook (B, other home, other app data, some of
    /// the same things under other ids, plus its own) joins, previews and
    /// commits the merge. Rows, files and Keychain items arrive under B's
    /// paths, B's matching rows take M's ids (with their per-id folders), and
    /// M loses nothing.
    #[tokio::test]
    async fn first_run_end_to_end_over_the_relay() {
        use crate::sync::crypto::{generate_group_key, SyncKeys};
        use crate::sync::engine::{RelayBlobs, StatusSink, SyncEngine, SyncStatus, Timing};
        use crate::sync::files::tests::{mem_db, tempdir};
        use crate::sync::secrets::tests::FakeKeychain;
        use crate::sync::test_relay::TestRelay;
        use crate::sync::transport::ConnectParams;
        use sha2::Digest as _;
        use std::sync::Arc;
        use std::time::Duration;

        let base = tempdir();
        let home_m = base.path().join("Users/m");
        let home_b = base.path().join("Users/b");
        let data_m = home_m.join("Library/Application Support/vn.papay.devdy");
        let data_b = home_b.join("Library/Application Support/vn.papay.devdy.dev");
        let (hm, hb) = (home_m.to_string_lossy().to_string(), home_b.to_string_lossy().to_string());
        let write = |p: &std::path::Path, body: &str| {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        let fast = Timing {
            outbox_poll: Duration::from_millis(50),
            periodic: Duration::from_secs(3600),
            kick_debounce: Duration::from_millis(10),
            backoff_min: Duration::from_millis(50),
            backoff_max: Duration::from_millis(200),
            rejected_retry: Duration::from_secs(3600),
            push_backoff_max: Duration::from_millis(400),
            pull_limit: 500,
        };
        let file_timing = crate::sync::files::Timing { quick: Duration::from_millis(50), full: Duration::from_millis(150) };
        let keys = SyncKeys::derive(&generate_group_key()).unwrap();
        let relay = TestRelay::start().await;
        let params = |device: &str| ConnectParams {
            relay_url: relay.url(),
            auth_token: TestRelay::TOKEN.into(),
            group_id: "g-first-run".into(),
            device_id: device.into(),
            device_token: format!("tok-{device}"),
        };
        let quiet: StatusSink = Arc::new(|_: &SyncStatus| {});

        // ---- M: the Mac mini, with everything ----
        let db_m = mem_db().await;
        let kc_m = Arc::new(FakeKeychain::default());
        let ctx_m = PathCtx::with_home(&home_m).with_app_data(&data_m);
        let project_m = home_m.join("w/devdy");
        std::fs::create_dir_all(&project_m).unwrap();
        let skill_m = data_m.join("skills/review");
        write(&skill_m.join("SKILL.md"), "# review (M)\n");
        write(&skill_m.join("refs/a.md"), "reference from M\n");
        let log_m = project_m.join(".devdy/runs/rn-M.log");
        write(&log_m, "{\"type\":\"system\"}\n");
        let claude_m = data_m.join("claude-accounts/ca-M");
        let transcript_m = claude_m
            .join("projects")
            .join(crate::commands::sessions::encode_project_dir(&project_m.to_string_lossy()))
            .join("s1.jsonl");
        write(&transcript_m, &format!("{{\"type\":\"user\",\"cwd\":\"{}\",\"message\":\"hi\"}}\n", project_m.to_string_lossy()));
        let key_m = data_m.join("server-keys/sv-M/id_ed25519");
        write(&key_m, "-----BEGIN OPENSSH PRIVATE KEY-----\nM\n");
        for sql in [
            "INSERT INTO github_accounts (id, label, username, created_at) VALUES ('gh-M', 'work', 'me', 'x')".to_string(),
            format!("INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('ca-M', 'max', '{}', 'x')", claude_m.to_string_lossy()),
            format!("INSERT INTO servers (id, label, host, username, auth_method, private_key_path, created_at) VALUES ('sv-M', 'vps', 'h', 'u', 'key', '{}', 'x')", key_m.to_string_lossy()),
            format!("INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-M', 'review', '', '{}', 'x')", skill_m.to_string_lossy()),
            format!("INSERT INTO projects (id, name, path, created_at, github_account_id, claude_account_id) VALUES ('pr-M', 'devdy', '{}', 'x', 'gh-M', 'ca-M')", project_m.to_string_lossy()),
            "INSERT INTO project_skills (project_id, skill_id, applied_at) VALUES ('pr-M', 'sk-M', 'x')".to_string(),
            format!(
                "INSERT INTO runs (id, project_id, type, status, engine, session_id, claude_account_id, output_path, transcript_path, created_at) \
                 VALUES ('rn-M', 'pr-M', 'session', 'done', 'claude', 's1', 'ca-M', '{}', '{}', 'x')",
                log_m.to_string_lossy(),
                transcript_m.to_string_lossy()
            ),
            "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) VALUES ('no-M', 'plan', 'from M', 'pr-M', 0, 'x', 'x')".to_string(),
        ] {
            exec(&db_m, &sql).await;
        }
        kc_m.put("github", "gh-M", "ghp_from_M");

        // M creates the group: registers as its creator and uploads everything.
        let dev_m = crate::sync::ensure_device(&db_m).await.unwrap();
        let (etx, _erx) = tokio::sync::mpsc::unbounded_channel();
        let conn_m = Connection::connect(&params(&dev_m), true, etx).await.unwrap();
        prepare_new_group(&db_m).await.unwrap();
        let store_m = SqliteSyncStore::new(db_m.clone(), dev_m.clone())
            .with_paths(ctx_m.clone())
            .with_secrets(kc_m.clone())
            .with_file_timing(file_timing.clone());
        let engine_m = SyncEngine::start(params(&dev_m), keys.clone(), Arc::new(store_m), quiet.clone(), false, fast.clone());
        let outbox_empty = |db: &Db| {
            let db = db.clone();
            move || -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
                let db = db.clone();
                Box::pin(async move { count(&db, "SELECT COUNT(*) FROM sync_outbox").await == 0 })
            }
        };
        eventually("M uploaded", outbox_empty(&db_m)).await;
        let files_up = || -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
            let relay = relay.clone();
            Box::pin(async move { relay.blob_counts().0 >= 5 })
        };
        eventually("M's files on the relay", files_up).await;
        eventually("M's file ops pushed", outbox_empty(&db_m)).await;

        // ---- B: the MacBook, with its own (overlapping) data ----
        let db_b = mem_db().await;
        let kc_b = Arc::new(FakeKeychain::default());
        let ctx_b = PathCtx::with_home(&home_b).with_app_data(&data_b);
        let project_b = home_b.join("w/devdy");
        std::fs::create_dir_all(&project_b).unwrap();
        let skill_b = data_b.join("skills/review");
        write(&skill_b.join("SKILL.md"), "# review (B)\n");
        let claude_b = data_b.join("claude-accounts/ca-B");
        write(&claude_b.join(".credentials.json"), "{\"login\":\"B\"}");
        for sql in [
            "INSERT INTO github_accounts (id, label, username, created_at) VALUES ('gh-B', 'work', 'me', 'x')".to_string(),
            format!("INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('ca-B', 'max', '{}', 'x')", claude_b.to_string_lossy()),
            format!("INSERT INTO skills (id, name, description, source_path, updated_at) VALUES ('sk-B', 'review', '', '{}', 'x')", skill_b.to_string_lossy()),
            format!("INSERT INTO projects (id, name, path, created_at, github_account_id, claude_account_id) VALUES ('pr-B', 'devdy', '{}', 'x', 'gh-B', 'ca-B')", project_b.to_string_lossy()),
            "INSERT INTO runs (id, project_id, type, status, engine, session_id, claude_account_id, created_at) VALUES ('rn-B', 'pr-B', 'session', 'done', 'claude', 's-b', 'ca-B', 'x')".to_string(),
            "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) VALUES ('no-B', 'mine', 'from B', 'pr-B', 1, 'x', 'x')".to_string(),
        ] {
            exec(&db_b, &sql).await;
        }
        // B's rows predate sync (the group's versions of matched rows win).
        exec(&db_b, "DELETE FROM sync_outbox").await;
        kc_b.put("github", "gh-B", "ghp_from_B");

        // B joins with a ticket M announced, and holds the engine.
        let ticket = "first-run-ticket";
        conn_m.sync_invite(&hex::encode(sha2::Sha256::digest(ticket.as_bytes())), 60).await.unwrap();
        let dev_b = crate::sync::ensure_device(&db_b).await.unwrap();
        let (etx, _erx) = tokio::sync::mpsc::unbounded_channel();
        let conn_b = Connection::connect_with_ticket(&params(&dev_b), false, Some(ticket), etx).await.unwrap();
        mark_joined(&db_b).await.unwrap();

        let state = fetch_group_state(&conn_b, &keys).await.unwrap();
        assert_eq!(state.undecryptable, 0);
        let git = HashMap::new();
        let p = preview(&db_b, &ctx_b, &state, &git).await.unwrap();
        let by: HashMap<&str, Option<&str>> = p.projects.iter().map(|m| (m.remote_id.as_str(), m.matched_by.as_deref())).collect();
        assert_eq!(by["pr-M"], Some("path"));
        assert_eq!(p.projects[0].local_path, project_b.to_string_lossy());
        for tbl in ["projects", "skills", "claude_accounts", "github_accounts"] {
            assert_eq!((table(&p, tbl).matched, table(&p, tbl).rekeyed), (1, 1), "{tbl}");
        }
        assert_eq!(scalar(&db_b, "SELECT id FROM projects").await.unwrap(), "pr-B", "preview writes nothing");

        let store_b = SqliteSyncStore::new(db_b.clone(), dev_b.clone())
            .with_paths(ctx_b.clone())
            .with_secrets(kc_b.clone())
            .with_file_timing(file_timing.clone());
        let report = commit(&db_b, &store_b, &ctx_b, state, &git).await.unwrap();
        assert_eq!(report.applied.deferred, 0, "{report:?}");
        assert!(!is_pending(&db_b).await.unwrap());
        // M7: files and Keychain items are left to the engine; its first
        // full scan fetches them.
        assert!(count(&db_b, "SELECT COUNT(*) FROM sync_extra_deferred").await >= 5);
        assert!(!skill_b.join("refs/a.md").exists(), "nothing downloaded by the merge");
        store_b.stage(&RelayBlobs::new(&conn_b, &keys)).await.unwrap();
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_extra_deferred").await, 0);

        // Matched rows took M's ids; B's own rows follow them.
        for (tbl, id) in [("projects", "pr-M"), ("skills", "sk-M"), ("claude_accounts", "ca-M"), ("github_accounts", "gh-M")] {
            assert_eq!(count(&db_b, &format!("SELECT COUNT(*) FROM {tbl}")).await, 1, "{tbl}");
            assert_eq!(scalar(&db_b, &format!("SELECT id FROM {tbl}")).await.unwrap(), id, "{tbl}");
        }
        assert_eq!(scalar(&db_b, "SELECT project_id FROM runs WHERE id = 'rn-B'").await.unwrap(), "pr-M");
        assert_eq!(scalar(&db_b, "SELECT claude_account_id FROM runs WHERE id = 'rn-B'").await.unwrap(), "ca-M");
        assert_eq!(scalar(&db_b, "SELECT project_id FROM notes WHERE id = 'no-B'").await.unwrap(), "pr-M");

        // Every path is B's: no portable marker, nothing of M's home left.
        for (tbl, v) in all_synced_strings(&db_b).await {
            assert!(!v.contains("@data") && !v.contains(&hm) && !v.starts_with('~'), "{tbl}: {v}");
        }
        let claude_dir_b = data_b.join("claude-accounts/ca-M");
        assert_eq!(scalar(&db_b, "SELECT config_dir FROM claude_accounts").await.unwrap(), claude_dir_b.to_string_lossy());
        assert_eq!(scalar(&db_b, "SELECT path FROM projects").await.unwrap(), project_b.to_string_lossy());
        assert_eq!(scalar(&db_b, "SELECT source_path FROM skills").await.unwrap(), skill_b.to_string_lossy());
        let key_b = data_b.join("server-keys/sv-M/id_ed25519");
        assert_eq!(scalar(&db_b, "SELECT private_key_path FROM servers").await.unwrap(), key_b.to_string_lossy());

        // N12: B's Claude login followed its account to the new id.
        assert_eq!(std::fs::read_to_string(claude_dir_b.join(".credentials.json")).unwrap(), "{\"login\":\"B\"}");
        assert!(!claude_b.exists());

        // N1: files arrived at B's paths.
        assert_eq!(std::fs::read_to_string(skill_b.join("refs/a.md")).unwrap(), "reference from M\n");
        assert!(skill_b.join("SKILL.md").is_file());
        let log_b: String = scalar(&db_b, "SELECT output_path FROM runs WHERE id = 'rn-M'").await.unwrap();
        assert_eq!(log_b, project_b.join(".devdy/runs/rn-M.log").to_string_lossy());
        assert_eq!(std::fs::read_to_string(&log_b).unwrap(), "{\"type\":\"system\"}\n");
        let transcript_b: String = scalar(&db_b, "SELECT transcript_path FROM runs WHERE id = 'rn-M'").await.unwrap();
        let enc_b = crate::commands::sessions::encode_project_dir(&project_b.to_string_lossy());
        assert_eq!(transcript_b, claude_dir_b.join("projects").join(enc_b).join("s1.jsonl").to_string_lossy(), "B's project folder");
        let t = std::fs::read_to_string(&transcript_b).unwrap();
        assert!(t.contains(&*project_b.to_string_lossy()) && !t.contains(&hm), "{t}");
        assert_eq!(std::fs::read_to_string(&key_b).unwrap(), "-----BEGIN OPENSSH PRIVATE KEY-----\nM\n");
        // ...and the Keychain item, under M's id.
        assert_eq!(kc_b.get("github", "gh-M").as_deref(), Some("ghp_from_M"));
        assert_eq!(kc_b.get("github", "gh-B"), None);

        // B syncs on: M gets B's own rows and loses nothing.
        drop(conn_b);
        let engine_b = SyncEngine::start(params(&dev_b), keys.clone(), Arc::new(store_b), quiet.clone(), false, fast.clone());
        let got_note = || -> std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>> {
            let db = db_m.clone();
            Box::pin(async move { count(&db, "SELECT COUNT(*) FROM notes WHERE id = 'no-B'").await == 1 })
        };
        eventually("B's note on M", got_note).await;
        eventually("B pushed", outbox_empty(&db_b)).await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        for (tbl, id) in [("projects", "pr-M"), ("skills", "sk-M"), ("claude_accounts", "ca-M"), ("github_accounts", "gh-M"), ("servers", "sv-M"), ("runs", "rn-M"), ("notes", "no-M")] {
            assert_eq!(count(&db_m, &format!("SELECT COUNT(*) FROM {tbl} WHERE id = '{id}'")).await, 1, "{tbl} {id} kept on M");
        }
        assert_eq!(scalar(&db_m, "SELECT project_id FROM runs WHERE id = 'rn-B'").await.as_deref(), Some("pr-M"));
        assert_eq!(scalar(&db_m, "SELECT path FROM projects").await.unwrap(), project_m.to_string_lossy());
        for (tbl, v) in all_synced_strings(&db_m).await {
            assert!(!v.contains("@data") && !v.contains(&hb), "{tbl}: {v}");
        }
        // A library file both machines authored before sync: B's never-synced
        // copy is newer, so it wins everywhere; M's version is kept on B as a
        // conflict copy (files::apply_op) and nothing else of M's is touched.
        let conflict_copy = std::fs::read_dir(&skill_b)
            .unwrap()
            .flatten()
            .find(|e| e.file_name().to_string_lossy().starts_with("SKILL.md.conflict-"))
            .expect("M's SKILL.md kept on B");
        assert_eq!(std::fs::read_to_string(conflict_copy.path()).unwrap(), "# review (M)\n");
        assert_eq!(std::fs::read_to_string(skill_m.join("refs/a.md")).unwrap(), "reference from M\n");
        assert_eq!(std::fs::read_to_string(&log_m).unwrap(), "{\"type\":\"system\"}\n");
        assert_eq!(kc_m.get("github", "gh-M").as_deref(), Some("ghp_from_M"));
        engine_b.stop().await;
        engine_m.stop().await;
    }

    #[test]
    fn run_starts_are_refused_while_a_merge_commits() {
        assert!(refuse_if_committing().is_ok());
        let guard = begin_commit().unwrap();
        assert!(refuse_if_committing().unwrap_err().starts_with(COMMITTING_CODE));
        assert!(begin_commit().is_err(), "one merge at a time");
        drop(guard);
        assert!(refuse_if_committing().is_ok());
    }

    #[tokio::test]
    async fn large_group_state_applies_in_chunks_parents_first() {
        let (db_a, a, _) = device("a", "/Users/a").await;
        let (db_b, b, home_b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p', 'p', '/Users/a/p', 'x')").await;
        exec(&db_a, "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1200) \
                     INSERT INTO todos (id, text, project_id, position, created_at) SELECT 't' || i, 'x', 'p', i, 'x' FROM n").await;
        let mut state = group_state(&a).await;
        // Children first in the log: the chunks still land parents first.
        state.ops.reverse();
        mark_joined(&db_b).await.unwrap();
        let r = commit(&db_b, &b, &home_b, state, &HashMap::new()).await.unwrap();
        assert_eq!((r.applied.applied, r.applied.deferred), (1201, 0), "{r:?}");
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM todos WHERE project_id = 'p'").await, 1200);
        assert!(!is_pending(&db_b).await.unwrap());
    }

    #[tokio::test]
    async fn rekeyed_rows_take_their_app_data_folders_along() {
        let home = std::env::temp_dir().join(format!("devdy-dirmove-{}", uuid::Uuid::new_v4().simple()));
        let data = home.join("data");
        let h = home.to_string_lossy().to_string();
        let (db_a, a, _) = device("a", "/Users/a").await;
        let db_b = mem_db().await;
        let ctx_b = PathCtx::with_home(&home).with_app_data(&data);
        let b = SqliteSyncStore::new(db_b.clone(), "b".into()).with_paths(ctx_b.clone());
        exec(&db_a, "INSERT INTO azure_accounts (id, label, created_at) VALUES ('az-A', 'az', 'x')").await;
        exec(&db_a, "INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('ca-A', 'max', '/elsewhere', 'x')").await;
        exec(&db_b, "INSERT INTO azure_accounts (id, label, created_at) VALUES ('az-B', 'az', 'x')").await;
        exec(&db_b, &format!("INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('ca-B', 'max', '{h}/data/claude-accounts/ca-B', 'x')")).await;
        // M3: other paths into the folder (a run's transcript, its file
        // record) follow it too; a sibling id sharing the prefix does not.
        let transcript = format!("{h}/data/claude-accounts/ca-B/projects/-w-p/s1.jsonl");
        exec(&db_b, &format!("INSERT INTO projects (id, name, path, created_at) VALUES ('pr-B', 'p', '{h}/w/p', 'x')")).await;
        exec(&db_b, &format!(
            "INSERT INTO runs (id, project_id, type, engine, session_id, transcript_path, output_path, created_at) \
             VALUES ('rn-B', 'pr-B', 'session', 'claude', 's1', '{transcript}', '{h}/data/claude-accounts/ca-Bx/o.log', 'x')"
        )).await;
        exec(&db_b, &format!(
            "INSERT INTO sync_files (kind, path, local_path, size, sha256) \
             VALUES ('claude_transcript', '@data/claude-accounts/ca-B/projects/-w-p/s1.jsonl', '{transcript}', 3, 'abc')"
        )).await;
        exec(&db_b, "DELETE FROM sync_outbox").await;
        for (dir, id) in [("azure-accounts", "az-B"), ("claude-accounts", "ca-B")] {
            std::fs::create_dir_all(data.join(dir).join(id)).unwrap();
            std::fs::write(data.join(dir).join(id).join("login"), id).unwrap();
        }
        // b has an unpushed change to its account: its row (and its folder) win.
        exec(&db_b, "UPDATE claude_accounts SET label = 'max' WHERE id = 'ca-B'").await;
        exec(&db_b, "UPDATE sync_outbox SET changed_at = 9000000000000").await;
        let state = group_state(&a).await;

        preview(&db_b, &ctx_b, &state, &HashMap::new()).await.unwrap();
        assert!(data.join("claude-accounts/ca-B").is_dir(), "preview moves nothing");
        commit(&db_b, &b, &ctx_b, state, &HashMap::new()).await.unwrap();
        assert_eq!(std::fs::read_to_string(data.join("azure-accounts/az-A/login")).unwrap(), "az-B");
        assert_eq!(std::fs::read_to_string(data.join("claude-accounts/ca-A/login")).unwrap(), "ca-B");
        assert!(!data.join("claude-accounts/ca-B").exists());
        assert_eq!(scalar(&db_b, "SELECT config_dir FROM claude_accounts").await.unwrap(), format!("{h}/data/claude-accounts/ca-A"));
        let moved = format!("{h}/data/claude-accounts/ca-A/projects/-w-p/s1.jsonl");
        assert_eq!(scalar(&db_b, "SELECT transcript_path FROM runs").await.unwrap(), moved);
        assert_eq!(scalar(&db_b, "SELECT output_path FROM runs").await.unwrap(), format!("{h}/data/claude-accounts/ca-Bx/o.log"));
        assert_eq!(
            scalar(&db_b, "SELECT path || ' ' || local_path FROM sync_files").await.unwrap(),
            format!("@data/claude-accounts/ca-A/projects/-w-p/s1.jsonl {moved}")
        );
        // The record goes out under its new path.
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_outbox WHERE tbl = 'file' AND instr(pk, 'ca-A/projects') > 0").await, 1);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_state WHERE key = 'dir_moves' AND value != '[]'").await, 0);
        std::fs::remove_dir_all(&home).ok();
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
