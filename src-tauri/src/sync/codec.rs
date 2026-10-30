//! Row <-> JSON map using the registry: drops machine-local columns, turns path
//! columns into a portable form (`@data/...` under Devdy's app data dir,
//! `~/...` under $HOME, absolute otherwise) and expands them again on the
//! receiving device.
//!
//! Claude transcript stores name each project folder after the encoded cwd
//! (`-Users-alice-w-p`), so inside a `projects/` folder the encoded home prefix
//! becomes `~` too (`~/.claude/projects/~-w-p/<session>.jsonl`) and is
//! re-encoded with this device's home.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sqlx::SqliteConnection;

use super::schema::{q, TableSpec};

/// Where this device keeps things: the home dir, Devdy's app data dir
/// (library, managed SSH keys; differs between the prod and dev builds) and the
/// project roots that live at a different place here than on the device that
/// synced them.
#[derive(Debug, Clone)]
pub struct PathCtx {
    pub home: PathBuf,
    pub app_data: Option<PathBuf>,
    /// (portable root as synced, local root), longest synced root first. Kept
    /// in `sync_state.project_path_map` (see `bootstrap::load_project_map`).
    pub project_map: Vec<(String, String)>,
    /// Config dirs of the Claude accounts set up here (`claude_accounts`):
    /// besides `~/.claude`, the only places a synced transcript may land.
    pub claude_dirs: Vec<PathBuf>,
}

/// Portable prefix of paths under the app data dir.
const DATA_MARK: &str = "@data";

impl PathCtx {
    pub fn from_env() -> Self {
        PathCtx {
            home: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default(),
            app_data: None,
            project_map: Vec::new(),
            claude_dirs: Vec::new(),
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        PathCtx { home: home.into(), app_data: None, project_map: Vec::new(), claude_dirs: Vec::new() }
    }

    pub fn with_app_data(mut self, dir: impl Into<PathBuf>) -> Self {
        self.app_data = Some(dir.into());
        self
    }

    pub fn with_claude_dirs(mut self, dirs: impl IntoIterator<Item = PathBuf>) -> Self {
        self.claude_dirs = dirs.into_iter().filter(|d| d.is_absolute()).collect();
        self
    }

    pub fn with_project_map(mut self, map: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut map: Vec<(String, String)> = map.into_iter().collect();
        map.sort_by(|a, b| b.0.len().cmp(&a.0.len()).then_with(|| a.0.cmp(&b.0)));
        self.project_map = map;
        self
    }
}

/// The part of `path` below `root` ("" for the root itself), if it is under it.
fn under<'a>(path: &'a str, root: &str) -> Option<&'a str> {
    let root = root.trim_end_matches('/');
    let rest = path.strip_prefix(root)?;
    if rest.is_empty() {
        Some("")
    } else {
        rest.strip_prefix('/')
    }
}

fn join(root: &str, rest: &str) -> String {
    if rest.is_empty() {
        root.to_string()
    } else {
        format!("{}/{rest}", root.trim_end_matches('/'))
    }
}

