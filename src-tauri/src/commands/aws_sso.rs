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
use std::path::{Path, PathBuf};
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

pub(crate) fn aws_config_path() -> Option<PathBuf> {
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

/// Parse section headers and their `key = value` pairs, following the
/// `configparser` rules the AWS CLI (botocore) uses: a header is `[...]` at the
/// start of a line (anything after the closing `]` is ignored), keys are split
/// on the first `=` or `:` and lowercased, a line indented deeper than the key
/// line continues that key's value (nested sub-settings), and `#` / `;` lines
/// are comments. A quoted
/// section name (`[profile "x"]`) resolves to the same profile as `x`.
fn parse_config(text: &str) -> Vec<ConfigSection> {
    let mut out: Vec<ConfigSection> = Vec::new();
    // Indent of the current key line, while a value may still continue.
    let mut key_indent: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        if key_indent.is_some_and(|k| indent > k) {
            continue; // continuation of the previous key's value
        }
        key_indent = None;
        if line.starts_with('[') {
            if let Some(end) = line.rfind(']').filter(|&end| end > 1) {
                let inner = line[1..end].trim();
                let (kind, name) = match inner.split_once(char::is_whitespace) {
                    Some((k, n)) => (k.trim().to_string(), unquote(n.trim()).to_string()),
                    None => (inner.to_string(), inner.to_string()), // e.g. [default]
                };
                out.push(ConfigSection {
                    kind,
                    name,
                    kv: BTreeMap::new(),
                });
            }
            continue;
        }
        let split = line.find(['=', ':']).map(|i| (&line[..i], &line[i + 1..]));
        if let (Some((k, v)), Some(sec)) = (split, out.last_mut()) {
            sec.kv.insert(k.trim().to_lowercase(), v.trim().to_string());
            key_indent = Some(indent);
        }
    }
    out
}

/// Strip one pair of matching surrounding quotes, as botocore's `shlex` split does.
fn unquote(name: &str) -> &str {
    for q in ['"', '\''] {
        if name.len() >= 2 && name.starts_with(q) && name.ends_with(q) {
            return &name[1..name.len() - 1];
        }
    }
    name
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

/// Profile / sso-session names Devdy writes: plain tokens only, so botocore's
/// `shlex` split reads them back unchanged and nothing can break out of a header.
fn valid_name(s: &str) -> bool {
    // First char alphanumeric so `--profile <name>` is never read as an option.
    s.starts_with(|c: char| c.is_ascii_alphanumeric())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+' | '@'))
}

/// Whether `def` is an SSO profile (else an access-key one).
pub fn is_sso(def: &AwsProfileDef) -> bool {
    def.profile.keys().any(|k| k.starts_with("sso_")) || def.sso_session.is_some()
}

/// Reject anything that could break out of a section or line when written, or
/// that would not give a usable profile.
fn validate_def(name: &str, def: &AwsProfileDef) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("unsupported profile name '{name}'"));
    }
    // `[profile default]` and `[default]` are the same profile to the CLI.
    if name == "default" {
        return Err("the default profile is never written".to_string());
    }
    let maps = std::iter::once(&def.profile).chain(def.sso_session.iter());
    for map in maps {
        if map.values().any(|v| v.contains(['\n', '\r'])) {
            return Err("invalid profile value".to_string());
        }
    }
    let p = &def.profile;
    if !is_sso(def) {
        // Access-key profile: the config side only carries region / output.
        return match p.keys().all(|k| STATIC_PROFILE_KEYS.contains(&k.as_str())) {
            true if def.sso_session.is_none() => Ok(()),
            _ => Err("incomplete SSO profile".to_string()),
        };
    }
    if !p.contains_key("sso_account_id") || !p.contains_key("sso_role_name") {
        return Err("incomplete SSO profile".to_string());
    }
    match p.get("sso_session") {
        Some(session) if !valid_name(session) => {
            Err(format!("unsupported sso-session name '{session}'"))
        }
        Some(_) => match &def.sso_session {
            Some(kv) if kv.contains_key("sso_start_url") && kv.contains_key("sso_region") => Ok(()),
            _ => Err("sso-session is incomplete".to_string()),
        },
        None if p.contains_key("sso_start_url") && p.contains_key("sso_region") => Ok(()),
        None => Err("SSO start URL / region missing".to_string()),
    }
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
        // An access-key profile with no region/output has nothing to add or compare.
        _ if profile.is_empty() && !is_sso(def) => {}
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
    Ok((ProfileMerge::Added, Some(append_sections(text, &append)?)))
}

