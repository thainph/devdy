//! Row <-> JSON map using the registry: drops machine-local columns, turns path
//! columns into a portable form (`~/...` under $HOME, absolute otherwise) and
//! expands them again on the receiving device.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value};
use sqlx::SqliteConnection;

use super::schema::{q, TableSpec};

/// Where this device keeps things; only the home dir for now.
#[derive(Debug, Clone)]
pub struct PathCtx {
    pub home: PathBuf,
}

impl PathCtx {
    pub fn from_env() -> Self {
        PathCtx { home: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default() }
    }

    pub fn with_home(home: impl Into<PathBuf>) -> Self {
        PathCtx { home: home.into() }
    }
}

/// `~/rel` when `path` is under the home dir, `path` unchanged otherwise.
pub fn to_portable(ctx: &PathCtx, path: &str) -> String {
    if ctx.home.as_os_str().is_empty() {
        return path.to_string();
    }
    match Path::new(path).strip_prefix(&ctx.home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".to_string(),
        Ok(rel) => format!("~/{}", rel.to_string_lossy()),
        Err(_) => path.to_string(),
    }
}

/// Inverse of `to_portable` for this device.
pub fn expand_portable(ctx: &PathCtx, portable: &str) -> String {
    if portable == "~" {
        return ctx.home.to_string_lossy().into_owned();
    }
    match portable.strip_prefix("~/") {
        Some(rel) => ctx.home.join(rel).to_string_lossy().into_owned(),
        None => portable.to_string(),
    }
}

/// The single seam mapping a project (or repo) checkout path from another
/// device to this one. Wave 1: expand `~`, else keep the path as given.
/// Wave 2 adds the git remote URL / project name fallbacks here.
pub fn map_project_path(ctx: &PathCtx, portable: &str) -> String {
    expand_portable(ctx, portable)
}

fn local_path(ctx: &PathCtx, spec: &TableSpec, col: &str, portable: &str) -> String {
    match (spec.name, col) {
        ("projects", "path") | ("repos", "path") => map_project_path(ctx, portable),
        _ => expand_portable(ctx, portable),
    }
}

/// A local row (all columns) -> the wire form.
pub fn encode_row(ctx: &PathCtx, spec: &TableSpec, row: Map<String, Value>) -> Map<String, Value> {
    row.into_iter()
        .filter(|(c, _)| !spec.is_skipped(c))
        .map(|(c, v)| match v {
            Value::String(s) if spec.is_path(&c) => {
                let p = to_portable(ctx, &s);
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
            Value::String(s) if spec.is_path(c) => (c.clone(), Value::String(local_path(ctx, spec, c, s))),
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
