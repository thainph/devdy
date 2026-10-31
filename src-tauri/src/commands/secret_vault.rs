//! Tauri commands of the secret notes vault (`crate::secret_vault`).
//!
//! Secret notes window only: every command refuses any other webview — the Remote
//! Control bridge (the remote handler also rewrites `secret_*` calls before
//! they reach it), remote-desktop windows, and the windows that render agent
//! output. The one exception is `secret_note_from_note`, which the Notes window
//! may also call ("Make secret"): it only writes into the vault, never reads it.
//! Argon2id runs on the blocking pool; password attempts are serialized
//! so parallel calls cannot slip past the backoff. Changes emit `SECRET_VAULT_EVENT` so
//! the other windows refresh; it carries no note data.

use crate::db::Db;
use crate::secret_vault::{self as vault, SecretNoteMeta, VaultData, DEFAULT_KDF};
use crate::secrets::with_vault;
use serde::Serialize;
use sqlx::Row;
use tauri::{AppHandle, Emitter, State, Webview};

pub const SECRET_VAULT_EVENT: &str = "secret-vault://changed";

/// The local Secret notes window (`SECRET_NOTES_WINDOW_LABEL` in src/lib/secretNotesWindow.ts).
const SECRET_NOTES_WINDOW: &str = "secret-notes";
/// The local Notes window (`itemListWindowLabel('note')` in src/lib/itemListWindow.ts).
const NOTES_WINDOW: &str = "item-list-note";

fn allowed(label: &str, extra: &[&str]) -> bool {
    label == SECRET_NOTES_WINDOW || extra.contains(&label)
}

fn local_only(webview: &Webview) -> Result<(), String> {
    local_only_or(webview, &[])
}

/// `local_only`, also letting the `extra` local windows through.
fn local_only_or(webview: &Webview, extra: &[&str]) -> Result<(), String> {
    if !allowed(webview.label(), extra) {
        return Err("remote_denied".into());
    }
    Ok(())
}

/// One password check at a time (unlock, merge, change password).
static PASSWORD_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn changed(app: &AppHandle) {
    let _ = app.emit(SECRET_VAULT_EVENT, ());
}

fn current_id(data: &VaultData) -> Option<String> {
    data.header.as_ref().map(|h| h.vault_id.clone())
}

/// Read the vault, and the DEK of its current header (else `vault_locked`).
fn unlocked<R>(write: bool, f: impl FnOnce(&mut VaultData, &[u8; 32]) -> Result<R, String>) -> Result<R, String> {
    with_vault(write, |data| {
        let dek = vault::session_dek(current_id(data).as_deref())?;
        f(data, &dek)
    })
}

async fn blocking<R: Send + 'static>(f: impl FnOnce() -> Result<R, String> + Send + 'static) -> Result<R, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

#[derive(Debug, Serialize)]
pub struct VaultStatus {
    pub exists: bool,
    pub unlocked: bool,
    /// Notes of the current vault.
    pub note_count: usize,
    /// Notes of an older vault (sync or import), mergeable with its password.
    pub legacy_count: usize,
    /// Notes whose vault has not reached this device yet.
    pub pending_count: usize,
    pub has_recovery: bool,
    pub auto_lock_secs: u64,
}

#[tauri::command]
pub async fn secret_vault_status(webview: Webview) -> Result<VaultStatus, String> {
    local_only(&webview)?;
    with_vault(false, |data| {
        let id = current_id(data);
        Ok(VaultStatus {
            exists: id.is_some(),
            unlocked: vault::is_unlocked(id.as_deref()),
            note_count: data.notes.values().filter(|n| Some(&n.vault_id) == id.as_ref()).count(),
            legacy_count: data.legacy_count(),
            pending_count: data.pending_count(),
            has_recovery: data.header.as_ref().is_some_and(|h| h.recovery_wrapped.is_some()),
            auto_lock_secs: vault::AUTO_LOCK.as_secs(),
        })
    })
}

/// Create the vault (unlocked) and return the recovery key, shown once.
#[tauri::command]
pub async fn secret_vault_setup(app: AppHandle, webview: Webview, password: String) -> Result<String, String> {
    local_only(&webview)?;
    if with_vault(false, |d| Ok(d.header.is_some()))? {
        return Err(vault::E_EXISTS.into());
    }
    let (header, dek, recovery) = blocking(move || vault::new_header(&password, DEFAULT_KDF)).await?;
    let id = header.vault_id.clone();
    with_vault(true, move |d| {
        if d.header.is_some() {
            return Err(vault::E_EXISTS.to_string());
        }
        d.header = Some(header);
        Ok(())
    })?;
    vault::set_unlocked(&id, dek, false);
    changed(&app);
    Ok(recovery)
}

