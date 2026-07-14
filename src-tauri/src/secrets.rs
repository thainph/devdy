use anyhow::{anyhow, Result};
use keyring::Entry;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

const SERVICE_NAME: &str = "vn.papay.devdy";

/// Single consolidated Keychain item that holds ALL PATs (GitHub + GitLab).
///
/// WHY ONE ITEM: on macOS every Keychain item has its own ACL bound to the app's
/// code signature. An ad-hoc / unsigned build gets a NEW signature on each
/// reinstall, so the OS re-prompts ("allow access…") for every item whose ACL no
/// longer matches. With one item per account the user was asked once *per
/// account*. Collapsing everything into ONE item means at most ONE prompt after a
/// reinstall, regardless of how many accounts are stored.
const STORE_KEY: &str = "devdy_secret_store_v1";

/// In-memory view of the consolidated store. Keyed by account id → PAT.
#[derive(Default, Serialize, Deserialize, Clone)]
struct SecretStore {
    #[serde(default)]
    github: HashMap<String, String>,
    #[serde(default)]
    gitlab: HashMap<String, String>,
}

/// Process-lifetime cache of the decrypted store.
///
/// The FIRST access reads the Keychain (the single possible prompt); every
/// get/set/delete afterwards works purely in memory, so no further prompts appear
/// while the app is running — even if the user clicked "Allow" (not "Always
/// Allow"). Cleared only when the process exits.
///
/// SECURITY: this retains PATs in RAM for the app lifetime. That is an
/// intentional trade-off for the "prompt once per reinstall" UX; the app already
/// forwards these tokens to the broker/shim, so the retention surface is the same
/// class of secret it already handles.
static CACHE: Mutex<Option<SecretStore>> = Mutex::new(None);

enum Provider {
    Github,
    Gitlab,
}

fn store_entry() -> Result<Entry> {
    Ok(Entry::new(SERVICE_NAME, STORE_KEY)?)
}

/// Legacy per-account key format used before consolidation. Kept only so old
/// installs migrate transparently on first read.
fn legacy_key(provider: &Provider, account_id: &str) -> String {
    match provider {
        Provider::Github => format!("account_{}", account_id),
        Provider::Gitlab => format!("gitlab_account_{}", account_id),
    }
}

/// Load the store into the cache if not already loaded. This is the ONLY place
/// that may trigger a Keychain password prompt.
fn ensure_loaded(guard: &mut Option<SecretStore>) {
    if guard.is_some() {
        return;
    }
    let store = match store_entry().ok().and_then(|e| e.get_password().ok()) {
        Some(json) => serde_json::from_str(&json).unwrap_or_default(),
        None => SecretStore::default(),
    };
    *guard = Some(store);
}

/// Persist the whole store back to the single Keychain item.
fn persist(store: &SecretStore) -> Result<()> {
    let json = serde_json::to_string(store)?;
    store_entry()?.set_password(&json)?;
    Ok(())
}

/// Read a legacy per-account item and remove it (one-time migration). Returns the
/// PAT if the legacy item existed. May prompt once per legacy item — unavoidable
/// for pre-consolidation installs, but only happens until every account is moved
/// into the consolidated store.
fn take_legacy(provider: &Provider, account_id: &str) -> Option<String> {
    let entry = Entry::new(SERVICE_NAME, &legacy_key(provider, account_id)).ok()?;
    match entry.get_password() {
        Ok(pat) => {
            let _ = entry.delete_credential();
            Some(pat)
        }
        Err(_) => None,
    }
}

/// Delete a legacy per-account item if present (no prompt on delete).
fn drop_legacy(provider: &Provider, account_id: &str) {
    if let Ok(entry) = Entry::new(SERVICE_NAME, &legacy_key(provider, account_id)) {
        let _ = entry.delete_credential();
    }
}

