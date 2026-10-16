//! Inbox API: a loopback HTTP API the Chrome extension sends Slack thread
//! links (handed to `crate::slack_threads`) and translation requests to.
//!
//! - `server`: axum server on 127.0.0.1:47821..47830 (bearer token auth).

pub mod server;

use crate::db::Db;
use serde::Serialize;
use std::sync::{Arc, RwLock};

/// `settings` key holding the API bearer token.
pub const TOKEN_SETTING_KEY: &str = "inbox_api_token";

/// `settings` key for the server on/off switch (`"true"` / `"false"`; missing
/// means enabled).
pub const ENABLED_SETTING_KEY: &str = "inbox_api_enabled";

/// Dir that held web page captures (feature removed); deleted at startup.
const LEGACY_WEB_PAGES_DIR: &str = "web-pages";

/// Remove `<app_data>/web-pages/` left over from the removed web page capture
/// feature (its DB tables are dropped by migration 0042). Only that exact dir
/// is touched — `slack-threads/` is unrelated.
pub fn remove_legacy_web_pages(app_data: &std::path::Path) {
    let dir = app_data.join(LEGACY_WEB_PAGES_DIR);
    if dir.is_dir() {
        match std::fs::remove_dir_all(&dir) {
            Ok(()) => tracing::info!(event = "legacy_web_pages_removed"),
            Err(e) => tracing::warn!(event = "legacy_web_pages_remove_failed", error = %e),
        }
    }
}

/// Snapshot returned to the frontend by `get_inbox_api_info`.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InboxApiInfo {
    pub enabled: bool,
    pub running: bool,
    pub port: Option<u16>,
    pub token: String,
    pub base_url: Option<String>,
}

#[derive(Default)]
struct Inner {
    token: String,
    port: Option<u16>,
    enabled: bool,
}

/// Managed state shared by the HTTP server and the commands. The token lives
/// here (not only in the DB) so a regeneration takes effect immediately.
#[derive(Clone, Default)]
pub struct InboxApiState {
    inner: Arc<RwLock<Inner>>,
    /// Handle of the running server; the async lock serializes start / stop.
    server: Arc<tokio::sync::Mutex<Option<server::ServerHandle>>>,
}

impl InboxApiState {
    pub fn token(&self) -> String {
        self.inner
            .read()
            .map(|g| g.token.clone())
            .unwrap_or_default()
    }

    pub fn set_token(&self, token: String) {
        if let Ok(mut g) = self.inner.write() {
            g.token = token;
        }
    }

    pub fn port(&self) -> Option<u16> {
        self.inner.read().ok().and_then(|g| g.port)
    }

    pub fn set_port(&self, port: Option<u16>) {
        if let Ok(mut g) = self.inner.write() {
            g.port = port;
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.read().map(|g| g.enabled).unwrap_or(false)
    }

    pub fn set_enabled(&self, enabled: bool) {
        if let Ok(mut g) = self.inner.write() {
            g.enabled = enabled;
        }
    }

    /// Constant-time comparison against the current token (empty never matches).
    pub fn check_token(&self, candidate: &str) -> bool {
        let token = self.token();
        if token.is_empty() || token.len() != candidate.len() {
            return false;
        }
        token
            .bytes()
            .zip(candidate.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }

    pub fn info(&self) -> InboxApiInfo {
        let port = self.port();
        InboxApiInfo {
            enabled: self.enabled(),
            running: port.is_some(),
            port,
            token: self.token(),
            base_url: port.map(|p| format!("http://127.0.0.1:{}", p)),
        }
    }
}

/// Random 32-byte token, hex encoded.
pub fn generate_token() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Load the persisted token, generating and storing one on first start.
pub async fn load_or_create_token(db: &Db) -> Result<String, sqlx::Error> {
    let existing: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(TOKEN_SETTING_KEY)
        .fetch_optional(db)
        .await?;
    if let Some(token) = existing.filter(|t| !t.trim().is_empty()) {
        return Ok(token);
    }
    let token = generate_token();
    save_token(db, &token).await?;
    Ok(token)
}

pub async fn save_token(db: &Db, token: &str) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(TOKEN_SETTING_KEY)
        .bind(token)
        .execute(db)
        .await?;
    Ok(())
}

/// Whether the server should run (missing / unreadable setting → enabled).
pub async fn load_enabled(db: &Db) -> bool {
    let value: Option<String> = sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
        .bind(ENABLED_SETTING_KEY)
        .fetch_optional(db)
        .await
        .ok()
        .flatten();
    value.is_none_or(|v| v.trim() != "false")
}

pub async fn save_enabled(db: &Db, enabled: bool) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
        .bind(ENABLED_SETTING_KEY)
        .bind(if enabled { "true" } else { "false" })
        .execute(db)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_check_and_regeneration() {
        let state = InboxApiState::default();
        assert!(!state.check_token(""));
        let t1 = generate_token();
        assert_eq!(t1.len(), 64);
        state.set_token(t1.clone());
        assert!(state.check_token(&t1));
        let t2 = generate_token();
        state.set_token(t2.clone());
        assert!(!state.check_token(&t1));
        assert!(state.check_token(&t2));
        assert!(!state.info().running);
        state.set_port(Some(47821));
        assert_eq!(
            state.info().base_url.as_deref(),
            Some("http://127.0.0.1:47821")
        );
    }

    #[test]
    fn legacy_web_pages_dir_is_removed_and_slack_kept() {
        let root = std::env::temp_dir().join(format!(
            "devdy-webpages-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(root.join("web-pages/w1/files")).unwrap();
        std::fs::write(root.join("web-pages/w1/files/a.png"), b"x").unwrap();
        std::fs::create_dir_all(root.join("slack-threads/s1/files")).unwrap();
        std::fs::write(root.join("slack-threads/s1/files/b.png"), b"y").unwrap();
        remove_legacy_web_pages(&root);
        assert!(!root.join("web-pages").exists());
        assert!(root.join("slack-threads/s1/files/b.png").is_file());
        // Missing dir is a no-op.
        remove_legacy_web_pages(&root);
        let _ = std::fs::remove_dir_all(&root);
    }
}