async fn unlock_with(
    app: &AppHandle,
    secret: String,
    via_recovery: bool,
    open: fn(&vault::VaultHeader, &str) -> Result<zeroize::Zeroizing<[u8; 32]>, String>,
) -> Result<(), String> {
    let _gate = PASSWORD_GATE.lock().await;
    vault::check_throttle()?;
    let header = with_vault(false, |d| d.header.clone().ok_or_else(|| vault::E_MISSING.to_string()))?;
    let id = header.vault_id.clone();
    let result = blocking(move || open(&header, &secret)).await;
    vault::record_attempt(result.is_ok());
    vault::set_unlocked(&id, result?, via_recovery);
    changed(app);
    Ok(())
}

#[tauri::command]
pub async fn secret_vault_unlock(app: AppHandle, webview: Webview, password: String) -> Result<(), String> {
    local_only(&webview)?;
    unlock_with(&app, password, false, vault::unwrap_password).await
}

/// Unlock with the recovery key (then set a new password).
#[tauri::command]
pub async fn secret_vault_unlock_recovery(app: AppHandle, webview: Webview, recovery_key: String) -> Result<(), String> {
    local_only(&webview)?;
    unlock_with(&app, recovery_key, true, vault::unwrap_recovery).await
}

#[tauri::command]
pub async fn secret_vault_lock(app: AppHandle, webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    vault::lock();
    changed(&app);
    Ok(())
}

/// The user is active in the unlocked vault (typing in the editor): push the
/// idle lock back. `vault_locked` when it already locked.
#[tauri::command]
pub async fn secret_vault_touch(webview: Webview) -> Result<(), String> {
    local_only(&webview)?;
    unlocked(false, |_, _| Ok(()))
}

/// New vault password (vault unlocked): rotates to a new vault (id, DEK,
/// recovery key) so the old password, header and recovery key open nothing
/// current. Needs the current password, unless the vault was opened with the
/// recovery key. Resolves to the new recovery key, to show once.
#[tauri::command]
pub async fn secret_vault_change_password(
    app: AppHandle,
    webview: Webview,
    current_password: Option<String>,
    password: String,
) -> Result<String, String> {
    local_only(&webview)?;
    let _gate = PASSWORD_GATE.lock().await;
    let header = unlocked(false, |d, _| d.header.clone().ok_or_else(|| vault::E_MISSING.to_string()))?;
    if !vault::unlocked_via_recovery() {
        vault::check_throttle()?;
        let current = current_password.unwrap_or_default();
        let h = header.clone();
        let check = blocking(move || vault::unwrap_password(&h, &current).map(|_| ())).await;
        vault::record_attempt(check.is_ok());
        check?;
    }
    let id = header.vault_id;
    let (new_id, dek, recovery) = blocking(move || {
        with_vault(true, |d| {
            // Replaced meanwhile (sync): start over from the newer vault.
            if current_id(d).as_deref() != Some(id.as_str()) {
                return Err(vault::E_LOCKED.to_string());
            }
            let dek = vault::session_dek(Some(&id))?;
            let (new_dek, recovery) = vault::rotate(d, &dek, &password, DEFAULT_KDF)?;
            Ok((current_id(d).unwrap_or_default(), new_dek, recovery))
        })
    })
    .await?;
    vault::set_unlocked(&new_id, dek, false);
    changed(&app);
    Ok(recovery)
}

/// Merge the notes of older vaults `password` opens into the current one.
#[tauri::command]
pub async fn secret_vault_merge_old(app: AppHandle, webview: Webview, password: String) -> Result<usize, String> {
    local_only(&webview)?;
    let _gate = PASSWORD_GATE.lock().await;
    vault::check_throttle()?;
    let (mut data, dek) = unlocked(false, |d, dek| Ok((d.clone(), zeroize::Zeroizing::new(*dek))))?;
    let before = current_id(&data);
    let (data, merged) = blocking(move || {
        let r = vault::merge_old(&mut data, &dek, &password);
        vault::record_attempt(r.is_ok());
        Ok((data, r?))
    })
    .await?;
    with_vault(true, move |d| {
        if current_id(d) != before {
            return Err(vault::E_LOCKED.to_string());
        }
        // Keep notes changed meanwhile; take the re-sealed ones.
        for (id, note) in data.notes {
            if d.notes.get(&id).is_some_and(|n| n.updated_at == note.updated_at) {
                d.notes.insert(id, note);
            }
        }
        d.drop_unused_old_headers();
        Ok(())
    })?;
    changed(&app);
    Ok(merged)
}

#[tauri::command]
pub async fn secret_notes_list(webview: Webview) -> Result<Vec<SecretNoteMeta>, String> {
    local_only(&webview)?;
    unlocked(false, |d, dek| vault::list(d, dek))
}

