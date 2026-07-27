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
    /// MCP server secrets, keyed by server_id → env/header VALUEs. Kept in the
    /// SAME consolidated item as PATs so the whole app costs at most ONE Keychain
    /// prompt after a reinstall (see WHY ONE ITEM above).
    #[serde(default)]
    mcp: HashMap<String, McpSecrets>,
    /// Server secrets (SSH private-key passphrase), keyed by server_id. Kept in
    /// the SAME consolidated item as PATs/MCP so the whole app costs at most ONE
    /// Keychain prompt after a reinstall (see WHY ONE ITEM above).
    #[serde(default)]
    servers: HashMap<String, ServerSecrets>,
    /// AWS account secrets, keyed by aws_account_id. Secret Access Key and
    /// Session Token live here; SQLite only stores non-secret metadata.
    #[serde(default)]
    aws: HashMap<String, AwsSecrets>,
    /// Remote Control (C1) secrets: the Host↔relay `auth_token` (SEC-001) and the
    /// per-device pairing `psk` values (SEC-002/BR-003), kept in the SAME
    /// consolidated item so the whole app costs at most ONE Keychain prompt.
    #[serde(default)]
    remote: RemoteSecrets,
    /// Per-process guard: `"<provider>:<account_id>"` keys we already attempted to
    /// migrate from a legacy per-account item. A denied or missing legacy read is
    /// recorded here so it is NEVER retried within the same run — otherwise a
    /// user who clicks "Deny" would be re-prompted on every subsequent git op.
    /// Not persisted (rebuilt each launch).
    #[serde(skip)]
    tried_legacy: std::collections::HashSet<String>,
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

fn provider_tag(provider: &Provider) -> &'static str {
    match provider {
        Provider::Github => "github",
        Provider::Gitlab => "gitlab",
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

fn map_of_mut<'a>(
    store: &'a mut SecretStore,
    provider: &Provider,
) -> &'a mut HashMap<String, String> {
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
    // Transparent one-time migration from a pre-consolidation install. Attempt at
    // most ONCE per account per process: record the attempt BEFORE reading so a
    // denied/missing legacy read is never retried (which would re-prompt on every
    // git op). `legacy_exists` is metadata-only (no prompt), so a non-existent
    // legacy item costs nothing and never shows a dialog.
    let marker = format!("{}:{}", provider_tag(&provider), account_id);
    if store.tried_legacy.insert(marker) && legacy_exists(&provider, account_id) {
        if let Some(pat) = take_legacy(&provider, account_id) {
            map_of_mut(store, &provider).insert(account_id.to_string(), pat.clone());
            let _ = persist(store);
            return Ok(pat);
        }
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

// ---- MCP server secrets ----------------------------------------------------
//
// MCP secret VALUEs (`{"env":{KEY:VALUE...},"headers":{KEY:VALUE...}}`) live in
// the SAME consolidated Keychain item as the PATs, keyed by server_id. SQLite
// never holds the VALUEs — only the KEY names, for form rendering.
//
// WHY CONSOLIDATED: previously each server had its OWN item (service
// `devdy-mcp`). On macOS every item has a separate ACL bound to the app's code
// signature, so an ad-hoc/unsigned rebuild re-prompts ("allow access…") once
// PER item — i.e. once per MCP server. Folding the secrets into the single
// store means at most ONE prompt for the whole app, matching the PAT behaviour.
// Legacy per-server items are migrated transparently on first read.

/// Legacy per-server Keychain service, kept only for one-time migration.
const MCP_SERVICE_NAME: &str = "devdy-mcp";

/// Decrypted secret payload for a single MCP server.
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct McpSecrets {
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default)]
    pub headers: HashMap<String, String>,
}

/// Legacy per-server item (service `devdy-mcp`, account = server_id).
fn legacy_mcp_entry(server_id: &str) -> Result<Entry> {
    Ok(Entry::new(MCP_SERVICE_NAME, server_id)?)
}

