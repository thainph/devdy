//! Capture inbox: a loopback HTTP API the Chrome extension pushes exported
//! Slack threads and web pages to, plus the shared parsing/storage used by the
//! Tauri commands. Both kinds live in the `captures` table.
//!
//! - `ingest`: pure payload parsing (front matter, zip guards).
//! - `store`: SQLite + attachment files on disk.
//! - `server`: axum server on 127.0.0.1:47821..47830 (bearer token auth).

pub mod ingest;
pub mod server;
pub mod store;

use crate::db::Db;
use serde::Serialize;
use std::sync::{Arc, RwLock};
use tauri::{AppHandle, Emitter};

/// `settings` key holding the API bearer token.
pub const TOKEN_SETTING_KEY: &str = "inbox_api_token";

/// Event emitted to all windows on every create/update/delete.
pub const CAPTURES_CHANGED_EVENT: &str = "captures://changed";

#[derive(Clone, Serialize)]
struct ChangedPayload<'a> {
    id: &'a str,
    kind: &'a str,
    action: &'a str,
}

/// Emit `captures://changed` with `{ id, kind, action }`.
pub fn emit_changed(app: &AppHandle, id: &str, kind: ingest::CaptureKind, action: &str) {
    let _ = app.emit(
        CAPTURES_CHANGED_EVENT,
        ChangedPayload {
            id,
            kind: kind.as_str(),
            action,
        },
    );
}

/// Emitted when the user clicks a "capture received" notification; the
/// frontend opens that capture's detail window.
pub const CAPTURE_NOTIFICATION_CLICKED_EVENT: &str = "capture-notification-clicked";

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureNotificationClick {
    id: String,
    kind: &'static str,
}

/// Show the "Slack thread / Web page received" OS notification. Clicking it
/// raises the main window and emits `capture-notification-clicked`
/// `{ id, kind }`.
pub fn notify_received(app: &AppHandle, id: &str, kind: ingest::CaptureKind, title: &str) {
    let id = id.to_string();
    crate::commands::notifications::show_clickable(
        app.clone(),
        "Devdy".to_string(),
        kind.received_message(title),
        "capture notification",
        move |app| {
            let _ = app.emit(
                CAPTURE_NOTIFICATION_CLICKED_EVENT,
                CaptureNotificationClick {
                    id,
                    kind: kind.as_str(),
                },
            );
        },
    );
}

/// Snapshot returned to the frontend by `get_inbox_api_info`.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct InboxApiInfo {
    pub running: bool,
    pub port: Option<u16>,
    pub token: String,
    pub base_url: Option<String>,
}

#[derive(Default)]
struct Inner {
    token: String,
    port: Option<u16>,
}

/// Managed state shared by the HTTP server and the commands. The token lives
/// here (not only in the DB) so a regeneration takes effect immediately.
#[derive(Clone, Default)]
pub struct InboxApiState {
    inner: Arc<RwLock<Inner>>,
}

impl InboxApiState {
    pub fn token(&self) -> String {
        self.inner.read().map(|g| g.token.clone()).unwrap_or_default()
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
    let existing: Option<String> =
        sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
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
        assert_eq!(state.info().base_url.as_deref(), Some("http://127.0.0.1:47821"));
    }
}
