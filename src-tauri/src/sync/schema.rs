//! Registry of the synced tables (design §2). The capture triggers, the row
//! codec and the apply engine are all driven from `SYNCED_TABLES`; a table that
//! is not listed here is never captured nor accepted from remote.
//!
//! `SYNCED_TABLES` is ordered parents-first so a batch applied in this order
//! rarely needs the FK deferral path.
//!
//! Not synced (design §0 D5), checked against every migration up to 0055 — the
//! `every_table_is_classified` test fails when a new table is neither synced
//! nor listed in `EXCLUDED_TABLES`:
//! - `remote_audit`: Remote Control audit of this machine's host agent.
//! - `conductor_sessions`, `conductor_events`: live state of an in-process
//!   conductor, meaningless on another machine.
//! - `sync_*`: the sync bookkeeping itself.
//! - `sync_conflicts`, `rule_sync_conflicts`: drift between the library and
//!   this machine's working-tree copies of skills/rules; each machine detects
//!   its own.
//! - `project_secrets`: legacy per-project PAT Keychain reference, no longer
//!   read or written by the app (secrets sync is a separate channel, D4).
//! - `pulse_requests`: work queue drained by the local Pulse poller (singleton
//!   jobs run on the leader, D6).
//! - `run_usage_baseline`: per-run parse baseline derived from the local run
//!   logs (rebuilt by `model_usage::rebuild_from_logs`).
//! - `slack_legacy_orphans`: one-off migration bookkeeping.
//! - `sqlite_sequence`, `_sqlx_migrations`: SQLite / sqlx internals.
//! - `project_skills_new` does not exist any more (0006 renamed it).
//! - `settings` rows whose key is not in `PORTABLE_SETTING_KEYS` nor matches
//!   `SYNCED_SETTING_PREFIXES` (CLI paths, `remote_*`, window/UI state, ...).

use crate::commands::settings::PORTABLE_SETTING_KEYS;

/// Only rows whose `col` value is one of `allowed`, or starts with one of
/// `prefixes`, are synced.
#[derive(Debug, Clone, Copy)]
pub struct RowFilter {
    pub col: &'static str,
    pub allowed: &'static [&'static str],
    pub prefixes: &'static [&'static str],
}

impl RowFilter {
    pub fn accepts(&self, v: &str) -> bool {
        self.allowed.contains(&v) || self.prefixes.iter().any(|p| v.starts_with(p))
    }
}

/// Sync-only settings, outside `PORTABLE_SETTING_KEYS` so file backups don't
/// carry them (see `sync::leader`): the leader device, and one name record per
/// device (a prefix, so each device writes only its own row).
pub const SYNCED_SETTING_PREFIXES: &[&str] = &["sync_leader_device", "sync_device_name:"];

#[derive(Debug, Clone, Copy)]
pub struct TableSpec {
    pub name: &'static str,
    pub pk: &'static [&'static str],
    /// Machine-local columns: never sent, never overwritten on apply (a new row
    /// gets the column default).
    pub skip_cols: &'static [&'static str],
    /// Filesystem paths, sent in portable form (`~/...`) and re-expanded.
    pub path_cols: &'static [&'static str],
    /// Columns identifying "the same thing" across devices whose ids differ
    /// (used by the bootstrap merge; a NULL in any of them never matches).
    /// `projects` is matched by `bootstrap` itself (path, git remote, name).
    pub natural_key: &'static [&'static str],
    pub filter: Option<RowFilter>,
}

