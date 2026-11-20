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

use serde::{Deserialize, Serialize};
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

/// Run `aws sso login --profile <name>` (browser flow) against the user's real
/// `~/.aws` config and block until it finishes. Shared by the profile picker and
/// profile-based AWS accounts. `kill_on_drop` so a timed-out login doesn't keep
/// an `aws` process waiting on the browser forever.
pub async fn run_sso_login(profile_name: &str) -> Result<(), String> {
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
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // GUI launches don't inherit the login-shell PATH; recover it so `aws` resolves.
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

/// Refresh an SSO token for a `~/.aws/config` profile so a profile whose token
/// expired can be re-validated from the UI.
#[tauri::command]
pub async fn aws_sso_login_profile(profile_name: String) -> Result<(), String> {
    run_sso_login(&profile_name).await
}

// ---------------------------------------------------------------------------
// Profile definitions carried by sync
// ---------------------------------------------------------------------------
//
// Settings backup / device sync carry an SSO profile's definition (never a
// credential) so a new machine gets the same `~/.aws/config` entries and only
// needs `aws sso login`. Writing is append-only: an existing profile or
// sso-session is never modified or removed.

/// Non-secret keys copied from a `[profile X]` section.
const PROFILE_KEYS: &[&str] = &[
    "region",
    "output",
    "sso_session",
    "sso_account_id",
    "sso_role_name",
    "sso_start_url",
    "sso_region",
    "sso_registration_scopes",
];

/// Keys copied from the `[sso-session Y]` section a profile references.
const SSO_SESSION_KEYS: &[&str] = &["sso_start_url", "sso_region", "sso_registration_scopes"];

/// Comment written above sections appended by sync.
const SYNC_MARKER: &str = "# Added by Devdy sync";

/// An SSO profile's `~/.aws/config` definition: its own keys plus the
/// `sso-session` section it points at (named by `profile["sso_session"]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsProfileDef {
    pub profile: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sso_session: Option<BTreeMap<String, String>>,
}

/// What [`merge_profile_def`] did to the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileMerge {
    /// The profile and/or its sso-session were appended.
    Added,
    /// Both were already present with the same values.
    Unchanged,
    /// A section with this name exists with different values; nothing written.
    /// Carries the section header, e.g. `sso-session yahagi-admin`.
    Conflict(String),
}

fn pick(kv: &BTreeMap<String, String>, keys: &[&str]) -> BTreeMap<String, String> {
    keys.iter()
        .filter_map(|k| kv.get(*k).map(|v| (k.to_string(), v.clone())))
        .collect()
}

/// Same values, ignoring a trailing `/` on the start URL (the console hands out
/// both spellings).
fn same_values(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> bool {
    let norm = |m: &BTreeMap<String, String>| -> BTreeMap<String, String> {
        m.iter()
            .map(|(k, v)| {
                let v = if k == "sso_start_url" {
                    v.trim_end_matches('/')
                } else {
                    v.as_str()
                };
                (k.clone(), v.to_string())
            })
            .collect()
    };
    norm(a) == norm(b)
}

fn find<'a>(sections: &'a [ConfigSection], kind: &str, name: &str) -> Option<&'a ConfigSection> {
    sections.iter().find(|s| s.kind == kind && s.name == name)
}

/// Extract the definition of SSO profile `name` from config text. `None` when
/// the profile is missing or not SSO-based (static-key profiles are not synced).
pub fn profile_def_from_text(text: &str, name: &str) -> Option<AwsProfileDef> {
    let sections = parse_config(text);
    let profile = pick(&find(&sections, "profile", name)?.kv, PROFILE_KEYS);
    if !profile.contains_key("sso_account_id") || !profile.contains_key("sso_role_name") {
        return None;
    }
    let sso_session = match profile.get("sso_session") {
        Some(session) => Some(pick(
            &find(&sections, "sso-session", session)?.kv,
            SSO_SESSION_KEYS,
        )),
        // Legacy profile: start URL / region sit on the profile itself.
        None if profile.contains_key("sso_start_url") => None,
        None => return None,
    };
    Some(AwsProfileDef {
        profile,
        sso_session,
    })
}

