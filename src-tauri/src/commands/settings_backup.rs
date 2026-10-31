//! Export / import of the whole Devdy configuration as one file, to carry a setup to another
//! machine.
//!
//! The file is JSON (`*.devdy-backup`). It holds the portable app settings, the Skills / Rules
//! library (file contents included) with their groups, MCP servers, VPS servers, account metadata
//! (GitHub, GitLab, AWS, Azure, Google, Claude), notes / todos and Slack threads (messages and
//! translations; attachments are re-downloaded) and projects with their repos, enabled
//! Skills / Rules / groups / MCP / VPS servers, account links. Sessions and
//! usage history stay on the machine.
//!
//! Secrets (PATs, MCP env/header VALUEs, Google OAuth client + refresh tokens, SSH passphrases and
//! Devdy-managed private key files) are only exported when the user gives a password: they are
//! sealed with XChaCha20-Poly1305 under an Argon2id key derived from it. Without a password the
//! file carries no secret at all and the accounts must be re-authenticated after import.
//!
//! Import merges by name: an item whose name/label exists locally is overwritten with the
//! imported version, a new one is created, and nothing local is ever deleted. Notes and todos
//! match by id, Slack threads by channel + thread ts, projects by path then name (paths under the
//! exporting machine's home are moved under this one's; a missing project folder is created only
//! when the user allows it). Library files replaced by an import are moved to the legacy backups
//! first.

use crate::commands::groups::{set_group_members_inner, GroupKind};
use crate::commands::mcp::McpServer;
use crate::commands::settings::PORTABLE_SETTING_KEYS;
use crate::commands::{mcp, rules, skills, vps_servers};
use crate::db::Db;
use crate::secrets::{self, GoogleClientCreds, McpSecrets};
use crate::slack_threads::{store as slack_store, SlackThreadsState};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt, Key,
    Nonce,
};
use dryoc::classic::crypto_pwhash::{crypto_pwhash, PasswordHashAlgorithm};
use dryoc::rng::copy_randombytes;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

const FORMAT: &str = "devdy-settings-backup";
const VERSION: u32 = 1;
/// Bound into the AEAD tag so a sealed blob can't be replayed into another format/version.
const AAD: &[u8] = b"devdy-settings-backup:v1";
const KDF_NAME: &str = "argon2id13";
const KDF_OPSLIMIT: u64 = 3;
const KDF_MEMLIMIT: usize = 64 * 1024 * 1024;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
const MIN_PASSWORD_LEN: usize = 8;

/// Outcome of upserting one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upsert {
    Created,
    Updated,
    Unchanged,
}

// ---- File format -------------------------------------------------------------

#[derive(Serialize, Deserialize)]
struct Bundle {
    format: String,
    version: u32,
    exported_at: String,
    #[serde(default)]
    app_version: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
    #[serde(default)]
    skills: Vec<SkillEntry>,
    #[serde(default)]
    rules: Vec<RuleEntry>,
    #[serde(default)]
    skill_groups: Vec<GroupEntry>,
    #[serde(default)]
    rule_groups: Vec<GroupEntry>,
    #[serde(default)]
    mcp_servers: Vec<McpServer>,
    #[serde(default)]
    vps_servers: Vec<VpsEntry>,
    #[serde(default)]
    github_accounts: Vec<GithubEntry>,
    #[serde(default)]
    gitlab_accounts: Vec<GitlabEntry>,
    #[serde(default)]
    aws_accounts: Vec<AwsEntry>,
    #[serde(default)]
    azure_accounts: Vec<AzureEntry>,
    #[serde(default)]
    google_accounts: Vec<GoogleEntry>,
    #[serde(default)]
    claude_accounts: Vec<ClaudeEntry>,
    #[serde(default)]
    notes: Vec<NoteEntry>,
    #[serde(default)]
    todos: Vec<TodoEntry>,
    #[serde(default)]
    slack_threads: Vec<SlackThreadEntry>,
    #[serde(default)]
    slack_users: Vec<SlackUserEntry>,
    #[serde(default)]
    slack_channels: Vec<SlackChannelEntry>,
    #[serde(default)]
    projects: Vec<ProjectEntry>,
    /// `$HOME` of the exporting machine: project paths under it are moved under this machine's.
    #[serde(default)]
    home_dir: Option<String>,
    #[serde(default)]
    secrets: Option<SealedSecrets>,
}

/// A project with its repos and everything it is linked to. Links travel by NAME / LABEL (ids
/// differ between machines); sessions and usage history stay on the machine.
#[derive(Serialize, Deserialize)]
struct ProjectEntry {
    id: String,
    name: String,
    path: String,
    #[serde(default)]
    github_owner: Option<String>,
    #[serde(default)]
    github_repo: Option<String>,
    default_engine: String,
    created_at: String,
    #[serde(default)]
    position: i64,
    #[serde(default)]
    github_project_board_url: Option<String>,
    #[serde(default)]
    github_project_field_mappings: Option<String>,
    #[serde(default)]
    github_account: Option<String>,
    #[serde(default)]
    github_reviewer_account: Option<String>,
    #[serde(default)]
    gitlab_account: Option<String>,
    #[serde(default)]
    aws_account: Option<String>,
    #[serde(default)]
    azure_account: Option<String>,
    #[serde(default)]
    claude_account: Option<String>,
    #[serde(default)]
    repos: Vec<RepoEntry>,
    #[serde(default)]
    skills: Vec<LinkedItem>,
    #[serde(default)]
    rules: Vec<LinkedItem>,
    #[serde(default)]
    skill_groups: Vec<String>,
    #[serde(default)]
    rule_groups: Vec<String>,
    #[serde(default)]
    mcp_servers: Vec<String>,
    #[serde(default)]
    servers: Vec<ProjectServerEntry>,
}

#[derive(Serialize, Deserialize)]
struct RepoEntry {
    name: String,
    path: String,
    #[serde(default)]
    github_owner: Option<String>,
    #[serde(default)]
    github_repo: Option<String>,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    gitlab_project_path: Option<String>,
    #[serde(default)]
    gitlab_project_id: Option<i64>,
    created_at: String,
}

/// A skill / rule switched on for the project; `manual = false` when a group brought it in.
#[derive(Serialize, Deserialize)]
struct LinkedItem {
    name: String,
    #[serde(default)]
    manual: bool,
}

#[derive(Serialize, Deserialize)]
struct ProjectServerEntry {
    /// VPS server label.
    server: String,
    role: String,
}

#[derive(Serialize, Deserialize)]
struct SkillEntry {
    id: String,
    name: String,
    description: String,
    target: String,
    files: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize)]
struct FileEntry {
    /// `/`-separated path relative to the skill folder.
    path: String,
    /// Base64 file contents.
    data: String,
}

#[derive(Serialize, Deserialize)]
struct RuleEntry {
    id: String,
    name: String,
    description: String,
    target: String,
    content: String,
}