impl TableSpec {
    const fn new(name: &'static str, pk: &'static [&'static str]) -> Self {
        TableSpec { name, pk, skip_cols: &[], path_cols: &[], natural_key: pk, filter: None }
    }
    const fn skip(mut self, cols: &'static [&'static str]) -> Self {
        self.skip_cols = cols;
        self
    }
    const fn paths(mut self, cols: &'static [&'static str]) -> Self {
        self.path_cols = cols;
        self
    }
    const fn natural(mut self, cols: &'static [&'static str]) -> Self {
        self.natural_key = cols;
        self
    }
    const fn filter(
        mut self,
        col: &'static str,
        allowed: &'static [&'static str],
        prefixes: &'static [&'static str],
    ) -> Self {
        self.filter = Some(RowFilter { col, allowed, prefixes });
        self
    }

    pub fn is_skipped(&self, col: &str) -> bool {
        self.skip_cols.contains(&col)
    }

    pub fn is_path(&self, col: &str) -> bool {
        self.path_cols.contains(&col)
    }

    /// Whether a row with this value in the filter column is synced.
    pub fn accepts(&self, value: Option<&str>) -> bool {
        match self.filter {
            None => true,
            Some(f) => value.is_some_and(|v| f.accepts(v)),
        }
    }

    /// Position in `SYNCED_TABLES` (parents first).
    pub fn level(&self) -> usize {
        SYNCED_TABLES.iter().position(|t| t.name == self.name).unwrap_or(usize::MAX)
    }
}

const ID: &[&str] = &["id"];
const LABEL: &[&str] = &["label"];
const NAME: &[&str] = &["name"];

pub static SYNCED_TABLES: &[TableSpec] = &[
    TableSpec::new("settings", &["key"]).filter("key", PORTABLE_SETTING_KEYS, SYNCED_SETTING_PREFIXES),
    // Account metadata only; tokens live in the Keychain (secrets channel).
    TableSpec::new("github_accounts", ID).natural(LABEL),
    TableSpec::new("gitlab_accounts", ID).natural(LABEL),
    TableSpec::new("aws_accounts", ID).natural(LABEL),
    TableSpec::new("azure_accounts", ID).natural(LABEL),
    TableSpec::new("google_accounts", ID).natural(LABEL),
    TableSpec::new("claude_accounts", ID)
        .natural(LABEL)
        .paths(&["config_dir"])
        .skip(&["status", "last_checked_at"]),
    TableSpec::new("servers", ID)
        .natural(LABEL)
        .paths(&["private_key_path"])
        .skip(&["status", "last_checked_at"]),
    TableSpec::new("mcp_servers", ID).natural(NAME),
    TableSpec::new("skills", ID).natural(NAME).paths(&["source_path"]),
    TableSpec::new("rules", ID).natural(NAME).paths(&["source_path"]),
    TableSpec::new("skill_groups", ID).natural(NAME),
    TableSpec::new("rule_groups", ID).natural(NAME),
    TableSpec::new("skill_group_members", &["group_id", "skill_id"]),
    TableSpec::new("rule_group_members", &["group_id", "rule_id"]),
    TableSpec::new("projects", ID).natural(&["path", "name"]).paths(&["path"]),
    TableSpec::new("repos", ID).natural(&["project_id", "name"]).paths(&["path"]),
    TableSpec::new("project_skills", &["project_id", "skill_id"]),
    TableSpec::new("project_rules", &["project_id", "rule_id"]),
    TableSpec::new("project_skill_groups", &["project_id", "group_id"]),
    TableSpec::new("project_rule_groups", &["project_id", "group_id"]),
    TableSpec::new("project_mcp_servers", &["project_id", "server_id"]),
    TableSpec::new("project_servers", &["project_id", "server_id", "role"]),
    // Same imported CLI session (unique index idx_runs_project_engine_session).
    TableSpec::new("runs", ID)
        .natural(&["project_id", "engine", "session_id"])
        .paths(&["output_path", "input_path", "transcript_path"])
        // How far the local session watcher has mirrored the transcript.
        .skip(&["transcript_synced_size"]),
    TableSpec::new("run_usage", ID),
    TableSpec::new("deleted_sessions", &["project_id", "session_id"]),
    TableSpec::new("notes", ID),
    TableSpec::new("todos", ID),
    TableSpec::new("note_runs", &["note_id", "run_id"]),
    TableSpec::new("todo_runs", &["todo_id", "run_id"]),
    TableSpec::new("slack_identities", &["team_id"]),
    // avatar_path is relative to this machine's app data dir (re-downloaded).
    TableSpec::new("slack_users", &["team_id", "user_id"]).skip(&["avatar_path"]),
    TableSpec::new("slack_channels", &["team_id", "channel_id"]),
    TableSpec::new("slack_threads", ID).natural(&["channel_id", "thread_ts"]),
    TableSpec::new("slack_messages", &["thread_id", "ts"]),
    // Attachments sync as metadata only; each machine downloads its own copy.
    TableSpec::new("slack_files", ID)
        .natural(&["thread_id", "slack_file_id"])
        .skip(&["file_path", "download_status", "download_error"]),
    TableSpec::new("slack_thread_runs", &["thread_id", "run_id"]),
    // Unique index idx_pulse_watches_source.
    TableSpec::new("pulse_watches", ID).natural(&["source_type", "source_ref"]),
    TableSpec::new("pulse_digests", ID),
];