/// Whether a legacy per-account item exists — checked via metadata only, which
/// does NOT trigger the macOS confidential-access prompt.
fn legacy_exists(provider: &Provider, account_id: &str) -> bool {
    Entry::new(SERVICE_NAME, &legacy_key(provider, account_id))
        .map(|e| e.get_attributes().is_ok())
        .unwrap_or(false)
}

/// Whether the consolidated store item exists — metadata-only, prompt-free.
fn store_exists() -> bool {
    store_entry()
        .map(|e| e.get_attributes().is_ok())
        .unwrap_or(false)
}

fn map_of<'a>(store: &'a SecretStore, provider: &Provider) -> &'a HashMap<String, String> {
    match provider {
        Provider::Github => &store.github,
        Provider::Gitlab => &store.gitlab,
    }
}

fn map_of_mut<'a>(store: &'a mut SecretStore, provider: &Provider) -> &'a mut HashMap<String, String> {
    match provider {
        Provider::Github => &mut store.github,
        Provider::Gitlab => &mut store.gitlab,
    }
}

fn set_pat(provider: Provider, account_id: &str, pat: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    map_of_mut(store, &provider).insert(account_id.to_string(), pat.to_string());
    persist(store)?;
    // Remove any stale legacy item so it can never shadow or re-prompt.
    drop_legacy(&provider, account_id);
    Ok(())
}

fn get_pat(provider: Provider, account_id: &str) -> Result<String> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    if let Some(pat) = map_of(store, &provider).get(account_id) {
        return Ok(pat.clone());
    }
    // Transparent migration from a pre-consolidation install.
    if let Some(pat) = take_legacy(&provider, account_id) {
        map_of_mut(store, &provider).insert(account_id.to_string(), pat.clone());
        let _ = persist(store);
        return Ok(pat);
    }
    Err(anyhow!("No PAT stored for account"))
}

fn delete_pat(provider: Provider, account_id: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    map_of_mut(store, &provider).remove(account_id);
    let _ = persist(store);
    drop_legacy(&provider, account_id);
    Ok(())
}

/// Check whether a PAT exists WITHOUT reading its secret value (no prompt).
///
/// Resolution order, all prompt-free:
/// 1. If the store is already cached, answer precisely from memory.
/// 2. Otherwise check the legacy per-account item's metadata.
/// 3. Otherwise fall back to "the consolidated store exists" — accounts are
///    always created together with their PAT, so this is accurate in practice
///    and never triggers a prompt before the first real read.
fn has_pat(provider: Provider, account_id: &str) -> bool {
    if let Ok(guard) = CACHE.lock() {
        if let Some(store) = guard.as_ref() {
            return map_of(store, &provider).contains_key(account_id)
                || legacy_exists(&provider, account_id);
        }
    }
    legacy_exists(&provider, account_id) || store_exists()
}

// ---- Public API (unchanged signatures; callers untouched) ------------------

pub fn set_account_pat(account_id: &str, pat: &str) -> Result<()> {
    set_pat(Provider::Github, account_id, pat)
}

pub fn get_account_pat(account_id: &str) -> Result<String> {
    get_pat(Provider::Github, account_id)
}

pub fn delete_account_pat(account_id: &str) -> Result<()> {
    delete_pat(Provider::Github, account_id)
}

pub fn has_account_pat(account_id: &str) -> bool {
    has_pat(Provider::Github, account_id)
}

pub fn set_gitlab_account_pat(account_id: &str, pat: &str) -> Result<()> {
    set_pat(Provider::Gitlab, account_id, pat)
}

pub fn get_gitlab_account_pat(account_id: &str) -> Result<String> {
    get_pat(Provider::Gitlab, account_id)
}

pub fn delete_gitlab_account_pat(account_id: &str) -> Result<()> {
    delete_pat(Provider::Gitlab, account_id)
}

pub fn has_gitlab_account_pat(account_id: &str) -> bool {
    has_pat(Provider::Gitlab, account_id)
}
