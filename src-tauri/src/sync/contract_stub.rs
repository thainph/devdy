//! TEMPORARY contract stub — replaced by sync-core at merge.
//!
//! sync-client codes against the wave-1 contract (design doc §9.1 / §9.3)
//! before sync-core lands. These definitions copy that contract verbatim; at
//! merge, delete this file and the `contract_stub` lines in `sync/mod.rs` so
//! `crate::sync::{op, store}` resolve to core's real modules. Client code only
//! imports through `crate::sync::op::*` / `crate::sync::store::*`.

pub mod op {
    use serde::{Deserialize, Serialize};

    /// Hybrid logical clock stamp, serialized
    /// `"<12 hex millis><4 hex counter>-<device_id>"`. Opaque to the client.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
    #[serde(transparent)]
    pub struct Hlc(pub String);

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub enum OpKind {
        Upsert,
        Delete,
    }

    /// One row change (plaintext, before sealing). §9.1.
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct Op {
        pub v: u32,
        pub schema: u32,
        pub hlc: Hlc,
        pub tbl: String,
        pub pk: serde_json::Value,
        pub kind: OpKind,
        pub row: Option<serde_json::Map<String, serde_json::Value>>,
    }
}

pub mod store {
    use super::op::Op;
    use crate::db::Db;
    use async_trait::async_trait;

    /// Core ↔ client boundary. §9.3.
    #[allow(clippy::double_must_use)]
    #[async_trait]
    pub trait SyncStore: Send + Sync {
        async fn pending(&self, limit: usize) -> Result<Vec<(i64, Op)>, String>;
        async fn mark_pushed(&self, up_to_outbox_seq: i64) -> Result<(), String>;
        async fn apply_remote(&self, ops: Vec<Op>) -> Result<ApplyReport, String>;
        async fn cursor(&self) -> Result<u64, String>;
        async fn set_cursor(&self, seq: u64) -> Result<(), String>;
    }

    #[derive(Debug, Clone, Default)]
    pub struct ApplyReport {
        pub applied: usize,
        pub skipped_older: usize,
        pub conflicts: usize,
        pub deferred: usize,
    }

    /// Placeholder for core's SQLite-backed store so the commands compile
    /// before the merge. Every call fails, which the engine surfaces as an
    /// `error` status.
    pub struct SqliteSyncStore;

    impl SqliteSyncStore {
        pub fn new(_db: Db, _device_id: String) -> Self {
            SqliteSyncStore
        }
    }

    const NOT_MERGED: &str = "sync core is not available in this build";

    #[async_trait]
    impl SyncStore for SqliteSyncStore {
        async fn pending(&self, _limit: usize) -> Result<Vec<(i64, Op)>, String> {
            Err(NOT_MERGED.into())
        }
        async fn mark_pushed(&self, _up_to_outbox_seq: i64) -> Result<(), String> {
            Err(NOT_MERGED.into())
        }
        async fn apply_remote(&self, _ops: Vec<Op>) -> Result<ApplyReport, String> {
            Err(NOT_MERGED.into())
        }
        async fn cursor(&self) -> Result<u64, String> {
            Err(NOT_MERGED.into())
        }
        async fn set_cursor(&self, _seq: u64) -> Result<(), String> {
            Err(NOT_MERGED.into())
        }
    }
}
