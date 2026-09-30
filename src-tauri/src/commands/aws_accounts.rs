use crate::db::Db;
use crate::secrets;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::process::Stdio;
use std::time::Duration;
use tauri::State;
use tokio::process::Command;
use uuid::Uuid;

const DEFAULT_REGION: &str = "ap-northeast-1";
const STS_TIMEOUT: Duration = Duration::from_secs(15);
// SSO login opens a browser and blocks until the user finishes the flow, so it
// needs a much more generous window than a plain STS call.
const SSO_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
// Sentinel error the frontend matches to offer an "SSO login" button instead of
// a generic validation-failed message.
pub(crate) const SSO_LOGIN_REQUIRED: &str = "SSO_LOGIN_REQUIRED";

/// Detect the AWS CLI stderr signatures that mean the profile is SSO-based and
/// its cached token is missing or expired (i.e. `aws sso login` is needed).
pub(crate) fn is_sso_login_required(stderr: &str) -> bool {
    let lower = stderr.to_lowercase();
    lower.contains("error loading sso token")
        || (lower.contains("token") && lower.contains("does not exist"))
        || (lower.contains("token") && lower.contains("has expired"))
        || (lower.contains("sso") && lower.contains("expired"))
        || lower.contains("the sso session associated with this profile has expired")
}

