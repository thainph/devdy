//! Selective sync (on-demand-sync.md §3): what this machine takes from the
//! group. Everything here is machine-local and filters on the RECEIVER, when
//! pulled ops are applied — the senders push everything.
//!
//! - Categories (`sync_categories_off`, a JSON array in `settings`): ops of a
//!   disabled category are skipped. `secrets` is the existing
//!   `sync_secrets_enabled` toggle (it also stops this machine's own pushes).
//! - Projects (`sync_project_prefs.enabled`): project-scoped rows (any row
//!   with a `project_id`, rows under a run / note / todo / Slack thread / Pulse
//!   watch / repo of the project) and the run logs / transcripts of an
//!   unselected project are skipped. The `projects` rows themselves always
//!   apply, so every project shows in the list.
//! - A row of an enabled category pointing (declared FK) at a row of a disabled
//!   category that is not here: a nullable reference is cut to NULL (and put
//!   back when the row is pushed), a required one skips the row.
//!
//! A skipped op leaves no trace in `sync_rows`, so enabling a category or
//! selecting a project resets the pull cursor to 0: the next Sync re-pulls the
//! whole log, and apply is LWW-idempotent for what is already here.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sqlx::{Row, SqliteConnection};

use super::bootstrap::{self, REFS};
use super::codec::{read_row, PathCtx};
use super::files::{self, FileKind};
use super::op::{Op, OpKind};
use super::schema::{self, q, SYNCED_SETTING_PREFIXES};
use super::store::CURSOR_KEY;
use crate::db::Db;

/// `settings` key: default source device of the Sync button (machine-local).
pub const SOURCE_KEY: &str = "sync_source_device";
/// `settings` key: JSON array of disabled category ids (machine-local; never
/// holds `secrets`, which lives in `secrets::SETTING`).
pub const CATEGORIES_OFF_KEY: &str = "sync_categories_off";
/// `sync_state` flag set by migration 0066 until [`seed`] ran.
const SEED_KEY: &str = "prefs_seed_pending";
/// `sync_state` test hook read by the `projects` insert trigger: whether a
/// project arriving from another device starts selected (default 0).
pub const SYNCED_PROJECT_ENABLED_KEY: &str = "prefs_synced_project_enabled";

pub const SECRETS: &str = "secrets";
pub const SESSIONS: &str = "sessions";

/// Category ids, in UI order.
pub const CATEGORIES: &[&str] =
    &["config", "accounts", SECRETS, "servers", "mcp", "library", "notes", "slack", "pulse", SESSIONS];

/// Folder sync excludes on by default (`default_excludes`), on top of `.git`
/// internals and `.devdy/`, which are always excluded.
pub const DEFAULT_EXCLUDES: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
    ".gradle",
    "Pods",
    ".DS_Store",
];

/// Synced tables that are never filtered by category (`repos` still follow
/// their project's selection). `settings` sync-only rows are always on too
/// (see [`op_category`]).
#[cfg_attr(not(test), allow(dead_code))]
pub static ALWAYS_ON: &[&str] = &["projects", "repos"];

/// Category of every other synced table.
pub static TABLE_CATEGORIES: &[(&str, &str)] = &[
    ("settings", "config"),
    ("github_accounts", "accounts"),
    ("gitlab_accounts", "accounts"),
    ("aws_accounts", "accounts"),
    ("azure_accounts", "accounts"),
    ("google_accounts", "accounts"),
    ("claude_accounts", "accounts"),
    ("servers", "servers"),
    ("project_servers", "servers"),
    ("mcp_servers", "mcp"),
    ("project_mcp_servers", "mcp"),
    ("skills", "library"),
    ("rules", "library"),
    ("skill_groups", "library"),
    ("rule_groups", "library"),
    ("skill_group_members", "library"),
    ("rule_group_members", "library"),
    ("project_skills", "library"),
    ("project_rules", "library"),
    ("project_skill_groups", "library"),
    ("project_rule_groups", "library"),
    ("notes", "notes"),
    ("todos", "notes"),
    ("note_runs", "notes"),
    ("todo_runs", "notes"),
    ("slack_identities", "slack"),
    ("slack_users", "slack"),
    ("slack_channels", "slack"),
    ("slack_threads", "slack"),
    ("slack_messages", "slack"),
    ("slack_files", "slack"),
    ("slack_thread_runs", "slack"),
    ("pulse_watches", "pulse"),
    ("pulse_digests", "pulse"),
    ("runs", SESSIONS),
    ("run_usage", SESSIONS),
    ("deleted_sessions", SESSIONS),
];

/// Project-scoped tables (single `id` pk, a `project_id` column) whose rows
/// others point at: a child belongs to its parent's project too.
const SCOPED_PARENTS: &[&str] = &["runs", "notes", "todos", "slack_threads", "pulse_watches", "repos"];

/// Category of a synced table; `None` = always on.
pub fn table_category(tbl: &str) -> Option<&'static str> {
    TABLE_CATEGORIES.iter().find(|(t, _)| *t == tbl).map(|(_, c)| *c)
}

/// Category of a `file` op kind.
pub fn file_category(kind: FileKind) -> &'static str {
    match kind {
        FileKind::Library => "library",
        FileKind::RunLog | FileKind::ClaudeTranscript | FileKind::CodexRollout => SESSIONS,
        FileKind::SshKey => SECRETS,
    }
}

/// Category of a row op; `None` = always on (incl. sync-only settings).
fn op_category(op: &Op) -> Option<&'static str> {
    if op.tbl == "settings" {
        let key = op.pk.as_str().unwrap_or_default();
        if SYNCED_SETTING_PREFIXES.iter().any(|p| key.starts_with(p)) {
            return None;
        }
    }
    table_category(&op.tbl)
}

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

async fn get_setting(conn: &mut SqliteConnection, key: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar("SELECT value FROM settings WHERE key = ?").bind(key).fetch_optional(&mut *conn).await.map_err(err)
}