/// Read + remove a legacy per-server item (one-time migration). May prompt once
/// per legacy item — unavoidable for pre-consolidation installs, but only until
/// every server is moved into the consolidated store.
fn take_legacy_mcp(server_id: &str) -> Option<McpSecrets> {
    let entry = legacy_mcp_entry(server_id).ok()?;
    match entry.get_password() {
        Ok(json) => {
            let _ = entry.delete_credential();
            Some(serde_json::from_str(&json).unwrap_or_default())
        }
        Err(_) => None,
    }
}

/// Delete a legacy per-server item if present (no prompt on delete).
fn drop_legacy_mcp(server_id: &str) {
    if let Ok(entry) = legacy_mcp_entry(server_id) {
        let _ = entry.delete_credential();
    }
}

/// Whether a legacy per-server item exists — metadata-only, prompt-free.
fn legacy_mcp_exists(server_id: &str) -> bool {
    legacy_mcp_entry(server_id)
        .map(|e| e.get_attributes().is_ok())
        .unwrap_or(false)
}

/// Persist a server's secret env/header VALUEs into the consolidated store.
pub fn set_mcp_secrets(
    server_id: &str,
    env: &HashMap<String, String>,
    headers: &HashMap<String, String>,
) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.mcp.insert(
        server_id.to_string(),
        McpSecrets {
            env: env.clone(),
            headers: headers.clone(),
        },
    );
    persist(store)?;
    // Remove any stale legacy item so it can never shadow or re-prompt.
    drop_legacy_mcp(server_id);
    Ok(())
}

/// Read a server's secret env/header VALUEs. Fail-closed: returns an empty
/// payload if the item is missing or unreadable (never errors the caller).
pub fn get_mcp_secrets(server_id: &str) -> McpSecrets {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return McpSecrets::default(),
    };
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    if let Some(secret) = store.mcp.get(server_id) {
        return secret.clone();
    }
    // Transparent one-time migration from a pre-consolidation install. Attempt
    // at most ONCE per server per process (recorded BEFORE reading), and only
    // when the legacy item actually exists — the existence check is
    // metadata-only so a missing item never shows a dialog.
    let marker = format!("mcp:{}", server_id);
    if store.tried_legacy.insert(marker) && legacy_mcp_exists(server_id) {
        if let Some(secret) = take_legacy_mcp(server_id) {
            store.mcp.insert(server_id.to_string(), secret.clone());
            let _ = persist(store);
            return secret;
        }
    }
    McpSecrets::default()
}

/// Delete a server's secrets from the consolidated store (no-op if absent).
pub fn delete_mcp_secrets(server_id: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.mcp.remove(server_id);
    let _ = persist(store);
    drop_legacy_mcp(server_id);
    Ok(())
}

// ---- VPS/server secrets ----------------------------------------------------
//
// The SSH private-key passphrase (if any) for a managed VPS lives in the SAME
// consolidated Keychain item as the PATs/MCP secrets, keyed by server_id.
// SQLite never holds the passphrase. This table is NEW (no pre-consolidation
// per-item history), so there is no legacy migration path.

/// Decrypted secret payload for a single managed server.
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct ServerSecrets {
    #[serde(default)]
    pub passphrase: Option<String>,
}

/// Persist a server's SSH private-key passphrase into the consolidated store.
pub fn set_server_secret(server_id: &str, passphrase: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.servers.insert(
        server_id.to_string(),
        ServerSecrets {
            passphrase: Some(passphrase.to_string()),
        },
    );
    persist(store)?;
    Ok(())
}

/// Read a server's secret payload. Fail-closed: returns an empty payload if the
/// item is missing or unreadable (never errors the caller).
///
/// Foundation API: GĐ1 stores the passphrase; the reader is consumed by the
/// later AI-deploy phase (GĐ3) that actually opens the SSH session.
#[allow(dead_code)]
pub fn get_server_secret(server_id: &str) -> ServerSecrets {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return ServerSecrets::default(),
    };
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.servers.get(server_id).cloned().unwrap_or_default()
}

