// AWS profile discovery from `~/.aws/config`.
//
// The account-linking UI lets the user pick an existing CLI profile: we scan the
// user's `~/.aws/config`, list the profiles found (each already encodes one SSO
// role via `sso_role_name`), and the chosen profile name is stored as-is. A small
// SSO-login helper refreshes an expired token for a profile.
//
// We intentionally read the canonical `~/.aws/config` (never an inherited
// `AWS_CONFIG_FILE`, which may point at an ephemeral per-run sandbox) so the
// picker surfaces the same profiles the user's own `aws` CLI sees.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

// SSO login opens a browser and blocks until the user finishes the flow.
const SSO_LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsProfileInfo {
    pub name: String,
    pub sso_role_name: Option<String>,
    pub sso_account_id: Option<String>,
    pub sso_session: Option<String>,
    pub region: Option<String>,
    /// True when the profile is SSO-based (has a role/session/start URL).
    pub is_sso: bool,
}

// ---------------------------------------------------------------------------
// ~/.aws/config parsing (minimal INI)
// ---------------------------------------------------------------------------

struct ConfigSection {
    kind: String, // "profile" | "sso-session" | "default" | ...
    name: String,
    kv: BTreeMap<String, String>,
}

fn aws_config_path() -> Option<PathBuf> {
    // Always target the canonical `~/.aws/config`; ignore any AWS_CONFIG_FILE
    // override (it may be an ephemeral per-run sandbox config).
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".aws").join("config"))
}

fn read_config_text() -> String {
    aws_config_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default()
}

/// Parse section headers and their `key = value` pairs. Comments and blank lines
/// are ignored; nested/indented sub-settings are harmless noise.
fn parse_config(text: &str) -> Vec<ConfigSection> {
    let mut out: Vec<ConfigSection> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let inner = line[1..line.len() - 1].trim();
            let (kind, name) = match inner.split_once(char::is_whitespace) {
                Some((k, n)) => (k.trim().to_string(), n.trim().to_string()),
                None => (inner.to_string(), inner.to_string()), // e.g. [default]
            };
            out.push(ConfigSection {
                kind,
                name,
                kv: BTreeMap::new(),
            });
        } else if let Some((k, v)) = line.split_once('=') {
            if let Some(sec) = out.last_mut() {
                sec.kv.insert(k.trim().to_string(), v.trim().to_string());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// List the profiles defined in `~/.aws/config`, each with its SSO role/account
/// (when present) so the UI can show what a profile grants.
#[tauri::command]
pub async fn list_aws_profiles() -> Result<Vec<AwsProfileInfo>, String> {
    let sections = parse_config(&read_config_text());
    let mut out: Vec<AwsProfileInfo> = sections
        .iter()
        .filter(|s| s.kind == "profile" || s.kind == "default")
        .map(|s| {
            let sso_role_name = s.kv.get("sso_role_name").cloned();
            let sso_session = s.kv.get("sso_session").cloned();
            let has_start_url = s.kv.contains_key("sso_start_url");
            AwsProfileInfo {
                name: s.name.clone(),
                sso_account_id: s.kv.get("sso_account_id").cloned(),
                region: s.kv.get("region").cloned(),
                is_sso: sso_role_name.is_some() || sso_session.is_some() || has_start_url,
                sso_role_name,
                sso_session,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(out)
}

/// Refresh an SSO token by running `aws sso login --profile <name>` (browser flow)
/// so a profile whose token expired can be re-validated from the UI.
#[tauri::command]
pub async fn aws_sso_login_profile(profile_name: String) -> Result<(), String> {
    let profile_name = profile_name.trim();
    if profile_name.is_empty() {
        return Err("profile_name is required".to_string());
    }
    let mut cmd = Command::new("aws");
    cmd.arg("sso")
        .arg("login")
        .arg("--profile")
        .arg(profile_name)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    crate::runs::sidecar::augment_command_path(&mut cmd);
    cmd.env_remove("AWS_PROFILE");
    cmd.env_remove("AWS_DEFAULT_PROFILE");
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