async fn put_setting(conn: &mut SqliteConnection, key: &str, value: Option<&str>) -> Result<(), String> {
    match value {
        Some(v) => sqlx::query("INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(key)
            .bind(v)
            .execute(&mut *conn)
            .await
            .map_err(err)?,
        None => sqlx::query("DELETE FROM settings WHERE key = ?").bind(key).execute(&mut *conn).await.map_err(err)?,
    };
    Ok(())
}

/// Next pull starts from the beginning of the group log.
async fn reset_cursor(conn: &mut SqliteConnection) -> Result<(), String> {
    sqlx::query("INSERT INTO sync_state (key, value) VALUES (?, '0') ON CONFLICT(key) DO UPDATE SET value = '0'")
        .bind(CURSOR_KEY)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

// ---- Startup ------------------------------------------------------------------------

/// Once after migration 0066: projects whose folder is not on this machine
/// were synced from elsewhere — unselect them (the others keep syncing).
pub async fn seed(db: &Db) -> Result<(), String> {
    let pending: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(SEED_KEY)
        .fetch_optional(db)
        .await
        .map_err(err)?;
    if pending.as_deref() != Some("1") {
        return Ok(());
    }
    let projects: Vec<(String, String)> = sqlx::query_as("SELECT id, path FROM projects").fetch_all(db).await.map_err(err)?;
    let mut tx = db.begin().await.map_err(err)?;
    for (id, path) in projects {
        if !Path::new(&path).is_dir() {
            sqlx::query("UPDATE sync_project_prefs SET enabled = 0 WHERE project_id = ?")
                .bind(&id)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
        }
    }
    sqlx::query("DELETE FROM sync_state WHERE key = ?").bind(SEED_KEY).execute(&mut *tx).await.map_err(err)?;
    tx.commit().await.map_err(err)
}

// ---- Categories -----------------------------------------------------------------------

/// Disabled categories, `secrets` included when secrets sync is off.
pub async fn categories_off(conn: &mut SqliteConnection) -> Result<Vec<String>, String> {
    let stored: Vec<String> =
        get_setting(conn, CATEGORIES_OFF_KEY).await?.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
    let secrets_off = !super::secrets::enabled(conn).await?;
    Ok(CATEGORIES
        .iter()
        .filter(|c| if **c == SECRETS { secrets_off } else { stored.iter().any(|s| s == *c) })
        .map(|c| c.to_string())
        .collect())
}

/// Replace the disabled categories. `secrets` maps onto `sync_secrets_enabled`.
/// Enabling any category resets the pull cursor (and lets rows whose
/// references were cut be applied again in full).
pub async fn set_categories_off(db: &Db, off: &[String]) -> Result<(), String> {
    if let Some(bad) = off.iter().find(|c| !CATEGORIES.contains(&c.as_str())) {
        return Err(format!("unknown sync category: {bad}"));
    }
    let mut tx = db.begin().await.map_err(err)?;
    let before = categories_off(&mut tx).await?;
    let rows: Vec<&str> = CATEGORIES.iter().copied().filter(|c| *c != SECRETS && off.iter().any(|o| o == c)).collect();
    let json = serde_json::to_string(&rows).map_err(|e| e.to_string())?;
    put_setting(&mut tx, CATEGORIES_OFF_KEY, (!rows.is_empty()).then_some(json.as_str())).await?;
    let secrets_on = !off.iter().any(|c| c == SECRETS);
    put_setting(&mut tx, super::secrets::SETTING, Some(if secrets_on { "true" } else { "false" })).await?;
    if before.iter().any(|c| !off.contains(c)) {
        reset_cursor(&mut tx).await?;
        // Re-applied in full by the re-pull (or cut again if still needed).
        sqlx::query("DELETE FROM sync_rows WHERE EXISTS (SELECT 1 FROM sync_stripped_refs s WHERE s.tbl = sync_rows.tbl AND s.pk = sync_rows.pk)")
            .execute(&mut *tx)
            .await
            .map_err(err)?;
        sqlx::query("DELETE FROM sync_stripped_refs").execute(&mut *tx).await.map_err(err)?;
    }
    tx.commit().await.map_err(err)
}

pub async fn source_device(db: &Db) -> Result<Option<String>, String> {
    let mut conn = db.acquire().await.map_err(err)?;
    Ok(get_setting(&mut conn, SOURCE_KEY).await?.filter(|s| !s.is_empty()))
}

/// Default source of the Sync button; empty clears it.
pub async fn set_source_device(db: &Db, device_id: &str) -> Result<(), String> {
    let mut conn = db.acquire().await.map_err(err)?;
    put_setting(&mut conn, SOURCE_KEY, (!device_id.is_empty()).then_some(device_id)).await
}

// ---- Projects -------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProjectPref {
    pub project_id: String,
    pub name: String,
    /// The root as synced to the other devices (`~/...`).
    pub synced_path: String,
    /// This machine's root.
    pub local_path: String,
    /// The local root is a folder on this machine.
    pub exists: bool,
    /// The local root was set by the bootstrap match or by hand.
    pub mapped: bool,
    pub enabled: bool,
    pub folder: bool,
    pub default_excludes: bool,
    pub excludes: Vec<String>,
    pub max_file_mb: i64,
    pub last_backup_at: Option<String>,
    /// A session of this project is running on this machine.
    pub running_here: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProjectPrefPatch {
    pub enabled: Option<bool>,
    pub folder: Option<bool>,
    pub default_excludes: Option<bool>,
    pub excludes: Option<Vec<String>>,
    pub max_file_mb: Option<i64>,
    pub local_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SyncPrefs {
    pub source_device: Option<String>,
    pub categories_off: Vec<String>,
    pub projects: Vec<ProjectPref>,
}

type PrefRow = (Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<i64>, Option<String>);

async fn pref_of(conn: &mut SqliteConnection, m: bootstrap::ProjectMapping) -> Result<ProjectPref, String> {
    let row: Option<PrefRow> = sqlx::query_as(
        "SELECT enabled, folder, default_excludes, excludes, max_file_mb, last_backup_at FROM sync_project_prefs WHERE project_id = ?",
    )
    .bind(&m.project_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err)?;
    // No row: a project from before the table, treated as selected.
    let (enabled, folder, default_excludes, excludes, max_file_mb, last_backup_at) =
        row.unwrap_or((Some(1), None, None, None, None, None));
    let running_here: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM runs WHERE project_id = ? AND status = 'running' AND {})",
        super::sessions::OWNED_HERE
    ))
    .bind(&m.project_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(err)?;
    Ok(ProjectPref {
        project_id: m.project_id,
        name: m.name,
        synced_path: m.synced_path,
        local_path: m.path,
        exists: m.on_this_machine,
        mapped: m.mapped,
        enabled: enabled.unwrap_or(1) != 0,
        folder: folder.unwrap_or(0) != 0,
        default_excludes: default_excludes.unwrap_or(1) != 0,
        excludes: excludes.and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default(),
        max_file_mb: max_file_mb.unwrap_or(500),
        last_backup_at,
        running_here,
    })
}

/// Prefs of every project (by name).
pub async fn project_prefs(db: &Db, paths: &PathCtx) -> Result<Vec<ProjectPref>, String> {
    let mappings = bootstrap::project_mappings(db, paths).await?;
    let mut conn = db.acquire().await.map_err(err)?;
    let mut out = Vec::with_capacity(mappings.len());
    for m in mappings {
        out.push(pref_of(&mut conn, m).await?);
    }
    Ok(out)
}

async fn project_pref_with(db: &Db, paths: &PathCtx, project_id: &str) -> Result<ProjectPref, String> {
    let m = bootstrap::project_mappings(db, paths)
        .await?
        .into_iter()
        .find(|m| m.project_id == project_id)
        .ok_or("project not found")?;
    let mut conn = db.acquire().await.map_err(err)?;
    pref_of(&mut conn, m).await
}

/// One project's prefs (folder sync reads its paths, excludes and limits here).
#[allow(dead_code)] // folder sync
pub async fn project_pref(db: &Db, project_id: &str) -> Result<ProjectPref, String> {
    project_pref_with(db, &PathCtx::from_env(), project_id).await
}

pub async fn prefs(db: &Db, paths: &PathCtx) -> Result<SyncPrefs, String> {
    let categories_off = {
        let mut conn = db.acquire().await.map_err(err)?;
        categories_off(&mut conn).await?
    };
    Ok(SyncPrefs { source_device: source_device(db).await?, categories_off, projects: project_prefs(db, paths).await? })
}

/// Update a project's prefs. Selecting it resets the pull cursor; a new
/// `local_path` goes through the project mapping (it may not exist yet).
pub async fn set_project_pref(db: &Db, paths: &PathCtx, project_id: &str, patch: ProjectPrefPatch) -> Result<ProjectPref, String> {
    if let Some(mb) = patch.max_file_mb {
        if mb <= 0 {
            return Err("max_file_mb must be positive".into());
        }
    }
    let excludes = match &patch.excludes {
        Some(list) => {
            let clean: Vec<&str> = list.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
            Some(serde_json::to_string(&clean).map_err(|e| e.to_string())?)
        }
        None => None,
    };
    let before = project_pref_with(db, paths, project_id).await?;
    if let Some(local) = patch.local_path.as_deref().filter(|p| *p != before.local_path) {
        bootstrap::set_project_mapping(db, paths, project_id, local).await?;
    }
    let mut tx = db.begin().await.map_err(err)?;
    sqlx::query("INSERT OR IGNORE INTO sync_project_prefs (project_id, enabled) VALUES (?, ?)")
        .bind(project_id)
        .bind(before.enabled as i64)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    sqlx::query(
        "UPDATE sync_project_prefs SET enabled = COALESCE(?, enabled), folder = COALESCE(?, folder), \
         default_excludes = COALESCE(?, default_excludes), excludes = COALESCE(?, excludes), \
         max_file_mb = COALESCE(?, max_file_mb) WHERE project_id = ?",
    )
    .bind(patch.enabled.map(i64::from))
    .bind(patch.folder.map(i64::from))
    .bind(patch.default_excludes.map(i64::from))
    .bind(excludes)
    .bind(patch.max_file_mb)
    .bind(project_id)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    if patch.enabled == Some(true) && !before.enabled {
        reset_cursor(&mut tx).await?;
        sqlx::query("DELETE FROM sync_skipped_rows WHERE project_id = ?")
            .bind(project_id)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    }
    tx.commit().await.map_err(err)?;
    project_pref_with(db, paths, project_id).await
}

/// Folder sync made a backup of the project.
#[allow(dead_code)] // folder sync
pub async fn set_last_backup_at(db: &Db, project_id: &str, at: &str) -> Result<(), String> {
    sqlx::query(
        "INSERT INTO sync_project_prefs (project_id, enabled, last_backup_at) VALUES (?, 1, ?) \
         ON CONFLICT(project_id) DO UPDATE SET last_backup_at = excluded.last_backup_at",
    )
    .bind(project_id)
    .bind(at)
    .execute(db)
    .await
    .map_err(err)?;
    Ok(())
}

// ---- Apply-side filter ------------------------------------------------------------------

/// A declared FK of a synced table.
#[derive(Debug, Clone)]
struct Fk {
    col: String,
    parent: String,
    parent_col: String,
    nullable: bool,
}

/// What this machine refuses, loaded once per apply call.
#[derive(Debug, Default)]
pub struct Filter {
    off: HashSet<String>,
    /// Projects with `enabled = 0`.
    unselected: HashSet<String>,
}

impl Filter {
    pub async fn load(conn: &mut SqliteConnection) -> Result<Self, String> {
        let off = categories_off(conn).await?.into_iter().collect();
        let unselected = sqlx::query_scalar("SELECT project_id FROM sync_project_prefs WHERE enabled = 0")
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?
            .into_iter()
            .collect();
        Ok(Filter { off, unselected })
    }

    /// Nothing is filtered (`secrets` is enforced by its own channel).
    fn is_noop(&self) -> bool {
        self.unselected.is_empty() && self.off.iter().all(|c| c == SECRETS)
    }

    pub fn category_off(&self, cat: &str) -> bool {
        self.off.contains(cat)
    }
}

/// Result of [`screen`]: the ops to apply (references to absent rows of a
/// disabled category cut) and how many were skipped.
pub struct Screened {
    pub keep: Vec<Op>,
    pub skipped: usize,
}

async fn fks(conn: &mut SqliteConnection, cache: &mut HashMap<String, Vec<Fk>>, tbl: &str) -> Result<Vec<Fk>, String> {
    if let Some(v) = cache.get(tbl) {
        return Ok(v.clone());
    }
    let rows: Vec<(String, String, Option<String>)> =
        sqlx::query_as(r#"SELECT "from", "table", "to" FROM pragma_foreign_key_list(?)"#)
            .bind(tbl)
            .fetch_all(&mut *conn)
            .await
            .map_err(err)?;
    let mut out = Vec::with_capacity(rows.len());
    for (col, parent, to) in rows {
        let notnull: Option<i64> = sqlx::query_scalar(r#"SELECT "notnull" FROM pragma_table_info(?) WHERE name = ?"#)
            .bind(tbl)
            .bind(&col)
            .fetch_optional(&mut *conn)
            .await
            .map_err(err)?;
        out.push(Fk { col, parent, parent_col: to.unwrap_or_else(|| "id".into()), nullable: notnull.unwrap_or(0) == 0 });
    }
    cache.insert(tbl.to_string(), out.clone());
    Ok(out)
}

/// Project of a project-scoped parent row: here, in this batch, or skipped
/// earlier. `None` = global (or unknown).
async fn parent_project(
    conn: &mut SqliteConnection,
    parent: &str,
    id: &str,
    batch: &HashMap<(String, String), Option<String>>,
) -> Result<Option<String>, String> {
    let here: Option<Option<String>> = sqlx::query_scalar(&format!("SELECT project_id FROM {} WHERE id = ?", q(parent)))
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if let Some(p) = here {
        return Ok(p);
    }
    if let Some(p) = batch.get(&(parent.to_string(), id.to_string())) {
        return Ok(p.clone());
    }
    sqlx::query_scalar("SELECT project_id FROM sync_skipped_rows WHERE tbl = ? AND id = ?")
        .bind(parent)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)
}

/// The projects a row belongs to: its own `project_id` and its scoped parents'.
async fn row_projects(
    conn: &mut SqliteConnection,
    tbl: &str,
    values: &Map<String, Value>,
    batch: &HashMap<(String, String), Option<String>>,
) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    if let Some(p) = values.get("project_id").and_then(Value::as_str) {
        out.push(p.to_string());
    }
    for (parent, _, col) in REFS.iter().filter(|(p, c, _)| *c == tbl && *p != tbl && SCOPED_PARENTS.contains(p)) {
        if let Some(id) = values.get(*col).and_then(Value::as_str) {
            if let Some(p) = parent_project(conn, parent, id, batch).await? {
                out.push(p);
            }
        }
    }
    Ok(out)
}

/// Apply-side filter over pulled row ops (see the module docs). Records the
/// skipped scoped parents and the cut references as it goes.
pub async fn screen(conn: &mut SqliteConnection, ops: Vec<Op>) -> Result<Screened, String> {
    let mut filter = Filter::load(conn).await?;
    // Projects new here arrive unselected (the insert trigger): so are the
    // rows travelling with them.
    let new_enabled: Option<String> = sqlx::query_scalar("SELECT value FROM sync_state WHERE key = ?")
        .bind(SYNCED_PROJECT_ENABLED_KEY)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if new_enabled.as_deref() != Some("1") {
        for op in ops.iter().filter(|o| o.tbl == "projects" && o.kind == OpKind::Upsert) {
            let Some(id) = op.pk.as_str() else { continue };
            let known: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ?1) OR EXISTS (SELECT 1 FROM sync_project_prefs WHERE project_id = ?1)",
            )
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
            if !known {
                filter.unselected.insert(id.to_string());
            }
        }
    }
    if filter.is_noop() {
        return Ok(Screened { keep: ops, skipped: 0 });
    }
    // Scoped parents arriving in this batch, for children ahead of them.
    let batch: HashMap<(String, String), Option<String>> = ops
        .iter()
        .filter(|o| o.kind == OpKind::Upsert && SCOPED_PARENTS.contains(&o.tbl.as_str()))
        .filter_map(|o| {
            let id = o.pk.as_str()?.to_string();
            let project = o.row.as_ref()?.get("project_id").and_then(Value::as_str).map(str::to_string);
            Some(((o.tbl.clone(), id), project))
        })
        .collect();
    let mut fk_cache = HashMap::new();
    let mut keep = Vec::with_capacity(ops.len());
    let mut skipped = 0;
    for mut op in ops {
        if op_category(&op).is_some_and(|c| filter.category_off(c)) {
            skipped += 1;
            continue;
        }
        let Some(spec) = schema::spec(&op.tbl) else {
            keep.push(op);
            continue;
        };
        if !filter.unselected.is_empty() && op.tbl != "projects" {
            let mut values = match op.kind {
                OpKind::Upsert => op.row.clone().unwrap_or_default(),
                // A delete is judged on the row it would remove.
                OpKind::Delete => read_row(conn, spec, &op.pk).await?.unwrap_or_default(),
            };
            if let Value::Object(pk) = &op.pk {
                values.extend(pk.clone());
            }
            let projects = row_projects(conn, &op.tbl, &values, &batch).await?;
            if let Some(p) = projects.iter().find(|p| filter.unselected.contains(*p)) {
                skipped += 1;
                if op.kind == OpKind::Upsert && SCOPED_PARENTS.contains(&op.tbl.as_str()) {
                    if let Some(id) = op.pk.as_str() {
                        sqlx::query(
                            "INSERT INTO sync_skipped_rows (tbl, id, project_id, session_id) VALUES (?, ?, ?, ?) \
                             ON CONFLICT(tbl, id) DO UPDATE SET project_id = excluded.project_id, session_id = excluded.session_id",
                        )
                        .bind(&op.tbl)
                        .bind(id)
                        .bind(p)
                        .bind(values.get("session_id").and_then(Value::as_str))
                        .execute(&mut *conn)
                        .await
                        .map_err(err)?;
                    }
                }
                continue;
            }
        }
        if op.kind == OpKind::Upsert {
            let mut cut = Map::new();
            let mut required_missing = false;
            for fk in fks(conn, &mut fk_cache, &op.tbl).await? {
                if !table_category(&fk.parent).is_some_and(|c| filter.category_off(c)) {
                    continue;
                }
                let Some(v) = op.row.as_ref().and_then(|r| r.get(&fk.col)).filter(|v| !v.is_null()).cloned() else {
                    continue;
                };
                let sql = format!("SELECT EXISTS (SELECT 1 FROM {} WHERE {} = ?)", q(&fk.parent), q(&fk.parent_col));
                let present: bool = super::codec::bind_value(sqlx::query(&sql), &v)
                    .fetch_one(&mut *conn)
                    .await
                    .map_err(err)?
                    .get(0);
                if present {
                    continue;
                }
                if !fk.nullable {
                    required_missing = true;
                    break;
                }
                cut.insert(fk.col.clone(), v);
            }
            if required_missing {
                skipped += 1;
                continue;
            }
            if !cut.is_empty() {
                if let Some(row) = op.row.as_mut() {
                    for col in cut.keys() {
                        row.insert(col.clone(), Value::Null);
                    }
                }
                sqlx::query(
                    "INSERT INTO sync_stripped_refs (tbl, pk, refs) VALUES (?, ?, ?) \
                     ON CONFLICT(tbl, pk) DO UPDATE SET refs = excluded.refs",
                )
                .bind(&op.tbl)
                .bind(op.pk_key())
                .bind(Value::Object(cut).to_string())
                .execute(&mut *conn)
                .await
                .map_err(err)?;
            }
        }
        keep.push(op);
    }
    Ok(Screened { keep, skipped })
}