/// Read profile `name` from the canonical `~/.aws/config` for export.
pub fn read_profile_def(name: &str) -> Option<AwsProfileDef> {
    profile_def_from_text(&read_config_text(), name)
}

/// Reject anything that could break out of a section or line when written.
fn validate_def(name: &str, def: &AwsProfileDef) -> Result<(), String> {
    let bad_name = |s: &str| {
        s.trim().is_empty() || s.trim() != s || s.contains(['[', ']', '\n', '\r', '=', '#', ';'])
    };
    let bad_value = |s: &str| s.contains(['\n', '\r']);
    if bad_name(name) {
        return Err("invalid profile name".to_string());
    }
    if let Some(session) = def.profile.get("sso_session") {
        if bad_name(session) {
            return Err("invalid sso-session name".to_string());
        }
    }
    let maps = std::iter::once(&def.profile).chain(def.sso_session.iter());
    for map in maps {
        if map.values().any(|v| bad_value(v)) {
            return Err("invalid profile value".to_string());
        }
    }
    Ok(())
}

fn render_section(out: &mut String, header: &str, kv: &BTreeMap<String, String>) {
    out.push_str(&format!("[{header}]\n"));
    for (k, v) in kv {
        out.push_str(&format!("{k} = {v}\n"));
    }
}

/// Merge profile `name` into config `text`. Returns the new text when something
/// was appended. Existing sections are never changed: a same-named section with
/// different values is a conflict and nothing is written.
pub fn merge_profile_def(
    text: &str,
    name: &str,
    def: &AwsProfileDef,
) -> Result<(ProfileMerge, Option<String>), String> {
    validate_def(name, def)?;
    let profile = pick(&def.profile, PROFILE_KEYS);
    let sections = parse_config(text);
    let mut append: Vec<(String, BTreeMap<String, String>)> = Vec::new();

    let profile_header = format!("profile {name}");
    match find(&sections, "profile", name) {
        Some(existing) if same_values(&pick(&existing.kv, PROFILE_KEYS), &profile) => {}
        Some(_) => return Ok((ProfileMerge::Conflict(profile_header), None)),
        None => append.push((profile_header, profile.clone())),
    }
    if let (Some(session), Some(kv)) = (profile.get("sso_session"), &def.sso_session) {
        let kv = pick(kv, SSO_SESSION_KEYS);
        let header = format!("sso-session {session}");
        match find(&sections, "sso-session", session) {
            Some(existing) if same_values(&pick(&existing.kv, SSO_SESSION_KEYS), &kv) => {}
            Some(_) => return Ok((ProfileMerge::Conflict(header), None)),
            None => append.push((header, kv)),
        }
    }
    if append.is_empty() {
        return Ok((ProfileMerge::Unchanged, None));
    }

    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(SYNC_MARKER);
    out.push('\n');
    for (header, kv) in &append {
        render_section(&mut out, header, kv);
    }
    Ok((ProfileMerge::Added, Some(out)))
}

/// Write `text` to the config atomically with owner-only permissions, keeping
/// the previous file as `config.devdy.bak`.
fn write_config(path: &std::path::Path, text: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("invalid ~/.aws path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create ~/.aws: {e}"))?;
    if path.exists() {
        std::fs::copy(path, dir.join("config.devdy.bak"))
            .map_err(|e| format!("back up ~/.aws/config: {e}"))?;
    }
    let tmp = dir.join(format!(".config.devdy.{}.tmp", std::process::id()));
    std::fs::write(&tmp, text).map_err(|e| format!("write ~/.aws/config: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("write ~/.aws/config: {e}")
    })
}