/// Delete a server's secret from the consolidated store (no-op if absent).
pub fn delete_server_secret(server_id: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.servers.remove(server_id);
    persist(store)?;
    Ok(())
}

/// Whether a passphrase is stored for `server_id` WITHOUT reading its value.
///
/// LOW-1 fix (AC-108): answer PRECISELY from the consolidated store instead of
/// the old `store_exists()` fallback, which was a false positive — the store
/// item exists as soon as ANY secret (a PAT, an MCP secret) is written, so a
/// server created without a passphrase would wrongly report `has_passphrase =
/// true`. We `ensure_loaded` (the single existing load path — no NEW Keychain
/// prompt beyond the one already amortized across the app) and read the exact
/// `servers` map entry. Fail-closed to `false` if the cache lock is poisoned.
pub fn has_server_secret(server_id: &str) -> bool {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    ensure_loaded(&mut guard);
    guard
        .as_ref()
        .and_then(|store| store.servers.get(server_id))
        .map(|s| s.passphrase.is_some())
        .unwrap_or(false)
}

// ---- AWS account secrets ---------------------------------------------------
//
// Secret Access Key and optional Session Token for a managed AWS account live in
// the SAME consolidated Keychain item as the PATs/MCP/server secrets, keyed by
// aws_account_id. SQLite never holds these values.

/// Decrypted secret payload for a single AWS account.
#[derive(Default, Serialize, Deserialize, Clone)]
pub struct AwsSecrets {
    #[serde(default)]
    pub secret_access_key: Option<String>,
    #[serde(default)]
    pub session_token: Option<String>,
}

/// Persist an AWS account's secret material into the consolidated store.
pub fn set_aws_secret(
    account_id: &str,
    secret_access_key: &str,
    session_token: Option<&str>,
) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.aws.insert(
        account_id.to_string(),
        AwsSecrets {
            secret_access_key: Some(secret_access_key.to_string()),
            session_token: session_token
                .map(|s| s.to_string())
                .filter(|s| !s.trim().is_empty()),
        },
    );
    persist(store)?;
    Ok(())
}

/// Read an AWS account's secret payload. Missing values return an error so
/// callers can fail-closed.
pub fn get_aws_secret(account_id: &str) -> Result<AwsSecrets> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store
        .aws
        .get(account_id)
        .cloned()
        .ok_or_else(|| anyhow!("No AWS secret stored for account"))
}

/// Delete an AWS account's secret from the consolidated store (no-op if absent).
pub fn delete_aws_secret(account_id: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.aws.remove(account_id);
    persist(store)?;
    Ok(())
}

/// Whether a Secret Access Key is stored for `account_id` WITHOUT returning it.
pub fn has_aws_secret(account_id: &str) -> bool {
    let mut guard = match CACHE.lock() {
        Ok(g) => g,
        Err(_) => return false,
    };
    ensure_loaded(&mut guard);
    guard
        .as_ref()
        .and_then(|store| store.aws.get(account_id))
        .and_then(|s| s.secret_access_key.as_ref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false)
}

// ---- Remote Control (C1) secrets -------------------------------------------
//
// The Host↔relay `auth_token` (SEC-001) and each paired device's pairing `psk`
// (SEC-002/BR-003) live in the SAME consolidated Keychain item, so the whole app
// still costs at most ONE Keychain prompt after a reinstall. SQLite never holds
// either value. No legacy migration path (this feature is new).