/// Append `sections` (header, keys) under the sync marker. Fails if any of them
/// would end up twice in the file, which makes it unreadable to the CLI: raw
/// header lines are counted too (any indentation), so a parser mistake fails
/// closed rather than writing that.
fn append_sections(
    text: &str,
    sections: &[(String, BTreeMap<String, String>)],
) -> Result<String, String> {
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(SYNC_MARKER);
    out.push('\n');
    for (header, kv) in sections {
        render_section(&mut out, header, kv);
    }
    let split = |inner: &str| -> (String, String) {
        match inner.split_once(char::is_whitespace) {
            Some((k, n)) => (k.to_string(), unquote(n.trim()).to_string()),
            None => (inner.to_string(), inner.to_string()),
        }
    };
    let merged = parse_config(&out);
    let raw_headers: Vec<(String, String)> = out
        .lines()
        .filter_map(|l| {
            let inner = l.trim().strip_prefix('[')?;
            Some(split(inner[..inner.rfind(']')?].trim()))
        })
        .collect();
    for (header, _) in sections {
        let (kind, name) = split(header);
        let parsed = merged
            .iter()
            .filter(|s| s.kind == kind && s.name == name)
            .count();
        let raw = raw_headers
            .iter()
            .filter(|(k, n)| *k == kind && *n == name)
            .count();
        if parsed != 1 || raw != 1 {
            return Err(format!("[{header}] would be duplicated, left unchanged"));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Access-key profiles (~/.aws/credentials)
// ---------------------------------------------------------------------------

/// Config keys an access-key profile carries (its keys live in `credentials`).
const STATIC_PROFILE_KEYS: &[&str] = &["region", "output"];

/// Config keys meaning the profile gets credentials some other way (role chain,
/// external process, web identity): never synced as an access-key profile.
const OTHER_AUTH_KEYS: &[&str] = &[
    "role_arn",
    "source_profile",
    "credential_source",
    "credential_process",
    "web_identity_token_file",
];

/// Keys that put credentials straight into a config / credentials section.
const INLINE_KEY_KEYS: &[&str] = &[
    "aws_access_key_id",
    "aws_secret_access_key",
    "aws_session_token",
    "aws_security_token",
];

/// Long-lived access keys of a profile, from `~/.aws/credentials`. Secret:
/// carried only inside the sealed sync payload, never logged (no `Debug`).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwsStaticKeys {
    pub access_key_id: String,
    pub secret_access_key: String,
}

/// The `[name]` section of a credentials file (no `profile ` prefix there).
fn credentials_section<'a>(sections: &'a [ConfigSection], name: &str) -> Option<&'a ConfigSection> {
    sections.iter().find(|s| s.kind == name && s.name == name)
}

/// Access keys of profile `name` in credentials `text`. `None` when missing,
/// incomplete, or temporary (a session token expires, so it is not worth syncing).
pub fn static_keys_from_text(text: &str, name: &str) -> Option<AwsStaticKeys> {
    let sections = parse_config(text);
    let kv = &credentials_section(&sections, name)?.kv;
    if kv.contains_key("aws_session_token") || kv.contains_key("aws_security_token") {
        return None;
    }
    let access_key_id = kv.get("aws_access_key_id")?;
    // `ASIA…` ids are temporary STS keys: useless without their token.
    if access_key_id.starts_with("ASIA") {
        return None;
    }
    Some(AwsStaticKeys {
        access_key_id: access_key_id.clone(),
        secret_access_key: kv.get("aws_secret_access_key")?.clone(),
    })
}

/// Config side of access-key profile `name`: its region / output (possibly
/// none). `None` when the profile is SSO or gets credentials another way.
pub fn static_def_from_text(config: &str, name: &str) -> Option<AwsProfileDef> {
    // The default profile is never written on import, so don't carry its keys.
    if name == "default" {
        return None;
    }
    let sections = parse_config(config);
    let kv = find(&sections, "profile", name)
        .map(|s| s.kv.clone())
        .unwrap_or_default();
    let other_auth = kv
        .keys()
        .any(|k| k.starts_with("sso_") || OTHER_AUTH_KEYS.contains(&k.as_str()));
    (!other_auth).then(|| AwsProfileDef {
        profile: pick(&kv, STATIC_PROFILE_KEYS),
        sso_session: None,
    })
}

fn aws_credentials_path() -> Option<PathBuf> {
    // Canonical `~/.aws/credentials`, like `aws_config_path`.
    aws_config_path().map(|p| p.with_file_name("credentials"))
}

/// Read access-key profile `name` from the canonical `~/.aws` files for export.
pub fn read_static_profile(name: &str) -> Option<(AwsProfileDef, AwsStaticKeys)> {
    let creds = std::fs::read_to_string(aws_credentials_path()?).ok()?;
    let keys = static_keys_from_text(&creds, name)?;
    let def = static_def_from_text(&read_config_text(), name)?;
    Some((def, keys))
}

fn validate_keys(keys: &AwsStaticKeys) -> Result<(), String> {
    // Never echo the values: they are secrets.
    let id_ok = (16..=128).contains(&keys.access_key_id.len())
        && keys
            .access_key_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric());
    let secret_ok = (1..=256).contains(&keys.secret_access_key.len())
        && keys.secret_access_key.chars().all(|c| c.is_ascii_graphic());
    if id_ok && secret_ok {
        Ok(())
    } else {
        Err("invalid access keys".to_string())
    }
}