/// Make sure profile `name` (and its sso-session) exists in `~/.aws/config`,
/// appending it when missing. Never edits or removes existing entries.
pub fn ensure_profile_def(name: &str, def: &AwsProfileDef) -> Result<ProfileMerge, String> {
    let path = aws_config_path().ok_or("HOME is not set")?;
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("read ~/.aws/config: {e}")),
    };
    let (outcome, new_text) = merge_profile_def(&text, name, def)?;
    if let Some(new_text) = new_text {
        write_config(&path, &new_text)?;
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "\
[profile malme]
region = ap-northeast-1
[profile yahagi-admin]
sso_session = yahagi
sso_account_id = 229015218011
sso_role_name = malme-sso-ps-admin
[sso-session yahagi]
sso_start_url = https://d-1.awsapps.com/start/
sso_region = ap-northeast-1
sso_registration_scopes = sso:account:access
";

    fn def() -> AwsProfileDef {
        profile_def_from_text(SOURCE, "yahagi-admin").expect("sso profile")
    }

    #[test]
    fn extracts_sso_profile_with_its_session() {
        let d = def();
        assert_eq!(
            d.profile.get("sso_role_name").unwrap(),
            "malme-sso-ps-admin"
        );
        assert_eq!(
            d.sso_session.unwrap().get("sso_start_url").unwrap(),
            "https://d-1.awsapps.com/start/"
        );
    }

    #[test]
    fn static_and_missing_profiles_are_not_exported() {
        assert!(profile_def_from_text(SOURCE, "malme").is_none());
        assert!(profile_def_from_text(SOURCE, "nope").is_none());
        // Session section missing → incomplete, skip.
        let text = "[profile x]\nsso_session = s\nsso_account_id = 1\nsso_role_name = r\n";
        assert!(profile_def_from_text(text, "x").is_none());
    }

    #[test]
    fn appends_profile_and_session_to_existing_config() {
        let target = "[profile malme]\nregion = ap-northeast-1";
        let (outcome, text) = merge_profile_def(target, "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Added);
        let text = text.unwrap();
        assert!(
            text.starts_with("[profile malme]\nregion = ap-northeast-1\n# Added by Devdy sync\n")
        );
        assert_eq!(profile_def_from_text(&text, "yahagi-admin"), Some(def()));
        assert!(profile_def_from_text(&text, "malme").is_none());
    }

    #[test]
    fn writes_into_an_empty_config() {
        let (outcome, text) = merge_profile_def("", "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Added);
        assert_eq!(
            profile_def_from_text(&text.unwrap(), "yahagi-admin"),
            Some(def())
        );
    }

    #[test]
    fn identical_entries_are_left_alone() {
        let (outcome, text) = merge_profile_def(SOURCE, "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Unchanged);
        assert!(text.is_none());
        // Trailing-slash difference on the start URL still counts as the same.
        let target = SOURCE.replace("/start/", "/start");
        assert_eq!(
            merge_profile_def(&target, "yahagi-admin", &def())
                .unwrap()
                .0,
            ProfileMerge::Unchanged
        );
    }

    #[test]
    fn shared_session_is_written_once() {
        let mut other = def();
        other
            .profile
            .insert("sso_role_name".into(), "ReadOnly".into());
        let (outcome, text) = merge_profile_def(SOURCE, "yahagi-ro", &other).unwrap();
        assert_eq!(outcome, ProfileMerge::Added);
        let text = text.unwrap();
        assert_eq!(text.matches("[sso-session yahagi]").count(), 1);
        assert!(text.contains("[profile yahagi-ro]"));
    }

    #[test]
    fn conflicting_entries_are_never_overwritten() {
        let target = SOURCE.replace("malme-sso-ps-admin", "AdministratorAccess");
        let (outcome, text) = merge_profile_def(&target, "yahagi-admin", &def()).unwrap();
        assert_eq!(
            outcome,
            ProfileMerge::Conflict("profile yahagi-admin".into())
        );
        assert!(text.is_none());

        let target = "[sso-session yahagi]\nsso_start_url = https://other/start\n";
        let (outcome, text) = merge_profile_def(target, "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Conflict("sso-session yahagi".into()));
        assert!(text.is_none());
    }

    #[test]
    fn rejects_values_that_would_inject_sections() {
        let mut bad = def();
        bad.profile
            .insert("region".into(), "x\n[profile evil]".into());
        assert!(merge_profile_def("", "yahagi-admin", &bad).is_err());
        assert!(merge_profile_def("", "a]\n[b", &def()).is_err());
    }
}