/// `encode_project_dir` of Claude Code (kept local so the codec stays pure).
fn encode_dir(path: &str) -> String {
    path.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

/// Rewrites the folder right after a `projects` component: `from` prefix -> `to`.
fn swap_encoded_home(rel: &str, from: &str, to: &str) -> String {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    for (i, part) in parts.iter().enumerate() {
        let after_projects = i >= 2 && parts[i - 1] == "projects";
        match part.strip_prefix(from) {
            Some(rest) if after_projects && !from.is_empty() && (rest.is_empty() || rest.starts_with('-')) => {
                out.push(format!("{to}{rest}"))
            }
            _ => out.push((*part).to_string()),
        }
    }
    out.join("/")
}

/// `@data/rel` under the app data dir, `~/rel` under the home dir, `path`
/// unchanged otherwise.
pub fn to_portable(ctx: &PathCtx, path: &str) -> String {
    if let Some(data) = ctx.app_data.as_deref().filter(|d| !d.as_os_str().is_empty()) {
        match Path::new(path).strip_prefix(data) {
            Ok(rel) if rel.as_os_str().is_empty() => return DATA_MARK.to_string(),
            Ok(rel) => return format!("{DATA_MARK}/{}", rel.to_string_lossy()),
            Err(_) => {}
        }
    }
    if ctx.home.as_os_str().is_empty() {
        return path.to_string();
    }
    match Path::new(path).strip_prefix(&ctx.home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => {
            let rel = swap_encoded_home(&rel.to_string_lossy(), &encode_dir(&ctx.home.to_string_lossy()), "~");
            format!("~/{rel}")
        }
        Err(_) => path.to_string(),
    }
}

/// Inverse of `to_portable` for this device. `@data` paths stay as they are
/// when the app data dir is unknown.
pub fn expand_portable(ctx: &PathCtx, portable: &str) -> String {
    if portable == "~" {
        return ctx.home.to_string_lossy().into_owned();
    }
    if let Some(data) = &ctx.app_data {
        if portable == DATA_MARK {
            return data.to_string_lossy().into_owned();
        }
        // `@data//x` must not become `/x` (`join` of an absolute path).
        if let Some(rel) = portable.strip_prefix("@data/").filter(|r| !r.starts_with('/')) {
            return data.join(rel).to_string_lossy().into_owned();
        }
    }
    match portable.strip_prefix("~/").filter(|r| !r.starts_with('/')) {
        Some(rel) => {
            let rel = swap_encoded_home(rel, "~", &encode_dir(&ctx.home.to_string_lossy()));
            ctx.home.join(rel).to_string_lossy().into_owned()
        }
        None => portable.to_string(),
    }
}

/// The single seam mapping a path from another device to this one: a path
/// under a mapped project root (matched by path, git remote URL or name at
/// bootstrap, or set by hand) moves under the local root; anything else is
/// `expand_portable`.
pub fn map_project_path(ctx: &PathCtx, portable: &str) -> String {
    for (synced, local) in &ctx.project_map {
        if let Some(rest) = under(portable, synced) {
            return join(local, rest);
        }
    }
    expand_portable(ctx, portable)
}

/// A file inside a project's `.devdy/` folder follows the project's mapping;
/// `map_project_path` already maps anything under a mapped root.
pub fn map_in_project(ctx: &PathCtx, portable: &str) -> String {
    map_project_path(ctx, portable)
}

/// Inverse of `map_project_path`: what this device sends for a local path.
pub fn portable_path(ctx: &PathCtx, path: &str) -> String {
    for (synced, local) in &ctx.project_map {
        if let Some(rest) = under(path, local) {
            return join(synced, rest);
        }
    }
    to_portable(ctx, path)
}

/// A local row (all columns) -> the wire form.
pub fn encode_row(ctx: &PathCtx, spec: &TableSpec, row: Map<String, Value>) -> Map<String, Value> {
    row.into_iter()
        .filter(|(c, _)| !spec.is_skipped(c))
        .map(|(c, v)| match v {
            Value::String(s) if spec.is_path(&c) => {
                let p = portable_path(ctx, &s);
                (c, Value::String(p))
            }
            v => (c, v),
        })
        .collect()
}

/// The wire form -> columns to write locally (skip columns removed).
pub fn decode_row(ctx: &PathCtx, spec: &TableSpec, row: &Map<String, Value>) -> Map<String, Value> {
    row.iter()
        .filter(|(c, _)| !spec.is_skipped(c))
        .map(|(c, v)| match v {
            Value::String(s) if spec.is_path(c) => (c.clone(), Value::String(map_project_path(ctx, s))),
            v => (c.clone(), v.clone()),
        })
        .collect()
}

pub async fn table_columns(conn: &mut SqliteConnection, tbl: &str) -> Result<Vec<String>, String> {
    sqlx::query_scalar("SELECT name FROM pragma_table_info(?) ORDER BY cid")
        .bind(tbl)
        .fetch_all(&mut *conn)
        .await
        .map_err(|e| e.to_string())
}

/// The pk column values of a canonical pk (`"id"` or `{"a":..,"b":..}`), in
/// `spec.pk` order.
pub fn pk_values(spec: &TableSpec, pk: &Value) -> Result<Vec<Value>, String> {
    match (spec.pk, pk) {
        ([_], v @ (Value::String(_) | Value::Number(_))) => Ok(vec![v.clone()]),
        (cols, Value::Object(m)) => cols
            .iter()
            .map(|c| m.get(*c).cloned().ok_or_else(|| format!("{}: pk is missing {c}", spec.name)))
            .collect(),
        _ => Err(format!("{}: malformed pk {pk}", spec.name)),
    }
}

/// Builds the pk value of a row from its columns (the inverse of `pk_values`).
#[cfg_attr(not(test), allow(dead_code))]
pub fn pk_of_row(spec: &TableSpec, row: &Map<String, Value>) -> Option<Value> {
    if let [only] = spec.pk {
        return row.get(*only).cloned();
    }
    let mut m = Map::new();
    for c in spec.pk {
        m.insert((*c).to_string(), row.get(*c)?.clone());
    }
    Some(Value::Object(m))
}

fn pk_where(spec: &TableSpec) -> String {
    spec.pk.iter().map(|c| format!("{} = ?", q(c))).collect::<Vec<_>>().join(" AND ")
}

pub fn bind_value<'q>(
    query: sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>>,
    v: &Value,
) -> sqlx::query::Query<'q, sqlx::Sqlite, sqlx::sqlite::SqliteArguments<'q>> {
    match v {
        Value::Null => query.bind(None::<String>),
        Value::Bool(b) => query.bind(*b as i64),
        Value::Number(n) => match n.as_i64() {
            Some(i) => query.bind(i),
            None => query.bind(n.as_f64()),
        },
        Value::String(s) => query.bind(s.clone()),
        other => query.bind(other.to_string()),
    }
}