/// Tables deliberately left out (see the module docs for the reasons).
#[cfg_attr(not(test), allow(dead_code))]
pub static EXCLUDED_TABLES: &[&str] = &[
    "remote_audit",
    "conductor_sessions",
    "conductor_events",
    "sync_conflicts",
    "rule_sync_conflicts",
    "project_secrets",
    "pulse_requests",
    "run_usage_baseline",
    "slack_legacy_orphans",
    "sqlite_sequence",
    "_sqlx_migrations",
];

pub fn spec(tbl: &str) -> Option<&'static TableSpec> {
    SYNCED_TABLES.iter().find(|t| t.name == tbl)
}

/// SQL identifier quoting.
pub fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// SQL string literal quoting.
pub fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// SQL expression giving the canonical pk JSON of a row: `json_quote(R."id")`
/// for a single key, `json_object(...)` with sorted keys for a composite one.
/// `row` is a qualifier such as `NEW`, `OLD`, or a table name.
pub fn pk_sql(spec: &TableSpec, row: &str) -> String {
    if let [only] = spec.pk {
        return format!("json_quote({row}.{})", q(only));
    }
    let mut cols: Vec<&str> = spec.pk.to_vec();
    cols.sort();
    let args: Vec<String> = cols.iter().map(|c| format!("{}, {row}.{}", lit(c), q(c))).collect();
    format!("json_object({})", args.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_consistent() {
        let mut names = std::collections::HashSet::new();
        for t in SYNCED_TABLES {
            assert!(names.insert(t.name), "duplicate spec {}", t.name);
            assert!(!t.pk.is_empty(), "{} has no pk", t.name);
            assert!(!EXCLUDED_TABLES.contains(&t.name), "{} both synced and excluded", t.name);
            for c in t.pk {
                assert!(!t.is_skipped(c), "{}: pk column {c} skipped", t.name);
            }
        }
    }

    #[test]
    fn settings_filter_uses_portable_keys() {
        let s = spec("settings").unwrap();
        assert!(s.accepts(Some("theme")));
        assert!(!s.accepts(Some("claude_path")));
        assert!(!s.accepts(None));
        assert!(s.accepts(Some("sync_leader_device")));
        assert!(s.accepts(Some("sync_device_name:abc")));
        assert!(!s.accepts(Some("sync_paused")));
        assert!(!s.accepts(Some("sync_secrets_enabled")));
        assert!(spec("notes").unwrap().accepts(None));
    }

    #[test]
    fn pk_sql_sorts_composite_keys() {
        let s = spec("project_servers").unwrap();
        assert_eq!(
            pk_sql(s, "NEW"),
            r#"json_object('project_id', NEW."project_id", 'role', NEW."role", 'server_id', NEW."server_id")"#
        );
        assert_eq!(pk_sql(spec("notes").unwrap(), "OLD"), r#"json_quote(OLD."id")"#);
    }
}
