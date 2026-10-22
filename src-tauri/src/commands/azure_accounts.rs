//! Azure account management.
//!
//! Each Devdy Azure account is an isolated Azure CLI config dir
//! (`AZURE_CONFIG_DIR`, see `azure_sandbox`). Sign-in happens in-app via
//! `az login` (browser flow) INTO that dir, and the chosen subscription is set as the dir's default
//! there with `az account set`. The user's own `~/.azure` is never touched.

use crate::db::Db;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tauri::{AppHandle, Manager, State};
use tokio::process::Command;
use uuid::Uuid;

const AZ_TIMEOUT: Duration = Duration::from_secs(30);
// `az login` opens a browser and blocks until the user finishes the flow.
const AZ_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
// Sentinel error the frontend matches to offer an "az login" button instead of a
// generic failure message.
pub(crate) const AZURE_LOGIN_REQUIRED: &str = "AZURE_LOGIN_REQUIRED";

/// Inherited Azure / azurerm credential env that must never leak into an `az`
/// invocation (app-side) or a run's sidecar: each would let the CLI or an SDK
/// authenticate as something other than the Devdy-managed account.
pub(crate) const AZURE_CREDENTIAL_ENV: &[&str] = &[
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "AZURE_TENANT_ID",
    "AZURE_SUBSCRIPTION_ID",
    "AZURE_CLIENT_CERTIFICATE_PATH",
    "AZURE_CLIENT_CERTIFICATE_PASSWORD",
    "AZURE_FEDERATED_TOKEN_FILE",
    "AZURE_USERNAME",
    "AZURE_PASSWORD",
    "ARM_CLIENT_ID",
    "ARM_CLIENT_SECRET",
    "ARM_TENANT_ID",
    "ARM_SUBSCRIPTION_ID",
    "ARM_USE_MSI",
    "ARM_USE_OIDC",
    "ARM_OIDC_TOKEN",
    "ARM_OIDC_TOKEN_FILE_PATH",
    "ARM_CLIENT_CERTIFICATE_PATH",
    "ARM_CLIENT_CERTIFICATE_PASSWORD",
    "ARM_ACCESS_KEY",
    "ARM_SAS_TOKEN",
    "MSI_ENDPOINT",
    "IDENTITY_ENDPOINT",
    "IDENTITY_HEADER",
];

/// Detect Azure CLI stderr signatures meaning the dir has no usable session
/// (never signed in, refresh token expired/revoked, MFA/consent required).
pub(crate) fn is_login_required(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    s.contains("az login")
        || s.contains("aadsts")
        || s.contains("refresh token")
        || s.contains("interactionrequired")
        || s.contains("interaction_required")
        || s.contains("no subscription found")
}

