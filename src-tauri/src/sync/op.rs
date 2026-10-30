//! The unit of replication (design §9.1). Rows are full snapshots, so a later op
//! on the same `(tbl, pk)` supersedes every earlier one.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::clock::Hlc;

/// Current `Op.v`.
pub const OP_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub v: u32,
    /// Highest applied migration number of the sender.
    pub schema: u32,
    pub hlc: Hlc,
    /// Table name, or "file" / "secret".
    pub tbl: String,
    /// Primary key: a string, or an object for composite keys.
    pub pk: Value,
    pub kind: OpKind,
    /// Full row, portable-encoded; `None` for `Delete`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OpKind {
    Upsert,
    Delete,
}

impl Op {
    /// Canonical JSON of the primary key, as stored in `sync_outbox.pk` /
    /// `sync_rows.pk`.
    pub fn pk_key(&self) -> String {
        canonical_json(&self.pk)
    }
}

/// Compact JSON with object keys sorted recursively — stable regardless of how
/// the value was built (serde_json may preserve insertion order).
pub fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let body: Vec<String> = keys
                .into_iter()
                .map(|k| format!("{}:{}", Value::String(k.clone()), canonical_json(&m[k])))
                .collect();
            format!("{{{}}}", body.join(","))
        }
        Value::Array(a) => {
            let body: Vec<String> = a.iter().map(canonical_json).collect();
            format!("[{}]", body.join(","))
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn canonical_json_sorts_keys() {
        assert_eq!(canonical_json(&json!({"b": "2", "a": "1"})), r#"{"a":"1","b":"2"}"#);
        assert_eq!(canonical_json(&json!("x\"y")), r#""x\"y""#);
    }

    #[test]
    fn op_round_trips_through_json() {
        let op = Op {
            v: OP_VERSION,
            schema: 55,
            hlc: "000000000001000a-dev".parse().unwrap(),
            tbl: "notes".into(),
            pk: json!("n1"),
            kind: OpKind::Delete,
            row: None,
        };
        let s = serde_json::to_string(&op).unwrap();
        assert!(s.contains(r#""hlc":"000000000001000a-dev""#));
        assert_eq!(serde_json::from_str::<Op>(&s).unwrap(), op);
    }
}
