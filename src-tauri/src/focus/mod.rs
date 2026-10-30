//! Focus (docs/focus-board-spec.md): the global todo board.
//!
//! - `score`: work-hours arithmetic, buckets, score + reasons, start_by,
//!   calibration (pure, BR-03..06, BR-10)
//! - `source`: todo source link normalisation (pure, BR-02)
//! - `validate`: todo field checks (pure, BR-01)
//! - `settings`: `focus.*` rows of the settings table (§5.2)
//! - `source_content`, `github`, `ai_util`: kept from Project Pulse for the AI
//!   commands of phase P3

pub mod ai_util;
pub mod github;
pub mod score;
pub mod settings;
pub mod source;
pub mod source_content;
pub mod validate;

use crate::item_links::{self, ItemKind};
use tauri::{AppHandle, Emitter};

/// Broadcast after every todo write (with `item:changed`).
pub const CHANGED_EVENT: &str = "focus:changed";

/// Tell the Focus board and the item lists that todos changed.
pub fn emit_todos_changed(app: &AppHandle) {
    let _ = app.emit(CHANGED_EVENT, ());
    item_links::emit_changed(app, Some(ItemKind::Todo));
}