#[derive(Debug, Serialize, Clone)]
pub struct AzureAccount {
    pub id: String,
    pub label: String,
    pub tenant: Option<String>,
    pub tenant_id: Option<String>,
    pub subscription_id: Option<String>,
    pub subscription_name: Option<String>,
    pub user_name: Option<String>,
    pub tags: Option<String>,
    pub last_validated_at: Option<String>,
    pub created_at: String,
    /// The account dir exists and holds a signed-in profile (cheap local check).
    pub logged_in: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AzureValidation {
    pub tenant_id: String,
    pub subscription_id: String,
    pub subscription_name: String,
    pub user_name: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AzureSubscription {
    pub id: String,
    pub name: String,
    pub tenant_id: String,
    pub is_default: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAzureAccountPayload {
    pub label: String,
    pub tenant: Option<String>,
    pub subscription: Option<String>,
    pub tags: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAzureAccountPayload {
    pub id: String,
    pub label: String,
    pub subscription: Option<String>,
    pub tags: Option<String>,
}

/// One entry of `az account show` / `az account list` JSON.
#[derive(Deserialize)]
struct AzAccountJson {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "tenantId", default)]
    tenant_id: String,
    #[serde(rename = "isDefault", default)]
    is_default: bool,
    #[serde(default)]
    user: Option<AzUserJson>,
}

#[derive(Deserialize)]
struct AzUserJson {
    #[serde(default)]
    name: String,
}

fn clean_required(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err(format!("{label} is required"))
    } else {
        Ok(value.to_string())
    }
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn app_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path().app_data_dir().map_err(|e| e.to_string())
}

fn account_dir(app: &AppHandle, id: &str) -> Result<PathBuf, String> {
    crate::azure_sandbox::account_dir(&app_data_dir(app)?, id)
        .ok_or_else(|| "invalid Azure account id".to_string())
}

const SELECT_COLUMNS: &str = "SELECT id, label, tenant, tenant_id, subscription_id, \
     subscription_name, user_name, tags, last_validated_at, created_at FROM azure_accounts";

fn row_to_account(row: &sqlx::sqlite::SqliteRow, root: Option<&Path>) -> AzureAccount {
    let id: String = row.get("id");
    let logged_in = root
        .and_then(|r| crate::azure_sandbox::account_dir_in(r, &id))
        .map(|d| crate::azure_sandbox::is_logged_in(&d))
        .unwrap_or(false);
    AzureAccount {
        label: row.get("label"),
        tenant: row.get("tenant"),
        tenant_id: row.get("tenant_id"),
        subscription_id: row.get("subscription_id"),
        subscription_name: row.get("subscription_name"),
        user_name: row.get("user_name"),
        tags: row.get("tags"),
        last_validated_at: row.get("last_validated_at"),
        created_at: row.get("created_at"),
        logged_in,
        id,
    }
}

async fn fetch_account(app: &AppHandle, db: &Db, id: &str) -> Result<AzureAccount, String> {
    let row = sqlx::query(&format!("{SELECT_COLUMNS} WHERE id = ?"))
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let root = app_data_dir(app)
        .ok()
        .map(|d| crate::azure_sandbox::accounts_root(&d));
    Ok(row_to_account(&row, root.as_deref()))
}

/// Is `key` an Azure CLI / azurerm env var? az reads arbitrary config from
/// `AZURE_<SECTION>_<KEY>` (e.g. `AZURE_EXTENSION_DIR` loads Python code), so
/// every inherited one is dropped, not just the credential ones.
pub(crate) fn is_azure_env(key: &str) -> bool {
    key.starts_with("AZURE_") || key.starts_with("ARM_")
}

/// Per-account-dir lock: serializes `az login` / `account set` / `account show`
/// so two concurrent operations never write into the same config dir.
fn account_lock(id: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: OnceLock<std::sync::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
        OnceLock::new();
    let map = LOCKS.get_or_init(|| std::sync::Mutex::new(HashMap::new()));
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard.entry(id.to_string()).or_default().clone()
}

/// Build an `az` command bound to one account dir. Recovers the login-shell PATH
/// (GUI launches lack it), never inherits Azure/ARM env, and pins
/// `AZURE_CONFIG_DIR` explicitly so the user's `~/.azure` is never used.
/// `kill_on_drop`: a timed-out `az login` must die rather than later write a
/// token into a dir that was already removed.
fn az_command(dir: &Path) -> Command {
    let mut cmd = Command::new("az");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    crate::runs::sidecar::augment_command_path(&mut cmd);
    for key in AZURE_CREDENTIAL_ENV {
        cmd.env_remove(key);
    }
    for (key, _) in std::env::vars_os() {
        if key.to_str().map(is_azure_env).unwrap_or(false) {
            cmd.env_remove(key);
        }
    }
    cmd.env("AZURE_CONFIG_DIR", dir);
    // No interactive subscription picker after `az login` (we pin it ourselves).
    cmd.env("AZURE_CORE_LOGIN_EXPERIENCE_V2", "off");
    // Newer Pythons print SyntaxWarnings from the CLI's own packages on stderr;
    // keep them out of error messages.
    cmd.env("PYTHONWARNINGS", "ignore");
    cmd
}

/// Condense az stderr into a short, user-facing detail: prefer `ERROR:` lines,
/// else the trimmed text. Never contains secrets (az does not print tokens to
/// stderr).
fn stderr_detail(stderr: &str) -> String {
    let errors: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("ERROR"))
        .collect();
    if errors.is_empty() {
        stderr.trim().to_string()
    } else {
        errors.join(" ")
    }
}

/// Run az and return stdout on success. Login-required failures map to the
/// `AZURE_LOGIN_REQUIRED` sentinel; other failures carry a short detail.
async fn run_az(
    dir: &Path,
    args: &[&str],
    timeout: Duration,
    what: &str,
) -> Result<Vec<u8>, String> {
    run_az_with(dir, args, timeout, what, true).await
}

