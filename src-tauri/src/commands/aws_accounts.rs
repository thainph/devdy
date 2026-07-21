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
    pub auth_method: String,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
    pub profile_name: Option<String>,
    pub tags: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateAwsAccountPayload {
    pub id: String,
    pub label: String,
    pub auth_method: String,
    pub region: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
    pub session_token: Option<String>,
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

enum AwsAuthForSts<'a> {
    Keys {
        access_key_id: &'a str,
        secret_access_key: &'a str,
        session_token: Option<&'a str>,
    },
    Profile {
        profile_name: &'a str,
    },
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

fn normalize_auth_method(value: &str) -> Result<String, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "keys" => Ok("keys".to_string()),
        "profile" => Ok("profile".to_string()),
        _ => Err("auth_method must be 'keys' or 'profile'".to_string()),
    }
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

async fn validate_aws_identity(
    auth: AwsAuthForSts<'_>,
    region: &str,
) -> Result<AwsValidation, String> {
    let mut cmd = Command::new("aws");
    cmd.arg("sts")
        .arg("get-caller-identity")
        .arg("--output")
        .arg("json")
        .arg("--region")
        .arg(region)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // GUI launches (Finder/Dock) don't inherit the login-shell PATH, so `aws`
    // (typically /usr/local/bin or /opt/homebrew/bin) isn't found. Recover the
    // login PATH like the sidecar/ssh commands do.
    crate::runs::sidecar::augment_command_path(&mut cmd);

    match auth {
        AwsAuthForSts::Keys {
            access_key_id,
            secret_access_key,
            session_token,
        } => {
            cmd.env("AWS_ACCESS_KEY_ID", access_key_id);
            cmd.env("AWS_SECRET_ACCESS_KEY", secret_access_key);
            if let Some(token) = session_token.filter(|s| !s.trim().is_empty()) {
                cmd.env("AWS_SESSION_TOKEN", token);
            } else {
                cmd.env_remove("AWS_SESSION_TOKEN");
            }
            cmd.env_remove("AWS_PROFILE");
            cmd.env_remove("AWS_DEFAULT_PROFILE");
        }
        AwsAuthForSts::Profile { profile_name } => {
            cmd.arg("--profile").arg(profile_name);
            cmd.env_remove("AWS_ACCESS_KEY_ID");
            cmd.env_remove("AWS_SECRET_ACCESS_KEY");
            cmd.env_remove("AWS_SESSION_TOKEN");
        }
    }

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
    let auth_method = normalize_auth_method(&payload.auth_method)?;
    let region = normalize_region(payload.region);
    let tags = clean_optional(payload.tags);
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let (access_key_id, profile_name, validation) = if auth_method == "keys" {
        let access_key_id = clean_optional(payload.access_key_id)
            .ok_or_else(|| "access_key_id is required for keys auth".to_string())?;
        let secret_access_key = clean_optional(payload.secret_access_key)
            .ok_or_else(|| "secret_access_key is required for keys auth".to_string())?;
        let session_token = clean_optional(payload.session_token);
        let validation = validate_aws_identity(
            AwsAuthForSts::Keys {
                access_key_id: &access_key_id,
                secret_access_key: &secret_access_key,
                session_token: session_token.as_deref(),
            },
            &region,
        )
        .await?;
        secrets::set_aws_secret(&id, &secret_access_key, session_token.as_deref())
            .map_err(|e| e.to_string())?;
        (Some(access_key_id), None, validation)
    } else {
        let profile_name = clean_optional(payload.profile_name)
            .ok_or_else(|| "profile_name is required for profile auth".to_string())?;
        let validation = validate_aws_identity(
            AwsAuthForSts::Profile {
                profile_name: &profile_name,
            },
            &region,
        )
        .await?;
        (None, Some(profile_name), validation)
    };

    let insert_result = sqlx::query(
        "INSERT INTO aws_accounts \
         (id, label, auth_method, account_id, arn, region, access_key_id, profile_name, tags, last_validated_at, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&label)
    .bind(&auth_method)
    .bind(&validation.account_id)
    .bind(&validation.arn)
    .bind(&region)
    .bind(&access_key_id)
    .bind(&profile_name)
    .bind(&tags)
    .bind(&now)
    .bind(&now)
    .execute(db.inner())
    .await;

    if let Err(e) = insert_result {
        let _ = secrets::delete_aws_secret(&id);
        return Err(e.to_string());
    }

    fetch_account(db.inner(), &id).await
}

#[tauri::command]
pub async fn update_aws_account(
    db: State<'_, Db>,
    payload: UpdateAwsAccountPayload,
) -> Result<AwsAccount, String> {
    let current = fetch_account(db.inner(), &payload.id).await?;
    let label = clean_required(&payload.label, "label")?;
    let auth_method = normalize_auth_method(&payload.auth_method)?;
    let region = normalize_region(payload.region);
    let tags = clean_optional(payload.tags);
    let mut account_id = current.account_id.clone();
    let mut arn = current.arn.clone();
    let mut last_validated_at = current.last_validated_at.clone();
    let mut secret_to_persist: Option<(String, Option<String>)> = None;

    let access_key_id = if auth_method == "keys" {
        Some(
            clean_optional(payload.access_key_id)
                .ok_or_else(|| "access_key_id is required for keys auth".to_string())?,
        )
    } else {
        None
    };
    let profile_name = if auth_method == "profile" {
        Some(
            clean_optional(payload.profile_name)
                .ok_or_else(|| "profile_name is required for profile auth".to_string())?,
        )
    } else {
        None
    };

    let new_secret = clean_optional(payload.secret_access_key);
    let session_token_was_sent = payload.session_token.is_some();
    let new_session_token = clean_optional(payload.session_token);
    let secret_update_requested = new_secret.is_some() || session_token_was_sent;
    let auth_changed = auth_method != current.auth_method
        || region != current.region
        || access_key_id != current.access_key_id
        || profile_name != current.profile_name
        || secret_update_requested;

    if auth_changed {
        let validation = if auth_method == "keys" {
            let access_key_id = access_key_id.as_deref().expect("keys access key checked");
            let stored_secret = if new_secret.is_none() || !session_token_was_sent {
                Some(secrets::get_aws_secret(&payload.id).map_err(|e| e.to_string())?)
            } else {
                None
            };
            let secret_access_key = new_secret.clone().or_else(|| {
                    stored_secret
                        .as_ref()
                        .and_then(|s| s.secret_access_key.clone())
                })
                .ok_or_else(|| "secret_access_key is required for keys auth".to_string())?;
            let session_token = if session_token_was_sent {
                new_session_token.clone()
            } else {
                stored_secret
                    .as_ref()
                    .and_then(|s| s.session_token.clone())
            };
            if secret_update_requested {
                secret_to_persist = Some((secret_access_key.clone(), session_token.clone()));
            }
            validate_aws_identity(
                AwsAuthForSts::Keys {
                    access_key_id,
                    secret_access_key: &secret_access_key,
                    session_token: session_token.as_deref(),
                },
                &region,
            )
            .await?
        } else {
            let profile_name = profile_name.as_deref().expect("profile checked");
            validate_aws_identity(AwsAuthForSts::Profile { profile_name }, &region).await?
        };
        let now = chrono::Utc::now().to_rfc3339();
        account_id = Some(validation.account_id);
        arn = Some(validation.arn);
        last_validated_at = Some(now);

        if auth_method == "keys" {
            if let Some((secret_access_key, session_token)) = secret_to_persist.as_ref() {
                secrets::set_aws_secret(&payload.id, secret_access_key, session_token.as_deref())
                    .map_err(|e| e.to_string())?;
            }
        } else {
            let _ = secrets::delete_aws_secret(&payload.id);
        }
    }

    sqlx::query(
        "UPDATE aws_accounts SET \
         label = ?, auth_method = ?, account_id = ?, arn = ?, region = ?, access_key_id = ?, \
         profile_name = ?, tags = ?, last_validated_at = ? WHERE id = ?",
    )
    .bind(&label)
    .bind(&auth_method)
    .bind(&account_id)
    .bind(&arn)
    .bind(&region)
    .bind(&access_key_id)
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
    let validation = if account.auth_method == "keys" {
        let access_key_id = account
            .access_key_id
            .as_deref()
            .ok_or_else(|| "access_key_id is missing".to_string())?;
        let secret = secrets::get_aws_secret(&id).map_err(|e| e.to_string())?;
        let secret_access_key = secret
            .secret_access_key
            .as_deref()
            .ok_or_else(|| "secret_access_key is missing".to_string())?;
        validate_aws_identity(
            AwsAuthForSts::Keys {
                access_key_id,
                secret_access_key,
                session_token: secret.session_token.as_deref(),
            },
            &account.region,
        )
        .await?
    } else {
        let profile_name = account
            .profile_name
            .as_deref()
            .ok_or_else(|| "profile_name is missing".to_string())?;
        validate_aws_identity(AwsAuthForSts::Profile { profile_name }, &account.region).await?
    };

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
