//! STUB of the device-sync bundle API so the transport compiles on its own.
//! The real implementation lands from the bundle branch; drop this file then.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::db::Db;

/// What the pulling device asked for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncCategories {
    pub todos: bool,
    pub notes: bool,
    pub secret_notes: bool,
    pub slack_threads: bool,
    pub skills_rules: bool,
    pub projects: bool,
    pub settings: bool,
    pub mcp_servers: bool,
    pub vps_servers: bool,
    pub accounts: bool,
    /// Apply-side only: mirror the source (the host never reads it).
    pub mirror: bool,
}

/// Per-category outcome of an apply.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncCounts {
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub deleted: u32,
    pub skipped: u32,
}

/// Result of applying a bundle on the pulling device.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub counts: BTreeMap<String, SyncCounts>,
    pub warnings: Vec<String>,
}

pub async fn build_sync_bundle(
    _app: &AppHandle,
    _db: &Db,
    _cats: &SyncCategories,
    _key: &[u8; 32],
) -> Result<Vec<u8>, String> {
    Ok(Vec::new())
}

pub async fn apply_sync_bundle(
    _app: &AppHandle,
    _db: &Db,
    _bytes: &[u8],
    _key: &[u8; 32],
    _cats: &SyncCategories,
) -> Result<SyncReport, String> {
    Err("not implemented".to_string())
}
