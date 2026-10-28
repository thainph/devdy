//! Export / import of the whole Devdy configuration as one file, to carry a setup to another
//! machine.
//!
//! The file is JSON (`*.devdy-backup`). It holds the portable app settings, the Skills / Rules
//! library (file contents included) with their groups, MCP servers, VPS servers, account metadata
//! (GitHub, GitLab, AWS, Azure, Google, Claude) and notes / todos. Projects, sessions and usage
//! history stay on the machine.
//!
//! Secrets (PATs, MCP env/header VALUEs, Google OAuth client + refresh tokens, SSH passphrases and
//! Devdy-managed private key files) are only exported when the user gives a password: they are
//! sealed with XChaCha20-Poly1305 under an Argon2id key derived from it. Without a password the
//! file carries no secret at all and the accounts must be re-authenticated after import.
//!
//! Import merges by name: an item whose name/label exists locally is overwritten with the
//! imported version, a new one is created, and nothing local is ever deleted. Notes and todos
//! match by id. Library files replaced by an import are moved to the legacy backups first.

use crate::commands::groups::{set_group_members_inner, GroupKind};
use crate::commands::mcp::McpServer;
use crate::commands::settings::PORTABLE_SETTING_KEYS;
use crate::commands::{mcp, rules, skills, vps_servers};
use crate::db::Db;
use crate::secrets::{self, GoogleClientCreds, McpSecrets};
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
    secrets: Option<SealedSecrets>,
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
    project: Option<ProjectRef>,
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
    if bundle.format != FORMAT {
        return Err("Not a Devdy settings backup file".to_string());
    }
    if bundle.version > VERSION {
        return Err(format!(
            "This backup was made by a newer Devdy (format v{}); update Devdy first",
            bundle.version
        ));
    }
    Ok(bundle)
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
    let todos = sqlx::query(
        "SELECT t.id, t.text, t.done, t.position, t.created_at, \
                p.name AS project_name, p.path AS project_path \
         FROM todos t LEFT JOIN projects p ON p.id = t.project_id ORDER BY t.position",
    )
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
        project: project_of(row),
    })
    .collect();

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
        secrets: None,
    };
    Ok((bundle, secret))
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
    let (mut bundle, payload) = collect(&app, db.inner(), password.is_some()).await?;
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

// ---- Import ------------------------------------------------------------------

#[tauri::command]
pub async fn inspect_settings_backup(path: String) -> Result<BackupInfo, String> {
    Ok(read_bundle(&path)?.info())
}

#[tauri::command]
pub async fn import_settings_backup(
    app: AppHandle,
    db: State<'_, Db>,
    path: String,
    password: Option<String>,
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
    let secret = secret.unwrap_or_default();
    let db = db.inner();

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
    for entry in &bundle.notes {
        let result = import_note(db, entry).await;
        report.record("notes", &entry.title, result);
    }
    for entry in &bundle.todos {
        let result = import_todo(db, entry).await;
        report.record("todos", &entry.text, result);
    }

    Ok(report)
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

async fn import_todo(db: &Db, entry: &TodoEntry) -> Result<Upsert, String> {
    let project_id = resolve_project(db, &entry.project).await;
    let exists = id_by(db, "todos", "id", &entry.id).await?.is_some();
    if exists {
        let changed = sqlx::query(
            "UPDATE todos SET text = ?, done = ?, position = ?, project_id = COALESCE(?, project_id) \
             WHERE id = ? AND (text IS NOT ? OR done IS NOT ? OR position IS NOT ? \
             OR (? IS NOT NULL AND project_id IS NOT ?))",
        )
        .bind(&entry.text)
        .bind(entry.done as i64)
        .bind(entry.position)
        .bind(&project_id)
        .bind(&entry.id)
        .bind(&entry.text)
        .bind(entry.done as i64)
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
        "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(&entry.id)
    .bind(&entry.text)
    .bind(entry.done as i64)
    .bind(entry.position)
    .bind(&entry.created_at)
    .bind(&project_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(Upsert::Created)
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
            project: None,
        };
        assert_eq!(import_todo(&db, &todo).await.unwrap(), Upsert::Created);
        assert_eq!(import_todo(&db, &todo).await.unwrap(), Upsert::Unchanged);
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

    #[test]
    fn open_rejects_hostile_kdf_params() {
        let payload = SecretPayload::default();
        let mut sealed = seal(&payload, "password1").unwrap();
        sealed.memlimit = 64 * 1024 * 1024 * 1024;
        assert!(open_sealed(&sealed, "password1").is_err());
    }
}