#[derive(Debug, Serialize, Clone)]
pub struct AwsAccount {
    pub id: String,
    pub label: String,
    pub auth_method: String,
    pub account_id: Option<String>,
    pub arn: Option<String>,
    pub region: String,
    pub access_key_id: Option<String>,
    pub profile_name: Option<String>,
    pub tags: Option<String>,
    pub has_secret: bool,
    pub last_validated_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AwsValidation {
    pub account_id: String,
    pub arn: String,
    pub user_id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAwsAccountPayload {
    pub label: String,
    pub region: Option<String>,
    pub profile_name: Option<String>,
    pub tags: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAwsAccountPayload {
    pub id: String,
    pub label: String,
    pub region: Option<String>,
    pub profile_name: Option<String>,
    pub tags: Option<String>,
}

#[derive(Deserialize)]
struct StsCallerIdentity {
    #[serde(rename = "Account")]
    account: String,
    #[serde(rename = "Arn")]
    arn: String,
    #[serde(rename = "UserId")]
    user_id: String,
}

fn row_to_account(row: &sqlx::sqlite::SqliteRow) -> AwsAccount {
    let id: String = row.get("id");
    AwsAccount {
        label: row.get("label"),
        auth_method: row.get("auth_method"),
        account_id: row.get("account_id"),
        arn: row.get("arn"),
        region: row.get("region"),
        access_key_id: row.get("access_key_id"),
        profile_name: row.get("profile_name"),
        tags: row.get("tags"),
        has_secret: secrets::has_aws_secret(&id),
        last_validated_at: row.get("last_validated_at"),
        created_at: row.get("created_at"),
        id,
    }
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

fn normalize_region(value: Option<String>) -> String {
    clean_optional(value).unwrap_or_else(|| DEFAULT_REGION.to_string())
}

async fn fetch_account(db: &Db, id: &str) -> Result<AwsAccount, String> {
    let row = sqlx::query(
        "SELECT id, label, auth_method, account_id, arn, region, access_key_id, \
                profile_name, tags, last_validated_at, created_at \
         FROM aws_accounts WHERE id = ?",
    )
    .bind(id)
    .fetch_one(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(row_to_account(&row))
}

async fn validate_aws_identity(profile_name: &str, region: &str) -> Result<AwsValidation, String> {
    let mut cmd = Command::new("aws");
    cmd.arg("sts")
        .arg("get-caller-identity")
        .arg("--output")
        .arg("json")
        .arg("--region")
        .arg(region)
        .arg("--profile")
        .arg(profile_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // GUI launches (Finder/Dock) don't inherit the login-shell PATH, so `aws`
    // (typically /usr/local/bin or /opt/homebrew/bin) isn't found. Recover the
    // login PATH like the sidecar/ssh commands do.
    crate::runs::sidecar::augment_command_path(&mut cmd);
    // The app may have inherited a per-run AWS_CONFIG_FILE / credentials override
    // (Devdy's sandbox writes a minimal one for run children). Force the CLI back
    // to the user's real ~/.aws config so `--profile` resolves against the same
    // file the profile picker reads.
    cmd.env_remove("AWS_CONFIG_FILE");
    cmd.env_remove("AWS_SHARED_CREDENTIALS_FILE");
    cmd.env_remove("AWS_ACCESS_KEY_ID");
    cmd.env_remove("AWS_SECRET_ACCESS_KEY");
    cmd.env_remove("AWS_SESSION_TOKEN");

    let output = match tokio::time::timeout(STS_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Failed to run aws CLI: {e}")),
        Err(_) => {
            return Err(format!(
                "AWS STS validation timed out after {}s",
                STS_TIMEOUT.as_secs()
            ))
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if is_sso_login_required(&stderr) {
            return Err(SSO_LOGIN_REQUIRED.to_string());
        }
        return Err(
            "AWS STS validation failed. Check AWS CLI, region, profile, and permissions."
                .to_string(),
        );
    }

    let identity: StsCallerIdentity = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("Failed to parse AWS STS response: {e}"))?;

    Ok(AwsValidation {
        account_id: identity.account,
        arn: identity.arn,
        user_id: identity.user_id,
    })
}

#[tauri::command]
pub async fn list_aws_accounts(db: State<'_, Db>) -> Result<Vec<AwsAccount>, String> {
    let rows = sqlx::query(
        "SELECT id, label, auth_method, account_id, arn, region, access_key_id, \
                profile_name, tags, last_validated_at, created_at \
         FROM aws_accounts ORDER BY label",
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(row_to_account).collect())
}

#[tauri::command]
pub async fn create_aws_account(
    db: State<'_, Db>,
    payload: CreateAwsAccountPayload,
) -> Result<AwsAccount, String> {
    let label = clean_required(&payload.label, "label")?;
    let region = normalize_region(payload.region);
    let tags = clean_optional(payload.tags);
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let profile_name = clean_optional(payload.profile_name)
        .ok_or_else(|| "profile_name is required".to_string())?;
    let validation = validate_aws_identity(&profile_name, &region).await?;

    sqlx::query(
        "INSERT INTO aws_accounts \
         (id, label, auth_method, account_id, arn, region, access_key_id, profile_name, tags, last_validated_at, created_at) \
         VALUES (?, ?, 'profile', ?, ?, ?, NULL, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&label)
    .bind(&validation.account_id)
    .bind(&validation.arn)
    .bind(&region)
    .bind(&profile_name)
    .bind(&tags)
    .bind(&now)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    fetch_account(db.inner(), &id).await
}

#[tauri::command]
pub async fn update_aws_account(
    db: State<'_, Db>,
    payload: UpdateAwsAccountPayload,
) -> Result<AwsAccount, String> {
    let current = fetch_account(db.inner(), &payload.id).await?;
    let label = clean_required(&payload.label, "label")?;
    let region = normalize_region(payload.region);
    let tags = clean_optional(payload.tags);
    let mut account_id = current.account_id.clone();
    let mut arn = current.arn.clone();
    let mut last_validated_at = current.last_validated_at.clone();

    let profile_name = clean_optional(payload.profile_name)
        .ok_or_else(|| "profile_name is required".to_string())?;

    // Re-validate when the profile or region changed.
    let auth_changed = region != current.region || Some(&profile_name) != current.profile_name.as_ref();
    if auth_changed {
        let validation = validate_aws_identity(&profile_name, &region).await?;
        account_id = Some(validation.account_id);
        arn = Some(validation.arn);
        last_validated_at = Some(chrono::Utc::now().to_rfc3339());
    }

    sqlx::query(
        "UPDATE aws_accounts SET \
         label = ?, auth_method = 'profile', account_id = ?, arn = ?, region = ?, \
         access_key_id = NULL, profile_name = ?, tags = ?, last_validated_at = ? WHERE id = ?",
    )
    .bind(&label)
    .bind(&account_id)
    .bind(&arn)
    .bind(&region)
    .bind(&profile_name)
    .bind(&tags)
    .bind(&last_validated_at)
    .bind(&payload.id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    fetch_account(db.inner(), &payload.id).await
}

#[tauri::command]
pub async fn delete_aws_account(db: State<'_, Db>, id: String) -> Result<(), String> {
    let _ = secrets::delete_aws_secret(&id);
    let _ = sqlx::query("UPDATE projects SET aws_account_id = NULL WHERE aws_account_id = ?")
        .bind(&id)
        .execute(db.inner())
        .await;
    sqlx::query("DELETE FROM aws_accounts WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn validate_aws_account(db: State<'_, Db>, id: String) -> Result<AwsValidation, String> {
    let account = fetch_account(db.inner(), &id).await?;
    let profile_name = account
        .profile_name
        .as_deref()
        .ok_or_else(|| "profile_name is missing".to_string())?;
    let validation = validate_aws_identity(profile_name, &account.region).await?;

    let now = chrono::Utc::now().to_rfc3339();
    let _ = sqlx::query(
        "UPDATE aws_accounts SET account_id = ?, arn = ?, last_validated_at = ? WHERE id = ?",
    )
    .bind(&validation.account_id)
    .bind(&validation.arn)
    .bind(&now)
    .bind(&id)
    .execute(db.inner())
    .await;

    Ok(validation)
}

/// Run `aws sso login` for a profile-based account so the user can refresh an
/// expired/missing SSO token from the UI instead of dropping to a terminal.
/// This launches the browser SSO flow and blocks until it completes.
#[tauri::command]
pub async fn aws_sso_login(db: State<'_, Db>, id: String) -> Result<(), String> {
    let account = fetch_account(db.inner(), &id).await?;
    if account.auth_method != "profile" {
        return Err("SSO login is only available for profile-based accounts.".to_string());
    }
    let profile_name = account
        .profile_name
        .as_deref()
        .ok_or_else(|| "profile_name is missing".to_string())?;

    let mut cmd = Command::new("aws");
    cmd.arg("sso")
        .arg("login")
        .arg("--profile")
        .arg(profile_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // GUI launches don't inherit the login-shell PATH; recover it like the STS
    // validation path does so `aws` resolves.
    crate::runs::sidecar::augment_command_path(&mut cmd);
    cmd.env_remove("AWS_ACCESS_KEY_ID");
    cmd.env_remove("AWS_SECRET_ACCESS_KEY");
    cmd.env_remove("AWS_SESSION_TOKEN");
    // Use the user's real ~/.aws config, not any inherited per-run override.
    cmd.env_remove("AWS_CONFIG_FILE");
    cmd.env_remove("AWS_SHARED_CREDENTIALS_FILE");

    let output = match tokio::time::timeout(SSO_LOGIN_TIMEOUT, cmd.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => return Err(format!("Failed to run aws CLI: {e}")),
        Err(_) => {
            return Err(format!(
                "AWS SSO login timed out after {}s",
                SSO_LOGIN_TIMEOUT.as_secs()
            ))
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            return Err("AWS SSO login failed.".to_string());
        }
        return Err(format!("AWS SSO login failed: {detail}"));
    }

    Ok(())
}

#[tauri::command]
pub async fn set_project_aws_account(
    db: State<'_, Db>,
    project_id: String,
    account_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE projects SET aws_account_id = ? WHERE id = ?")
        .bind(&account_id)
        .bind(&project_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