#[tauri::command]
pub async fn secret_note_reveal(webview: Webview, id: String) -> Result<String, String> {
    local_only(&webview)?;
    unlocked(false, |d, dek| vault::reveal(d, dek, &id))
}

/// Copy a note's content to the clipboard; cleared after `CLIPBOARD_CLEAR`
/// unless something else was copied meanwhile. The content never reaches the
/// webview. Returns the delay in seconds.
#[tauri::command]
pub async fn secret_note_copy(webview: Webview, id: String) -> Result<u64, String> {
    local_only(&webview)?;
    let content = zeroize::Zeroizing::new(unlocked(false, |d, dek| vault::reveal(d, dek, &id))?);
    let count = vault::clipboard_put(&content)?;
    vault::clipboard_remember(count);
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(vault::CLIPBOARD_CLEAR).await;
        vault::clipboard_clear_if(count);
    });
    Ok(vault::CLIPBOARD_CLEAR.as_secs())
}

#[tauri::command]
pub async fn secret_note_create(
    app: AppHandle,
    webview: Webview,
    title: String,
    content: String,
    project_id: Option<String>,
) -> Result<SecretNoteMeta, String> {
    local_only(&webview)?;
    let m = unlocked(true, |d, dek| vault::create(d, dek, &title, &content, project_id))?;
    changed(&app);
    Ok(m)
}

#[tauri::command]
pub async fn secret_note_update(
    app: AppHandle,
    webview: Webview,
    id: String,
    title: String,
    content: String,
) -> Result<SecretNoteMeta, String> {
    local_only(&webview)?;
    let m = unlocked(true, |d, dek| vault::update(d, dek, &id, &title, &content))?;
    changed(&app);
    Ok(m)
}

#[tauri::command]
pub async fn secret_note_set_project(
    app: AppHandle,
    webview: Webview,
    id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    local_only(&webview)?;
    unlocked(true, |d, _| vault::set_project(d, &id, project_id))?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn secret_note_delete(app: AppHandle, webview: Webview, id: String) -> Result<(), String> {
    local_only(&webview)?;
    unlocked(true, |d, _| vault::delete(d, &id))?;
    changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn secret_notes_reorder(app: AppHandle, webview: Webview, ids: Vec<String>) -> Result<(), String> {
    local_only(&webview)?;
    unlocked(true, |d, _| vault::reorder(d, &ids))?;
    changed(&app);
    Ok(())
}

/// Turn a regular note into a secret one: seal it into the vault, then delete
/// the row (and its session links). Text an agent already read stays in
/// those sessions' logs; the UI says so.
#[tauri::command]
pub async fn secret_note_from_note(
    app: AppHandle,
    webview: Webview,
    db: State<'_, Db>,
    note_id: String,
) -> Result<SecretNoteMeta, String> {
    local_only_or(&webview, &[NOTES_WINDOW])?;
    let row = sqlx::query("SELECT title, content, project_id FROM notes WHERE id = ?")
        .bind(&note_id)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| vault::E_NOT_FOUND.to_string())?;
    let (title, content, project_id): (String, String, Option<String>) =
        (row.get("title"), row.get("content"), row.get("project_id"));
    let m = unlocked(true, |d, dek| vault::create(d, dek, &title, &content, project_id))?;
    crate::commands::notes::delete_note(db, note_id).await?;
    changed(&app);
    let _ = app.emit("item:changed", serde_json::json!({ "kind": "note" }));
    Ok(m)
}

/// Lock the vault once idle for `AUTO_LOCK`, while the screen is locked, and
/// after the machine slept (a tick that comes much later than due, by wall
/// clock: the monotonic timer stops while asleep).
pub fn spawn_auto_lock(app: AppHandle) {
    const TICK: std::time::Duration = std::time::Duration::from_secs(15);
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(TICK);
        let mut last = std::time::SystemTime::now();
        loop {
            tick.tick().await;
            let now = std::time::SystemTime::now();
            let slept = now.duration_since(last).map_or(true, |gap| gap > TICK * 4);
            last = now;
            let locked = if slept || vault::screen_locked() { vault::lock_if_unlocked() } else { vault::lock_if_idle() };
            if locked {
                changed(&app);
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_secret_notes_window_is_allowed() {
        assert!(allowed(SECRET_NOTES_WINDOW, &[]));
        for label in [NOTES_WINDOW, "main", "remote-secret-notes", "remote-bridge", "session-1", "item-list-todo"] {
            assert!(!allowed(label, &[]), "{label} must be refused");
        }
    }

    #[test]
    fn make_secret_is_also_allowed_from_the_notes_window() {
        assert!(allowed(NOTES_WINDOW, &[NOTES_WINDOW]));
        assert!(allowed(SECRET_NOTES_WINDOW, &[NOTES_WINDOW]));
        assert!(!allowed("remote-item-list-note", &[NOTES_WINDOW]));
    }
}
