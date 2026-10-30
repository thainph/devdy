//! Two-way sync over the relay (design: .workspace/docs/03-design/two-way-sync.md).
//!
//! Core: table registry, trigger-based change capture into `sync_outbox`, HLC,
//! row codec, and `SqliteSyncStore` — the only thing the sync engine talks to.
//! Client: crypto, relay transport, push/pull engine and pairing.
//! Non-DB ops: `files` (library, run logs, transcripts, keys) and `secrets`
//! (Keychain items).

pub mod apply;
pub mod bootstrap;
pub mod capture;
pub mod clock;
pub mod codec;
pub mod events;
pub mod op;
pub mod schema;
pub mod sessions;
pub mod store;

pub mod files;
pub mod secrets;

pub mod crypto;
pub mod engine;
pub mod leader;
pub mod pair;
pub mod transport;
#[cfg(test)]
mod test_relay;

use crate::db::Db;

/// Highest migration this build knows; ops from a newer schema are deferred.
pub const SCHEMA_VERSION: u32 = 65;

/// This machine's sync device id, created on first use.
pub async fn ensure_device(db: &Db) -> Result<String, String> {
    let existing: Option<String> = sqlx::query_scalar("SELECT id FROM sync_device LIMIT 1")
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let id = uuid::Uuid::new_v4().simple().to_string();
    sqlx::query("INSERT INTO sync_device (id, name, created_at) VALUES (?, '', ?)")
        .bind(&id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(id)
}

/// Startup hook (after migrations): device id + capture triggers.
pub async fn install(db: &Db) -> Result<String, String> {
    let device = ensure_device(db).await?;
    capture::install_triggers(db).await?;
    Ok(device)
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema_version_matches_latest_migration() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
        let latest = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().to_str()?.get(..4)?.parse::<u32>().ok())
            .max()
            .unwrap();
        assert_eq!(latest, super::SCHEMA_VERSION, "bump sync::SCHEMA_VERSION with new migrations");
    }
}