/// Remote Control secret material kept in the consolidated store.
#[derive(Default, Serialize, Deserialize, Clone)]
struct RemoteSecrets {
    /// Host→relay auth token (SEC-001). `None` until the Owner enables remote.
    #[serde(default)]
    auth_token: Option<String>,
    /// The single active remote session's reconnect credential. `None` when no
    /// session is bound. Holds the reusable rendezvous code, the Host's E2E
    /// keypair (so the pinned fingerprint stays valid), the `session_secret`
    /// (mixed into the reconnect KDF instead of the OTP) and the sliding idle
    /// expiry.
    #[serde(default)]
    session: Option<RemoteSession>,
    /// Optional owner-chosen master password. When set, a controller may
    /// authenticate with it (mixed into the pairing KDF exactly like the OTP)
    /// instead of reading the per-join one-time code — simpler for a single
    /// owner. `None`/empty falls back to the OTP flow.
    #[serde(default)]
    master_password: Option<String>,
}

/// The single active remote session's durable reconnect credential.
#[derive(Serialize, Deserialize, Clone)]
pub struct RemoteSession {
    /// The single bound run this session controls.
    pub run_id: String,
    /// Reusable relay rendezvous code (bearer routing key) the controller rejoins.
    pub rendezvous_code: String,
    /// Session secret (hex) mixed into the reconnect-KDF in place of the OTP.
    pub session_secret: String,
    /// Host ephemeral X25519 public key (hex) — reused so the fingerprint the
    /// controller pinned still matches on reconnect.
    pub host_pub: String,
    /// Host ephemeral X25519 secret key (hex). Secret — Keychain only.
    pub host_secret: String,
    /// Sliding idle expiry as a Unix timestamp (seconds). Refreshed on each
    /// accepted command; past it the session must re-authenticate with the OTP.
    pub idle_expires_at: u64,
}

/// Persist the Host↔relay auth token.
pub fn set_remote_auth_token(token: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.remote.auth_token = Some(token.to_string());
    persist(store)
}

/// Read the Host↔relay auth token, if any.
pub fn get_remote_auth_token() -> Option<String> {
    let mut guard = CACHE.lock().ok()?;
    ensure_loaded(&mut guard);
    guard
        .as_ref()
        .and_then(|store| store.remote.auth_token.clone())
        .filter(|t| !t.trim().is_empty())
}

/// Whether a non-empty auth token is stored WITHOUT returning its value.
pub fn has_remote_auth_token() -> bool {
    get_remote_auth_token().is_some()
}

/// Delete the Host↔relay auth token (no-op if absent). Part of the Remote
/// Control secret API surface (used when the Owner clears remote config).
#[allow(dead_code)]
pub fn delete_remote_auth_token() -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.remote.auth_token = None;
    persist(store)
}

/// Persist (or clear) the owner's remote master password. An empty string
/// clears it (falls back to the OTP flow).
pub fn set_remote_master_password(password: &str) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.remote.master_password = if password.trim().is_empty() {
        None
    } else {
        Some(password.to_string())
    };
    persist(store)
}

/// Read the owner's remote master password, if a non-empty one is stored.
pub fn get_remote_master_password() -> Option<String> {
    let mut guard = CACHE.lock().ok()?;
    ensure_loaded(&mut guard);
    guard
        .as_ref()
        .and_then(|store| store.remote.master_password.clone())
        .filter(|p| !p.trim().is_empty())
}

/// Whether a non-empty master password is stored WITHOUT returning its value.
pub fn has_remote_master_password() -> bool {
    get_remote_master_password().is_some()
}

/// Persist (or replace) the single remote session's reconnect credential.
pub fn set_remote_session(session: &RemoteSession) -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.remote.session = Some(session.clone());
    persist(store)
}

/// Read the single remote session's reconnect credential, if any.
pub fn get_remote_session() -> Option<RemoteSession> {
    let mut guard = CACHE.lock().ok()?;
    ensure_loaded(&mut guard);
    guard.as_ref().and_then(|store| store.remote.session.clone())
}

/// Delete the remote session credential (end-session / idle expiry). No-op if
/// absent.
pub fn delete_remote_session() -> Result<()> {
    let mut guard = CACHE.lock().map_err(|_| anyhow!("secret cache poisoned"))?;
    ensure_loaded(&mut guard);
    let store = guard.as_mut().expect("store loaded");
    store.remote.session = None;
    persist(store)
}