/// Push side: put back the references [`screen`] cut from this row (still
/// NULL here), so this machine never erases them on the other devices.
pub async fn restore_refs(conn: &mut SqliteConnection, tbl: &str, pk: &str, wire: &mut Map<String, Value>) -> Result<(), String> {
    let refs: Option<String> = sqlx::query_scalar("SELECT refs FROM sync_stripped_refs WHERE tbl = ? AND pk = ?")
        .bind(tbl)
        .bind(pk)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    let Some(Value::Object(refs)) = refs.and_then(|s| serde_json::from_str(&s).ok()) else { return Ok(()) };
    for (col, v) in refs {
        if wire.get(&col).is_none_or(Value::is_null) {
            wire.insert(col, v);
        }
    }
    Ok(())
}

/// The project a session file (run log, transcript, rollout) belongs to.
async fn file_project(db: &Db, paths: &PathCtx, kind: FileKind, portable: &str, op: &Op) -> Option<String> {
    if let Some(run) = files::run_of_op(db, paths, op).await {
        return sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?").bind(run).fetch_optional(db).await.ok().flatten();
    }
    let path = files::localize(paths, kind, portable).ok()?;
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let session = match kind {
        FileKind::CodexRollout => stem.strip_prefix("rollout-").and_then(|s| s.get(20..)).unwrap_or_default().to_string(),
        _ => stem.clone(),
    };
    let skipped: Option<String> = sqlx::query_scalar(
        "SELECT project_id FROM sync_skipped_rows WHERE tbl = 'runs' AND (id = ? OR session_id = ?) LIMIT 1",
    )
    .bind(&stem)
    .bind(&session)
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if skipped.is_some() {
        return skipped;
    }
    let projects: Vec<(String, String)> = sqlx::query_as("SELECT id, path FROM projects").fetch_all(db).await.ok()?;
    match kind {
        FileKind::RunLog => projects.into_iter().find(|(_, root)| path.starts_with(root)).map(|(id, _)| id),
        FileKind::ClaudeTranscript => {
            let dir = path.parent()?.file_name()?.to_string_lossy().into_owned();
            projects.into_iter().find(|(_, root)| crate::commands::sessions::encode_project_dir(root) == dir).map(|(id, _)| id)
        }
        _ => None,
    }
}