/// Merge the access keys of profile `name` into credentials `text`, like
/// [`merge_profile_def`]: append when missing, never change an existing section.
pub fn merge_credentials(
    text: &str,
    name: &str,
    keys: &AwsStaticKeys,
) -> Result<(ProfileMerge, Option<String>), String> {
    if !valid_name(name) || name == "default" {
        return Err(format!("unsupported profile name '{name}'"));
    }
    validate_keys(keys)?;
    let sections = parse_config(text);
    if let Some(existing) = credentials_section(&sections, name) {
        let same = existing.kv.get("aws_access_key_id") == Some(&keys.access_key_id)
            && existing.kv.get("aws_secret_access_key") == Some(&keys.secret_access_key);
        let outcome = if same {
            ProfileMerge::Unchanged
        } else {
            ProfileMerge::Conflict(format!("credentials {name}"))
        };
        return Ok((outcome, None));
    }
    let kv = BTreeMap::from([
        ("aws_access_key_id".to_string(), keys.access_key_id.clone()),
        (
            "aws_secret_access_key".to_string(),
            keys.secret_access_key.clone(),
        ),
    ]);
    let out = append_sections(text, &[(name.to_string(), kv)])?;
    Ok((ProfileMerge::Added, Some(out)))
}

/// `~/.aws/<file>` for messages (never the content).
fn shown(path: &Path) -> String {
    let file = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    format!("~/.aws/{file}")
}

fn read_existing(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("read {}: {e}", shown(path))),
    }
}