/// The current local row (all columns, raw local form), if it exists.
pub async fn read_row(
    conn: &mut SqliteConnection,
    spec: &TableSpec,
    pk: &Value,
) -> Result<Option<Map<String, Value>>, String> {
    let cols = table_columns(conn, spec.name).await?;
    let args: Vec<String> = cols.iter().map(|c| format!("{}, {}", super::schema::lit(c), q(c))).collect();
    let sql = format!(
        "SELECT json_object({}) FROM {} WHERE {}",
        args.join(", "),
        q(spec.name),
        pk_where(spec)
    );
    let mut query = sqlx::query_scalar::<_, String>(&sql);
    for v in pk_values(spec, pk)? {
        query = match v {
            Value::String(s) => query.bind(s),
            Value::Number(n) => query.bind(n.as_i64()),
            other => query.bind(other.to_string()),
        };
    }
    let json: Option<String> = query.fetch_optional(&mut *conn).await.map_err(|e| e.to_string())?;
    match json {
        None => Ok(None),
        Some(s) => match serde_json::from_str::<Value>(&s).map_err(|e| e.to_string())? {
            Value::Object(m) => Ok(Some(m)),
            _ => Err("row is not an object".into()),
        },
    }
}

/// Insert or update a row in place (never `REPLACE`: that would cascade-delete
/// the children). Only columns that exist locally are written.
pub async fn upsert_row(
    conn: &mut SqliteConnection,
    spec: &TableSpec,
    row: &Map<String, Value>,
) -> Result<(), String> {
    let local = table_columns(conn, spec.name).await?;
    let cols: Vec<&String> = local.iter().filter(|c| row.contains_key(c.as_str()) && !spec.is_skipped(c)).collect();
    for p in spec.pk {
        if !cols.iter().any(|c| c.as_str() == *p) {
            return Err(format!("{}: row is missing pk column {p}", spec.name));
        }
    }
    let col_list: Vec<String> = cols.iter().map(|c| q(c)).collect();
    let marks = vec!["?"; cols.len()].join(", ");
    let pk_list: Vec<String> = spec.pk.iter().map(|c| q(c)).collect();
    let updates: Vec<String> = cols
        .iter()
        .filter(|c| !spec.pk.contains(&c.as_str()))
        .map(|c| format!("{0} = excluded.{0}", q(c)))
        .collect();
    let on_conflict = if updates.is_empty() {
        "DO NOTHING".to_string()
    } else {
        format!("DO UPDATE SET {}", updates.join(", "))
    };
    let sql = format!(
        "INSERT INTO {} ({}) VALUES ({marks}) ON CONFLICT({}) {on_conflict}",
        q(spec.name),
        col_list.join(", "),
        pk_list.join(", "),
    );
    let mut query = sqlx::query(&sql);
    for c in &cols {
        query = bind_value(query, &row[c.as_str()]);
    }
    query.execute(&mut *conn).await.map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn delete_row(conn: &mut SqliteConnection, spec: &TableSpec, pk: &Value) -> Result<(), String> {
    let sql = format!("DELETE FROM {} WHERE {}", q(spec.name), pk_where(spec));
    let mut query = sqlx::query(&sql);
    for v in pk_values(spec, pk)? {
        query = bind_value(query, &v);
    }
    query.execute(&mut *conn).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::schema::spec;
    use serde_json::json;

    #[test]
    fn portable_paths_round_trip() {
        let mac_a = PathCtx::with_home("/Users/alice");
        let mac_b = PathCtx::with_home("/Users/bob");
        assert_eq!(to_portable(&mac_a, "/Users/alice/Workspace/devdy"), "~/Workspace/devdy");
        assert_eq!(to_portable(&mac_a, "/Users/alice"), "~");
        assert_eq!(to_portable(&mac_a, "/Users/alicex/p"), "/Users/alicex/p");
        assert_eq!(to_portable(&mac_a, "/opt/repo"), "/opt/repo");
        assert_eq!(expand_portable(&mac_b, "~/Workspace/devdy"), "/Users/bob/Workspace/devdy");
        assert_eq!(expand_portable(&mac_b, "~"), "/Users/bob");
        assert_eq!(expand_portable(&mac_b, "/opt/repo"), "/opt/repo");
        assert_eq!(map_project_path(&mac_b, "~/w/p"), "/Users/bob/w/p");
        assert_eq!(map_in_project(&mac_b, "~/w/p/.devdy/runs/r.log"), "/Users/bob/w/p/.devdy/runs/r.log");
    }

    #[test]
    fn app_data_and_claude_store_paths() {
        let a = PathCtx::with_home("/Users/alice").with_app_data("/Users/alice/Library/Application Support/vn.papay.devdy");
        let b = PathCtx::with_home("/Users/bob").with_app_data("/Users/bob/Library/Application Support/vn.papay.devdy.dev");
        let p = to_portable(&a, "/Users/alice/Library/Application Support/vn.papay.devdy/skills/x/SKILL.md");
        assert_eq!(p, "@data/skills/x/SKILL.md");
        assert_eq!(expand_portable(&b, &p), "/Users/bob/Library/Application Support/vn.papay.devdy.dev/skills/x/SKILL.md");
        // Without an app data dir the marker is kept (never guessed).
        assert_eq!(expand_portable(&PathCtx::with_home("/h"), &p), p);

        let t = to_portable(&a, "/Users/alice/.claude/projects/-Users-alice-Workspace-devdy/s1.jsonl");
        assert_eq!(t, "~/.claude/projects/~-Workspace-devdy/s1.jsonl");
        assert_eq!(expand_portable(&b, &t), "/Users/bob/.claude/projects/-Users-bob-Workspace-devdy/s1.jsonl");
        // Only the folder right after `projects`, and only a whole home prefix.
        let other = to_portable(&a, "/Users/alice/.claude/projects/-Users-alicex-p/s.jsonl");
        assert_eq!(other, "~/.claude/projects/-Users-alicex-p/s.jsonl");
        assert_eq!(to_portable(&a, "/Users/alice/w/-Users-alice-x"), "~/w/-Users-alice-x");
        assert_eq!(expand_portable(&b, "~/w/~-x"), "/Users/bob/w/~-x");
    }

    #[test]
    fn project_map_rewrites_paths_both_ways() {
        let b = PathCtx::with_home("/Users/bob").with_project_map([
            ("~/Workspace/devdy".to_string(), "/Users/bob/code/devdy".to_string()),
            ("~/Workspace".to_string(), "/Volumes/w".to_string()),
        ]);
        assert_eq!(map_project_path(&b, "~/Workspace/devdy"), "/Users/bob/code/devdy");
        assert_eq!(map_project_path(&b, "~/Workspace/devdy/.devdy/runs/r.md"), "/Users/bob/code/devdy/.devdy/runs/r.md");
        assert_eq!(map_project_path(&b, "~/Workspace/devdy2"), "/Volumes/w/devdy2");
        assert_eq!(map_project_path(&b, "~/Other"), "/Users/bob/Other");
        assert_eq!(portable_path(&b, "/Users/bob/code/devdy/.devdy/runs/r.md"), "~/Workspace/devdy/.devdy/runs/r.md");
        assert_eq!(portable_path(&b, "/Users/bob/code/devdyx"), "~/code/devdyx");
        assert_eq!(portable_path(&b, "/Volumes/w"), "~/Workspace");
        let runs = spec("runs").unwrap();
        let wire = json!({"id": "r", "output_path": "~/Workspace/devdy/.devdy/runs/r.md"});
        let local = decode_row(&b, runs, wire.as_object().unwrap());
        assert_eq!(local["output_path"], "/Users/bob/code/devdy/.devdy/runs/r.md");
        assert_eq!(encode_row(&b, runs, local)["output_path"], "~/Workspace/devdy/.devdy/runs/r.md");
    }

    #[test]
    fn encode_drops_skip_cols_and_ports_paths() {
        let a = PathCtx::with_home("/Users/alice");
        let b = PathCtx::with_home("/Users/bob");
        let runs = spec("runs").unwrap();
        let row = json!({
            "id": "r1",
            "output_path": "/Users/alice/p/.devdy/runs/r1.md",
            "transcript_path": null,
            "transcript_synced_size": 42,
        });
        let wire = encode_row(&a, runs, row.as_object().unwrap().clone());
        assert_eq!(wire["output_path"], "~/p/.devdy/runs/r1.md");
        assert!(!wire.contains_key("transcript_synced_size"));
        assert_eq!(wire["transcript_path"], Value::Null);
        let local = decode_row(&b, runs, &wire);
        assert_eq!(local["output_path"], "/Users/bob/p/.devdy/runs/r1.md");
    }

    #[test]
    fn pk_helpers() {
        let s = spec("project_rules").unwrap();
        let pk = json!({"rule_id": "r", "project_id": "p"});
        assert_eq!(pk_values(s, &pk).unwrap(), vec![json!("p"), json!("r")]);
        let row = json!({"project_id": "p", "rule_id": "r", "target": "both"});
        assert_eq!(pk_of_row(s, row.as_object().unwrap()).unwrap(), pk);
        assert!(pk_values(spec("notes").unwrap(), &json!(true)).is_err());
    }
}