/// Whether a pulled `file` op is refused here (disabled category, or a
/// session file of an unselected project).
pub async fn skip_file(db: &Db, filter: &Filter, paths: &PathCtx, op: &Op) -> Result<bool, String> {
    let kind = op.pk.get("kind").and_then(Value::as_str).and_then(FileKind::parse);
    let (Some(kind), Some(portable)) = (kind, op.pk.get("path").and_then(Value::as_str)) else { return Ok(false) };
    let cat = file_category(kind);
    if cat == SECRETS {
        // Gated by `with_keys` in `files::apply_op`.
        return Ok(false);
    }
    if filter.category_off(cat) {
        return Ok(true);
    }
    if cat != SESSIONS || filter.unselected.is_empty() {
        return Ok(false);
    }
    Ok(file_project(db, paths, kind, portable, op).await.is_some_and(|p| filter.unselected.contains(&p)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::settings::PORTABLE_SETTING_KEYS;

    #[test]
    fn every_table_is_classified() {
        let mut seen = HashSet::new();
        for t in schema::SYNCED_TABLES {
            let n = ALWAYS_ON.contains(&t.name) as usize + TABLE_CATEGORIES.iter().filter(|(x, _)| *x == t.name).count();
            assert_eq!(n, 1, "{} must be in exactly one of ALWAYS_ON / TABLE_CATEGORIES", t.name);
            seen.insert(t.name);
        }
        for (t, c) in TABLE_CATEGORIES {
            assert!(seen.contains(t), "{t} is not a synced table");
            assert!(CATEGORIES.contains(c), "{t}: unknown category {c}");
        }
        for t in ALWAYS_ON {
            assert!(seen.contains(t), "{t} is not a synced table");
        }
        for p in SCOPED_PARENTS {
            assert!(schema::spec(p).is_some_and(|s| s.pk == ["id"]), "{p}");
        }
        assert_eq!(table_category("projects"), None);
        assert_eq!(table_category("runs"), Some(SESSIONS));
    }

    #[test]
    fn prefs_settings_are_machine_local() {
        let s = schema::spec("settings").unwrap();
        for key in [SOURCE_KEY, CATEGORIES_OFF_KEY] {
            assert!(!PORTABLE_SETTING_KEYS.contains(&key), "{key} must not be portable");
            assert!(!s.accepts(Some(key)), "{key} must not be synced");
        }
    }

    // ---- Behaviour over two in-memory devices ----

    use crate::sync::clock::Hlc;
    use crate::sync::op::OP_VERSION;
    use crate::sync::store::{ApplyReport, SqliteSyncStore, SyncStore};
    use crate::sync::SCHEMA_VERSION;
    use serde_json::json;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    /// A device with the production default: synced projects start unselected.
    async fn device(name: &str, home: &str) -> (Db, SqliteSyncStore) {
        let db = mem_db().await;
        exec(&db, &format!("DELETE FROM sync_state WHERE key = '{SYNCED_PROJECT_ENABLED_KEY}'")).await;
        let store = SqliteSyncStore::new(db.clone(), name.into()).with_paths(PathCtx::with_home(home));
        (db, store)
    }

    async fn exec(db: &Db, sql: &str) {
        if let Err(e) = sqlx::query(sql).execute(db).await {
            panic!("{sql}: {e}");
        }
    }

    async fn scalar(db: &Db, sql: &str) -> Option<String> {
        sqlx::query_scalar(sql).fetch_optional(db).await.unwrap()
    }

    async fn count(db: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
    }

    /// Everything `from` has pending (kept for a re-pull), applied to `to`.
    async fn push(from: &SqliteSyncStore) -> Vec<Op> {
        let ops = from.pending(1000).await.unwrap();
        if let Some((seq, _)) = ops.last() {
            from.mark_pushed(*seq).await.unwrap();
        }
        ops.into_iter().map(|(_, o)| o).collect()
    }

    async fn source_with_project() -> (Db, SqliteSyncStore, Vec<Op>) {
        let (db_a, a) = device("a", "/Users/a").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p1', '/Users/a/w/p1', 'x')").await;
        exec(&db_a, "INSERT INTO runs (id, project_id, type, created_at, engine, session_id) VALUES ('r1', 'p1', 'session', 'x', 'claude', 's1')").await;
        exec(&db_a, "INSERT INTO run_usage (id, run_id, project_id, engine, created_at) VALUES ('u1', 'r1', 'p1', 'claude', 'x')").await;
        exec(&db_a, "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) VALUES ('n1', 'in p1', 'b', 'p1', 0, 'x', 'x')").await;
        exec(&db_a, "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n2', 'global', 'b', 1, 'x', 'x')").await;
        exec(&db_a, "INSERT INTO note_runs (note_id, run_id, project_id, source, linked_at) VALUES ('n2', 'r1', 'p1', 'manual', 'x')").await;
        let ops = push(&a).await;
        (db_a, a, ops)
    }

    #[tokio::test]
    async fn unselected_synced_project_is_listed_but_its_rows_are_skipped() {
        let (_db_a, _a, ops) = source_with_project().await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_b, "INSERT INTO sync_state (key, value) VALUES ('pull_cursor', '42')").await;

        let r = b.apply_remote(ops.clone()).await.unwrap();
        // projects + the global note; the run, its usage, the p1 note and the
        // link to the p1 run are skipped — not conflicts, not deferred.
        assert_eq!(r, ApplyReport { applied: 2, skipped_older: 4, conflicts: 0, deferred: 0 });
        assert_eq!(scalar(&db_b, "SELECT name FROM projects WHERE id = 'p1'").await.as_deref(), Some("p1"));
        assert_eq!(count(&db_b, "SELECT enabled FROM sync_project_prefs WHERE project_id = 'p1'").await, 0);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM runs").await, 0);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM notes").await, 1);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_deferred").await, 0);
        assert_eq!(scalar(&db_b, "SELECT project_id FROM sync_skipped_rows WHERE tbl = 'runs' AND id = 'r1'").await.as_deref(), Some("p1"));
        let listed = prefs(&db_b, &PathCtx::with_home("/Users/b")).await.unwrap();
        let p = &listed.projects[0];
        assert_eq!((p.project_id.as_str(), p.enabled, p.exists, p.running_here), ("p1", false, false, false));

        // Selecting it resets the cursor; the re-pull applies what was skipped.
        let patch = ProjectPrefPatch { enabled: Some(true), ..Default::default() };
        let p = set_project_pref(&db_b, &PathCtx::with_home("/Users/b"), "p1", patch).await.unwrap();
        assert!(p.enabled);
        assert_eq!(b.cursor().await.unwrap(), 0);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_skipped_rows").await, 0);
        let r = b.apply_remote(ops.clone()).await.unwrap();
        assert_eq!(r, ApplyReport { applied: 4, skipped_older: 2, conflicts: 0, deferred: 0 });
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM runs").await, 1);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM notes").await, 2);
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM note_runs").await, 1);

        // Re-applying the whole log again (or older versions) is harmless.
        let r = b.apply_remote(ops.clone()).await.unwrap();
        assert_eq!(r, ApplyReport { applied: 0, skipped_older: 6, conflicts: 0, deferred: 0 });
        let mut older = ops.iter().find(|o| o.pk == json!("n1")).unwrap().clone();
        older.hlc = Hlc::new(1, 0, "a");
        older.row.as_mut().unwrap().insert("title".into(), json!("stale"));
        assert_eq!(b.apply_remote(vec![older]).await.unwrap().skipped_older, 1);
        assert_eq!(scalar(&db_b, "SELECT title FROM notes WHERE id = 'n1'").await.as_deref(), Some("in p1"));
        assert!(b.pending(10).await.unwrap().is_empty(), "nothing applied is echoed");
    }

    #[tokio::test]
    async fn deletes_and_children_of_an_unselected_project_are_skipped() {
        let (db_a, a, ops) = source_with_project().await;
        let (db_b, b) = device("b", "/Users/b").await;
        // Selected first, then unselected: its rows stay but stop changing.
        b.apply_remote(ops.clone()).await.unwrap();
        exec(&db_b, "UPDATE sync_project_prefs SET enabled = 1").await;
        b.apply_remote(ops).await.unwrap();
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM runs").await, 1);
        exec(&db_b, "UPDATE sync_project_prefs SET enabled = 0").await;
        exec(&db_a, "DELETE FROM notes WHERE id = 'n1'").await;
        exec(&db_a, "UPDATE runs SET status = 'done' WHERE id = 'r1'").await;
        let r = b.apply_remote(push(&a).await).await.unwrap();
        assert_eq!((r.applied, r.skipped_older, r.deferred), (0, 2, 0));
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM notes WHERE id = 'n1'").await, 1);
    }

    #[tokio::test]
    async fn locally_created_projects_are_selected_and_seed_unselects_absent_ones() {
        let db = mem_db().await;
        let here = std::env::temp_dir().to_string_lossy().to_string();
        exec(&db, &format!("INSERT INTO projects (id, name, path, created_at) VALUES ('here', 'h', '{here}', 'x')")).await;
        exec(&db, "INSERT INTO projects (id, name, path, created_at) VALUES ('gone', 'g', '/definitely/not/here', 'x')").await;
        assert_eq!(count(&db, "SELECT SUM(enabled) FROM sync_project_prefs").await, 2);
        // As migration 0066 leaves it for the first start.
        exec(&db, "INSERT INTO sync_state (key, value) VALUES ('prefs_seed_pending', '1')").await;
        seed(&db).await.unwrap();
        assert_eq!(scalar(&db, "SELECT project_id FROM sync_project_prefs WHERE enabled = 1").await.as_deref(), Some("here"));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM sync_state WHERE key = 'prefs_seed_pending'").await, 0);
        // A re-keyed project keeps its prefs; a removed one drops them.
        exec(&db, "UPDATE projects SET id = 'here2' WHERE id = 'here'").await;
        assert_eq!(count(&db, "SELECT enabled FROM sync_project_prefs WHERE project_id = 'here2'").await, 1);
        exec(&db, "DELETE FROM projects WHERE id = 'gone'").await;
        assert_eq!(count(&db, "SELECT COUNT(*) FROM sync_project_prefs").await, 1);
    }

    #[tokio::test]
    async fn categories_skip_and_enabling_resets_the_cursor() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'x', 0, 'x')").await;
        exec(&db_a, "INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'dark'), ('sync_leader_device', 'a')").await;
        set_categories_off(&db_b, &["notes".into(), "config".into()]).await.unwrap();
        let mut conn = db_b.acquire().await.unwrap();
        assert_eq!(categories_off(&mut conn).await.unwrap(), vec!["config", "notes"]);
        drop(conn);
        let ops = push(&a).await;
        let r = b.apply_remote(ops.clone()).await.unwrap();
        // Sync-only settings are always on.
        assert_eq!((r.applied, r.skipped_older, r.conflicts, r.deferred), (1, 2, 0, 0));
        assert_eq!(scalar(&db_b, "SELECT value FROM settings WHERE key = 'sync_leader_device'").await.as_deref(), Some("a"));
        assert_eq!(scalar(&db_b, "SELECT value FROM settings WHERE key = 'theme'").await.as_deref(), Some("system"));

        b.set_cursor(42).await.unwrap();
        set_categories_off(&db_b, &["config".into(), "notes".into()]).await.unwrap();
        assert_eq!(b.cursor().await.unwrap(), 42, "nothing enabled: cursor kept");
        set_categories_off(&db_b, &["config".into()]).await.unwrap();
        assert_eq!(b.cursor().await.unwrap(), 0);
        let r = b.apply_remote(ops).await.unwrap();
        assert_eq!((r.applied, r.skipped_older), (1, 2));
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM todos").await, 1);
        assert!(set_categories_off(&db_b, &["nope".into()]).await.is_err());
    }

    #[tokio::test]
    async fn secrets_category_is_the_secrets_toggle() {
        let db = mem_db().await;
        set_categories_off(&db, &[SECRETS.into()]).await.unwrap();
        assert_eq!(scalar(&db, "SELECT value FROM settings WHERE key = 'sync_secrets_enabled'").await.as_deref(), Some("false"));
        assert_eq!(scalar(&db, &format!("SELECT value FROM settings WHERE key = '{CATEGORIES_OFF_KEY}'")).await, None);
        // Turned back on elsewhere (`sync_set_secrets`): reported on.
        exec(&db, "UPDATE settings SET value = 'true' WHERE key = 'sync_secrets_enabled'").await;
        let p = prefs(&db, &PathCtx::with_home("/Users/b")).await.unwrap();
        assert!(p.categories_off.is_empty());
        exec(&db, "UPDATE settings SET value = 'false' WHERE key = 'sync_secrets_enabled'").await;
        let p = prefs(&db, &PathCtx::with_home("/Users/b")).await.unwrap();
        assert_eq!(p.categories_off, vec![SECRETS]);
        set_source_device(&db, "dev-a").await.unwrap();
        assert_eq!(source_device(&db).await.unwrap().as_deref(), Some("dev-a"));
        set_source_device(&db, "").await.unwrap();
        assert_eq!(source_device(&db).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_reference_into_a_disabled_category_is_cut_and_pushed_back() {
        let (db_a, a) = device("a", "/Users/a").await;
        let (db_b, b) = device("b", "/Users/b").await;
        exec(&db_a, "INSERT INTO github_accounts (id, label, created_at) VALUES ('g1', 'gh', 'x')").await;
        exec(&db_a, "INSERT INTO projects (id, name, path, created_at, github_account_id) VALUES ('p1', 'p1', '/Users/a/w/p1', 'x', 'g1')").await;
        set_categories_off(&db_b, &["accounts".into()]).await.unwrap();
        let ops = push(&a).await;
        let r = b.apply_remote(ops.clone()).await.unwrap();
        assert_eq!((r.applied, r.skipped_older, r.deferred), (1, 1, 0), "the project is not held back for its account");
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM projects WHERE github_account_id IS NULL").await, 1);

        // A local edit here never erases the link on the other devices.
        exec(&db_b, "UPDATE projects SET name = 'renamed' WHERE id = 'p1'").await;
        let out = push(&b).await;
        let row = out.iter().find(|o| o.tbl == "projects").unwrap().row.clone().unwrap();
        assert_eq!((row["name"].clone(), row["github_account_id"].clone()), (json!("renamed"), json!("g1")));

        // Enabling accounts re-applies the row in full on the re-pull.
        set_categories_off(&db_b, &[]).await.unwrap();
        assert_eq!(count(&db_b, "SELECT COUNT(*) FROM sync_stripped_refs").await, 0);
        b.apply_remote(ops).await.unwrap();
        assert_eq!(scalar(&db_b, "SELECT github_account_id FROM projects WHERE id = 'p1'").await.as_deref(), Some("g1"));
    }

    fn file_op(kind: &str, path: &str) -> Op {
        Op {
            v: OP_VERSION,
            schema: SCHEMA_VERSION,
            hlc: Hlc::new(1, 0, "a"),
            tbl: files::TBL.into(),
            pk: json!({"kind": kind, "path": path}),
            kind: OpKind::Upsert,
            row: Some(Map::new()),
        }
    }

    #[tokio::test]
    async fn session_files_of_unselected_projects_and_disabled_categories_are_skipped() {
        let (_db_a, _a, ops) = source_with_project().await;
        let (db_b, b) = device("b", "/Users/b").await;
        b.apply_remote(ops).await.unwrap();
        let paths = PathCtx::with_home("/Users/b").with_app_data("/Users/b/data");
        let filter = Filter::load(&mut db_b.acquire().await.unwrap()).await.unwrap();
        let log = file_op("run_log", "~/w/p1/.devdy/runs/r1.log");
        let transcript = file_op("claude_transcript", "~/.claude/projects/-Users-b-w-p1/s1.jsonl");
        let other = file_op("run_log", "~/w/other/.devdy/runs/r9.log");
        let lib = file_op("library", "@data/skills/x/SKILL.md");
        assert!(skip_file(&db_b, &filter, &paths, &log).await.unwrap(), "run of the unselected project");
        assert!(skip_file(&db_b, &filter, &paths, &transcript).await.unwrap(), "session of the skipped run");
        assert!(!skip_file(&db_b, &filter, &paths, &other).await.unwrap(), "unknown project: kept");
        assert!(!skip_file(&db_b, &filter, &paths, &lib).await.unwrap());
        set_categories_off(&db_b, &["library".into()]).await.unwrap();
        let filter = Filter::load(&mut db_b.acquire().await.unwrap()).await.unwrap();
        assert!(skip_file(&db_b, &filter, &paths, &lib).await.unwrap());
    }

    #[tokio::test]
    async fn project_pref_patch_and_running_here() {
        let db = mem_db().await;
        let home = PathCtx::with_home("/Users/b");
        exec(&db, "INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p1', '/Users/b/w/p1', 'x')").await;
        let patch = ProjectPrefPatch {
            folder: Some(true),
            excludes: Some(vec![" *.log ".into(), "".into()]),
            max_file_mb: Some(50),
            ..Default::default()
        };
        let p = set_project_pref(&db, &home, "p1", patch).await.unwrap();
        assert_eq!((p.enabled, p.folder, p.default_excludes, p.max_file_mb), (true, true, true, 50));
        assert_eq!(p.excludes, vec!["*.log"]);
        assert!(set_project_pref(&db, &home, "p1", ProjectPrefPatch { max_file_mb: Some(0), ..Default::default() }).await.is_err());
        assert!(set_project_pref(&db, &home, "nope", ProjectPrefPatch::default()).await.is_err());
        let patch: ProjectPrefPatch = serde_json::from_value(json!({"default_excludes": false})).unwrap();
        assert!(!set_project_pref(&db, &home, "p1", patch).await.unwrap().default_excludes);

        exec(&db, "INSERT INTO runs (id, project_id, type, status, created_at, owner_device) VALUES ('r', 'p1', 'session', 'running', 'x', 'elsewhere')").await;
        assert!(!project_pref_with(&db, &home, "p1").await.unwrap().running_here, "running on another device");
        exec(&db, "UPDATE runs SET owner_device = NULL").await;
        assert!(project_pref_with(&db, &home, "p1").await.unwrap().running_here);

        set_last_backup_at(&db, "p1", "2026-10-30T00:00:00Z").await.unwrap();
        assert_eq!(project_pref_with(&db, &home, "p1").await.unwrap().last_backup_at.as_deref(), Some("2026-10-30T00:00:00Z"));
    }
}