#[derive(Serialize, Deserialize)]
struct GroupEntry {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color: Option<String>,
    /// Member skill / rule NAMES (ids differ between machines).
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct VpsEntry {
    id: String,
    label: String,
    host: String,
    port: i64,
    username: String,
    auth_method: String,
    /// A user path such as `~/.ssh/id_ed25519`; `None` for a Devdy-managed key.
    #[serde(default)]
    private_key_path: Option<String>,
    /// The key was copied into Devdy's key store; its contents travel in the sealed secrets.
    #[serde(default)]
    managed_key: bool,
    #[serde(default)]
    tags: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct GithubEntry {
    id: String,
    label: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    scopes: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct GitlabEntry {
    id: String,
    label: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    scopes: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct AwsEntry {
    id: String,
    label: String,
    auth_method: String,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    arn: Option<String>,
    region: String,
    #[serde(default)]
    profile_name: Option<String>,
    #[serde(default)]
    tags: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct AzureEntry {
    id: String,
    label: String,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    subscription_id: Option<String>,
    #[serde(default)]
    subscription_name: Option<String>,
    #[serde(default)]
    tags: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct GoogleEntry {
    id: String,
    label: String,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    is_default: bool,
}

#[derive(Serialize, Deserialize)]
struct ClaudeEntry {
    id: String,
    label: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct ProjectRef {
    name: String,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct NoteEntry {
    id: String,
    title: String,
    content: String,
    position: i64,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    project: Option<ProjectRef>,
}

#[derive(Serialize, Deserialize)]
struct TodoEntry {
    id: String,
    text: String,
    done: bool,
    position: i64,
    created_at: String,
    #[serde(default)]
    due_date: Option<String>,
    #[serde(default)]
    project: Option<ProjectRef>,
    /// Focus columns (`TODO_FOCUS_COLS`), nulls left out.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    focus: serde_json::Map<String, serde_json::Value>,
}

/// Focus columns of `todos` (migration 0067) carried by a backup.
const TODO_FOCUS_COLS: [&str; 16] = [
    "due_time",
    "priority",
    "estimate_min",
    "estimate_source",
    "status",
    "snoozed_until",
    "remind_at",
    "source_type",
    "source_ref",
    "source_url",
    "started_at",
    "completed_at",
    "actual_min",
    "ai_plan",
    "ai_analyzed_at",
    "updated_at",
];

/// `json_object(...)` of the Focus columns of todo row `t`.
fn todo_focus_sql() -> String {
    let args: Vec<String> = TODO_FOCUS_COLS
        .iter()
        .map(|c| format!("'{c}', t.{c}"))
        .collect();
    format!("json_object({})", args.join(", "))
}

fn todo_focus_of(json: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut m: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(json).unwrap_or_default();
    m.retain(|_, v| !v.is_null());
    m
}

/// A fetched Slack thread with its messages (original + Vietnamese) and file metadata. File
/// contents are not carried: an imported thread is re-fetched, which downloads them again.
#[derive(Serialize, Deserialize)]
struct SlackThreadEntry {
    id: String,
    title: String,
    #[serde(default)]
    title_custom: bool,
    position: i64,
    created_at: String,
    updated_at: String,
    #[serde(default)]
    workspace_domain: Option<String>,
    #[serde(default)]
    team_id: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
    #[serde(default)]
    channel_id: Option<String>,
    #[serde(default)]
    channel: Option<String>,
    #[serde(default)]
    thread_ts: Option<String>,
    #[serde(default)]
    thread_url: Option<String>,
    #[serde(default)]
    fetched_at: Option<String>,
    #[serde(default)]
    reply_count: i64,
    #[serde(default)]
    last_reply_ts: Option<String>,
    #[serde(default)]
    last_seen_ts: Option<String>,
    #[serde(default)]
    search_text: String,
    #[serde(default)]
    legacy: bool,
    #[serde(default)]
    legacy_content: Option<String>,
    #[serde(default)]
    project: Option<ProjectRef>,
    #[serde(default)]
    messages: Vec<SlackMessageEntry>,
    #[serde(default)]
    files: Vec<SlackFileEntry>,
}

#[derive(Serialize, Deserialize)]
struct SlackMessageEntry {
    ts: String,
    seq: i64,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    bot_id: Option<String>,
    #[serde(default)]
    author_name: Option<String>,
    #[serde(default)]
    author_icon_url: Option<String>,
    #[serde(default)]
    subtype: Option<String>,
    text_raw: String,
    #[serde(default)]
    text_vi: Option<String>,
    src_hash: String,
    translate_status: String,
    #[serde(default)]
    edited_ts: Option<String>,
    /// JSON, as stored.
    #[serde(default)]
    reactions: String,
    /// JSON array of the EXPORTED `files[].id`.
    #[serde(default)]
    file_ids: String,
}

#[derive(Serialize, Deserialize)]
struct SlackFileEntry {
    id: String,
    slack_file_id: String,
    #[serde(default)]
    message_ts: Option<String>,
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    mime: Option<String>,
    #[serde(default)]
    filetype: Option<String>,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    remote_updated: Option<i64>,
    #[serde(default)]
    permalink: Option<String>,
    #[serde(default)]
    width: Option<i64>,
    #[serde(default)]
    height: Option<i64>,
}

/// Cached Slack user names, so imported messages show authors before the first re-fetch.
#[derive(Serialize, Deserialize)]
struct SlackUserEntry {
    team_id: String,
    user_id: String,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    real_name: Option<String>,
    #[serde(default)]
    avatar_url: Option<String>,
    #[serde(default)]
    is_bot: bool,
}

#[derive(Serialize, Deserialize)]
struct SlackChannelEntry {
    team_id: String,
    channel_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct SealedSecrets {
    kdf: String,
    opslimit: u64,
    memlimit: usize,
    salt: String,
    nonce: String,
    ciphertext: String,
}

/// Plaintext of the sealed secrets, keyed by the EXPORTED item ids. No `Debug`: it holds secrets.
#[derive(Default, Serialize, Deserialize)]
struct SecretPayload {
    #[serde(default)]
    github: BTreeMap<String, String>,
    #[serde(default)]
    gitlab: BTreeMap<String, String>,
    #[serde(default)]
    mcp: BTreeMap<String, McpSecrets>,
    #[serde(default)]
    servers: BTreeMap<String, ServerSecretEntry>,
    #[serde(default)]
    google_client: Option<GoogleClientCreds>,
    #[serde(default)]
    google_accounts: BTreeMap<String, String>,
    /// The secret notes vault as the Keychain holds it: already sealed under the vault
    /// password, so it needs that password too once imported.
    #[serde(default)]
    secret_vault: Option<crate::secret_vault::VaultData>,
}

#[derive(Default, Serialize, Deserialize)]
struct ServerSecretEntry {
    #[serde(default)]
    passphrase: Option<String>,
    #[serde(default)]
    key_file_name: Option<String>,
    /// Base64 contents of a Devdy-managed private key file.
    #[serde(default)]
    key_data: Option<String>,
}

// ---- Wire types for the UI ---------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub exported_at: String,
    pub app_version: String,
    pub has_secrets: bool,
    /// Item count per section (`skills`, `rules`, `githubAccounts`…).
    pub counts: BTreeMap<String, usize>,
}

#[derive(Debug, Serialize, Default, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct Counts {
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub counts: BTreeMap<String, Counts>,
    pub secrets_imported: bool,
    /// Per-item problems; the rest of the import still went through.
    pub warnings: Vec<String>,
}

impl ImportReport {
    fn tally(&mut self, section: &str, outcome: Upsert) {
        let c = self.counts.entry(section.to_string()).or_default();
        match outcome {
            Upsert::Created => c.created += 1,
            Upsert::Updated => c.updated += 1,
            Upsert::Unchanged => c.unchanged += 1,
        }
    }

    fn record(&mut self, section: &str, label: &str, result: Result<Upsert, String>) {
        match result {
            Ok(outcome) => self.tally(section, outcome),
            Err(e) => self.warnings.push(format!("{section} '{label}': {e}")),
        }
    }
}

impl Bundle {
    fn info(&self) -> BackupInfo {
        let counts = [
            ("settings", self.settings.len()),
            ("skills", self.skills.len()),
            ("rules", self.rules.len()),
            ("skillGroups", self.skill_groups.len()),
            ("ruleGroups", self.rule_groups.len()),
            ("mcpServers", self.mcp_servers.len()),
            ("vpsServers", self.vps_servers.len()),
            ("githubAccounts", self.github_accounts.len()),
            ("gitlabAccounts", self.gitlab_accounts.len()),
            ("awsAccounts", self.aws_accounts.len()),
            ("azureAccounts", self.azure_accounts.len()),
            ("googleAccounts", self.google_accounts.len()),
            ("claudeAccounts", self.claude_accounts.len()),
            ("notes", self.notes.len()),
            ("todos", self.todos.len()),
            ("slackThreads", self.slack_threads.len()),
            ("projects", self.projects.len()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
        BackupInfo {
            exported_at: self.exported_at.clone(),
            app_version: self.app_version.clone(),
            has_secrets: self.secrets.is_some(),
            counts,
        }
    }
}

// ---- Crypto ------------------------------------------------------------------

fn derive_key(password: &str, salt: &[u8], opslimit: u64, memlimit: usize) -> Result<Key, String> {
    let mut key: Key = [0u8; 32];
    crypto_pwhash(
        &mut key,
        password.as_bytes(),
        salt,
        opslimit,
        memlimit,
        PasswordHashAlgorithm::Argon2id13,
    )
    .map_err(|_| "Could not derive the encryption key".to_string())?;
    Ok(key)
}

fn seal(payload: &SecretPayload, password: &str) -> Result<SealedSecrets, String> {
    let plain = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let mut salt = [0u8; SALT_LEN];
    copy_randombytes(&mut salt);
    let mut nonce: Nonce = [0u8; NONCE_LEN];
    copy_randombytes(&mut nonce);
    let key = derive_key(password, &salt, KDF_OPSLIMIT, KDF_MEMLIMIT)?;
    let mut cipher = vec![0u8; plain.len() + TAG_LEN];
    crypto_aead_xchacha20poly1305_ietf_encrypt(&mut cipher, &plain, Some(AAD), &nonce, &key)
        .map_err(|_| "Could not encrypt the secrets".to_string())?;
    Ok(SealedSecrets {
        kdf: KDF_NAME.to_string(),
        opslimit: KDF_OPSLIMIT,
        memlimit: KDF_MEMLIMIT,
        salt: B64.encode(salt),
        nonce: B64.encode(nonce),
        ciphertext: B64.encode(cipher),
    })
}

fn open_sealed(sealed: &SealedSecrets, password: &str) -> Result<SecretPayload, String> {
    // The parameters come from the file: bound them so a crafted file can't make us burn
    // gigabytes of RAM or minutes of CPU.
    if sealed.kdf != KDF_NAME
        || !(1..=10).contains(&sealed.opslimit)
        || !(8 * 1024..=1024 * 1024 * 1024).contains(&sealed.memlimit)
    {
        return Err("Unsupported encryption parameters in the backup file".to_string());
    }
    let salt = B64
        .decode(&sealed.salt)
        .map_err(|_| "Corrupted backup file (salt)".to_string())?;
    let nonce: Nonce = B64
        .decode(&sealed.nonce)
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(|| "Corrupted backup file (nonce)".to_string())?;
    let cipher = B64
        .decode(&sealed.ciphertext)
        .map_err(|_| "Corrupted backup file (ciphertext)".to_string())?;
    if cipher.len() < TAG_LEN || salt.len() < 8 {
        return Err("Corrupted backup file".to_string());
    }
    let key = derive_key(password, &salt, sealed.opslimit, sealed.memlimit)?;
    let mut plain = vec![0u8; cipher.len() - TAG_LEN];
    crypto_aead_xchacha20poly1305_ietf_decrypt(&mut plain, &cipher, Some(AAD), &nonce, &key)
        .map_err(|_| "Wrong password, or the backup file is corrupted".to_string())?;
    serde_json::from_slice(&plain).map_err(|_| "Corrupted secrets in the backup file".to_string())
}

// ---- Helpers -----------------------------------------------------------------

/// A relative path made only of normal components (no `..`, no root) — so an imported skill can
/// never write outside its own folder.
fn safe_rel_path(raw: &str) -> Option<PathBuf> {
    let path = Path::new(raw);
    if raw.is_empty() || path.is_absolute() {
        return None;
    }
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn rel_path_string(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// The exported id when it is still free locally, a fresh one otherwise. Keeping ids stable lets
/// a later round-trip (A → B → A) line up with the same rows. `table` is always a literal.
async fn free_id(db: &Db, table: &str, hint: &str) -> String {
    if hint.is_empty() {
        return Uuid::new_v4().to_string();
    }
    let taken = sqlx::query(&format!("SELECT 1 FROM {table} WHERE id = ?"))
        .bind(hint)
        .fetch_optional(db)
        .await
        .map(|r| r.is_some())
        .unwrap_or(true);
    if taken {
        Uuid::new_v4().to_string()
    } else {
        hint.to_string()
    }
}

async fn id_by(db: &Db, table: &str, column: &str, value: &str) -> Result<Option<String>, String> {
    sqlx::query_scalar::<_, String>(&format!(
        "SELECT id FROM {table} WHERE {column} = ? LIMIT 1"
    ))
    .bind(value)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())
}

/// Find the local project an exported note/todo belonged to: same path first, then same name.
async fn resolve_project(db: &Db, project: &Option<ProjectRef>) -> Option<String> {
    let project = project.as_ref()?;
    if let Ok(Some(id)) = id_by(db, "projects", "path", &project.path).await {
        return Some(id);
    }
    id_by(db, "projects", "name", &project.name)
        .await
        .ok()
        .flatten()
}

fn write_private_file(path: &str, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|e| format!("{path}: {e}"))?;
    file.write_all(data).map_err(|e| format!("{path}: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn read_bundle(path: &str) -> Result<Bundle, String> {
    let raw = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let bundle: Bundle =
        serde_json::from_slice(&raw).map_err(|_| "Not a Devdy settings backup file".to_string())?;
    check_bundle(&bundle)?;
    Ok(bundle)
}

fn check_bundle(bundle: &Bundle) -> Result<(), String> {
    if bundle.format != FORMAT {
        return Err("Not a Devdy settings backup file".to_string());
    }
    if bundle.version > VERSION {
        return Err(format!(
            "This backup was made by a newer Devdy (format v{}); update Devdy first",
            bundle.version
        ));
    }
    Ok(())
}

// ---- Export ------------------------------------------------------------------

async fn collect(
    app: &AppHandle,
    db: &Db,
    with_secrets: bool,
) -> Result<(Bundle, SecretPayload), String> {
    let mut secret = SecretPayload::default();
    let err = |e: sqlx::Error| e.to_string();

    let mut settings = BTreeMap::new();
    for row in sqlx::query("SELECT key, value FROM settings")
        .fetch_all(db)
        .await
        .map_err(err)?
    {
        let key: String = row.get("key");
        if PORTABLE_SETTING_KEYS.contains(&key.as_str()) {
            settings.insert(key, row.get("value"));
        }
    }

    let mut skill_entries = Vec::new();
    for row in
        sqlx::query("SELECT id, name, description, target, source_path FROM skills ORDER BY name")
            .fetch_all(db)
            .await
            .map_err(err)?
    {
        let source: String = row.get("source_path");
        let files = skills::read_skill_tree(Path::new(&source))?
            .into_iter()
            .map(|(rel, data)| FileEntry {
                path: rel_path_string(&rel),
                data: B64.encode(data),
            })
            .collect();
        skill_entries.push(SkillEntry {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            target: row.get("target"),
            files,
        });
    }

    let mut rule_entries = Vec::new();
    for row in
        sqlx::query("SELECT id, name, description, target, source_path FROM rules ORDER BY name")
            .fetch_all(db)
            .await
            .map_err(err)?
    {
        let source: String = row.get("source_path");
        let name: String = row.get("name");
        let content =
            std::fs::read_to_string(&source).map_err(|e| format!("rule '{name}': {e}"))?;
        rule_entries.push(RuleEntry {
            id: row.get("id"),
            name,
            description: row.get("description"),
            target: row.get("target"),
            content,
        });
    }

    let skill_groups = collect_groups(
        db,
        "skill_groups",
        "skill_group_members",
        "skill_id",
        "skills",
    )
    .await?;
    let rule_groups =
        collect_groups(db, "rule_groups", "rule_group_members", "rule_id", "rules").await?;

    let mut mcp_servers = Vec::new();
    for id in sqlx::query_scalar::<_, String>("SELECT id FROM mcp_servers ORDER BY name")
        .fetch_all(db)
        .await
        .map_err(err)?
    {
        let server = mcp::get_mcp_server(app.state::<Db>(), id.clone()).await?;
        if with_secrets {
            let s = secrets::get_mcp_secrets(&id);
            if !s.env.is_empty() || !s.headers.is_empty() {
                secret.mcp.insert(id, s);
            }
        }
        mcp_servers.push(server);
    }

    let mut vps_entries = Vec::new();
    for row in sqlx::query(
        "SELECT id, label, host, port, username, auth_method, private_key_path, tags FROM servers ORDER BY label",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        let key_path: Option<String> = row.get("private_key_path");
        let managed = key_path
            .as_deref()
            .is_some_and(|p| vps_servers::is_managed_key_path(app, p));
        if with_secrets {
            let mut entry = ServerSecretEntry {
                passphrase: secrets::get_server_secret(&id).passphrase,
                ..Default::default()
            };
            if let Some(path) = key_path.as_deref().filter(|_| managed) {
                if let Ok(data) = std::fs::read(path) {
                    // Strip the "<uuid>-" prefix Devdy adds when it copies a key in.
                    let file = Path::new(path).file_name().map(|f| f.to_string_lossy().into_owned());
                    entry.key_file_name =
                        file.map(|f| f.get(37..).filter(|s| !s.is_empty()).unwrap_or(f.as_str()).to_string());
                    entry.key_data = Some(B64.encode(data));
                }
            }
            if entry.passphrase.is_some() || entry.key_data.is_some() {
                secret.servers.insert(id.clone(), entry);
            }
        }
        vps_entries.push(VpsEntry {
            id,
            label: row.get("label"),
            host: row.get("host"),
            port: row.get("port"),
            username: row.get("username"),
            auth_method: row.get("auth_method"),
            private_key_path: if managed { None } else { key_path },
            managed_key: managed,
            tags: row.get("tags"),
        });
    }

    let mut github_accounts = Vec::new();
    for row in sqlx::query("SELECT id, label, username, scopes FROM github_accounts ORDER BY label")
        .fetch_all(db)
        .await
        .map_err(err)?
    {
        let id: String = row.get("id");
        if with_secrets {
            if let Ok(pat) = secrets::get_account_pat(&id) {
                secret.github.insert(id.clone(), pat);
            }
        }
        github_accounts.push(GithubEntry {
            id,
            label: row.get("label"),
            username: row.get("username"),
            scopes: row.get("scopes"),
        });
    }

    let mut gitlab_accounts = Vec::new();
    for row in sqlx::query(
        "SELECT id, label, username, host, email, scopes FROM gitlab_accounts ORDER BY label",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        if with_secrets {
            if let Ok(pat) = secrets::get_gitlab_account_pat(&id) {
                secret.gitlab.insert(id.clone(), pat);
            }
        }
        gitlab_accounts.push(GitlabEntry {
            id,
            label: row.get("label"),
            username: row.get("username"),
            host: row.get("host"),
            email: row.get("email"),
            scopes: row.get("scopes"),
        });
    }

    let aws_accounts = sqlx::query(
        "SELECT id, label, auth_method, account_id, arn, region, profile_name, tags FROM aws_accounts ORDER BY label",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| AwsEntry {
        id: row.get("id"),
        label: row.get("label"),
        auth_method: row.get("auth_method"),
        account_id: row.get("account_id"),
        arn: row.get("arn"),
        region: row.get("region"),
        profile_name: row.get("profile_name"),
        tags: row.get("tags"),
    })
    .collect();

    let azure_accounts = sqlx::query(
        "SELECT id, label, tenant, subscription_id, subscription_name, tags FROM azure_accounts ORDER BY label",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| AzureEntry {
        id: row.get("id"),
        label: row.get("label"),
        tenant: row.get("tenant"),
        subscription_id: row.get("subscription_id"),
        subscription_name: row.get("subscription_name"),
        tags: row.get("tags"),
    })
    .collect();

    let mut google_accounts = Vec::new();
    for row in sqlx::query(
        "SELECT id, label, email, scope, is_default FROM google_accounts ORDER BY label",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        if with_secrets {
            if let Some(token) = secrets::get_google_account_token(&id) {
                secret.google_accounts.insert(id.clone(), token);
            }
        }
        google_accounts.push(GoogleEntry {
            id,
            label: row.get("label"),
            email: row.get("email"),
            scope: row.get("scope"),
            is_default: row.get::<i64, _>("is_default") != 0,
        });
    }
    if with_secrets {
        secret.google_client = secrets::get_google_client();
        secret.secret_vault = secrets::with_vault(false, |v| Ok(v.clone()))
            .ok()
            .filter(|v| v.header.is_some() || !v.old_headers.is_empty());
    }

    let claude_accounts = sqlx::query("SELECT id, label FROM claude_accounts ORDER BY label")
        .fetch_all(db)
        .await
        .map_err(err)?
        .iter()
        .map(|row| ClaudeEntry {
            id: row.get("id"),
            label: row.get("label"),
        })
        .collect();

    let project_of = |row: &sqlx::sqlite::SqliteRow| -> Option<ProjectRef> {
        let name: Option<String> = row.get("project_name");
        let path: Option<String> = row.get("project_path");
        Some(ProjectRef {
            name: name?,
            path: path?,
        })
    };
    let notes = sqlx::query(
        "SELECT n.id, n.title, n.content, n.position, n.created_at, n.updated_at, \
                p.name AS project_name, p.path AS project_path \
         FROM notes n LEFT JOIN projects p ON p.id = n.project_id ORDER BY n.position",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| NoteEntry {
        id: row.get("id"),
        title: row.get("title"),
        content: row.get("content"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        project: project_of(row),
    })
    .collect();
    let todos = sqlx::query(&format!(
        "SELECT t.id, t.text, t.done, t.position, t.created_at, t.due_date, \
                {} AS focus_json, p.name AS project_name, p.path AS project_path \
         FROM todos t LEFT JOIN projects p ON p.id = t.project_id ORDER BY t.position",
        todo_focus_sql()
    ))
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| TodoEntry {
        id: row.get("id"),
        text: row.get("text"),
        done: row.get::<i64, _>("done") != 0,
        position: row.get("position"),
        created_at: row.get("created_at"),
        due_date: row.get("due_date"),
        project: project_of(row),
        focus: todo_focus_of(row.get("focus_json")),
    })
    .collect();

    let (slack_threads, slack_users, slack_channels) = collect_slack(db).await?;
    let projects = collect_projects(db).await?;

    let bundle = Bundle {
        format: FORMAT.to_string(),
        version: VERSION,
        exported_at: chrono::Utc::now().to_rfc3339(),
        app_version: app.package_info().version.to_string(),
        settings,
        skills: skill_entries,
        rules: rule_entries,
        skill_groups,
        rule_groups,
        mcp_servers,
        vps_servers: vps_entries,
        github_accounts,
        gitlab_accounts,
        aws_accounts,
        azure_accounts,
        google_accounts,
        claude_accounts,
        notes,
        todos,
        slack_threads,
        slack_users,
        slack_channels,
        projects,
        home_dir: std::env::var("HOME").ok().filter(|h| !h.is_empty()),
        secrets: None,
    };
    Ok((bundle, secret))
}

async fn collect_projects(db: &Db) -> Result<Vec<ProjectEntry>, String> {
    let err = |e: sqlx::Error| e.to_string();
    let mut out = Vec::new();
    for row in sqlx::query(
        "SELECT p.id, p.name, p.path, p.github_owner, p.github_repo, p.default_engine, p.created_at, \
                p.position, p.github_project_board_url, p.github_project_field_mappings, \
                gh.label AS github_account, ghr.label AS github_reviewer_account, \
                gl.label AS gitlab_account, aws.label AS aws_account, az.label AS azure_account, \
                cl.label AS claude_account \
         FROM projects p \
         LEFT JOIN github_accounts gh ON gh.id = p.github_account_id \
         LEFT JOIN github_accounts ghr ON ghr.id = p.github_reviewer_account_id \
         LEFT JOIN gitlab_accounts gl ON gl.id = p.gitlab_account_id \
         LEFT JOIN aws_accounts aws ON aws.id = p.aws_account_id \
         LEFT JOIN azure_accounts az ON az.id = p.azure_account_id \
         LEFT JOIN claude_accounts cl ON cl.id = p.claude_account_id \
         ORDER BY p.position, p.name",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        let repos = sqlx::query(
            "SELECT name, path, github_owner, github_repo, provider, gitlab_project_path, \
                    gitlab_project_id, created_at FROM repos WHERE project_id = ? ORDER BY created_at",
        )
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(err)?
        .iter()
        .map(|r| RepoEntry {
            name: r.get("name"),
            path: r.get("path"),
            github_owner: r.get("github_owner"),
            github_repo: r.get("github_repo"),
            provider: r.get("provider"),
            gitlab_project_path: r.get("gitlab_project_path"),
            gitlab_project_id: r.get("gitlab_project_id"),
            created_at: r.get("created_at"),
        })
        .collect();
        let linked = |sql: &'static str| {
            let id = id.clone();
            async move {
                sqlx::query(sql)
                    .bind(&id)
                    .fetch_all(db)
                    .await
                    .map(|rows| {
                        rows.iter()
                            .map(|r| LinkedItem {
                                name: r.get("name"),
                                manual: r.get::<i64, _>("manual") != 0,
                            })
                            .collect::<Vec<_>>()
                    })
                    .map_err(|e| e.to_string())
            }
        };
        let skills = linked(
            "SELECT s.name, ps.manual FROM project_skills ps JOIN skills s ON s.id = ps.skill_id \
             WHERE ps.project_id = ? ORDER BY s.name",
        )
        .await?;
        let rules = linked(
            "SELECT r.name, pr.manual FROM project_rules pr JOIN rules r ON r.id = pr.rule_id \
             WHERE pr.project_id = ? ORDER BY r.name",
        )
        .await?;
        let names = |sql: &'static str| {
            let id = id.clone();
            async move {
                sqlx::query_scalar::<_, String>(sql)
                    .bind(&id)
                    .fetch_all(db)
                    .await
                    .map_err(|e| e.to_string())
            }
        };
        let skill_groups = names(
            "SELECT g.name FROM project_skill_groups pg JOIN skill_groups g ON g.id = pg.group_id \
             WHERE pg.project_id = ? ORDER BY g.name",
        )
        .await?;
        let rule_groups = names(
            "SELECT g.name FROM project_rule_groups pg JOIN rule_groups g ON g.id = pg.group_id \
             WHERE pg.project_id = ? ORDER BY g.name",
        )
        .await?;
        let mcp_servers = names(
            "SELECT m.name FROM project_mcp_servers pm JOIN mcp_servers m ON m.id = pm.server_id \
             WHERE pm.project_id = ? ORDER BY m.name",
        )
        .await?;
        let servers = sqlx::query(
            "SELECT s.label, ps.role FROM project_servers ps JOIN servers s ON s.id = ps.server_id \
             WHERE ps.project_id = ? ORDER BY ps.role, s.label",
        )
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(err)?
        .iter()
        .map(|r| ProjectServerEntry {
            server: r.get("label"),
            role: r.get("role"),
        })
        .collect();
        out.push(ProjectEntry {
            id,
            name: row.get("name"),
            path: row.get("path"),
            github_owner: row.get("github_owner"),
            github_repo: row.get("github_repo"),
            default_engine: row.get("default_engine"),
            created_at: row.get("created_at"),
            position: row.get("position"),
            github_project_board_url: row.get("github_project_board_url"),
            github_project_field_mappings: row.get("github_project_field_mappings"),
            github_account: row.get("github_account"),
            github_reviewer_account: row.get("github_reviewer_account"),
            gitlab_account: row.get("gitlab_account"),
            aws_account: row.get("aws_account"),
            azure_account: row.get("azure_account"),
            claude_account: row.get("claude_account"),
            repos,
            skills,
            rules,
            skill_groups,
            rule_groups,
            mcp_servers,
            servers,
        });
    }
    Ok(out)
}

async fn collect_slack(
    db: &Db,
) -> Result<
    (
        Vec<SlackThreadEntry>,
        Vec<SlackUserEntry>,
        Vec<SlackChannelEntry>,
    ),
    String,
> {
    let err = |e: sqlx::Error| e.to_string();
    let mut threads = Vec::new();
    for row in sqlx::query(
        "SELECT t.id, t.title, t.title_custom, t.position, t.created_at, t.updated_at, \
                t.workspace_domain, t.team_id, t.workspace, t.channel_id, t.channel, t.thread_ts, \
                t.thread_url, t.fetched_at, t.reply_count, t.last_reply_ts, t.last_seen_ts, \
                t.search_text, t.legacy, t.legacy_content, \
                p.name AS project_name, p.path AS project_path \
         FROM slack_threads t LEFT JOIN projects p ON p.id = t.project_id ORDER BY t.position",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    {
        let id: String = row.get("id");
        let messages = sqlx::query(
            "SELECT ts, seq, user_id, bot_id, author_name, author_icon_url, subtype, text_raw, \
                    text_vi, src_hash, translate_status, edited_ts, reactions, file_ids \
             FROM slack_messages WHERE thread_id = ? ORDER BY seq",
        )
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(err)?
        .iter()
        .map(|m| SlackMessageEntry {
            ts: m.get("ts"),
            seq: m.get("seq"),
            user_id: m.get("user_id"),
            bot_id: m.get("bot_id"),
            author_name: m.get("author_name"),
            author_icon_url: m.get("author_icon_url"),
            subtype: m.get("subtype"),
            text_raw: m.get("text_raw"),
            text_vi: m.get("text_vi"),
            src_hash: m.get("src_hash"),
            translate_status: m.get("translate_status"),
            edited_ts: m.get("edited_ts"),
            reactions: m.get("reactions"),
            file_ids: m.get("file_ids"),
        })
        .collect();
        // Legacy zip files (no Slack file id) can't be fetched again, so they are left out.
        let files = sqlx::query(
            "SELECT id, slack_file_id, message_ts, name, title, mime, filetype, size, \
                    remote_updated, permalink, width, height \
             FROM slack_files WHERE thread_id = ? AND slack_file_id IS NOT NULL ORDER BY created_at",
        )
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(err)?
        .iter()
        .map(|f| SlackFileEntry {
            id: f.get("id"),
            slack_file_id: f.get("slack_file_id"),
            message_ts: f.get("message_ts"),
            name: f.get("name"),
            title: f.get("title"),
            mime: f.get("mime"),
            filetype: f.get("filetype"),
            size: f.get("size"),
            remote_updated: f.get("remote_updated"),
            permalink: f.get("permalink"),
            width: f.get("width"),
            height: f.get("height"),
        })
        .collect();
        let name: Option<String> = row.get("project_name");
        let path: Option<String> = row.get("project_path");
        threads.push(SlackThreadEntry {
            id,
            title: row.get("title"),
            title_custom: row.get::<i64, _>("title_custom") != 0,
            position: row.get("position"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            workspace_domain: row.get("workspace_domain"),
            team_id: row.get("team_id"),
            workspace: row.get("workspace"),
            channel_id: row.get("channel_id"),
            channel: row.get("channel"),
            thread_ts: row.get("thread_ts"),
            thread_url: row.get("thread_url"),
            fetched_at: row.get("fetched_at"),
            reply_count: row.get("reply_count"),
            last_reply_ts: row.get("last_reply_ts"),
            last_seen_ts: row.get("last_seen_ts"),
            search_text: row.get("search_text"),
            legacy: row.get::<i64, _>("legacy") != 0,
            legacy_content: row.get("legacy_content"),
            project: name.zip(path).map(|(name, path)| ProjectRef { name, path }),
            messages,
            files,
        });
    }

    let users = sqlx::query(
        "SELECT team_id, user_id, display_name, real_name, avatar_url, is_bot FROM slack_users \
         ORDER BY team_id, user_id",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| SlackUserEntry {
        team_id: row.get("team_id"),
        user_id: row.get("user_id"),
        display_name: row.get("display_name"),
        real_name: row.get("real_name"),
        avatar_url: row.get("avatar_url"),
        is_bot: row.get::<i64, _>("is_bot") != 0,
    })
    .collect();
    let channels = sqlx::query(
        "SELECT team_id, channel_id, name, kind FROM slack_channels ORDER BY team_id, channel_id",
    )
    .fetch_all(db)
    .await
    .map_err(err)?
    .iter()
    .map(|row| SlackChannelEntry {
        team_id: row.get("team_id"),
        channel_id: row.get("channel_id"),
        name: row.get("name"),
        kind: row.get("kind"),
    })
    .collect();
    Ok((threads, users, channels))
}

/// Table / column names are literals from the callers, never user input.
async fn collect_groups(
    db: &Db,
    groups_table: &str,
    members_table: &str,
    item_col: &str,
    items_table: &str,
) -> Result<Vec<GroupEntry>, String> {
    let mut out = Vec::new();
    for row in sqlx::query(&format!(
        "SELECT id, name, description, color FROM {groups_table} ORDER BY position, name"
    ))
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?
    {
        let id: String = row.get("id");
        let members = sqlx::query_scalar::<_, String>(&format!(
            "SELECT i.name FROM {members_table} m JOIN {items_table} i ON i.id = m.{item_col} \
             WHERE m.group_id = ? ORDER BY m.position"
        ))
        .bind(&id)
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;
        out.push(GroupEntry {
            name: row.get("name"),
            description: row.get("description"),
            color: row.get("color"),
            members,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn export_settings_backup(
    app: AppHandle,
    webview: tauri::Webview,
    db: State<'_, Db>,
    path: String,
    password: Option<String>,
) -> Result<BackupInfo, String> {
    let password = password.filter(|p| !p.is_empty());
    if password
        .as_ref()
        .is_some_and(|p| p.chars().count() < MIN_PASSWORD_LEN)
    {
        return Err(format!(
            "The password must be at least {MIN_PASSWORD_LEN} characters"
        ));
    }
    let (mut bundle, mut payload) = collect(&app, db.inner(), password.is_some()).await?;
    if from_remote(&webview) {
        payload.secret_vault = None;
    }
    if let Some(password) = password {
        let sealed = tokio::task::spawn_blocking(move || seal(&payload, &password))
            .await
            .map_err(|e| e.to_string())??;
        bundle.secrets = Some(sealed);
    }
    let json = serde_json::to_vec_pretty(&bundle).map_err(|e| e.to_string())?;
    write_private_file(&path, &json)?;
    Ok(bundle.info())
}

/// Called by a Remote Control controller: the secret notes vault stays out of
/// it (its sealed data would allow offline guessing of the vault password, and
/// an import could overwrite notes).
fn from_remote(webview: &tauri::Webview) -> bool {
    webview.label() == crate::remote::ipc::BRIDGE_LABEL
}

// ---- Import ------------------------------------------------------------------

#[tauri::command]
pub async fn inspect_settings_backup(path: String) -> Result<BackupInfo, String> {
    Ok(read_bundle(&path)?.info())
}

#[tauri::command]
pub async fn import_settings_backup(
    app: AppHandle,
    webview: tauri::Webview,
    db: State<'_, Db>,
    path: String,
    password: Option<String>,
    create_missing_dirs: Option<bool>,
) -> Result<ImportReport, String> {
    let bundle = read_bundle(&path)?;
    let password = password.filter(|p| !p.is_empty());
    let mut report = ImportReport::default();

    // Decrypt first: a wrong password must stop the import before anything is written.
    let secret = match (&bundle.secrets, password) {
        (Some(sealed), Some(password)) => {
            let sealed = sealed.clone();
            let payload = tokio::task::spawn_blocking(move || open_sealed(&sealed, &password))
                .await
                .map_err(|e| e.to_string())??;
            report.secrets_imported = true;
            let mut payload = payload;
            if from_remote(&webview) {
                payload.secret_vault = None;
            }
            Some(payload)
        }
        (Some(_), None) => {
            report.warnings.push(
                "Secrets were skipped (no password given): re-enter tokens and log in again"
                    .to_string(),
            );
            None
        }
        (None, _) => None,
    };
    Ok(apply_bundle(
        &app,
        db.inner(),
        bundle,
        secret.unwrap_or_default(),
        create_missing_dirs.unwrap_or(false),
        report,
    )
    .await)
}

// ---- Device transfer -----------------------------------------------------------

/// A backup carried over an end-to-end encrypted device channel. The channel itself is the
/// protection, so the secrets travel as a plain payload inside it: no password, no Argon2.
#[allow(dead_code)] // reused by two-way sync bootstrap
#[derive(Serialize, Deserialize)]
struct Transfer {
    bundle: Bundle,
    #[serde(default)]
    secrets: Option<SecretPayload>,
}

/// A transfer received from another machine, parsed and waiting for the user to import it.
#[allow(dead_code)] // reused by two-way sync bootstrap
pub struct ReceivedBackup(Transfer);

#[allow(dead_code)] // reused by two-way sync bootstrap
impl ReceivedBackup {
    pub fn parse(raw: &[u8]) -> Result<Self, String> {
        let transfer: Transfer = serde_json::from_slice(raw)
            .map_err(|_| "The other device sent unreadable data".to_string())?;
        check_bundle(&transfer.bundle)?;
        Ok(Self(transfer))
    }

    pub fn info(&self) -> BackupInfo {
        let mut info = self.0.bundle.info();
        info.has_secrets = self.0.secrets.is_some();
        info
    }
}

/// The whole configuration of this machine, serialized for a device transfer.
#[allow(dead_code)] // reused by two-way sync bootstrap
pub async fn export_transfer(
    app: &AppHandle,
    db: &Db,
    with_secrets: bool,
) -> Result<Vec<u8>, String> {
    let (bundle, payload) = collect(app, db, with_secrets).await?;
    let transfer = Transfer {
        bundle,
        secrets: with_secrets.then_some(payload),
    };
    serde_json::to_vec(&transfer).map_err(|e| e.to_string())
}

#[allow(dead_code)] // reused by two-way sync bootstrap
pub async fn import_received(
    app: &AppHandle,
    db: &Db,
    received: ReceivedBackup,
    create_missing_dirs: bool,
) -> ImportReport {
    let Transfer { bundle, secrets } = received.0;
    let report = ImportReport {
        secrets_imported: secrets.is_some(),
        ..Default::default()
    };
    apply_bundle(
        app,
        db,
        bundle,
        secrets.unwrap_or_default(),
        create_missing_dirs,
        report,
    )
    .await
}

/// Merge `bundle` into this machine (shared by the file import and the device transfer).
async fn apply_bundle(
    app: &AppHandle,
    db: &Db,
    mut bundle: Bundle,
    secret: SecretPayload,
    create_missing_dirs: bool,
    mut report: ImportReport,
) -> ImportReport {
    remap_bundle_paths(&mut bundle, std::env::var("HOME").ok().as_deref());

    import_settings(db, &bundle, &mut report).await;

    for entry in &bundle.skills {
        let result = import_skill(&app, db, entry).await;
        record_with_sync(&mut report, "skills", &entry.name, result);
    }
    for entry in &bundle.rules {
        let result = rules::import_backup_rule(
            &app,
            db,
            &entry.id,
            &entry.name,
            &entry.description,
            &entry.target,
            &entry.content,
        )
        .await;
        record_with_sync(&mut report, "rules", &entry.name, result);
    }
    for entry in &bundle.skill_groups {
        let result = import_group(db, GroupKind::Skill, entry).await;
        report.record("skillGroups", &entry.name, result);
    }
    for entry in &bundle.rule_groups {
        let result = import_group(db, GroupKind::Rule, entry).await;
        report.record("ruleGroups", &entry.name, result);
    }
    for server in &bundle.mcp_servers {
        let result =
            mcp::import_backup_mcp(db, &server.id, server, secret.mcp.get(&server.id)).await;
        report.record("mcpServers", &server.name, result);
    }
    for entry in &bundle.vps_servers {
        let result = import_vps(&app, db, entry, secret.servers.get(&entry.id)).await;
        report.record("vpsServers", &entry.label, result);
    }
    for entry in &bundle.github_accounts {
        let result = import_github(db, entry, secret.github.get(&entry.id)).await;
        report.record("githubAccounts", &entry.label, result);
    }
    for entry in &bundle.gitlab_accounts {
        let result = import_gitlab(db, entry, secret.gitlab.get(&entry.id)).await;
        report.record("gitlabAccounts", &entry.label, result);
    }
    for entry in &bundle.aws_accounts {
        let result = import_aws(db, entry).await;
        report.record("awsAccounts", &entry.label, result);
    }
    for entry in &bundle.azure_accounts {
        let result = import_azure(&app, db, entry).await;
        report.record("azureAccounts", &entry.label, result);
    }
    if let Some(vault) = secret.secret_vault.clone() {
        match secrets::with_vault(true, |local| Ok(crate::secret_vault::import(local, vault))) {
            Ok(n) => report.counts.entry("secretNotes".to_string()).or_default().created += n,
            Err(e) => report.warnings.push(format!("Secret notes: {e}")),
        }
    }
    if let Some(client) = &secret.google_client {
        let same = secrets::get_google_client().is_some_and(|c| {
            c.client_id == client.client_id && c.client_secret == client.client_secret
        });
        if !same {
            if let Err(e) = secrets::set_google_client(&client.client_id, &client.client_secret) {
                report.warnings.push(format!("Google OAuth client: {e}"));
            }
        }
    }
    for entry in &bundle.google_accounts {
        let result = import_google(db, entry, secret.google_accounts.get(&entry.id)).await;
        report.record("googleAccounts", &entry.label, result);
    }
    for entry in &bundle.claude_accounts {
        let result = import_claude(&app, db, entry).await;
        report.record("claudeAccounts", &entry.label, result);
    }
    // Projects before notes / todos / threads so those can re-link to them.
    let app_data = app.path().app_data_dir().ok();
    let mut project_ids: Vec<(usize, String)> = Vec::new();
    for (index, entry) in bundle.projects.iter().enumerate() {
        match import_project(db, entry, create_missing_dirs).await {
            Ok((outcome, id, warnings)) => {
                report.tally("projects", outcome);
                for warning in warnings {
                    report
                        .warnings
                        .push(format!("projects '{}': {warning}", entry.name));
                }
                if let Some(dir) = &app_data {
                    crate::aws_sandbox::write_project_aws_config(db, dir, &id).await;
                }
                project_ids.push((index, id));
            }
            Err(e) => report
                .warnings
                .push(format!("projects '{}': {e}", entry.name)),
        }
    }

    for entry in &bundle.notes {
        let result = import_note(db, entry).await;
        report.record("notes", &entry.title, result);
    }
    for entry in &bundle.todos {
        let result = import_todo(db, entry).await;
        report.record("todos", &entry.text, result);
    }

    if !bundle.slack_threads.is_empty() {
        import_slack_caches(db, &bundle, &mut report).await;
        let mut created = Vec::new();
        for entry in &bundle.slack_threads {
            let result = import_slack_thread(db, entry).await.map(|(outcome, id)| {
                if outcome == Upsert::Created {
                    created.push(id);
                }
                outcome
            });
            report.record("slackThreads", &entry.title, result);
        }
        // New threads are `pending`: the fetch job pulls them again (files, new replies) and
        // keeps the imported translations of unchanged messages.
        if let Some(state) = app.try_state::<SlackThreadsState>() {
            for id in &created {
                state.enqueue(id, false);
            }
        }
        crate::slack_threads::emit_all_changed(&app);
    }

    report
}

/// `path` moved from the exporting machine's home to this one's (`/Users/a/x` → `/Users/b/x`).
fn remap_home(path: &str, from: Option<&str>, to: Option<&str>) -> String {
    let (Some(from), Some(to)) = (from, to) else {
        return path.to_string();
    };
    let from = from.trim_end_matches('/');
    let to = to.trim_end_matches('/');
    if from.is_empty() || to.is_empty() || from == to {
        return path.to_string();
    }
    match path.strip_prefix(from) {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{to}{rest}"),
        _ => path.to_string(),
    }
}

/// Rewrite every project path of the bundle for this machine's home directory.
fn remap_bundle_paths(bundle: &mut Bundle, local_home: Option<&str>) {
    let from = bundle.home_dir.clone();
    let remap = |p: &mut String| *p = remap_home(p, from.as_deref(), local_home);
    for project in &mut bundle.projects {
        remap(&mut project.path);
        for repo in &mut project.repos {
            remap(&mut repo.path);
        }
    }
    let refs = bundle
        .notes
        .iter_mut()
        .filter_map(|n| n.project.as_mut())
        .chain(bundle.todos.iter_mut().filter_map(|t| t.project.as_mut()))
        .chain(
            bundle
                .slack_threads
                .iter_mut()
                .filter_map(|t| t.project.as_mut()),
        );
    for project in refs {
        remap(&mut project.path);
    }
}

/// Record a skill/rule upsert, surfacing per-project sync failures as warnings.
fn record_with_sync(
    report: &mut ImportReport,
    section: &str,
    name: &str,
    result: Result<(Upsert, Vec<String>), String>,
) {
    match result {
        Ok((outcome, failures)) => {
            report.tally(section, outcome);
            for failure in failures {
                report
                    .warnings
                    .push(format!("{section} '{name}': sync failed for {failure}"));
            }
        }
        Err(e) => report.warnings.push(format!("{section} '{name}': {e}")),
    }
}

async fn import_settings(db: &Db, bundle: &Bundle, report: &mut ImportReport) {
    for (key, value) in &bundle.settings {
        if !PORTABLE_SETTING_KEYS.contains(&key.as_str()) {
            continue;
        }
        let current: Option<String> =
            sqlx::query_scalar("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(db)
                .await
                .ok()
                .flatten();
        if current.as_ref() == Some(value) {
            report.tally("settings", Upsert::Unchanged);
            continue;
        }
        let result = sqlx::query("INSERT OR REPLACE INTO settings (key, value) VALUES (?, ?)")
            .bind(key)
            .bind(value)
            .execute(db)
            .await
            .map(|_| {
                if current.is_some() {
                    Upsert::Updated
                } else {
                    Upsert::Created
                }
            })
            .map_err(|e| e.to_string());
        report.record("settings", key, result);
    }
}

async fn import_skill(
    app: &AppHandle,
    db: &Db,
    entry: &SkillEntry,
) -> Result<(Upsert, Vec<String>), String> {
    let mut files = Vec::with_capacity(entry.files.len());
    for file in &entry.files {
        let rel =
            safe_rel_path(&file.path).ok_or_else(|| format!("unsafe file path '{}'", file.path))?;
        let data = B64
            .decode(&file.data)
            .map_err(|_| format!("corrupted file '{}'", file.path))?;
        files.push((rel, data));
    }
    if !files.iter().any(|(p, _)| p == Path::new("SKILL.md")) {
        return Err("SKILL.md is missing".to_string());
    }
    skills::import_backup_skill(
        app,
        db,
        &entry.id,
        &entry.name,
        &entry.description,
        &entry.target,
        &files,
    )
    .await
}

async fn import_group(db: &Db, kind: GroupKind, entry: &GroupEntry) -> Result<Upsert, String> {
    let (groups_table, members_table, item_col, items_table) = match kind {
        GroupKind::Skill => ("skill_groups", "skill_group_members", "skill_id", "skills"),
        GroupKind::Rule => ("rule_groups", "rule_group_members", "rule_id", "rules"),
    };
    let name = entry.name.trim();
    if name.is_empty() {
        return Err("empty group name".to_string());
    }
    let now = chrono::Utc::now().to_rfc3339();
    let (group_id, mut outcome) = match id_by(db, groups_table, "name", name).await? {
        Some(id) => {
            let changed = sqlx::query(&format!(
                "UPDATE {groups_table} SET description = ?, color = ?, updated_at = ? \
                 WHERE id = ? AND (description IS NOT ? OR color IS NOT ?)"
            ))
            .bind(&entry.description)
            .bind(&entry.color)
            .bind(&now)
            .bind(&id)
            .bind(&entry.description)
            .bind(&entry.color)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            (
                id,
                if changed {
                    Upsert::Updated
                } else {
                    Upsert::Unchanged
                },
            )
        }
        None => {
            let id = Uuid::new_v4().to_string();
            let position: i64 = sqlx::query_scalar(&format!(
                "SELECT COALESCE(MAX(position), -1) + 1 FROM {groups_table}"
            ))
            .fetch_one(db)
            .await
            .map_err(|e| e.to_string())?;
            sqlx::query(&format!(
                "INSERT INTO {groups_table} (id, name, description, color, position, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)"
            ))
            .bind(&id)
            .bind(name)
            .bind(&entry.description)
            .bind(&entry.color)
            .bind(position)
            .bind(&now)
            .bind(&now)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            (id, Upsert::Created)
        }
    };

    // Members are merged (current ∪ imported), never dropped: dropping one would also pull it out
    // of every project that enabled the group.
    let mut members = sqlx::query_scalar::<_, String>(&format!(
        "SELECT {item_col} FROM {members_table} WHERE group_id = ? ORDER BY position"
    ))
    .bind(&group_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let before = members.len();
    for member in &entry.members {
        if let Some(item_id) = id_by(db, items_table, "name", member).await? {
            if !members.contains(&item_id) {
                members.push(item_id);
            }
        }
    }
    if members.len() != before {
        let applied = set_group_members_inner(db, kind, &group_id, members).await?;
        if !applied.failures.is_empty() {
            let names: Vec<String> = applied.failures.into_iter().map(|f| f.item_name).collect();
            return Err(format!("could not apply to projects: {}", names.join(", ")));
        }
        if outcome == Upsert::Unchanged {
            outcome = Upsert::Updated;
        }
    }
    Ok(outcome)
}

async fn import_vps(
    app: &AppHandle,
    db: &Db,
    entry: &VpsEntry,
    secret: Option<&ServerSecretEntry>,
) -> Result<Upsert, String> {
    if !matches!(entry.auth_method.as_str(), "agent" | "key") {
        return Err("unknown auth method".to_string());
    }
    let local = sqlx::query("SELECT id, private_key_path FROM servers WHERE label = ? LIMIT 1")
        .bind(&entry.label)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?;
    let (id, local_key, created) = match local {
        Some(row) => (
            row.get::<String, _>("id"),
            row.get::<Option<String>, _>("private_key_path"),
            false,
        ),
        None => (free_id(db, "servers", &entry.id).await, None, true),
    };

    // Managed key: restore it from the secrets, or keep whatever this machine already has.
    let key_data = secret.and_then(|s| s.key_data.as_deref());
    let key_path = if entry.auth_method != "key" {
        None
    } else if entry.managed_key {
        match key_data {
            Some(data) => {
                let data = B64
                    .decode(data)
                    .map_err(|_| "corrupted private key".to_string())?;
                let same = local_key
                    .as_deref()
                    .and_then(|p| std::fs::read(p).ok())
                    .is_some_and(|current| current == data);
                if same {
                    local_key.clone()
                } else {
                    let file = secret
                        .and_then(|s| s.key_file_name.as_deref())
                        .unwrap_or("id_key");
                    Some(vps_servers::restore_managed_key(app, &id, file, &data)?)
                }
            }
            None => local_key.clone(),
        }
    } else {
        entry.private_key_path.clone()
    };

    let mut outcome = if created {
        sqlx::query(
            "INSERT INTO servers (id, label, host, port, username, auth_method, private_key_path, \
             tags, status, last_checked_at, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL, NULL, ?)",
        )
        .bind(&id)
        .bind(&entry.label)
        .bind(&entry.host)
        .bind(entry.port)
        .bind(&entry.username)
        .bind(&entry.auth_method)
        .bind(&key_path)
        .bind(&entry.tags)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
        Upsert::Created
    } else {
        let changed = sqlx::query(
            "UPDATE servers SET host = ?, port = ?, username = ?, auth_method = ?, private_key_path = ?, tags = ? \
             WHERE id = ? AND (host IS NOT ? OR port IS NOT ? OR username IS NOT ? OR auth_method IS NOT ? \
             OR private_key_path IS NOT ? OR tags IS NOT ?)",
        )
        .bind(&entry.host)
        .bind(entry.port)
        .bind(&entry.username)
        .bind(&entry.auth_method)
        .bind(&key_path)
        .bind(&entry.tags)
        .bind(&id)
        .bind(&entry.host)
        .bind(entry.port)
        .bind(&entry.username)
        .bind(&entry.auth_method)
        .bind(&key_path)
        .bind(&entry.tags)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected()
            > 0;
        if changed {
            Upsert::Updated
        } else {
            Upsert::Unchanged
        }
    };

    if let Some(pass) = secret
        .and_then(|s| s.passphrase.as_deref())
        .filter(|p| !p.is_empty())
    {
        if secrets::get_server_secret(&id).passphrase.as_deref() != Some(pass) {
            secrets::set_server_secret(&id, pass)
                .map_err(|e| format!("Failed to store passphrase: {e}"))?;
            if outcome == Upsert::Unchanged {
                outcome = Upsert::Updated;
            }
        }
    }
    Ok(outcome)
}

/// Store an imported token when it differs from the local one; bumps `Unchanged` to `Updated`.
fn apply_token(
    outcome: Upsert,
    token: Option<&String>,
    current: impl FnOnce() -> Option<String>,
    store: impl FnOnce(&str) -> anyhow::Result<()>,
) -> Result<Upsert, String> {
    // Only touch the Keychain when the file actually carries a token.
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Ok(outcome);
    };
    if current().as_ref() == Some(token) {
        return Ok(outcome);
    }
    store(token).map_err(|e| format!("Failed to store token: {e}"))?;
    Ok(if outcome == Upsert::Unchanged {
        Upsert::Updated
    } else {
        outcome
    })
}

async fn import_github(
    db: &Db,
    entry: &GithubEntry,
    pat: Option<&String>,
) -> Result<Upsert, String> {
    let (id, outcome) = match id_by(db, "github_accounts", "label", &entry.label).await? {
        Some(id) => {
            let changed = sqlx::query(
                "UPDATE github_accounts SET username = ?, scopes = ? \
                 WHERE id = ? AND (username IS NOT ? OR scopes IS NOT ?)",
            )
            .bind(&entry.username)
            .bind(&entry.scopes)
            .bind(&id)
            .bind(&entry.username)
            .bind(&entry.scopes)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            (
                id,
                if changed {
                    Upsert::Updated
                } else {
                    Upsert::Unchanged
                },
            )
        }
        None => {
            let id = free_id(db, "github_accounts", &entry.id).await;
            sqlx::query(
                "INSERT INTO github_accounts (id, label, username, scopes, created_at) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&entry.label)
            .bind(&entry.username)
            .bind(&entry.scopes)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            (id, Upsert::Created)
        }
    };
    apply_token(
        outcome,
        pat,
        || secrets::get_account_pat(&id).ok(),
        |t| secrets::set_account_pat(&id, t),
    )
}

async fn import_gitlab(
    db: &Db,
    entry: &GitlabEntry,
    pat: Option<&String>,
) -> Result<Upsert, String> {
    let (id, outcome) = match id_by(db, "gitlab_accounts", "label", &entry.label).await? {
        Some(id) => {
            let changed = sqlx::query(
                "UPDATE gitlab_accounts SET username = ?, host = ?, email = ?, scopes = ? \
                 WHERE id = ? AND (username IS NOT ? OR host IS NOT ? OR email IS NOT ? OR scopes IS NOT ?)",
            )
            .bind(&entry.username)
            .bind(&entry.host)
            .bind(&entry.email)
            .bind(&entry.scopes)
            .bind(&id)
            .bind(&entry.username)
            .bind(&entry.host)
            .bind(&entry.email)
            .bind(&entry.scopes)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            (
                id,
                if changed {
                    Upsert::Updated
                } else {
                    Upsert::Unchanged
                },
            )
        }
        None => {
            let id = free_id(db, "gitlab_accounts", &entry.id).await;
            sqlx::query(
                "INSERT INTO gitlab_accounts (id, label, username, host, email, scopes, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&entry.label)
            .bind(&entry.username)
            .bind(&entry.host)
            .bind(&entry.email)
            .bind(&entry.scopes)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            (id, Upsert::Created)
        }
    };
    apply_token(
        outcome,
        pat,
        || secrets::get_gitlab_account_pat(&id).ok(),
        |t| secrets::set_gitlab_account_pat(&id, t),
    )
}

async fn import_aws(db: &Db, entry: &AwsEntry) -> Result<Upsert, String> {
    match id_by(db, "aws_accounts", "label", &entry.label).await? {
        Some(id) => {
            let changed = sqlx::query(
                "UPDATE aws_accounts SET auth_method = ?, region = ?, profile_name = ?, tags = ? \
                 WHERE id = ? AND (auth_method IS NOT ? OR region IS NOT ? OR profile_name IS NOT ? OR tags IS NOT ?)",
            )
            .bind(&entry.auth_method)
            .bind(&entry.region)
            .bind(&entry.profile_name)
            .bind(&entry.tags)
            .bind(&id)
            .bind(&entry.auth_method)
            .bind(&entry.region)
            .bind(&entry.profile_name)
            .bind(&entry.tags)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            Ok(if changed {
                Upsert::Updated
            } else {
                Upsert::Unchanged
            })
        }
        None => {
            // Not validated on this machine yet: the profile must exist in its ~/.aws/config.
            sqlx::query(
                "INSERT INTO aws_accounts (id, label, auth_method, account_id, arn, region, access_key_id, \
                 profile_name, tags, last_validated_at, created_at) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?, NULL, ?)",
            )
            .bind(free_id(db, "aws_accounts", &entry.id).await)
            .bind(&entry.label)
            .bind(&entry.auth_method)
            .bind(&entry.account_id)
            .bind(&entry.arn)
            .bind(&entry.region)
            .bind(&entry.profile_name)
            .bind(&entry.tags)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            Ok(Upsert::Created)
        }
    }
}

async fn import_azure(app: &AppHandle, db: &Db, entry: &AzureEntry) -> Result<Upsert, String> {
    match id_by(db, "azure_accounts", "label", &entry.label).await? {
        Some(id) => {
            let changed = sqlx::query(
                "UPDATE azure_accounts SET tenant = ?, tags = ? WHERE id = ? AND (tenant IS NOT ? OR tags IS NOT ?)",
            )
            .bind(&entry.tenant)
            .bind(&entry.tags)
            .bind(&id)
            .bind(&entry.tenant)
            .bind(&entry.tags)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            Ok(if changed {
                Upsert::Updated
            } else {
                Upsert::Unchanged
            })
        }
        None => {
            // The az login session lives in a per-account config dir and is not portable: the
            // account arrives logged out, with its subscription pinned for the next login.
            let id = free_id(db, "azure_accounts", &entry.id).await;
            let root = app.path().app_data_dir().map_err(|e| e.to_string())?;
            let dir = crate::azure_sandbox::account_dir(&root, &id)
                .ok_or_else(|| "invalid account id".to_string())?;
            crate::azure_sandbox::create_account_dir(&dir)
                .map_err(|e| format!("Failed to create Azure config dir: {e}"))?;
            sqlx::query(
                "INSERT INTO azure_accounts (id, label, tenant, subscription_id, subscription_name, tags, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&entry.label)
            .bind(&entry.tenant)
            .bind(&entry.subscription_id)
            .bind(&entry.subscription_name)
            .bind(&entry.tags)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            Ok(Upsert::Created)
        }
    }
}

async fn import_google(
    db: &Db,
    entry: &GoogleEntry,
    token: Option<&String>,
) -> Result<Upsert, String> {
    let (id, outcome) = match id_by(db, "google_accounts", "label", &entry.label).await? {
        Some(id) => {
            let changed = sqlx::query(
                "UPDATE google_accounts SET email = ?, scope = ? WHERE id = ? AND (email IS NOT ? OR scope IS NOT ?)",
            )
            .bind(&entry.email)
            .bind(&entry.scope)
            .bind(&id)
            .bind(&entry.email)
            .bind(&entry.scope)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?
            .rows_affected()
                > 0;
            (
                id,
                if changed {
                    Upsert::Updated
                } else {
                    Upsert::Unchanged
                },
            )
        }
        None => {
            let id = free_id(db, "google_accounts", &entry.id).await;
            // Only take over "default" when this machine has none yet.
            let has_default: i64 =
                sqlx::query_scalar("SELECT COUNT(*) FROM google_accounts WHERE is_default = 1")
                    .fetch_one(db)
                    .await
                    .map_err(|e| e.to_string())?;
            sqlx::query(
                "INSERT INTO google_accounts (id, label, email, scope, is_default, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&entry.label)
            .bind(&entry.email)
            .bind(&entry.scope)
            .bind((entry.is_default && has_default == 0) as i64)
            .bind(chrono::Utc::now().to_rfc3339())
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
            (id, Upsert::Created)
        }
    };
    apply_token(
        outcome,
        token,
        || secrets::get_google_account_token(&id),
        |t| secrets::set_google_account_token(&id, t),
    )
}

async fn import_claude(app: &AppHandle, db: &Db, entry: &ClaudeEntry) -> Result<Upsert, String> {
    if id_by(db, "claude_accounts", "label", &entry.label)
        .await?
        .is_some()
    {
        return Ok(Upsert::Unchanged);
    }
    // Claude Code keeps the OAuth login in its own store for the config dir, so the account
    // arrives logged out (status unknown) and never as the default until it is logged in.
    let id = free_id(db, "claude_accounts", &entry.id).await;
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?
        .join("claude-accounts")
        .join(&id);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    sqlx::query(
        "INSERT INTO claude_accounts (id, label, config_dir, status, is_default, created_at) \
         VALUES (?, ?, ?, 'unknown', 0, ?)",
    )
    .bind(&id)
    .bind(&entry.label)
    .bind(dir.to_string_lossy().as_ref())
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(Upsert::Created)
}

async fn import_note(db: &Db, entry: &NoteEntry) -> Result<Upsert, String> {
    let project_id = resolve_project(db, &entry.project).await;
    let exists = id_by(db, "notes", "id", &entry.id).await?.is_some();
    if exists {
        // An unresolvable project keeps the note's current local project.
        let changed = sqlx::query(
            "UPDATE notes SET title = ?, content = ?, position = ?, updated_at = ?, \
             project_id = COALESCE(?, project_id) \
             WHERE id = ? AND (title IS NOT ? OR content IS NOT ? OR position IS NOT ? \
             OR (? IS NOT NULL AND project_id IS NOT ?))",
        )
        .bind(&entry.title)
        .bind(&entry.content)
        .bind(entry.position)
        .bind(&entry.updated_at)
        .bind(&project_id)
        .bind(&entry.id)
        .bind(&entry.title)
        .bind(&entry.content)
        .bind(entry.position)
        .bind(&project_id)
        .bind(&project_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected()
            > 0;
        return Ok(if changed {
            Upsert::Updated
        } else {
            Upsert::Unchanged
        });
    }
    sqlx::query(
        "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(&entry.title)
    .bind(&entry.content)
    .bind(&project_id)
    .bind(entry.position)
    .bind(&entry.created_at)
    .bind(&entry.updated_at)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(Upsert::Created)
}

/// Write the backup's Focus columns over todo `id` (missing keys = column
/// default / null). A backup from before Focus (no columns) leaves the row
/// alone. Returns whether anything changed.
async fn import_todo_focus(db: &Db, id: &str, focus: &serde_json::Map<String, serde_json::Value>) -> Result<bool, String> {
    if focus.is_empty() {
        return Ok(false);
    }
    let before: String = sqlx::query_scalar(&format!("SELECT {} FROM todos t WHERE t.id = ?", todo_focus_sql()))
        .bind(id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let mut want = focus.clone();
    want.retain(|k, v| TODO_FOCUS_COLS.contains(&k.as_str()) && !v.is_null());
    want.entry("priority").or_insert(2.into());
    want.entry("status").or_insert("todo".into());
    if todo_focus_of(&before) == want {
        return Ok(false);
    }
    let sets: Vec<String> = TODO_FOCUS_COLS.iter().map(|c| format!("{c} = ?")).collect();
    let sql = format!("UPDATE todos SET {} WHERE id = ?", sets.join(", "));
    let mut q = sqlx::query(&sql);
    for c in TODO_FOCUS_COLS {
        q = match want.get(c) {
            Some(serde_json::Value::Number(n)) => q.bind(n.as_i64()),
            Some(serde_json::Value::String(v)) => q.bind(v.clone()),
            Some(v) => q.bind(v.to_string()),
            None => q.bind(None::<String>),
        };
    }
    q.bind(id).execute(db).await.map_err(|e| e.to_string())?;
    Ok(true)
}

async fn import_todo(db: &Db, entry: &TodoEntry) -> Result<Upsert, String> {
    let project_id = resolve_project(db, &entry.project).await;
    let exists = id_by(db, "todos", "id", &entry.id).await?.is_some();
    if exists {
        let changed = sqlx::query(
            "UPDATE todos SET text = ?, done = ?, position = ?, due_date = ?, \
             project_id = COALESCE(?, project_id) \
             WHERE id = ? AND (text IS NOT ? OR done IS NOT ? OR position IS NOT ? \
             OR due_date IS NOT ? OR (? IS NOT NULL AND project_id IS NOT ?))",
        )
        .bind(&entry.text)
        .bind(entry.done as i64)
        .bind(entry.position)
        .bind(&entry.due_date)
        .bind(&project_id)
        .bind(&entry.id)
        .bind(&entry.text)
        .bind(entry.done as i64)
        .bind(entry.position)
        .bind(&entry.due_date)
        .bind(&project_id)
        .bind(&project_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?
        .rows_affected()
            > 0;
        let focus_changed = import_todo_focus(db, &entry.id, &entry.focus).await?;
        return Ok(if changed || focus_changed {
            Upsert::Updated
        } else {
            Upsert::Unchanged
        });
    }
    sqlx::query(
        "INSERT INTO todos (id, text, done, position, created_at, due_date, project_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(&entry.text)
    .bind(entry.done as i64)
    .bind(entry.position)
    .bind(&entry.created_at)
    .bind(&entry.due_date)
    .bind(&project_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    import_todo_focus(db, &entry.id, &entry.focus).await?;
    Ok(Upsert::Created)
}

/// Slack user / channel name caches; local rows win. Imported users are marked stale so the
/// next fetch refreshes them (and downloads their avatars).
async fn import_slack_caches(db: &Db, bundle: &Bundle, report: &mut ImportReport) {
    const STALE: &str = "1970-01-01T00:00:00Z";
    for user in &bundle.slack_users {
        if let Err(e) = sqlx::query(
            "INSERT INTO slack_users (team_id, user_id, display_name, real_name, avatar_url, \
             avatar_path, is_bot, fetched_at) VALUES (?, ?, ?, ?, ?, NULL, ?, ?) \
             ON CONFLICT(team_id, user_id) DO NOTHING",
        )
        .bind(&user.team_id)
        .bind(&user.user_id)
        .bind(&user.display_name)
        .bind(&user.real_name)
        .bind(&user.avatar_url)
        .bind(user.is_bot as i64)
        .bind(STALE)
        .execute(db)
        .await
        {
            report
                .warnings
                .push(format!("slackUsers '{}': {e}", user.user_id));
        }
    }
    for channel in &bundle.slack_channels {
        if let Err(e) = sqlx::query(
            "INSERT INTO slack_channels (team_id, channel_id, name, kind, fetched_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT(team_id, channel_id) DO NOTHING",
        )
        .bind(&channel.team_id)
        .bind(&channel.channel_id)
        .bind(&channel.name)
        .bind(&channel.kind)
        .bind(STALE)
        .execute(db)
        .await
        {
            report
                .warnings
                .push(format!("slackChannels '{}': {e}", channel.channel_id));
        }
    }
}

/// Match by Slack key (channel + thread ts), else by id. An existing thread keeps its own
/// messages (the local fetch is newer); it only gains the project link, a custom title and
/// translations it is missing. A new thread is inserted `pending` with its file rows
/// undownloaded. Returns the local thread id.
async fn import_slack_thread(
    db: &Db,
    entry: &SlackThreadEntry,
) -> Result<(Upsert, String), String> {
    let err = |e: sqlx::Error| e.to_string();
    let local: Option<String> = match (&entry.channel_id, &entry.thread_ts) {
        (Some(channel), Some(ts)) => sqlx::query_scalar(
            "SELECT id FROM slack_threads WHERE channel_id = ? AND thread_ts = ? LIMIT 1",
        )
        .bind(channel)
        .bind(ts)
        .fetch_optional(db)
        .await
        .map_err(err)?,
        _ => id_by(db, "slack_threads", "id", &entry.id).await?,
    };
    let project_id = resolve_project(db, &entry.project).await;

    if let Some(id) = local {
        let mut changed = sqlx::query(
            "UPDATE slack_threads SET project_id = ? WHERE id = ? AND ? IS NOT NULL AND project_id IS NOT ?",
        )
        .bind(&project_id)
        .bind(&id)
        .bind(&project_id)
        .bind(&project_id)
        .execute(db)
        .await
        .map_err(err)?
        .rows_affected()
            > 0;
        if entry.title_custom {
            changed |= sqlx::query(
                "UPDATE slack_threads SET title = ?, title_custom = 1 \
                 WHERE id = ? AND (title IS NOT ? OR title_custom = 0)",
            )
            .bind(&entry.title)
            .bind(&id)
            .bind(&entry.title)
            .execute(db)
            .await
            .map_err(err)?
            .rows_affected()
                > 0;
        }
        for m in &entry.messages {
            let Some(vi) = m.text_vi.as_deref() else {
                continue;
            };
            changed |= sqlx::query(
                "UPDATE slack_messages SET text_vi = ?, translate_status = ?, translate_error = NULL \
                 WHERE thread_id = ? AND ts = ? AND src_hash = ? AND text_vi IS NULL",
            )
            .bind(vi)
            .bind(&m.translate_status)
            .bind(&id)
            .bind(&m.ts)
            .bind(&m.src_hash)
            .execute(db)
            .await
            .map_err(err)?
            .rows_affected()
                > 0;
        }
        let outcome = if changed {
            Upsert::Updated
        } else {
            Upsert::Unchanged
        };
        return Ok((outcome, id));
    }

    // Ids end up in filesystem paths: only reuse safe ones.
    let hint = if slack_store::is_safe_id(&entry.id) {
        entry.id.as_str()
    } else {
        ""
    };
    let id = free_id(db, "slack_threads", hint).await;
    let mut file_ids: BTreeMap<&str, String> = BTreeMap::new();
    for f in &entry.files {
        if crate::slack_threads::files::is_safe_file_id(&f.slack_file_id) {
            file_ids.insert(f.id.as_str(), free_id(db, "slack_files", &f.id).await);
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let mut tx = db.begin().await.map_err(err)?;
    sqlx::query(
        "INSERT INTO slack_threads (id, title, title_custom, project_id, position, created_at, \
         updated_at, workspace_domain, team_id, workspace, channel_id, channel, thread_ts, \
         thread_url, fetch_status, fetched_at, reply_count, last_reply_ts, last_seen_ts, \
         warnings, search_text, legacy, legacy_content) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'pending', ?, ?, ?, ?, '[]', ?, ?, ?)",
    )
    .bind(&id)
    .bind(&entry.title)
    .bind(entry.title_custom as i64)
    .bind(&project_id)
    .bind(entry.position)
    .bind(&entry.created_at)
    .bind(&entry.updated_at)
    .bind(&entry.workspace_domain)
    .bind(&entry.team_id)
    .bind(&entry.workspace)
    .bind(&entry.channel_id)
    .bind(&entry.channel)
    .bind(&entry.thread_ts)
    .bind(&entry.thread_url)
    .bind(&entry.fetched_at)
    .bind(entry.reply_count)
    .bind(&entry.last_reply_ts)
    .bind(&entry.last_seen_ts)
    .bind(&entry.search_text)
    .bind(entry.legacy as i64)
    .bind(&entry.legacy_content)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    for f in &entry.files {
        let Some(file_id) = file_ids.get(f.id.as_str()) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO slack_files (id, thread_id, slack_file_id, message_ts, name, title, mime, \
             filetype, size, file_path, remote_updated, permalink, width, height, download_status, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, 'pending', ?)",
        )
        .bind(file_id)
        .bind(&id)
        .bind(&f.slack_file_id)
        .bind(&f.message_ts)
        .bind(&f.name)
        .bind(&f.title)
        .bind(&f.mime)
        .bind(&f.filetype)
        .bind(f.size)
        .bind(f.remote_updated)
        .bind(&f.permalink)
        .bind(f.width)
        .bind(f.height)
        .bind(&now)
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    }
    for m in &entry.messages {
        let mapped: Vec<&str> = serde_json::from_str::<Vec<String>>(&m.file_ids)
            .unwrap_or_default()
            .iter()
            .filter_map(|old| file_ids.get(old.as_str()).map(String::as_str))
            .collect();
        let reactions = if m.reactions.is_empty() {
            "[]"
        } else {
            m.reactions.as_str()
        };
        sqlx::query(
            "INSERT INTO slack_messages (thread_id, ts, seq, user_id, bot_id, author_name, \
             author_icon_url, subtype, text_raw, text_vi, src_hash, translate_status, \
             translate_error, edited_ts, reactions, file_ids) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, ?, ?, ?)",
        )
        .bind(&id)
        .bind(&m.ts)
        .bind(m.seq)
        .bind(&m.user_id)
        .bind(&m.bot_id)
        .bind(&m.author_name)
        .bind(&m.author_icon_url)
        .bind(&m.subtype)
        .bind(&m.text_raw)
        .bind(&m.text_vi)
        .bind(&m.src_hash)
        .bind(&m.translate_status)
        .bind(&m.edited_ts)
        .bind(reactions)
        .bind(serde_json::to_string(&mapped).unwrap_or_else(|_| "[]".into()))
        .execute(&mut *tx)
        .await
        .map_err(err)?;
    }
    tx.commit().await.map_err(err)?;
    Ok((Upsert::Created, id))
}

/// Id of the row of `table` whose `label` matches; `None` for no label or no match.
async fn id_by_label(
    db: &Db,
    table: &str,
    label: &Option<String>,
) -> Result<Option<String>, String> {
    match label {
        Some(label) => id_by(db, table, "label", label).await,
        None => Ok(None),
    }
}

/// Match a project by path, then by name. A new project needs its folder: it is created when
/// `create_missing_dirs` is set, the project is skipped otherwise. Links (skills, rules, groups,
/// MCP / VPS servers) are added, never removed. Returns the local project id and
/// per-link problems.
async fn import_project(
    db: &Db,
    entry: &ProjectEntry,
    create_missing_dirs: bool,
) -> Result<(Upsert, String, Vec<String>), String> {
    let err = |e: sqlx::Error| e.to_string();
    let mut warnings = Vec::new();
    let local = match id_by(db, "projects", "path", &entry.path).await? {
        Some(id) => Some(id),
        None => id_by(db, "projects", "name", &entry.name).await?,
    };
    let path = match &local {
        Some(id) => sqlx::query_scalar::<_, String>("SELECT path FROM projects WHERE id = ?")
            .bind(id)
            .fetch_one(db)
            .await
            .map_err(err)?,
        None => entry.path.clone(),
    };
    if !Path::new(&path).is_dir() {
        if !create_missing_dirs {
            if local.is_none() {
                return Err(format!(
                    "folder {path} does not exist on this machine (turn on \"create missing project folders\")"
                ));
            }
            warnings.push(format!("folder {path} does not exist"));
        } else {
            std::fs::create_dir_all(&path).map_err(|e| format!("could not create {path}: {e}"))?;
        }
    }

    let github = id_by_label(db, "github_accounts", &entry.github_account).await?;
    let github_reviewer =
        id_by_label(db, "github_accounts", &entry.github_reviewer_account).await?;
    let gitlab = id_by_label(db, "gitlab_accounts", &entry.gitlab_account).await?;
    let aws = id_by_label(db, "aws_accounts", &entry.aws_account).await?;
    let azure = id_by_label(db, "azure_accounts", &entry.azure_account).await?;
    let claude = id_by_label(db, "claude_accounts", &entry.claude_account).await?;

    let (id, mut outcome) = match local {
        Some(id) => {
            // Account links only move when the imported account exists here.
            let changed = sqlx::query(
                "UPDATE projects SET default_engine = ?, github_owner = COALESCE(?, github_owner), \
                 github_repo = COALESCE(?, github_repo), \
                 github_project_board_url = COALESCE(?, github_project_board_url), \
                 github_project_field_mappings = COALESCE(?, github_project_field_mappings), \
                 github_account_id = COALESCE(?, github_account_id), \
                 github_reviewer_account_id = COALESCE(?, github_reviewer_account_id), \
                 gitlab_account_id = COALESCE(?, gitlab_account_id), \
                 aws_account_id = COALESCE(?, aws_account_id), \
                 azure_account_id = COALESCE(?, azure_account_id), \
                 claude_account_id = COALESCE(?, claude_account_id) \
                 WHERE id = ? AND (default_engine IS NOT ? \
                 OR (? IS NOT NULL AND github_owner IS NOT ?) OR (? IS NOT NULL AND github_repo IS NOT ?) \
                 OR (? IS NOT NULL AND github_project_board_url IS NOT ?) \
                 OR (? IS NOT NULL AND github_project_field_mappings IS NOT ?) \
                 OR (? IS NOT NULL AND github_account_id IS NOT ?) \
                 OR (? IS NOT NULL AND github_reviewer_account_id IS NOT ?) \
                 OR (? IS NOT NULL AND gitlab_account_id IS NOT ?) \
                 OR (? IS NOT NULL AND aws_account_id IS NOT ?) \
                 OR (? IS NOT NULL AND azure_account_id IS NOT ?) \
                 OR (? IS NOT NULL AND claude_account_id IS NOT ?))",
            );
            let values = [
                &entry.github_owner,
                &entry.github_repo,
                &entry.github_project_board_url,
                &entry.github_project_field_mappings,
                &github,
                &github_reviewer,
                &gitlab,
                &aws,
                &azure,
                &claude,
            ];
            let mut q = changed.bind(&entry.default_engine);
            for v in values {
                q = q.bind(v);
            }
            q = q.bind(&id).bind(&entry.default_engine);
            for v in values {
                q = q.bind(v).bind(v);
            }
            let changed = q.execute(db).await.map_err(err)?.rows_affected() > 0;
            (
                id,
                if changed {
                    Upsert::Updated
                } else {
                    Upsert::Unchanged
                },
            )
        }
        None => {
            let id = free_id(db, "projects", &entry.id).await;
            let position: i64 =
                sqlx::query_scalar("SELECT COALESCE(MAX(position), -1) + 1 FROM projects")
                    .fetch_one(db)
                    .await
                    .map_err(err)?;
            sqlx::query(
                "INSERT INTO projects (id, name, path, github_owner, github_repo, default_engine, \
                 created_at, position, github_project_board_url, github_project_field_mappings, \
                 github_account_id, github_reviewer_account_id, gitlab_account_id, aws_account_id, \
                 azure_account_id, claude_account_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(&id)
            .bind(&entry.name)
            .bind(&path)
            .bind(&entry.github_owner)
            .bind(&entry.github_repo)
            .bind(&entry.default_engine)
            .bind(&entry.created_at)
            .bind(position)
            .bind(&entry.github_project_board_url)
            .bind(&entry.github_project_field_mappings)
            .bind(&github)
            .bind(&github_reviewer)
            .bind(&gitlab)
            .bind(&aws)
            .bind(&azure)
            .bind(&claude)
            .execute(db)
            .await
            .map_err(err)?;
            (id, Upsert::Created)
        }
    };
    let mut touched = false;

    for repo in &entry.repos {
        let exists: Option<String> = sqlx::query_scalar(
            "SELECT id FROM repos WHERE project_id = ? AND (path = ? OR name = ?) LIMIT 1",
        )
        .bind(&id)
        .bind(&repo.path)
        .bind(&repo.name)
        .fetch_optional(db)
        .await
        .map_err(err)?;
        if exists.is_some() {
            continue;
        }
        let provider = if repo.provider.is_empty() {
            "github"
        } else {
            repo.provider.as_str()
        };
        sqlx::query(
            "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at, \
             provider, gitlab_project_path, gitlab_project_id) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(Uuid::new_v4().to_string())
        .bind(&id)
        .bind(&repo.name)
        .bind(&repo.path)
        .bind(&repo.github_owner)
        .bind(&repo.github_repo)
        .bind(&repo.created_at)
        .bind(provider)
        .bind(&repo.gitlab_project_path)
        .bind(repo.gitlab_project_id)
        .execute(db)
        .await
        .map_err(err)?;
        touched = true;
    }

    let now = chrono::Utc::now().to_rfc3339();
    for name in &entry.mcp_servers {
        match id_by(db, "mcp_servers", "name", name).await? {
            Some(server_id) => {
                touched |= sqlx::query(
                    "INSERT OR IGNORE INTO project_mcp_servers (project_id, server_id, enabled_at) \
                     VALUES (?, ?, ?)",
                )
                .bind(&id)
                .bind(&server_id)
                .bind(&now)
                .execute(db)
                .await
                .map_err(err)?
                .rows_affected()
                    > 0;
            }
            None => warnings.push(format!("MCP server '{name}' not found")),
        }
    }

    for link in &entry.servers {
        match id_by(db, "servers", "label", &link.server).await? {
            Some(server_id) => {
                touched |= sqlx::query(
                    "INSERT OR IGNORE INTO project_servers (project_id, server_id, role, created_at) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(&id)
                .bind(&server_id)
                .bind(&link.role)
                .bind(&now)
                .execute(db)
                .await
                .map_err(err)?
                .rows_affected()
                    > 0;
            }
            None => warnings.push(format!("VPS server '{}' not found", link.server)),
        }
    }

    // Groups first: enabling one applies its members (as group-owned, not manual).
    for (kind, names) in [
        (GroupKind::Skill, &entry.skill_groups),
        (GroupKind::Rule, &entry.rule_groups),
    ] {
        let (groups_table, link_table) = match kind {
            GroupKind::Skill => ("skill_groups", "project_skill_groups"),
            GroupKind::Rule => ("rule_groups", "project_rule_groups"),
        };
        for name in names {
            let Some(group_id) = id_by(db, groups_table, "name", name).await? else {
                warnings.push(format!("group '{name}' not found"));
                continue;
            };
            let enabled: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT 1 FROM {link_table} WHERE project_id = ? AND group_id = ?"
            ))
            .bind(&id)
            .bind(&group_id)
            .fetch_optional(db)
            .await
            .map_err(err)?;
            if enabled.is_some() {
                continue;
            }
            let applied = crate::commands::groups::enable_group(db, kind, &id, &group_id).await?;
            for failure in applied.failures {
                warnings.push(format!("'{}': {}", failure.item_name, failure.error));
            }
            touched = true;
        }
    }

    for (is_skill, items) in [(true, &entry.skills), (false, &entry.rules)] {
        let (items_table, link_table, item_col) = if is_skill {
            ("skills", "project_skills", "skill_id")
        } else {
            ("rules", "project_rules", "rule_id")
        };
        for item in items {
            let Some(item_id) = id_by(db, items_table, "name", &item.name).await? else {
                warnings.push(format!("{items_table} '{}' not found", item.name));
                continue;
            };
            let manual: Option<i64> = sqlx::query_scalar(&format!(
                "SELECT manual FROM {link_table} WHERE project_id = ? AND {item_col} = ?"
            ))
            .bind(&id)
            .bind(&item_id)
            .fetch_optional(db)
            .await
            .map_err(err)?;
            // Already applied (and at least as "manual" as the export says).
            if manual.is_some_and(|m| m != 0 || !item.manual) {
                continue;
            }
            let applied = if is_skill {
                crate::commands::projects::apply_skill_to_project(db, &id, &item_id, item.manual)
                    .await
            } else {
                rules::apply_rule_to_project(db, &id, &item_id, item.manual).await
            };
            match applied {
                Ok(()) => touched = true,
                Err(e) => warnings.push(format!("'{}': {e}", item.name)),
            }
        }
    }

    if touched && outcome == Upsert::Unchanged {
        outcome = Upsert::Updated;
    }
    Ok((outcome, id, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_rel_path_rejects_escapes() {
        assert_eq!(safe_rel_path("SKILL.md"), Some(PathBuf::from("SKILL.md")));
        assert_eq!(safe_rel_path("refs/a.md"), Some(PathBuf::from("refs/a.md")));
        assert_eq!(safe_rel_path("../evil"), None);
        assert_eq!(safe_rel_path("a/../../evil"), None);
        assert_eq!(safe_rel_path("/etc/passwd"), None);
        assert_eq!(safe_rel_path(""), None);
    }

    #[test]
    fn seal_round_trips_and_rejects_wrong_password() {
        let mut payload = SecretPayload::default();
        payload.github.insert("acc".into(), "ghp_secret".into());
        let sealed = seal(&payload, "correct horse").unwrap();
        assert!(!sealed.ciphertext.contains("ghp_secret"));
        let opened = open_sealed(&sealed, "correct horse").unwrap();
        assert_eq!(
            opened.github.get("acc").map(String::as_str),
            Some("ghp_secret")
        );
        assert!(open_sealed(&sealed, "wrong password").is_err());
    }

    async fn test_db() -> Db {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn notes_and_todos_merge_by_id_and_relink_projects() {
        let db = test_db().await;
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p-local', 'devdy', '/x/devdy', 'now')")
            .execute(&db)
            .await
            .unwrap();
        let mut note = NoteEntry {
            id: "n1".into(),
            title: "T".into(),
            content: "body".into(),
            position: 0,
            created_at: "c".into(),
            updated_at: "u".into(),
            // Same name, different path on the other machine → matched by name.
            project: Some(ProjectRef {
                name: "devdy".into(),
                path: "/other/devdy".into(),
            }),
        };
        assert_eq!(import_note(&db, &note).await.unwrap(), Upsert::Created);
        assert_eq!(import_note(&db, &note).await.unwrap(), Upsert::Unchanged);
        note.content = "edited".into();
        note.project = None;
        assert_eq!(import_note(&db, &note).await.unwrap(), Upsert::Updated);
        let (content, project): (String, Option<String>) =
            sqlx::query_as("SELECT content, project_id FROM notes WHERE id = 'n1'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(content, "edited");
        // An unresolvable project keeps the local link.
        assert_eq!(project.as_deref(), Some("p-local"));

        let todo = TodoEntry {
            id: "t1".into(),
            text: "do".into(),
            done: true,
            position: 3,
            created_at: "c".into(),
            due_date: Some("2026-11-05".into()),
            project: None,
            focus: serde_json::json!({ "due_time": "15:00", "priority": 1, "estimate_min": 90 })
                .as_object()
                .unwrap()
                .clone(),
        };
        assert_eq!(import_todo(&db, &todo).await.unwrap(), Upsert::Created);
        assert_eq!(import_todo(&db, &todo).await.unwrap(), Upsert::Unchanged);
        let row: (Option<String>, i64, Option<i64>, String) = sqlx::query_as(
            "SELECT due_time, priority, estimate_min, status FROM todos WHERE id = 't1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(row, (Some("15:00".into()), 1, Some(90), "todo".into()));
        let mut edited = todo;
        edited.focus.insert("priority".into(), 3.into());
        assert_eq!(import_todo(&db, &edited).await.unwrap(), Upsert::Updated);
    }

    #[tokio::test]
    async fn accounts_merge_by_label_without_touching_keychain() {
        let db = test_db().await;
        let entry = GithubEntry {
            id: "gh-1".into(),
            label: "work".into(),
            username: Some("me".into()),
            scopes: Some("repo".into()),
        };
        assert_eq!(
            import_github(&db, &entry, None).await.unwrap(),
            Upsert::Created
        );
        assert_eq!(
            import_github(&db, &entry, None).await.unwrap(),
            Upsert::Unchanged
        );
        let renamed = GithubEntry {
            id: "gh-other".into(),
            username: Some("me2".into()),
            ..entry
        };
        assert_eq!(
            import_github(&db, &renamed, None).await.unwrap(),
            Upsert::Updated
        );
        let ids: Vec<String> = sqlx::query_scalar("SELECT id FROM github_accounts")
            .fetch_all(&db)
            .await
            .unwrap();
        // Matched by label, so still one row, keeping the first id.
        assert_eq!(ids, vec!["gh-1".to_string()]);
    }

    #[tokio::test]
    async fn groups_merge_members_by_name() {
        let db = test_db().await;
        for (id, name) in [("s1", "alpha"), ("s2", "beta")] {
            sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES (?, ?, 'd', 'claude', '/nowhere', 'now')")
                .bind(id)
                .bind(name)
                .execute(&db)
                .await
                .unwrap();
        }
        let group = GroupEntry {
            name: "core".into(),
            description: String::new(),
            color: None,
            members: vec!["alpha".into(), "missing".into()],
        };
        assert_eq!(
            import_group(&db, GroupKind::Skill, &group).await.unwrap(),
            Upsert::Created
        );
        let more = GroupEntry {
            members: vec!["beta".into()],
            ..group
        };
        assert_eq!(
            import_group(&db, GroupKind::Skill, &more).await.unwrap(),
            Upsert::Updated
        );
        let members: Vec<String> = sqlx::query_scalar(
            "SELECT m.skill_id FROM skill_group_members m JOIN skill_groups g ON g.id = m.group_id ORDER BY m.position",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        // Union: alpha kept, beta added, unknown names skipped.
        assert_eq!(members, vec!["s1".to_string(), "s2".to_string()]);
    }

    #[tokio::test]
    async fn slack_threads_round_trip_and_merge_by_key() {
        let source = test_db().await;
        sqlx::query(
            "INSERT INTO slack_threads (id, title, title_custom, position, created_at, updated_at, \
             team_id, channel_id, channel, thread_ts, fetch_status, reply_count, search_text) \
             VALUES ('th-1', 'Deploy', 1, 0, 'c', 'u', 'T1', 'C123', 'ops', '1700000000.000100', \
             'ready', 1, 'hello')",
        )
        .execute(&source)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO slack_files (id, thread_id, slack_file_id, message_ts, name, size, \
             file_path, download_status, created_at) VALUES ('f-1', 'th-1', 'F1', \
             '1700000000.000100', 'shot.png', 10, 'slack-threads/th-1/files/F1/shot.png', 'done', 'c')",
        )
        .execute(&source)
        .await
        .unwrap();
        for (ts, seq, vi, files) in [
            ("1700000000.000100", 0, Some("xin chào"), r#"["f-1"]"#),
            ("1700000000.000200", 1, None, "[]"),
        ] {
            sqlx::query(
                "INSERT INTO slack_messages (thread_id, ts, seq, user_id, text_raw, text_vi, src_hash, \
                 translate_status, file_ids) VALUES ('th-1', ?, ?, 'U1', 'hello', ?, 'h', ?, ?)",
            )
            .bind(ts)
            .bind(seq)
            .bind(vi)
            .bind(if vi.is_some() { "done" } else { "pending" })
            .bind(files)
            .execute(&source)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO slack_users (team_id, user_id, display_name, fetched_at) VALUES ('T1', 'U1', 'An', 'now')",
        )
        .execute(&source)
        .await
        .unwrap();

        let (threads, users, channels) = collect_slack(&source).await.unwrap();
        assert_eq!(threads.len(), 1);
        assert_eq!(threads[0].messages.len(), 2);
        assert_eq!(threads[0].files.len(), 1);

        let target = test_db().await;
        let bundle = Bundle {
            format: FORMAT.into(),
            version: VERSION,
            exported_at: String::new(),
            app_version: String::new(),
            settings: BTreeMap::new(),
            skills: vec![],
            rules: vec![],
            skill_groups: vec![],
            rule_groups: vec![],
            mcp_servers: vec![],
            vps_servers: vec![],
            github_accounts: vec![],
            gitlab_accounts: vec![],
            aws_accounts: vec![],
            azure_accounts: vec![],
            google_accounts: vec![],
            claude_accounts: vec![],
            notes: vec![],
            todos: vec![],
            slack_threads: threads,
            slack_users: users,
            slack_channels: channels,
            projects: vec![],
            home_dir: None,
            secrets: None,
        };
        let mut report = ImportReport::default();
        import_slack_caches(&target, &bundle, &mut report).await;
        assert!(report.warnings.is_empty());
        let entry = &bundle.slack_threads[0];
        let (outcome, id) = import_slack_thread(&target, entry).await.unwrap();
        assert_eq!(outcome, Upsert::Created);
        assert_eq!(id, "th-1");

        let (status, title): (String, String) =
            sqlx::query_as("SELECT fetch_status, title FROM slack_threads WHERE id = 'th-1'")
                .fetch_one(&target)
                .await
                .unwrap();
        // Re-fetched after import so the attachments are downloaded again.
        assert_eq!((status.as_str(), title.as_str()), ("pending", "Deploy"));
        let (path, dl): (Option<String>, String) =
            sqlx::query_as("SELECT file_path, download_status FROM slack_files WHERE id = 'f-1'")
                .fetch_one(&target)
                .await
                .unwrap();
        assert_eq!((path, dl.as_str()), (None, "pending"));
        let file_ids: String =
            sqlx::query_scalar("SELECT file_ids FROM slack_messages WHERE seq = 0")
                .fetch_one(&target)
                .await
                .unwrap();
        assert_eq!(file_ids, r#"["f-1"]"#);

        // Same key again: nothing new.
        assert_eq!(
            import_slack_thread(&target, entry).await.unwrap().0,
            Upsert::Unchanged
        );
        // A local thread missing a translation gains it.
        sqlx::query(
            "UPDATE slack_messages SET text_vi = NULL, translate_status = 'pending' WHERE seq = 0",
        )
        .execute(&target)
        .await
        .unwrap();
        let (outcome, id) = import_slack_thread(&target, entry).await.unwrap();
        assert_eq!(outcome, Upsert::Updated);
        let vi: Option<String> = sqlx::query_scalar(
            "SELECT text_vi FROM slack_messages WHERE thread_id = ? AND seq = 0",
        )
        .bind(&id)
        .fetch_one(&target)
        .await
        .unwrap();
        assert_eq!(vi.as_deref(), Some("xin chào"));
    }

    #[test]
    fn remap_home_moves_paths_under_the_new_home() {
        let (a, b) = (Some("/Users/a"), Some("/Users/b"));
        assert_eq!(remap_home("/Users/a/work/x", a, b), "/Users/b/work/x");
        assert_eq!(remap_home("/Users/a", a, b), "/Users/b");
        // Only whole path components.
        assert_eq!(remap_home("/Users/ab/x", a, b), "/Users/ab/x");
        assert_eq!(remap_home("/opt/x", a, b), "/opt/x");
        assert_eq!(remap_home("/Users/a/x", a, None), "/Users/a/x");
    }

    #[tokio::test]
    async fn projects_round_trip_with_links_and_missing_folder() {
        let source = test_db().await;
        for sql in [
            "INSERT INTO projects (id, name, path, default_engine, created_at, github_project_board_url) \
             VALUES ('p1', 'shop', '/old/home/shop', 'codex', 'c', 'https://board')",
            "INSERT INTO repos (id, project_id, name, path, created_at, provider) \
             VALUES ('r1', 'p1', 'api', '/old/home/shop/api', 'c', 'gitlab')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m1', 'slack', 'stdio', 'c')",
            "INSERT INTO project_mcp_servers (project_id, server_id, enabled_at) VALUES ('p1', 'm1', 'c')",
            "INSERT INTO servers (id, label, host, username, auth_method, created_at) \
             VALUES ('s1', 'prod', 'h', 'root', 'agent', 'c')",
            "INSERT INTO project_servers (project_id, server_id, role, created_at) VALUES ('p1', 's1', 'production', 'c')",
        ] {
            sqlx::query(sql).execute(&source).await.unwrap();
        }
        let mut projects = collect_projects(&source).await.unwrap();
        assert_eq!(projects.len(), 1);
        let home = std::env::temp_dir().join(format!("devdy-backup-{}", Uuid::new_v4()));
        let new_path = home.join("shop");
        projects[0].path = new_path.to_string_lossy().into_owned();

        let target = test_db().await;
        for sql in [
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m9', 'slack', 'stdio', 'c')",
            "INSERT INTO servers (id, label, host, username, auth_method, created_at) \
             VALUES ('s9', 'prod', 'h', 'root', 'agent', 'c')",
        ] {
            sqlx::query(sql).execute(&target).await.unwrap();
        }
        let entry = &projects[0];
        // Missing folder without permission to create it: skipped.
        assert!(import_project(&target, entry, false).await.is_err());
        let (outcome, id, warnings) = import_project(&target, entry, true).await.unwrap();
        assert_eq!(outcome, Upsert::Created);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(new_path.is_dir());

        let (engine, board): (String, Option<String>) = sqlx::query_as(
            "SELECT default_engine, github_project_board_url FROM projects WHERE id = ?",
        )
        .bind(&id)
        .fetch_one(&target)
        .await
        .unwrap();
        assert_eq!(
            (engine.as_str(), board.as_deref()),
            ("codex", Some("https://board"))
        );
        let links: (i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT COUNT(*) FROM repos WHERE project_id = ?1 AND provider = 'gitlab'), \
                    (SELECT COUNT(*) FROM project_mcp_servers WHERE project_id = ?1 AND server_id = 'm9'), \
                    (SELECT COUNT(*) FROM project_servers WHERE project_id = ?1 AND server_id = 's9')",
        )
        .bind(&id)
        .fetch_one(&target)
        .await
        .unwrap();
        assert_eq!(links, (1, 1, 1));

        // Second import: matched by path, nothing new.
        let (outcome, again, _) = import_project(&target, entry, false).await.unwrap();
        assert_eq!((outcome, again), (Upsert::Unchanged, id));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn open_rejects_hostile_kdf_params() {
        let payload = SecretPayload::default();
        let mut sealed = seal(&payload, "password1").unwrap();
        sealed.memlimit = 64 * 1024 * 1024 * 1024;
        assert!(open_sealed(&sealed, "password1").is_err());
    }
}