async fn run_az_with(
    dir: &Path,
    args: &[&str],
    timeout: Duration,
    what: &str,
    map_login_required: bool,
) -> Result<Vec<u8>, String> {
    let mut cmd = az_command(dir);
    cmd.args(args);
    let output = match tokio::time::timeout(timeout, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Failed to run Azure CLI (az): {e}")),
        Err(_) => return Err(format!("{what} timed out after {}s", timeout.as_secs())),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if map_login_required && is_login_required(&stderr) {
            return Err(AZURE_LOGIN_REQUIRED.to_string());
        }
        let detail = stderr_detail(&stderr);
        if detail.is_empty() {
            return Err(format!("{what} failed."));
        }
        return Err(format!("{what} failed: {detail}"));
    }
    Ok(output.stdout)
}

/// ARM resource scope the MFA claims challenge is requested for.
const ARM_SCOPE: &str = "https://management.core.windows.net//.default";
/// Base64 of `{"access_token":{"acrs":{"essential":true,"values":["p1"]}}}` —
/// the claims challenge ARM returns with AADSTS50076 under Azure's mandatory
/// MFA. Sending it up front makes Entra ID step up to MFA even when the browser
/// already holds a password-only SSO session; without it a re-login silently
/// reuses that session and every write stays rejected.
const MFA_CLAIMS_CHALLENGE: &str =
    "eyJhY2Nlc3NfdG9rZW4iOnsiYWNycyI6eyJlc3NlbnRpYWwiOnRydWUsInZhbHVlcyI6WyJwMSJdfX19";

async fn az_login(dir: &Path, tenant: Option<&str>) -> Result<(), String> {
    let mut args = vec![
        "login",
        "--only-show-errors",
        "-o",
        "none",
        "--scope",
        ARM_SCOPE,
        "--claims-challenge",
        MFA_CLAIMS_CHALLENGE,
    ];
    if let Some(t) = tenant {
        args.push("--tenant");
        args.push(t);
    }
    // A failed interactive login is not "login required" — surface its detail.
    run_az_with(dir, &args, AZ_LOGIN_TIMEOUT, "Azure login", false)
        .await
        .map(|_| ())
}

async fn az_account_set(dir: &Path, subscription: &str) -> Result<(), String> {
    run_az(
        dir,
        &[
            "account",
            "set",
            "--subscription",
            subscription,
            "--only-show-errors",
        ],
        AZ_TIMEOUT,
        "Selecting the Azure subscription",
    )
    .await
    .map(|_| ())
}

async fn az_account_show(dir: &Path) -> Result<AzureValidation, String> {
    let stdout = run_az(
        dir,
        &["account", "show", "-o", "json", "--only-show-errors"],
        AZ_TIMEOUT,
        "Azure account check",
    )
    .await?;
    let acc: AzAccountJson = serde_json::from_slice(&stdout)
        .map_err(|e| format!("Failed to parse az account show output: {e}"))?;
    Ok(AzureValidation {
        tenant_id: acc.tenant_id,
        subscription_id: acc.id,
        subscription_name: acc.name,
        user_name: acc.user.map(|u| u.name).unwrap_or_default(),
    })
}

/// Prove the stored refresh token still works. `az account show` only reads the
/// local profile, so ask for a token — but request ONLY its expiry; the token
/// itself never reaches this process.
async fn az_check_session(dir: &Path) -> Result<(), String> {
    run_az(
        dir,
        &[
            "account",
            "get-access-token",
            "--query",
            "expiresOn",
            "-o",
            "tsv",
            "--only-show-errors",
        ],
        AZ_TIMEOUT,
        "Azure session check",
    )
    .await
    .map(|_| ())
}

/// Was the newest cached ARM access token issued with MFA? `None` when it can't
/// be told (no plaintext MSAL cache, no ARM token, undecodable). Only the `amr`
/// / `acrs` claims are read; the token never leaves this function.
fn arm_token_has_mfa(dir: &Path) -> Option<bool> {
    let raw = std::fs::read(dir.join("msal_token_cache.json")).ok()?;
    let cache: serde_json::Value = serde_json::from_slice(&raw).ok()?;
    let newest = cache
        .get("AccessToken")?
        .as_object()?
        .values()
        .filter(|t| {
            t.get("target")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.contains("management.core.windows.net"))
        })
        .max_by_key(|t| {
            t.get("cached_at")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
        })?;
    let jwt = newest.get("secret")?.as_str()?;
    token_claims_have_mfa(jwt)
}