/// Write `text` atomically (temp file + fsync + rename) with owner-only
/// permissions. `target` is the real file (symlinks already resolved).
/// `expected` is the content the merge was based on: if the file changed since,
/// nothing is written and `Ok(false)` asks the caller to merge again.
fn write_config(target: &Path, expected: &str, text: &str) -> Result<bool, String> {
    let dir = target.parent().ok_or("invalid ~/.aws path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create ~/.aws: {e}"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".config.devdy.{}.{nanos}.tmp", std::process::id()));
    let write_tmp = || -> std::io::Result<()> {
        use std::io::Write;
        let mut f = std::fs::File::create(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(text.as_bytes())?;
        f.sync_all()
    };
    let result = write_tmp()
        .map_err(|e| format!("write {}: {e}", shown(target)))
        .and_then(|()| read_existing(target))
        .and_then(|current| {
            if current != expected {
                return Ok(false);
            }
            std::fs::rename(&tmp, target)
                .map(|()| true)
                .map_err(|e| format!("write {}: {e}", shown(target)))
        });
    if result != Ok(true) {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// One profile to recreate: its name, config definition and, for an
/// access-key profile, the keys for `~/.aws/credentials`.
pub type ProfileItem = (String, AwsProfileDef, Option<AwsStaticKeys>);

/// Merge one item into both files' text. A conflict or error in either file
/// leaves both untouched for this item.
fn merge_item(
    config: &mut String,
    credentials: &mut String,
    (name, def, keys): &ProfileItem,
) -> Result<ProfileMerge, String> {
    let keys = match (is_sso(def), keys) {
        (true, _) => None,
        (false, Some(keys)) => Some(keys),
        (false, None) => return Err("access keys were not included".to_string()),
    };
    // Never give an existing profile a second way to authenticate: keys appended
    // to `credentials` win over SSO / credential_process in the CLI and SDKs, and
    // an SSO section would shadow existing keys.
    match keys {
        Some(_) if has_other_auth(config, name) => {
            return Ok(ProfileMerge::Conflict(format!("profile {name}")))
        }
        _ => {}
    }
    let (config_outcome, new_config) = merge_profile_def(config, name, def)?;
    let adds_profile = matches!(config_outcome, ProfileMerge::Added)
        && find(&parse_config(config), "profile", name).is_none();
    if keys.is_none()
        && adds_profile
        && credentials_section(&parse_config(credentials), name).is_some()
    {
        return Ok(ProfileMerge::Conflict(format!("credentials {name}")));
    }
    let (cred_outcome, new_credentials) = match keys {
        Some(keys) => merge_credentials(credentials, name, keys)?,
        None => (ProfileMerge::Unchanged, None),
    };
    for outcome in [&config_outcome, &cred_outcome] {
        if let ProfileMerge::Conflict(_) = outcome {
            return Ok(outcome.clone());
        }
    }
    let added = new_config.is_some() || new_credentials.is_some();
    if let Some(text) = new_config {
        *config = text;
    }
    if let Some(text) = new_credentials {
        *credentials = text;
    }
    Ok(if added {
        ProfileMerge::Added
    } else {
        ProfileMerge::Unchanged
    })
}

/// Whether the config's `[profile name]` authenticates some other way (SSO, role,
/// process, web identity, or keys inline in config).
fn has_other_auth(config: &str, name: &str) -> bool {
    let sections = parse_config(config);
    find(&sections, "profile", name).is_some_and(|s| {
        s.kv.keys().any(|k| {
            k.starts_with("sso_")
                || OTHER_AUTH_KEYS.contains(&k.as_str())
                || INLINE_KEY_KEYS.contains(&k.as_str())
        })
    })
}

/// Back up `original` beside `link` (in `~/.aws`, even for a symlinked file),
/// owner-only since the credentials backup holds secrets.
fn write_backup(link: &Path, original: &str) -> Result<(), String> {
    let file = link
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let backup = link.with_file_name(format!("{file}.devdy.bak"));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        // Replace, never write through, whatever sits at the backup path (a planted
        // symlink would otherwise receive the credentials backup).
        match std::fs::remove_file(&backup) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
            _ => {}
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        f.write_all(original.as_bytes())
    };
    write().map_err(|e| format!("back up ~/.aws/{file}: {e}"))
}

/// Make sure each profile exists in the config at `path` (`~/.aws/config` in
/// the app), and an access-key profile's keys in the `credentials` file beside
/// it, appending the missing entries in one write per file. Never edits or
/// removes existing entries. Before changing a file its previous content is
/// kept as `<file>.devdy.bak` beside it. One result per input, in order.
pub fn ensure_profile_defs_at(
    path: &Path,
    items: &[ProfileItem],
) -> Result<Vec<Result<ProfileMerge, String>>, String> {
    let cred_path = path.with_file_name("credentials");
    // Write through symlinked files (e.g. dotfiles) instead of replacing the link.
    let resolve = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (config_target, cred_target) = (resolve(path), resolve(&cred_path));
    // Items whose entries an earlier attempt already wrote (a retry sees them as
    // present and would otherwise report "unchanged").
    let mut written = vec![false; items.len()];
    for _ in 0..3 {
        let original_config = read_existing(&config_target)?;
        let original_cred = read_existing(&cred_target)?;
        let (mut config, mut cred) = (original_config.clone(), original_cred.clone());
        let results: Vec<Result<ProfileMerge, String>> = items
            .iter()
            .map(|item| merge_item(&mut config, &mut cred, item))
            .collect();
        let finish = |results: Vec<Result<ProfileMerge, String>>, written: &[bool]| {
            results
                .into_iter()
                .zip(written)
                .map(|(r, &w)| match r {
                    Ok(ProfileMerge::Unchanged) if w => Ok(ProfileMerge::Added),
                    r => r,
                })
                .collect::<Vec<_>>()
        };
        let files = [
            (
                cred_path.as_path(),
                cred_target.as_path(),
                &original_cred,
                &cred,
            ),
            (path, config_target.as_path(), &original_config, &config),
        ];
        let mut done = true;
        // Credentials first: a profile in config without its keys would fail, keys
        // without the config entry still work.
        for (index, (link, target, original, text)) in files.into_iter().enumerate() {
            if text == original {
                continue;
            }
            let step = (|| {
                if !original.is_empty() {
                    write_backup(link, original)?;
                }
                write_config(target, original, text)
            })();
            match step {
                Ok(true) => {
                    if index == 0 {
                        // Only access-key items put anything in `credentials`.
                        for ((w, r), (_, def, keys)) in written.iter_mut().zip(&results).zip(items)
                        {
                            *w |= r == &Ok(ProfileMerge::Added) && keys.is_some() && !is_sso(def);
                        }
                    }
                }
                Ok(false) => {
                    done = false;
                    break;
                }
                // Keys already written (this or an earlier attempt) but a later write
                // failed: say exactly that, keep conflicts / unchanged as they are.
                Err(e) if written.iter().any(|w| *w) => {
                    return Ok(results
                        .into_iter()
                        .zip(&written)
                        .map(|(r, &w)| match r {
                            _ if w => Err(format!(
                                "access keys written to ~/.aws/credentials, ~/.aws/config not updated: {e}"
                            )),
                            Ok(ProfileMerge::Added) => Err(e.clone()),
                            r => r,
                        })
                        .collect());
                }
                Err(e) => return Err(e),
            }
        }
        if done {
            return Ok(finish(results, &written));
        }
    }
    Err("~/.aws kept changing while writing, left unchanged".to_string())
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

    /// A throwaway `<tmp>/.aws/config` path; never the developer's real `~/.aws`.
    fn scratch_config(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "devdy-aws-sso-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        dir.join(".aws").join("config")
    }

    fn one(path: &Path) -> ProfileMerge {
        let mut results =
            ensure_profile_defs_at(path, &[("yahagi-admin".to_string(), def(), None)]).unwrap();
        results.remove(0).unwrap()
    }

    #[test]
    fn ensure_creates_missing_aws_dir_with_private_file() {
        let path = scratch_config("create");
        assert_eq!(one(&path), ProfileMerge::Added);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(profile_def_from_text(&text, "yahagi-admin"), Some(def()));
        // No backup when there was nothing to back up.
        assert!(!path.with_file_name("config.devdy.bak").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // Second run is a no-op and leaves no temp files behind.
        assert_eq!(one(&path), ProfileMerge::Unchanged);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn ensure_keeps_existing_content_byte_for_byte_and_backs_it_up() {
        let path = scratch_config("append");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "# my notes\n[default]\nregion = us-east-1\n\n[profile malme]\nregion = ap-northeast-1\ns3 =\n  max_concurrent_requests = 10\n";
        std::fs::write(&path, original).unwrap();
        assert_eq!(one(&path), ProfileMerge::Added);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.starts_with(original),
            "existing content changed:\n{text}"
        );
        assert_eq!(
            std::fs::read_to_string(path.with_file_name("config.devdy.bak")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn ensure_conflict_leaves_the_file_untouched() {
        let path = scratch_config("conflict");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = SOURCE.replace("malme-sso-ps-admin", "AdministratorAccess");
        std::fs::write(&path, &original).unwrap();
        assert!(matches!(one(&path), ProfileMerge::Conflict(_)));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        assert!(!path.with_file_name("config.devdy.bak").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn profile_def_survives_a_json_round_trip() {
        let json = serde_json::to_string(&def()).unwrap();
        let back: AwsProfileDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, def());
        // Legacy profile: no session section, field omitted.
        let legacy = AwsProfileDef {
            profile: def().profile,
            sso_session: None,
        };
        assert!(!serde_json::to_string(&legacy)
            .unwrap()
            .contains("ssoSession"));
    }

    // Review findings: the parser must see what the AWS CLI sees.

    #[test]
    fn header_with_trailing_comment_is_recognised() {
        let target = SOURCE
            .replace("[profile yahagi-admin]", "[profile yahagi-admin] # prod")
            .replace("[sso-session yahagi]", "[sso-session yahagi] ; shared");
        let (outcome, text) = merge_profile_def(&target, "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Unchanged);
        assert!(text.is_none());
    }

    #[test]
    fn quoted_colon_and_uppercase_forms_match_existing_sections() {
        let target = "\
[profile \"yahagi-admin\"]
SSO_Session: yahagi
sso_account_id = 229015218011
sso_role_name = malme-sso-ps-admin
[sso-session 'yahagi']
sso_start_url = https://d-1.awsapps.com/start/
sso_region = ap-northeast-1
sso_registration_scopes = sso:account:access
";
        assert_eq!(
            merge_profile_def(target, "yahagi-admin", &def()).unwrap().0,
            ProfileMerge::Unchanged
        );
    }

    #[test]
    fn indented_sub_settings_are_not_headers_or_keys() {
        let target =
            "[profile malme]\nregion = ap-northeast-1\ns3 =\n  max_concurrent_requests = 10\n";
        let (outcome, text) = merge_profile_def(target, "yahagi-admin", &def()).unwrap();
        assert_eq!(outcome, ProfileMerge::Added);
        assert!(text.unwrap().starts_with(target));
        let sections = parse_config(target);
        assert_eq!(sections.len(), 1);
        assert!(!sections[0].kv.contains_key("max_concurrent_requests"));
        // A header-looking line inside a value is ambiguous: refuse rather than risk
        // a duplicate section.
        let odd = format!("{target}  [profile yahagi-admin]\n");
        assert!(merge_profile_def(&odd, "yahagi-admin", &def()).is_err());
    }

    #[test]
    fn default_profile_is_never_written() {
        assert!(merge_profile_def("[default]\nregion = x\n", "default", &def()).is_err());
        assert!(merge_profile_def("", "default", &def()).is_err());
    }

    #[test]
    fn names_the_cli_would_misread_are_rejected() {
        for name in ["a b", "a\"b", "a'b", "", "a]b", "x\n[y"] {
            assert!(merge_profile_def("", name, &def()).is_err(), "{name:?}");
        }
        let mut bad = def();
        bad.profile.insert("sso_session".into(), "x\"y".into());
        assert!(merge_profile_def("", "yahagi-admin", &bad).is_err());
    }

    #[test]
    fn incomplete_definitions_are_rejected() {
        // Profile points at a session the bundle didn't carry.
        let mut no_session = def();
        no_session.sso_session = None;
        assert!(merge_profile_def("", "yahagi-admin", &no_session).is_err());
        // Session without a start URL.
        let mut no_url = def();
        no_url.sso_session.as_mut().unwrap().remove("sso_start_url");
        assert!(merge_profile_def("", "yahagi-admin", &no_url).is_err());
        // Not SSO at all.
        let mut no_role = def();
        no_role.profile.remove("sso_role_name");
        assert!(merge_profile_def("", "yahagi-admin", &no_role).is_err());
    }

    #[test]
    fn keys_outside_the_whitelist_are_dropped() {
        let mut evil = def();
        evil.profile
            .insert("credential_process".into(), "/bin/sh -c id".into());
        evil.sso_session
            .as_mut()
            .unwrap()
            .insert("credential_process".into(), "/bin/sh".into());
        let (_, text) = merge_profile_def("", "yahagi-admin", &evil).unwrap();
        assert!(!text.unwrap().contains("credential_process"));
    }

    #[test]
    fn several_profiles_are_written_once_with_the_pre_sync_backup() {
        let path = scratch_config("batch");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let original = "[profile malme]\nregion = ap-northeast-1\n";
        std::fs::write(&path, original).unwrap();
        let mut other = def();
        other
            .profile
            .insert("sso_role_name".into(), "ReadOnly".into());
        let mut bad = def();
        bad.sso_session = None;
        let results = ensure_profile_defs_at(
            &path,
            &[
                ("yahagi-admin".to_string(), def(), None),
                ("broken".to_string(), bad, None),
                ("yahagi-ro".to_string(), other, None),
            ],
        )
        .unwrap();
        assert_eq!(results[0], Ok(ProfileMerge::Added));
        assert!(results[1].is_err());
        assert_eq!(results[2], Ok(ProfileMerge::Added));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("[profile yahagi-admin]") && text.contains("[profile yahagi-ro]"));
        assert!(!text.contains("broken"));
        assert_eq!(text.matches("[sso-session yahagi]").count(), 1);
        // The backup is the state before this sync, not after the first profile.
        assert_eq!(
            std::fs::read_to_string(path.with_file_name("config.devdy.bak")).unwrap(),
            original
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_config_is_written_through() {
        let path = scratch_config("symlink");
        let aws = path.parent().unwrap();
        std::fs::create_dir_all(aws).unwrap();
        let real = aws.join("dotfiles-config");
        std::fs::write(&real, "[profile malme]\nregion = x\n").unwrap();
        std::os::unix::fs::symlink(&real, &path).unwrap();
        assert_eq!(one(&path), ProfileMerge::Added);
        assert!(std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(std::fs::read_to_string(&real)
            .unwrap()
            .contains("[profile yahagi-admin]"));
        assert!(path.with_file_name("config.devdy.bak").exists());
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn write_aborts_when_the_file_changed_underneath() {
        let path = scratch_config("race");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[profile malme]\n").unwrap();
        // Merge was based on different content than what is on disk now.
        assert_eq!(write_config(&path, "stale", "new"), Ok(false));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[profile malme]\n");
        let tmp_left = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!tmp_left);
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    #[test]
    fn indentation_follows_configparser() {
        // An indented header after a same-level key is still a header to the CLI.
        let target =
            "[profile q]\n  region = x\n  [profile yahagi-admin]\n  sso_session = yahagi\n";
        let names: Vec<_> = parse_config(target).into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["q", "yahagi-admin"]);
        assert!(merge_profile_def(target, "yahagi-admin", &def())
            .unwrap()
            .1
            .is_none());
        // Same-indented keys are all keys; deeper lines continue the value.
        let target = "[profile p]\n  region = x\n  output = json\n  s3 =\n    max = 1\n";
        let kv = &parse_config(target)[0].kv;
        assert_eq!(kv.get("output").map(String::as_str), Some("json"));
        assert!(!kv.contains_key("max"));
    }

    #[test]
    fn names_starting_with_a_dash_are_rejected() {
        assert!(merge_profile_def("", "-p", &def()).is_err());
        let mut bad = def();
        bad.profile.insert("sso_session".into(), "-s".into());
        assert!(merge_profile_def("", "yahagi-admin", &bad).is_err());
    }

    // Access-key profiles.

    const CREDS: &str = "\
[malme]
aws_access_key_id = AKIAEXAMPLEMALME0001
aws_secret_access_key = s3cr3t/Malme+Key
[temp]
aws_access_key_id = ASIAEXAMPLETEMP00001
aws_secret_access_key = x
aws_session_token = tok
";

    fn keys() -> AwsStaticKeys {
        static_keys_from_text(CREDS, "malme").expect("keys")
    }

    fn static_def() -> AwsProfileDef {
        static_def_from_text(SOURCE, "malme").expect("static profile")
    }

    #[test]
    fn reads_static_keys_and_config_side() {
        assert_eq!(keys().access_key_id, "AKIAEXAMPLEMALME0001");
        // Session tokens expire: not synced.
        assert!(static_keys_from_text(CREDS, "temp").is_none());
        assert!(static_keys_from_text(CREDS, "nope").is_none());
        let d = static_def();
        assert!(!is_sso(&d));
        assert_eq!(d.profile.get("region").unwrap(), "ap-northeast-1");
        // A profile only in credentials has an empty config side.
        assert!(static_def_from_text("", "cfk").unwrap().profile.is_empty());
        // SSO / role / process profiles are not access-key profiles.
        assert!(static_def_from_text(SOURCE, "yahagi-admin").is_none());
        for key in ["role_arn", "source_profile", "credential_process"] {
            let text = format!("[profile r]\n{key} = x\n");
            assert!(static_def_from_text(&text, "r").is_none(), "{key}");
        }
    }

    #[test]
    fn credentials_are_appended_never_overwritten() {
        let (outcome, text) =
            merge_credentials("[other]\naws_access_key_id = A\n", "malme", &keys()).unwrap();
        assert_eq!(outcome, ProfileMerge::Added);
        let text = text.unwrap();
        assert!(
            text.starts_with("[other]\naws_access_key_id = A\n# Added by Devdy sync\n[malme]\n")
        );
        assert!(static_keys_from_text(&text, "malme") == Some(keys()));

        let (outcome, text) = merge_credentials(CREDS, "malme", &keys()).unwrap();
        assert_eq!(outcome, ProfileMerge::Unchanged);
        assert!(text.is_none());

        let mut other = keys();
        other.secret_access_key = "different".into();
        let (outcome, text) = merge_credentials(CREDS, "malme", &other).unwrap();
        assert_eq!(outcome, ProfileMerge::Conflict("credentials malme".into()));
        assert!(text.is_none());
    }

    #[test]
    fn invalid_keys_are_rejected_without_echoing_them() {
        for (id, secret) in [
            ("AKIA\nEVIL", "Zq9SecretMark1"),
            ("short", "Zq9SecretMark2"),
            ("AKIAEXAMPLEMALME0001", "Zq9 SecretMark3"),
            ("AKIAEXAMPLEMALME0001", "Zq9SecretMark4\n[evil]"),
            ("AKIAEXAMPLEMALME0001", ""),
        ] {
            let k = AwsStaticKeys {
                access_key_id: id.into(),
                secret_access_key: secret.into(),
            };
            let err = merge_credentials("", "malme", &k).err().expect("rejected");
            assert!(secret.is_empty() || !err.contains("Zq9"), "{err}");
            assert!(!err.contains(id), "{err}");
        }
        assert!(merge_credentials("", "default", &keys()).is_err());
    }

    #[test]
    fn static_profile_writes_both_files_privately() {
        let path = scratch_config("static");
        let aws = path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&aws).unwrap();
        std::fs::write(aws.join("credentials"), "[bimcim]\naws_access_key_id = B\n").unwrap();
        let results = ensure_profile_defs_at(
            &path,
            &[
                ("malme".to_string(), static_def(), Some(keys())),
                (
                    "cfk".to_string(),
                    static_def_from_text("", "cfk").unwrap(),
                    Some(keys()),
                ),
                // Keys missing from the payload: nothing written for it.
                ("nokeys".to_string(), static_def(), None),
            ],
        )
        .unwrap();
        assert_eq!(results[0], Ok(ProfileMerge::Added));
        assert_eq!(results[1], Ok(ProfileMerge::Added));
        assert!(results[2].is_err());
        let config = std::fs::read_to_string(&path).unwrap();
        assert!(config.contains("[profile malme]\nregion = ap-northeast-1\n"));
        // Nothing to say about cfk in config.
        assert!(!config.contains("cfk") && !config.contains("nokeys"));
        let creds = std::fs::read_to_string(aws.join("credentials")).unwrap();
        assert!(creds.starts_with("[bimcim]\naws_access_key_id = B\n"));
        assert!(static_keys_from_text(&creds, "malme") == Some(keys()));
        assert!(static_keys_from_text(&creds, "cfk") == Some(keys()));
        assert!(!creds.contains("nokeys"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for f in ["credentials", "credentials.devdy.bak", "config"] {
                let mode = std::fs::metadata(aws.join(f)).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o600, "{f}");
            }
        }
        assert_eq!(
            std::fs::read_to_string(aws.join("credentials.devdy.bak")).unwrap(),
            "[bimcim]\naws_access_key_id = B\n"
        );
        // Re-import is a no-op.
        let again =
            ensure_profile_defs_at(&path, &[("malme".to_string(), static_def(), Some(keys()))])
                .unwrap();
        assert_eq!(again[0], Ok(ProfileMerge::Unchanged));
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn a_conflict_in_either_file_writes_neither() {
        let path = scratch_config("static-conflict");
        let aws = path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&aws).unwrap();
        // Credentials differ → the config entry is not added either.
        std::fs::write(
            aws.join("credentials"),
            "[malme]\naws_access_key_id = OTHER\n",
        )
        .unwrap();
        let r = ensure_profile_defs_at(&path, &[("malme".to_string(), static_def(), Some(keys()))])
            .unwrap();
        assert_eq!(r[0], Ok(ProfileMerge::Conflict("credentials malme".into())));
        assert!(!path.exists());
        // Config differs (an SSO profile of that name) → keys are not written.
        let _ = std::fs::remove_file(aws.join("credentials"));
        std::fs::write(
            &path,
            SOURCE.replace("yahagi-admin", "malme2").replace(
                "[profile malme]\nregion = ap-northeast-1\n",
                "[profile malme]\nregion = us-east-1\n",
            ),
        )
        .unwrap();
        let r = ensure_profile_defs_at(&path, &[("malme".to_string(), static_def(), Some(keys()))])
            .unwrap();
        assert_eq!(r[0], Ok(ProfileMerge::Conflict("profile malme".into())));
        assert!(!aws.join("credentials").exists());
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn sso_profiles_ignore_any_carried_keys() {
        let path = scratch_config("sso-keys");
        let r = ensure_profile_defs_at(&path, &[("yahagi-admin".to_string(), def(), Some(keys()))])
            .unwrap();
        assert_eq!(r[0], Ok(ProfileMerge::Added));
        assert!(!path.with_file_name("credentials").exists());
        let _ = std::fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }

    // Review round 3.

    fn scratch_aws(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let path = scratch_config(tag);
        let aws = path.parent().unwrap().to_path_buf();
        std::fs::create_dir_all(&aws).unwrap();
        (path, aws)
    }

    #[test]
    fn keys_never_give_an_existing_profile_a_second_auth() {
        // Bundle profile exists only in credentials (empty config side), target has
        // an SSO / process / role profile of that name: conflict, nothing written.
        for existing in [
            "[profile prod]\nsso_session = s\nsso_account_id = 1\nsso_role_name = r\n",
            "[profile prod]\nregion = ap-northeast-1\ncredential_process = /bin/get\n",
            "[profile prod]\nregion = ap-northeast-1\nrole_arn = arn:x\nsource_profile = base\n",
            "[profile prod]\naws_access_key_id = AKIAINLINE0000000000\n",
        ] {
            let (path, aws) = scratch_aws("second-auth");
            std::fs::write(&path, existing).unwrap();
            let empty = static_def_from_text("", "prod").unwrap();
            let regioned =
                static_def_from_text("[profile prod]\nregion = ap-northeast-1\n", "prod").unwrap();
            for def in [empty, regioned] {
                let r = ensure_profile_defs_at(&path, &[("prod".to_string(), def, Some(keys()))])
                    .unwrap();
                assert_eq!(
                    r[0],
                    Ok(ProfileMerge::Conflict("profile prod".into())),
                    "{existing}"
                );
            }
            assert!(!aws.join("credentials").exists(), "{existing}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), existing);
            let _ = std::fs::remove_dir_all(aws.parent().unwrap());
        }
    }

    #[test]
    fn sso_profile_never_shadows_existing_keys() {
        let (path, aws) = scratch_aws("shadow");
        std::fs::write(
            aws.join("credentials"),
            CREDS.replace("malme", "yahagi-admin"),
        )
        .unwrap();
        let r =
            ensure_profile_defs_at(&path, &[("yahagi-admin".to_string(), def(), None)]).unwrap();
        assert_eq!(
            r[0],
            Ok(ProfileMerge::Conflict("credentials yahagi-admin".into()))
        );
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn temporary_and_default_keys_are_not_exported() {
        let legacy = "[t]\naws_access_key_id = AKIAEXAMPLETEMP00002\naws_secret_access_key = x\naws_security_token = tok\n";
        assert!(static_keys_from_text(legacy, "t").is_none());
        let asia = "[t]\naws_access_key_id = ASIAEXAMPLETEMP00003\naws_secret_access_key = x\n";
        assert!(static_keys_from_text(asia, "t").is_none());
        assert!(static_def_from_text("[default]\nregion = x\n", "default").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn backup_replaces_a_planted_symlink_instead_of_following_it() {
        let (path, aws) = scratch_aws("bak-link");
        std::fs::write(aws.join("credentials"), "[bimcim]\naws_access_key_id = B\n").unwrap();
        let elsewhere = aws.parent().unwrap().join("stolen");
        std::fs::write(&elsewhere, "untouched").unwrap();
        std::os::unix::fs::symlink(&elsewhere, aws.join("credentials.devdy.bak")).unwrap();
        let r = ensure_profile_defs_at(&path, &[("malme".to_string(), static_def(), Some(keys()))])
            .unwrap();
        assert_eq!(r[0], Ok(ProfileMerge::Added));
        assert_eq!(std::fs::read_to_string(&elsewhere).unwrap(), "untouched");
        let bak = aws.join("credentials.devdy.bak");
        assert!(!std::fs::symlink_metadata(&bak)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&bak).unwrap(),
            "[bimcim]\naws_access_key_id = B\n"
        );
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn keys_written_but_config_failed_is_reported_as_such() {
        let (path, aws) = scratch_aws("partial");
        std::fs::write(&path, "[profile other]\nregion = x\n").unwrap();
        // The config backup can't be written (a non-empty directory sits there).
        std::fs::create_dir_all(aws.join("config.devdy.bak").join("x")).unwrap();
        let r = ensure_profile_defs_at(
            &path,
            &[
                ("malme".to_string(), static_def(), Some(keys())),
                ("yahagi-admin".to_string(), def(), None),
            ],
        )
        .unwrap();
        let err = r[0].clone().unwrap_err();
        assert!(
            err.starts_with("access keys written to ~/.aws/credentials"),
            "{err}"
        );
        assert!(!err.contains("s3cr3t"));
        assert!(r[1].is_err());
        assert!(
            static_keys_from_text(
                &std::fs::read_to_string(aws.join("credentials")).unwrap(),
                "malme"
            ) == Some(keys())
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "[profile other]\nregion = x\n"
        );
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn identical_sso_profile_with_same_named_keys_stays_unchanged() {
        let (path, aws) = scratch_aws("same-sso");
        std::fs::write(&path, SOURCE).unwrap();
        std::fs::write(
            aws.join("credentials"),
            CREDS.replace("malme", "yahagi-admin"),
        )
        .unwrap();
        let r =
            ensure_profile_defs_at(&path, &[("yahagi-admin".to_string(), def(), None)]).unwrap();
        assert_eq!(r[0], Ok(ProfileMerge::Unchanged));
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }

    #[test]
    fn partial_failure_keeps_other_items_outcomes() {
        let (path, aws) = scratch_aws("partial-mixed");
        std::fs::write(&path, "[profile malme2]\nregion = us-east-1\n").unwrap();
        std::fs::create_dir_all(aws.join("config.devdy.bak").join("x")).unwrap();
        let r = ensure_profile_defs_at(
            &path,
            &[
                ("malme".to_string(), static_def(), Some(keys())),
                ("malme2".to_string(), static_def(), Some(keys())),
            ],
        )
        .unwrap();
        assert!(r[0].clone().unwrap_err().starts_with("access keys written"));
        assert_eq!(r[1], Ok(ProfileMerge::Conflict("profile malme2".into())));
        let _ = std::fs::remove_dir_all(aws.parent().unwrap());
    }
}