fn token_claims_have_mfa(jwt: &str) -> Option<bool> {
    use base64::Engine as _;
    let payload = jwt.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let has = |claim: &str, want: &str| {
        claims
            .get(claim)
            .and_then(|v| v.as_array())
            .is_some_and(|a| a.iter().any(|x| x.as_str() == Some(want)))
    };
    Some(has("amr", "mfa") || has("acrs", "p1"))
}

async fn store_validation(db: &Db, id: &str, v: &AzureValidation) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "UPDATE azure_accounts SET tenant_id = ?, subscription_id = ?, subscription_name = ?, \
         user_name = ?, last_validated_at = ? WHERE id = ?",
    )
    .bind(&v.tenant_id)
    .bind(&v.subscription_id)
    .bind(&v.subscription_name)
    .bind(&v.user_name)
    .bind(&now)
    .bind(id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_azure_accounts(
    app: AppHandle,
    db: State<'_, Db>,
) -> Result<Vec<AzureAccount>, String> {
    let rows = sqlx::query(&format!("{SELECT_COLUMNS} ORDER BY label"))
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let root = app_data_dir(&app)
        .ok()
        .map(|d| crate::azure_sandbox::accounts_root(&d));
    Ok(rows
        .iter()
        .map(|r| row_to_account(r, root.as_deref()))
        .collect())
}

#[tauri::command]
pub async fn create_azure_account(
    app: AppHandle,
    db: State<'_, Db>,
    payload: CreateAzureAccountPayload,
) -> Result<AzureAccount, String> {
    let label = clean_required(&payload.label, "label")?;
    let tenant = clean_optional(payload.tenant);
    let subscription = clean_optional(payload.subscription);
    let tags = clean_optional(payload.tags);
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let dir = account_dir(&app, &id)?;

    sqlx::query(
        "INSERT INTO azure_accounts (id, label, tenant, tags, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&label)
    .bind(&tenant)
    .bind(&tags)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let lock = account_lock(&id);
    let _guard = lock.lock().await;
    let setup = async {
        crate::azure_sandbox::create_account_dir(&dir)
            .map_err(|e| format!("Failed to create Azure config dir: {e}"))?;
        az_login(&dir, tenant.as_deref()).await?;
        if let Some(sub) = subscription.as_deref() {
            az_account_set(&dir, sub).await?;
        }
        let validation = az_account_show(&dir).await?;
        store_validation(db.inner(), &id, &validation).await
    }
    .await;

    if let Err(e) = setup {
        let _ = sqlx::query("DELETE FROM azure_accounts WHERE id = ?")
            .bind(&id)
            .execute(db.inner())
            .await;
        crate::azure_sandbox::remove_account_dir(&dir);
        return Err(e);
    }

    fetch_account(&app, db.inner(), &id).await
}

#[tauri::command]
pub async fn update_azure_account(
    app: AppHandle,
    db: State<'_, Db>,
    payload: UpdateAzureAccountPayload,
) -> Result<AzureAccount, String> {
    let current = fetch_account(&app, db.inner(), &payload.id).await?;
    let label = clean_required(&payload.label, "label")?;
    let tags = clean_optional(payload.tags);

    // The subscription may be given as an id or a name; only re-pin when it
    // names something other than the current one.
    if let Some(sub) = clean_optional(payload.subscription) {
        let unchanged = current.subscription_id.as_deref() == Some(sub.as_str())
            || current.subscription_name.as_deref() == Some(sub.as_str());
        if !unchanged {
            let dir = account_dir(&app, &payload.id)?;
            let lock = account_lock(&payload.id);
            let _guard = lock.lock().await;
            az_account_set(&dir, &sub).await?;
            match az_account_show(&dir).await {
                Ok(validation) => store_validation(db.inner(), &payload.id, &validation).await?,
                Err(e) => {
                    // Keep DB and dir consistent: restore the previous default,
                    // or — if that is impossible — record the new one as given.
                    let restored = match current.subscription_id.as_deref() {
                        Some(old) => az_account_set(&dir, old).await.is_ok(),
                        None => false,
                    };
                    if !restored {
                        let _ = sqlx::query(
                            "UPDATE azure_accounts SET subscription_id = ?, subscription_name = NULL \
                             WHERE id = ?",
                        )
                        .bind(&sub)
                        .bind(&payload.id)
                        .execute(db.inner())
                        .await;
                    }
                    return Err(e);
                }
            }
        }
    }

    sqlx::query("UPDATE azure_accounts SET label = ?, tags = ? WHERE id = ?")
        .bind(&label)
        .bind(&tags)
        .bind(&payload.id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    fetch_account(&app, db.inner(), &payload.id).await
}

#[tauri::command]
pub async fn delete_azure_account(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    let lock = account_lock(&id);
    let _guard = lock.lock().await;
    let _ = sqlx::query("UPDATE projects SET azure_account_id = NULL WHERE azure_account_id = ?")
        .bind(&id)
        .execute(db.inner())
        .await;
    sqlx::query("DELETE FROM azure_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    // Dropping the dir discards the account's az session and token cache. There
    // are no per-project Azure sandboxes to clean: runs only ever see the shared
    // empty dir, and the shims hand out the account dir per call.
    if let Ok(dir) = account_dir(&app, &id) {
        crate::azure_sandbox::remove_account_dir(&dir);
    }
    Ok(())
}

#[tauri::command]
pub async fn validate_azure_account(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
) -> Result<AzureValidation, String> {
    let dir = account_dir(&app, &id)?;
    let lock = account_lock(&id);
    let _guard = lock.lock().await;
    if !crate::azure_sandbox::is_logged_in(&dir) {
        return Err(AZURE_LOGIN_REQUIRED.to_string());
    }
    let validation = az_account_show(&dir).await?;
    az_check_session(&dir).await?;
    // A password-only session still reads fine, but Azure's mandatory MFA
    // rejects every create/update/delete (AADSTS50076) — don't report green.
    if arm_token_has_mfa(&dir) == Some(false) {
        return Err(format!(
            "{AZURE_LOGIN_REQUIRED}: signed in without MFA — Azure blocks write operations. Sign in again to complete MFA."
        ));
    }
    store_validation(db.inner(), &id, &validation).await?;
    Ok(validation)
}

/// Re-run the browser `az login` inside the account dir (e.g. after the refresh
/// token expired), then re-pin the stored subscription.
#[tauri::command]
pub async fn azure_login(app: AppHandle, db: State<'_, Db>, id: String) -> Result<(), String> {
    let account = fetch_account(&app, db.inner(), &id).await?;
    let dir = account_dir(&app, &id)?;
    let lock = account_lock(&id);
    let _guard = lock.lock().await;
    crate::azure_sandbox::create_account_dir(&dir)
        .map_err(|e| format!("Failed to create Azure config dir: {e}"))?;
    az_login(&dir, account.tenant.as_deref()).await?;
    if let Some(sub) = account
        .subscription_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
    {
        az_account_set(&dir, sub).await?;
    }
    let validation = az_account_show(&dir).await?;
    store_validation(db.inner(), &id, &validation).await
}

#[tauri::command]
pub async fn list_azure_subscriptions(
    app: AppHandle,
    id: String,
) -> Result<Vec<AzureSubscription>, String> {
    let dir = account_dir(&app, &id)?;
    let lock = account_lock(&id);
    let _guard = lock.lock().await;
    if !crate::azure_sandbox::is_logged_in(&dir) {
        return Err(AZURE_LOGIN_REQUIRED.to_string());
    }
    let stdout = run_az(
        &dir,
        &["account", "list", "-o", "json", "--only-show-errors"],
        AZ_TIMEOUT,
        "Listing Azure subscriptions",
    )
    .await?;
    let list: Vec<AzAccountJson> = serde_json::from_slice(&stdout)
        .map_err(|e| format!("Failed to parse az account list output: {e}"))?;
    Ok(list
        .into_iter()
        .map(|a| AzureSubscription {
            id: a.id,
            name: a.name,
            tenant_id: a.tenant_id,
            is_default: a.is_default,
        })
        .collect())
}

#[tauri::command]
pub async fn set_project_azure_account(
    db: State<'_, Db>,
    project_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE projects SET azure_account_id = ? WHERE id = ?")
        .bind(&account_id)
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_login_required_stderr() {
        assert!(is_login_required(
            "ERROR: Please run 'az login' to setup account."
        ));
        assert!(is_login_required(
            "AADSTS700082: The refresh token has expired due to inactivity."
        ));
        assert!(is_login_required("InteractionRequired: consent needed"));
        assert!(is_login_required(
            "No subscription found. Run 'az account set'"
        ));
        assert!(!is_login_required(
            "ERROR: (ResourceGroupNotFound) Resource group 'x' could not be found."
        ));
    }

    #[test]
    fn azure_env_prefixes() {
        assert!(is_azure_env("AZURE_EXTENSION_DIR"));
        assert!(is_azure_env("ARM_METADATA_HOSTNAME"));
        assert!(!is_azure_env("AWS_REGION"));
        assert!(!is_azure_env("PATH"));
    }

    #[tokio::test]
    async fn account_lock_is_shared_per_id() {
        let a = account_lock("lock-test-a");
        let a2 = account_lock("lock-test-a");
        let b = account_lock("lock-test-b");
        assert!(Arc::ptr_eq(&a, &a2));
        assert!(!Arc::ptr_eq(&a, &b));
        let _held = a.lock().await;
        assert!(a2.try_lock().is_err(), "same id must contend");
        assert!(b.try_lock().is_ok(), "other ids are independent");
    }

    #[test]
    fn stderr_detail_prefers_error_lines() {
        let s =
            "  warning noise\nERROR: The subscription 'x' doesn't exist in cloud 'AzureCloud'.\n";
        assert_eq!(
            stderr_detail(s),
            "ERROR: The subscription 'x' doesn't exist in cloud 'AzureCloud'."
        );
        assert_eq!(stderr_detail("  plain failure \n"), "plain failure");
    }

    #[test]
    fn reads_mfa_from_token_claims() {
        use base64::Engine as _;
        let jwt = |claims: &str| {
            let b = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims);
            format!("h.{b}.s")
        };
        assert_eq!(
            token_claims_have_mfa(&jwt(r#"{"amr":["pwd"]}"#)),
            Some(false)
        );
        assert_eq!(
            token_claims_have_mfa(&jwt(r#"{"amr":["pwd","mfa"]}"#)),
            Some(true)
        );
        assert_eq!(
            token_claims_have_mfa(&jwt(r#"{"amr":["pwd"],"acrs":["p1"]}"#)),
            Some(true)
        );
        assert_eq!(
            token_claims_have_mfa(&jwt(r#"{"acrs":["pfdr"]}"#)),
            Some(false)
        );
        assert_eq!(token_claims_have_mfa("not-a-jwt"), None);
    }

    #[test]
    fn arm_mfa_uses_newest_management_token() {
        use base64::Engine as _;
        let enc = |c: &str| {
            format!(
                "h.{}.s",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(c)
            )
        };
        let dir = std::env::temp_dir().join(format!("devdy-az-mfa-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = serde_json::json!({ "AccessToken": {
            "old": { "target": "https://management.core.windows.net//.default", "cached_at": "100", "secret": enc(r#"{"amr":["mfa"]}"#) },
            "new": { "target": "https://management.core.windows.net//.default", "cached_at": "200", "secret": enc(r#"{"amr":["pwd"]}"#) },
            "graph": { "target": "https://graph.microsoft.com//.default", "cached_at": "300", "secret": enc(r#"{"amr":["mfa"]}"#) }
        }});
        std::fs::write(dir.join("msal_token_cache.json"), cache.to_string()).unwrap();
        assert_eq!(arm_token_has_mfa(&dir), Some(false));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(arm_token_has_mfa(&dir), None);
    }

    #[test]
    fn parses_az_account_json() {
        let json = br#"{"environmentName":"AzureCloud","id":"sub-1","isDefault":true,"name":"Dev","state":"Enabled","tenantId":"ten-1","user":{"name":"me@example.com","type":"user"}}"#;
        let a: AzAccountJson = serde_json::from_slice(json).unwrap();
        assert_eq!(a.id, "sub-1");
        assert_eq!(a.name, "Dev");
        assert_eq!(a.tenant_id, "ten-1");
        assert!(a.is_default);
        assert_eq!(a.user.unwrap().name, "me@example.com");
    }
}
