//! The data a device sync carries: built on the source device, applied on the one pulling it.
//!
//! Built from the settings backup's collectors (`commands::settings_backup`) for the categories
//! the user ticked, serialized as JSON, zlib-compressed and then sealed as a whole with
//! XChaCha20-Poly1305 under a 32-byte key the transport derives from the master password. Secrets
//! (PATs, MCP env/header values, Google tokens, SSH passphrases, managed keys) travel raw inside
//! the sealed bundle; the secret notes vault stays sealed under its own vault password. Claude,
//! AWS and Azure accounts carry metadata only.
//!
//! Applying upserts like a backup import (by id for notes / todos, Slack key for threads, name /
//! label for the rest). Projects are never created: only a project whose NAME already exists here
//! gets the source's settings and links, its path and repos stay untouched. With `mirror`, items
//! missing on the source are then deleted through the app's own delete paths, for todos, notes,
//! skills / rules (+ groups), MCP / VPS servers and accounts only — never secret notes, Slack
//! threads, projects or settings.

use crate::commands::groups::GroupKind;
use crate::commands::settings_backup::{
    self as backup, Bundle, ImportReport, ProjectEntry, ProjectMatch, SecretPayload, Sections,
    Upsert,
};
use crate::commands::{
    aws_accounts, azure_accounts, claude_accounts, github_accounts, gitlab_accounts, google, mcp,
    notes, rules, skills, todos, vps_servers,
};
use crate::db::Db;
use crate::item_links::ItemKind;
use crate::secrets;
use crate::slack_threads::SlackThreadsState;
use dryoc::classic::crypto_aead_xchacha20poly1305_ietf::{
    crypto_aead_xchacha20poly1305_ietf_decrypt, crypto_aead_xchacha20poly1305_ietf_encrypt, Nonce,
};
use dryoc::rng::copy_randombytes;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::PathBuf;
use tauri::{AppHandle, Manager};

const FORMAT: &str = "devdy-device-sync";
const VERSION: u32 = 1;
/// Bound into the AEAD tag so a sealed bundle can't be replayed as anything else.
const AAD: &[u8] = b"devdy-device-sync:v1";
/// Leading bytes of a sealed bundle (format + version), checked before decrypting.
const MAGIC: &[u8; 4] = b"DDS1";
const NONCE_LEN: usize = 24;
const TAG_LEN: usize = 16;
/// Cap on the decompressed payload, so a corrupted (or hostile) stream can't exhaust memory.
const MAX_PLAIN_LEN: u64 = 1024 * 1024 * 1024;

/// What to pull. `mirror` also deletes local items the source doesn't have (see module docs).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SyncCategories {
    pub todos: bool,
    pub notes: bool,
    pub secret_notes: bool,
    pub slack_threads: bool,
    pub skills_rules: bool,
    pub projects: bool,
    pub settings: bool,
    pub mcp_servers: bool,
    pub vps_servers: bool,
    pub accounts: bool,
    pub mirror: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    /// Per section (`todos`, `skills`, `githubAccounts`, `projects`…).
    pub counts: BTreeMap<String, SyncCounts>,
    /// Per-item problems; the rest of the sync still went through.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncCounts {
    pub created: u32,
    pub updated: u32,
    pub unchanged: u32,
    pub deleted: u32,
    pub skipped: u32,
}

/// Plaintext of a sealed bundle. No `Debug`: it holds secrets.
#[derive(Default, Serialize, Deserialize)]
struct SyncPayload {
    format: String,
    version: u32,
    /// The categories the source put in (`mirror` unused). A category it left out is never
    /// applied, nor mirrored.
    categories: SyncCategories,
    bundle: Bundle,
    #[serde(default)]
    secrets: SecretPayload,
    /// Built-in MCP servers switched off per project, keyed by project NAME.
    #[serde(default)]
    builtin_mcp_disabled: BTreeMap<String, Vec<String>>,
}

/// What applying needs from the app. `app` is `None` only in unit tests: the parts that need it
/// (skill / rule library files, VPS keys, Slack fetch queue, UI events) are then skipped.
struct ApplyEnv<'a> {
    app: Option<&'a AppHandle>,
    app_data: Option<PathBuf>,
}

fn sections_of(cats: &SyncCategories) -> Sections {
    Sections {
        settings: cats.settings,
        library: cats.skills_rules,
        mcp_servers: cats.mcp_servers,
        vps_servers: cats.vps_servers,
        accounts: cats.accounts,
        notes: cats.notes,
        todos: cats.todos,
        slack: cats.slack_threads,
        projects: cats.projects,
        secret_vault: cats.secret_notes,
    }
}

// ---- Build -------------------------------------------------------------------

/// Gather the ticked categories, compress and seal them with `key` (XChaCha20-Poly1305, random
/// nonce, AAD `devdy-device-sync:v1`).
pub async fn build_sync_bundle(
    app: &AppHandle,
    db: &Db,
    cats: &SyncCategories,
    key: &[u8; 32],
) -> Result<Vec<u8>, String> {
    let (mut bundle, secrets) = backup::collect_sections(app, db, true, sections_of(cats)).await?;
    // Project paths are never applied: don't ship this machine's layout.
    bundle.home_dir = None;
    let builtin_mcp_disabled = if cats.projects {
        collect_builtin_disabled(db).await?
    } else {
        BTreeMap::new()
    };
    let payload = SyncPayload {
        format: FORMAT.to_string(),
        version: VERSION,
        categories: SyncCategories {
            mirror: false,
            ..cats.clone()
        },
        bundle,
        secrets,
        builtin_mcp_disabled,
    };
    let key = *key;
    tokio::task::spawn_blocking(move || seal_payload(&payload, &key))
        .await
        .map_err(|e| e.to_string())?
}

async fn collect_builtin_disabled(db: &Db) -> Result<BTreeMap<String, Vec<String>>, String> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT p.name, d.name FROM project_builtin_mcp_disabled d \
         JOIN projects p ON p.id = d.project_id ORDER BY p.name, d.name",
    )
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (project, name) in rows {
        out.entry(project).or_default().push(name);
    }
    Ok(out)
}

fn seal_payload(payload: &SyncPayload, key: &[u8; 32]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(payload).map_err(|e| e.to_string())?;
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&json).map_err(|e| e.to_string())?;
    let plain = z.finish().map_err(|e| e.to_string())?;

    let mut nonce: Nonce = [0u8; NONCE_LEN];
    copy_randombytes(&mut nonce);
    let mut out = Vec::with_capacity(MAGIC.len() + NONCE_LEN + plain.len() + TAG_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    let mut cipher = vec![0u8; plain.len() + TAG_LEN];
    crypto_aead_xchacha20poly1305_ietf_encrypt(&mut cipher, &plain, Some(AAD), &nonce, key)
        .map_err(|_| "Could not encrypt the sync data".to_string())?;
    out.extend_from_slice(&cipher);
    Ok(out)
}

fn open_payload(bytes: &[u8], key: &[u8; 32]) -> Result<SyncPayload, String> {
    let header = MAGIC.len() + NONCE_LEN;
    if bytes.len() < header + TAG_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err("Not Devdy sync data (or made by an incompatible version)".to_string());
    }
    let nonce: Nonce = bytes[MAGIC.len()..header]
        .try_into()
        .map_err(|_| "Corrupted sync data".to_string())?;
    let cipher = &bytes[header..];
    let mut plain = vec![0u8; cipher.len() - TAG_LEN];
    crypto_aead_xchacha20poly1305_ietf_decrypt(&mut plain, cipher, Some(AAD), &nonce, key)
        .map_err(|_| "Wrong master password, or the sync data is corrupted".to_string())?;
    let mut json = Vec::new();
    flate2::read::ZlibDecoder::new(plain.as_slice())
        .take(MAX_PLAIN_LEN + 1)
        .read_to_end(&mut json)
        .map_err(|_| "Corrupted sync data".to_string())?;
    if json.len() as u64 > MAX_PLAIN_LEN {
        return Err("The sync data is too large".to_string());
    }
    let payload: SyncPayload =
        serde_json::from_slice(&json).map_err(|_| "Corrupted sync data".to_string())?;
    if payload.format != FORMAT {
        return Err("Not Devdy sync data".to_string());
    }
    if payload.version > VERSION {
        return Err(format!(
            "The other device runs a newer Devdy (sync format v{}); update Devdy first",
            payload.version
        ));
    }
    Ok(payload)
}

// ---- Apply -------------------------------------------------------------------

/// Decrypt `bytes` with `key` and apply the categories ticked in `cats` that the bundle carries.
/// A wrong key fails before anything is written.
pub async fn apply_sync_bundle(
    app: &AppHandle,
    db: &Db,
    bytes: &[u8],
    key: &[u8; 32],
    cats: &SyncCategories,
) -> Result<SyncReport, String> {
    let bytes = bytes.to_vec();
    let key = *key;
    let payload = tokio::task::spawn_blocking(move || open_payload(&bytes, &key))
        .await
        .map_err(|e| e.to_string())??;
    let env = ApplyEnv {
        app: Some(app),
        app_data: app.path().app_data_dir().ok(),
    };
    let src = payload.categories.clone();
    let report = apply_payload(&env, db, payload, cats).await;
    if cats.todos && src.todos {
        crate::focus::emit_todos_changed(app);
    }
    if cats.notes && src.notes {
        crate::item_links::emit_changed(app, Some(ItemKind::Note));
    }
    Ok(report)
}

/// Section counters on top of the backup's `ImportReport` (which has no deleted / skipped).
#[derive(Default)]
struct Extra {
    deleted: BTreeMap<String, u32>,
    skipped: BTreeMap<String, u32>,
}

async fn apply_payload(
    env: &ApplyEnv<'_>,
    db: &Db,
    payload: SyncPayload,
    cats: &SyncCategories,
) -> SyncReport {
    let src = &payload.categories;
    let bundle = &payload.bundle;
    let secret = &payload.secrets;
    let mut report = ImportReport::default();
    let mut extra = Extra::default();
    let app_data = env.app_data.as_deref();

    if cats.settings && src.settings {
        backup::import_settings(db, bundle, &mut report).await;
    }

    if cats.skills_rules && src.skills_rules {
        match env.app {
            Some(app) => {
                for entry in &bundle.skills {
                    let result = backup::import_skill(app, db, entry).await;
                    backup::record_with_sync(&mut report, "skills", &entry.name, result);
                }
                for entry in &bundle.rules {
                    let result = rules::import_backup_rule(
                        app,
                        db,
                        &entry.id,
                        &entry.name,
                        &entry.description,
                        &entry.target,
                        &entry.content,
                    )
                    .await;
                    backup::record_with_sync(&mut report, "rules", &entry.name, result);
                }
            }
            None => report
                .warnings
                .push("Skills and rules need the app; skipped".to_string()),
        }
        for entry in &bundle.skill_groups {
            let result = backup::import_group(db, GroupKind::Skill, entry).await;
            report.record("skillGroups", &entry.name, result);
        }
        for entry in &bundle.rule_groups {
            let result = backup::import_group(db, GroupKind::Rule, entry).await;
            report.record("ruleGroups", &entry.name, result);
        }
    }

    if cats.mcp_servers && src.mcp_servers {
        for server in &bundle.mcp_servers {
            let result =
                mcp::import_backup_mcp(db, &server.id, server, secret.mcp.get(&server.id)).await;
            report.record("mcpServers", &server.name, result);
        }
    }

    if cats.vps_servers && src.vps_servers {
        for entry in &bundle.vps_servers {
            let result = match env.app {
                Some(app) => {
                    backup::import_vps(app, db, entry, secret.servers.get(&entry.id)).await
                }
                None => Err("needs the app".to_string()),
            };
            report.record("vpsServers", &entry.label, result);
        }
    }

    if cats.accounts && src.accounts {
        for entry in &bundle.github_accounts {
            let result = backup::import_github(db, entry, secret.github.get(&entry.id)).await;
            report.record("githubAccounts", &entry.label, result);
        }
        for entry in &bundle.gitlab_accounts {
            let result = backup::import_gitlab(db, entry, secret.gitlab.get(&entry.id)).await;
            report.record("gitlabAccounts", &entry.label, result);
        }
        // Claude / AWS / Azure: metadata only. A new one arrives logged out; an existing one keeps
        // its local credentials.
        for entry in &bundle.aws_accounts {
            let result = backup::import_aws(db, entry).await;
            report.record("awsAccounts", &entry.label, result);
        }
        for entry in &bundle.azure_accounts {
            let result = backup::import_azure(app_data, db, entry).await;
            report.record("azureAccounts", &entry.label, result);
        }
        if let Some(client) = &secret.google_client {
            let same = secrets::get_google_client().is_some_and(|c| {
                c.client_id == client.client_id && c.client_secret == client.client_secret
            });
            if !same {
                if let Err(e) = secrets::set_google_client(&client.client_id, &client.client_secret)
                {
                    report.warnings.push(format!("Google OAuth client: {e}"));
                }
            }
        }
        for entry in &bundle.google_accounts {
            let result =
                backup::import_google(db, entry, secret.google_accounts.get(&entry.id)).await;
            report.record("googleAccounts", &entry.label, result);
        }
        for entry in &bundle.claude_accounts {
            let result = backup::import_claude(app_data, db, entry).await;
            report.record("claudeAccounts", &entry.label, result);
        }
    }

    if cats.secret_notes && src.secret_notes {
        if let Some(vault) = secret.secret_vault.clone() {
            match secrets::with_vault(true, |local| Ok(crate::secret_vault::import(local, vault))) {
                Ok(n) => {
                    report
                        .counts
                        .entry("secretNotes".to_string())
                        .or_default()
                        .updated += n
                }
                Err(e) => report.warnings.push(format!("Secret notes: {e}")),
            }
        }
    }

    // Projects before notes / todos / threads, like a backup import.
    if cats.projects && src.projects {
        for entry in &bundle.projects {
            let disabled = payload
                .builtin_mcp_disabled
                .get(&entry.name)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            match sync_project(db, entry, disabled).await {
                Ok(Some((outcome, id, warnings))) => {
                    report.tally("projects", outcome);
                    for warning in warnings {
                        report
                            .warnings
                            .push(format!("projects '{}': {warning}", entry.name));
                    }
                    if let Some(dir) = app_data {
                        crate::aws_sandbox::write_project_aws_config(db, dir, &id).await;
                    }
                }
                Ok(None) => *extra.skipped.entry("projects".to_string()).or_default() += 1,
                Err(e) => report
                    .warnings
                    .push(format!("projects '{}': {e}", entry.name)),
            }
        }
    }

    if cats.notes && src.notes {
        for entry in &bundle.notes {
            let result = backup::import_note_matching(db, entry, ProjectMatch::NameOnly).await;
            report.record("notes", &entry.title, result);
        }
    }

    if cats.todos && src.todos {
        for entry in &bundle.todos {
            // By project NAME only; no such project here → no project.
            let project_id = match &entry.project {
                Some(p) => backup::id_by(db, "projects", "name", &p.name)
                    .await
                    .ok()
                    .flatten(),
                None => None,
            };
            let result = backup::write_todo(db, entry, project_id, true).await;
            report.record("todos", &entry.text, result);
        }
    }

    if cats.slack_threads && src.slack_threads && !bundle.slack_threads.is_empty() {
        backup::import_slack_caches(db, bundle, &mut report).await;
        let mut created = Vec::new();
        for entry in &bundle.slack_threads {
            let result = backup::import_slack_thread_matching(db, entry, ProjectMatch::NameOnly)
                .await
                .map(|(outcome, id)| {
                    if outcome == Upsert::Created {
                        created.push(id);
                    }
                    outcome
                });
            report.record("slackThreads", &entry.title, result);
        }
        if let Some(app) = env.app {
            if let Some(state) = app.try_state::<SlackThreadsState>() {
                for id in &created {
                    state.enqueue(id, false);
                }
            }
            crate::slack_threads::emit_all_changed(app);
        }
    }

    if cats.mirror {
        let plan = plan_mirror(db, &payload, cats, &mut report.warnings).await;
        execute_mirror(env, db, plan, &mut extra, &mut report.warnings).await;
    }

    into_sync_report(report, extra)
}

fn into_sync_report(report: ImportReport, extra: Extra) -> SyncReport {
    let mut counts: BTreeMap<String, SyncCounts> = report
        .counts
        .into_iter()
        .map(|(section, c)| {
            let n = |v: usize| u32::try_from(v).unwrap_or(u32::MAX);
            (
                section,
                SyncCounts {
                    created: n(c.created),
                    updated: n(c.updated),
                    unchanged: n(c.unchanged),
                    ..Default::default()
                },
            )
        })
        .collect();
    for (section, n) in extra.deleted {
        counts.entry(section).or_default().deleted += n;
    }
    for (section, n) in extra.skipped {
        counts.entry(section).or_default().skipped += n;
    }
    SyncReport {
        counts,
        warnings: report.warnings,
    }
}

/// Apply a source project to the local project of the same NAME: its settings, links (added,
/// never removed) and built-in MCP switches. `None` when there is no such project — nothing is
/// created and no path or repo is touched. Returns the local id and per-link warnings.
async fn sync_project(
    db: &Db,
    entry: &ProjectEntry,
    builtin_disabled: &[String],
) -> Result<Option<(Upsert, String, Vec<String>)>, String> {
    let Some(id) = backup::id_by(db, "projects", "name", &entry.name).await? else {
        return Ok(None);
    };
    let mut warnings = Vec::new();
    let mut changed = backup::update_project_row(db, &id, entry).await?;
    changed |= backup::apply_project_links(db, &id, entry, &mut warnings).await?;
    for name in mcp::BUILTIN_MCP_NAMES {
        let want_off = builtin_disabled.iter().any(|d| d == name);
        let is_off = sqlx::query_scalar::<_, i64>(
            "SELECT 1 FROM project_builtin_mcp_disabled WHERE project_id = ? AND name = ?",
        )
        .bind(&id)
        .bind(name)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .is_some();
        if want_off != is_off {
            mcp::set_project_builtin_mcp(db, &id, name, !want_off).await?;
            changed = true;
        }
    }
    let outcome = if changed {
        Upsert::Updated
    } else {
        Upsert::Unchanged
    };
    Ok(Some((outcome, id, warnings)))
}

// ---- Mirror ------------------------------------------------------------------

/// One local item to delete because the source doesn't have it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Deletion {
    section: &'static str,
    id: String,
    label: String,
}

/// Local rows of `table` (`id`, match key, label); names are literals from the callers.
async fn local_rows(
    db: &Db,
    table: &str,
    key_col: &str,
    label_col: &str,
) -> Result<Vec<(String, String, String)>, String> {
    sqlx::query_as(&format!(
        "SELECT id, {key_col}, {label_col} FROM {table} ORDER BY {label_col}"
    ))
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())
}

/// The local items a mirror deletes: per ticked mirrorable category, those whose match key (id
/// for todos / notes, name / label otherwise) the source doesn't list. A category the source left
/// out, or a section it sent empty, deletes nothing (a warning says so) — an empty list is more
/// likely a problem on the source than a wish to wipe this device.
async fn plan_mirror(
    db: &Db,
    payload: &SyncPayload,
    cats: &SyncCategories,
    warnings: &mut Vec<String>,
) -> Vec<Deletion> {
    let src = &payload.categories;
    let b = &payload.bundle;
    let keys = |it: &mut dyn Iterator<Item = &String>| it.cloned().collect::<BTreeSet<String>>();
    // (category ticked, category sent, section, table, key column, label column, source keys)
    let sections: Vec<(bool, bool, &'static str, &str, &str, &str, BTreeSet<String>)> = vec![
        (
            cats.todos,
            src.todos,
            "todos",
            "todos",
            "id",
            "text",
            keys(&mut b.todos.iter().map(|e| &e.id)),
        ),
        (
            cats.notes,
            src.notes,
            "notes",
            "notes",
            "id",
            "title",
            keys(&mut b.notes.iter().map(|e| &e.id)),
        ),
        (
            cats.skills_rules,
            src.skills_rules,
            "skillGroups",
            "skill_groups",
            "name",
            "name",
            keys(&mut b.skill_groups.iter().map(|e| &e.name)),
        ),
        (
            cats.skills_rules,
            src.skills_rules,
            "ruleGroups",
            "rule_groups",
            "name",
            "name",
            keys(&mut b.rule_groups.iter().map(|e| &e.name)),
        ),
        (
            cats.skills_rules,
            src.skills_rules,
            "skills",
            "skills",
            "name",
            "name",
            keys(&mut b.skills.iter().map(|e| &e.name)),
        ),
        (
            cats.skills_rules,
            src.skills_rules,
            "rules",
            "rules",
            "name",
            "name",
            keys(&mut b.rules.iter().map(|e| &e.name)),
        ),
        (
            cats.mcp_servers,
            src.mcp_servers,
            "mcpServers",
            "mcp_servers",
            "name",
            "name",
            keys(&mut b.mcp_servers.iter().map(|e| &e.name)),
        ),
        (
            cats.vps_servers,
            src.vps_servers,
            "vpsServers",
            "servers",
            "label",
            "label",
            keys(&mut b.vps_servers.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "githubAccounts",
            "github_accounts",
            "label",
            "label",
            keys(&mut b.github_accounts.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "gitlabAccounts",
            "gitlab_accounts",
            "label",
            "label",
            keys(&mut b.gitlab_accounts.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "googleAccounts",
            "google_accounts",
            "label",
            "label",
            keys(&mut b.google_accounts.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "claudeAccounts",
            "claude_accounts",
            "label",
            "label",
            keys(&mut b.claude_accounts.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "awsAccounts",
            "aws_accounts",
            "label",
            "label",
            keys(&mut b.aws_accounts.iter().map(|e| &e.label)),
        ),
        (
            cats.accounts,
            src.accounts,
            "azureAccounts",
            "azure_accounts",
            "label",
            "label",
            keys(&mut b.azure_accounts.iter().map(|e| &e.label)),
        ),
    ];

    let mut plan = Vec::new();
    for (ticked, sent, section, table, key_col, label_col, wanted) in sections {
        if !ticked {
            continue;
        }
        if !sent {
            warnings.push(format!(
                "{section}: the other device did not send this category; nothing deleted"
            ));
            continue;
        }
        if wanted.is_empty() {
            warnings.push(format!(
                "{section}: the other device has none; nothing deleted to be safe"
            ));
            continue;
        }
        match local_rows(db, table, key_col, label_col).await {
            Ok(rows) => plan.extend(
                rows.into_iter()
                    .filter(|(_, key, _)| !wanted.contains(key))
                    .map(|(id, _, label)| Deletion { section, id, label }),
            ),
            Err(e) => warnings.push(format!("{section}: {e}")),
        }
    }
    plan
}

/// Delete the planned items through the app's own delete paths (Keychain secrets, library files
/// moved to the legacy backups, account config dirs, project links…).
async fn execute_mirror(
    env: &ApplyEnv<'_>,
    db: &Db,
    plan: Vec<Deletion>,
    extra: &mut Extra,
    warnings: &mut Vec<String>,
) {
    let app_data = env.app_data.as_deref();
    let todo_ids: Vec<String> = plan
        .iter()
        .filter(|d| d.section == "todos")
        .map(|d| d.id.clone())
        .collect();
    if !todo_ids.is_empty() {
        match todos::delete_todos_inner(db, &todo_ids).await {
            Ok(()) => {
                *extra.deleted.entry("todos".to_string()).or_default() += todo_ids.len() as u32
            }
            Err(e) => warnings.push(format!("todos: could not delete: {e}")),
        }
    }
    for d in plan.iter().filter(|d| d.section != "todos") {
        let id = d.id.as_str();
        let result = match d.section {
            "notes" => notes::delete_note_inner(db, id).await,
            "skillGroups" => {
                crate::commands::groups::delete_group_inner(db, GroupKind::Skill, id).await
            }
            "ruleGroups" => {
                crate::commands::groups::delete_group_inner(db, GroupKind::Rule, id).await
            }
            "skills" => skills::delete_skill_inner(db, id, true).await,
            "rules" => rules::delete_rule_inner(db, id, true).await,
            "mcpServers" => mcp::delete_mcp_server_inner(db, id).await,
            "vpsServers" => vps_servers::delete_vps_server_inner(env.app, db, id).await,
            "githubAccounts" => github_accounts::delete_github_account_inner(db, id).await,
            "gitlabAccounts" => gitlab_accounts::delete_gitlab_account_inner(db, id).await,
            "googleAccounts" => google::delete_google_account_inner(db, id).await,
            "claudeAccounts" => claude_accounts::delete_claude_account_inner(db, id).await,
            "awsAccounts" => aws_accounts::delete_aws_account_inner(app_data, db, id).await,
            "azureAccounts" => azure_accounts::delete_azure_account_inner(app_data, db, id).await,
            other => Err(format!("unknown section {other}")),
        };
        match result {
            Ok(()) => *extra.deleted.entry(d.section.to_string()).or_default() += 1,
            Err(e) => warnings.push(format!(
                "{} '{}': could not delete: {e}",
                d.section, d.label
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn test_db() -> Db {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool
    }

    async fn exec(db: &Db, sqls: &[&str]) {
        for sql in sqls {
            sqlx::query(sql).execute(db).await.unwrap();
        }
    }

    async fn count(db: &Db, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
    }

    fn payload(categories: SyncCategories, sections: serde_json::Value) -> SyncPayload {
        let mut bundle =
            json!({ "format": "devdy-settings-backup", "version": 1, "exported_at": "" });
        for (k, v) in sections.as_object().unwrap() {
            bundle[k] = v.clone();
        }
        SyncPayload {
            format: FORMAT.into(),
            version: VERSION,
            categories,
            bundle: serde_json::from_value(bundle).unwrap(),
            ..Default::default()
        }
    }

    fn env(dir: &std::path::Path) -> ApplyEnv<'static> {
        ApplyEnv {
            app: None,
            app_data: Some(dir.to_path_buf()),
        }
    }

    fn temp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-sync-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn counts<'a>(report: &'a SyncReport, section: &str) -> &'a SyncCounts {
        report
            .counts
            .get(section)
            .unwrap_or_else(|| panic!("no {section} in {report:?}"))
    }

    fn todo(id: &str, text: &str, project: Option<&str>) -> serde_json::Value {
        json!({
            "id": id, "text": text, "done": false, "position": 0, "created_at": "c",
            "due_date": "2026-11-05",
            "project": project.map(|p| json!({ "name": p, "path": format!("/source/{p}") })),
            "focus": { "due_time": "09:30", "priority": 1, "status": "todo" },
        })
    }

    fn note(id: &str) -> serde_json::Value {
        json!({ "id": id, "title": id, "content": "c", "position": 0, "created_at": "c", "updated_at": "u" })
    }

    #[test]
    fn sealed_bundle_round_trips_and_rejects_a_wrong_key() {
        let cats = SyncCategories {
            todos: true,
            ..Default::default()
        };
        let p = payload(cats, json!({ "todos": [todo("t1", "buy milk", None)] }));
        let mut p = p;
        p.secrets
            .github
            .insert("gh".into(), "ghp_secret_token".into());
        let key = [7u8; 32];
        let sealed = seal_payload(&p, &key).unwrap();
        // Everything is encrypted, not just the secrets.
        let raw = String::from_utf8_lossy(&sealed);
        assert!(!raw.contains("buy milk") && !raw.contains("ghp_secret_token"));

        let opened = open_payload(&sealed, &key).unwrap();
        assert_eq!(opened.bundle.todos[0].text, "buy milk");
        assert_eq!(
            opened.secrets.github.get("gh").map(String::as_str),
            Some("ghp_secret_token")
        );
        assert!(opened.categories.todos && !opened.categories.notes);

        let err = open_payload(&sealed, &[8u8; 32]).err().unwrap();
        assert!(err.contains("Wrong master password"), "{err}");
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(open_payload(&tampered, &key).is_err());
        assert!(open_payload(b"DDS1short", &key).is_err());
        // Two seals of the same data differ (random nonce).
        assert_ne!(seal_payload(&p, &key).unwrap(), sealed);
    }

    #[tokio::test]
    async fn todos_upsert_without_mirror_and_resolve_projects_by_name() {
        let db = test_db().await;
        let dir = temp_dir();
        exec(&db, &[
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p-shop', 'shop', '/here/shop', 'c')",
            "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t1', 'old', 0, 0, 'c', 'p-shop')",
            "INSERT INTO todos (id, text, done, position, created_at) VALUES ('t-local', 'mine', 0, 1, 'c')",
        ]).await;
        let cats = SyncCategories {
            todos: true,
            ..Default::default()
        };
        let todos = json!({ "todos": [
            // Its project doesn't exist here: the link is cleared, not kept.
            todo("t1", "new", Some("ghost")),
            // Same project name, different path on the source: matched by name.
            todo("t2", "fresh", Some("shop")),
        ]});
        let report =
            apply_payload(&env(&dir), &db, payload(cats.clone(), todos.clone()), &cats).await;
        let c = counts(&report, "todos");
        assert_eq!((c.created, c.updated, c.deleted), (1, 1, 0));
        // mirror = false: the local-only todo stays.
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM todos WHERE id = 't-local'").await,
            1
        );
        let t1: (String, Option<String>, Option<String>, String) = sqlx::query_as(
            "SELECT text, project_id, due_time, due_date FROM todos WHERE id = 't1'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(
            t1,
            (
                "new".into(),
                None,
                Some("09:30".into()),
                "2026-11-05".into()
            )
        );
        let t2: Option<String> = sqlx::query_scalar("SELECT project_id FROM todos WHERE id = 't2'")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(t2.as_deref(), Some("p-shop"));

        // mirror = true: the local-only one goes.
        let mirror = SyncCategories {
            mirror: true,
            ..cats
        };
        let report = apply_payload(&env(&dir), &db, payload(mirror.clone(), todos), &mirror).await;
        assert_eq!(counts(&report, "todos").deleted, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM todos").await, 2);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM todos WHERE id = 't-local'").await,
            0
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn notes_and_slack_threads_resolve_projects_by_name_only() {
        let db = test_db().await;
        let dir = temp_dir();
        // 'beta' here sits at the path 'alpha' has on the source.
        exec(&db, &[
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p-alpha', 'alpha', '/elsewhere', 'c')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p-beta', 'beta', '/source/alpha', 'c')",
        ]).await;
        let cats = SyncCategories {
            notes: true,
            slack_threads: true,
            ..Default::default()
        };
        let project = json!({ "name": "alpha", "path": "/source/alpha" });
        let mut n = note("n1");
        n["project"] = project.clone();
        let thread = json!({
            "id": "st1", "title": "thread", "position": 0, "created_at": "c", "updated_at": "u",
            "channel_id": "C1", "thread_ts": "1.0", "project": project,
        });
        let sections = json!({ "notes": [n], "slack_threads": [thread] });
        apply_payload(&env(&dir), &db, payload(cats.clone(), sections), &cats).await;
        let note_project: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM notes WHERE id = 'n1'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(note_project.as_deref(), Some("p-alpha"));
        let thread_project: Option<String> = sqlx::query_scalar(
            "SELECT project_id FROM slack_threads WHERE channel_id = 'C1' AND thread_ts = '1.0'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(thread_project.as_deref(), Some("p-alpha"));

        // A backup restore still prefers the path.
        let entry: backup::NoteEntry = serde_json::from_value(json!({
            "id": "n2", "title": "n2", "content": "c", "position": 0, "created_at": "c",
            "updated_at": "u", "project": { "name": "alpha", "path": "/source/alpha" },
        }))
        .unwrap();
        backup::import_note(&db, &entry).await.unwrap();
        let backup_project: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM notes WHERE id = 'n2'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(backup_project.as_deref(), Some("p-beta"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn projects_apply_only_to_an_existing_name() {
        let db = test_db().await;
        let dir = temp_dir();
        let missing = dir.join("not-created");
        exec(&db, &[
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p-shop', 'shop', '/nowhere/b/shop', 'c')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m1', 'slack', 'stdio', 'c')",
            "INSERT INTO project_builtin_mcp_disabled (project_id, name, created_at) VALUES ('p-shop', 'devdy', 'c')",
        ]).await;
        let cats = SyncCategories {
            projects: true,
            mirror: true,
            ..Default::default()
        };
        let mut p = payload(
            cats.clone(),
            json!({ "projects": [
                {
                    "id": "src-1", "name": "shop", "path": missing.to_string_lossy(), "default_engine": "codex",
                    "created_at": "c", "github_project_board_url": "https://board",
                    "repos": [{ "name": "api", "path": "/source/shop/api", "created_at": "c" }],
                    "mcp_servers": ["slack"],
                    "servers": [{ "server": "prod", "role": "production" }],
                },
                { "id": "src-2", "name": "ghost", "path": missing.join("ghost").to_string_lossy(), "default_engine": "claude", "created_at": "c" },
            ]}),
        );
        p.builtin_mcp_disabled
            .insert("shop".into(), vec!["google".into()]);
        let report = apply_payload(&env(&dir), &db, p, &cats).await;

        let c = counts(&report, "projects");
        assert_eq!((c.created, c.updated, c.skipped, c.deleted), (0, 1, 1, 0));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM projects").await, 1);
        let (path, engine, board): (String, String, Option<String>) = sqlx::query_as(
            "SELECT path, default_engine, github_project_board_url FROM projects WHERE id = 'p-shop'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        // Path untouched, no repo added, no folder created.
        assert_eq!(path, "/nowhere/b/shop");
        assert_eq!(
            (engine.as_str(), board.as_deref()),
            ("codex", Some("https://board"))
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM repos").await, 0);
        assert!(!missing.exists());
        assert_eq!(count(&db, "SELECT COUNT(*) FROM project_mcp_servers WHERE project_id = 'p-shop' AND server_id = 'm1'").await, 1);
        let off: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM project_builtin_mcp_disabled WHERE project_id = 'p-shop' ORDER BY name",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(off, vec!["google".to_string()]);
        // The VPS server doesn't exist here: a warning, not an error.
        assert!(
            report
                .warnings
                .iter()
                .any(|w| w.contains("VPS server 'prod' not found")),
            "{:?}",
            report.warnings
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn claude_aws_azure_accounts_carry_no_credentials() {
        let source = test_db().await;
        exec(&source, &[
            "INSERT INTO aws_accounts (id, label, auth_method, region, access_key_id, created_at) \
             VALUES ('a1', 'prod', 'keys', 'eu-west-1', 'AKIASOURCEKEY', 'c')",
            "INSERT INTO claude_accounts (id, label, config_dir, email, status, created_at) \
             VALUES ('c1', 'work', '/source/claude-dir', 'me@example.com', 'ok', 'c')",
            "INSERT INTO claude_accounts (id, label, config_dir, status, created_at) \
             VALUES ('c2', 'fresh', '/source/claude-dir-2', 'ok', 'c')",
            "INSERT INTO azure_accounts (id, label, tenant, user_name, created_at) \
             VALUES ('z1', 'corp', 'contoso', 'source-user', 'c')",
        ]).await;
        let mut secret = SecretPayload::default();
        let accounts = backup::collect_accounts(&source, false, &mut secret)
            .await
            .unwrap();
        let mut bundle = Bundle {
            aws_accounts: accounts.aws,
            azure_accounts: accounts.azure,
            claude_accounts: accounts.claude,
            ..Default::default()
        };
        bundle.format = "devdy-settings-backup".into();
        let json = serde_json::to_string(&bundle).unwrap();
        for leaked in [
            "AKIASOURCEKEY",
            "/source/claude-dir",
            "source-user",
            "me@example.com",
        ] {
            assert!(!json.contains(leaked), "{leaked} leaked: {json}");
        }

        let target = test_db().await;
        let dir = temp_dir();
        exec(&target, &[
            "INSERT INTO aws_accounts (id, label, auth_method, region, access_key_id, created_at) \
             VALUES ('a9', 'prod', 'keys', 'us-east-1', 'AKIALOCALKEY', 'c')",
            "INSERT INTO claude_accounts (id, label, config_dir, status, is_default, created_at) \
             VALUES ('c9', 'work', '/local/claude', 'ok', 1, 'c')",
        ]).await;
        let cats = SyncCategories {
            accounts: true,
            ..Default::default()
        };
        let p = SyncPayload {
            format: FORMAT.into(),
            version: VERSION,
            categories: cats.clone(),
            bundle,
            ..Default::default()
        };
        let report = apply_payload(&env(&dir), &target, p, &cats).await;
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // Existing AWS: metadata updated, local key id kept.
        let (region, key): (String, Option<String>) =
            sqlx::query_as("SELECT region, access_key_id FROM aws_accounts WHERE label = 'prod'")
                .fetch_one(&target)
                .await
                .unwrap();
        assert_eq!(
            (region.as_str(), key.as_deref()),
            ("eu-west-1", Some("AKIALOCALKEY"))
        );
        // Existing Claude: untouched login.
        let (dir_kept, status): (String, String) =
            sqlx::query_as("SELECT config_dir, status FROM claude_accounts WHERE label = 'work'")
                .fetch_one(&target)
                .await
                .unwrap();
        assert_eq!(
            (dir_kept.as_str(), status.as_str()),
            ("/local/claude", "ok")
        );
        // New Claude: logged out, own config dir here, not default.
        let (fresh_dir, status, default): (String, String, i64) = sqlx::query_as(
            "SELECT config_dir, status, is_default FROM claude_accounts WHERE label = 'fresh'",
        )
        .fetch_one(&target)
        .await
        .unwrap();
        assert!(
            fresh_dir.starts_with(&*dir.to_string_lossy()),
            "{fresh_dir}"
        );
        assert_eq!((status.as_str(), default), ("unknown", 0));
        // New Azure: no signed-in user.
        let user: Option<String> =
            sqlx::query_scalar("SELECT user_name FROM azure_accounts WHERE label = 'corp'")
                .fetch_one(&target)
                .await
                .unwrap();
        assert_eq!(user, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unticked_categories_are_left_alone() {
        let db = test_db().await;
        let dir = temp_dir();
        exec(&db, &[
            "INSERT INTO todos (id, text, done, position, created_at) VALUES ('t-local', 'mine', 0, 0, 'c')",
            "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n-local', 'n', '', 0, 'c', 'u')",
        ]).await;
        let sent = SyncCategories {
            todos: true,
            notes: true,
            ..Default::default()
        };
        let p = payload(
            sent,
            json!({ "todos": [todo("t-new", "x", None)], "notes": [note("n-new")] }),
        );
        // Only notes ticked, mirror on.
        let cats = SyncCategories {
            notes: true,
            mirror: true,
            ..Default::default()
        };
        let report = apply_payload(&env(&dir), &db, p, &cats).await;
        assert!(!report.counts.contains_key("todos"));
        let todos: Vec<String> = sqlx::query_scalar("SELECT id FROM todos")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(todos, vec!["t-local".to_string()]);
        let notes: Vec<String> = sqlx::query_scalar("SELECT id FROM notes")
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(notes, vec!["n-new".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mirror_deletes_only_mirrorable_ticked_categories() {
        let db = test_db().await;
        let dir = temp_dir();
        exec(&db, &[
            "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n-local', 'n', '', 0, 'c', 'u')",
            "INSERT INTO todos (id, text, done, position, created_at) VALUES ('t-local', 'mine', 0, 0, 'c')",
            "INSERT INTO skills (id, name, description, target, source_path, updated_at) \
             VALUES ('s-local', 'local-skill', 'd', 'claude', '/nowhere/skill', 'now')",
            "INSERT INTO skill_groups (id, name, created_at, updated_at) VALUES ('g-local', 'local-group', 'c', 'u')",
            "INSERT INTO skill_groups (id, name, created_at, updated_at) VALUES ('g-keep', 'core', 'c', 'u')",
            "INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('c-old', 'old', '/x/old', 'c')",
            "INSERT INTO claude_accounts (id, label, config_dir, created_at) VALUES ('c-keep', 'work', '/x/work', 'c')",
            "INSERT INTO projects (id, name, path, created_at) VALUES ('p-local', 'local-project', '/x/p', 'c')",
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('theme', 'dark')",
            "INSERT INTO slack_threads (id, title, position, created_at, updated_at, channel_id, thread_ts, \
             fetch_status, search_text) VALUES ('th-local', 'T', 0, 'c', 'u', 'C1', '1.0', 'ready', '')",
        ]).await;
        let sent = SyncCategories {
            todos: true,
            notes: true,
            skills_rules: true,
            accounts: true,
            projects: true,
            settings: true,
            slack_threads: true,
            secret_notes: true,
            ..Default::default()
        };
        let p = payload(
            sent,
            json!({
                "todos": [todo("t-src", "x", None)],
                "notes": [note("n-src")],
                "skill_groups": [{ "name": "core" }],
                "claude_accounts": [{ "id": "c-src", "label": "work" }],
                "projects": [{ "id": "p-src", "name": "elsewhere", "path": "/s/p", "default_engine": "claude", "created_at": "c" }],
                "settings": { "language": "vi" },
                "slack_threads": [{ "id": "th-src", "title": "S", "position": 0, "created_at": "c", "updated_at": "u",
                                    "channel_id": "C2", "thread_ts": "2.0" }],
            }),
        );
        // Everything ticked except todos.
        let cats = SyncCategories {
            todos: false,
            mirror: true,
            ..p.categories.clone()
        };
        let report = apply_payload(&env(&dir), &db, p, &cats).await;

        assert_eq!(counts(&report, "notes").deleted, 1);
        assert_eq!(counts(&report, "skillGroups").deleted, 1);
        assert_eq!(counts(&report, "claudeAccounts").deleted, 1);
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM notes WHERE id = 'n-local'").await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM skill_groups WHERE id = 'g-local'"
            )
            .await,
            0
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM skill_groups WHERE id = 'g-keep'").await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM claude_accounts WHERE id = 'c-old'"
            )
            .await,
            0
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM claude_accounts WHERE id = 'c-keep'"
            )
            .await,
            1
        );
        // The source sent no skills at all: nothing deleted, with a warning.
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM skills WHERE id = 's-local'").await,
            1
        );
        assert!(
            report.warnings.iter().any(|w| w.starts_with("skills:")),
            "{:?}",
            report.warnings
        );
        // Never mirrored: todos (unticked), projects, settings, Slack threads.
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM todos WHERE id = 't-local'").await,
            1
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM projects WHERE id = 'p-local'").await,
            1
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM settings WHERE key = 'theme' AND value = 'dark'"
            )
            .await,
            1
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM slack_threads").await, 2);
        for section in ["projects", "settings", "slackThreads", "secretNotes"] {
            assert_eq!(
                report.counts.get(section).map_or(0, |c| c.deleted),
                0,
                "{section}"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn mirror_skips_empty_or_missing_categories() {
        let db = test_db().await;
        exec(&db, &[
            "INSERT INTO todos (id, text, done, position, created_at) VALUES ('t-local', 'mine', 0, 0, 'c')",
            "INSERT INTO notes (id, title, content, position, created_at, updated_at) VALUES ('n-local', 'n', '', 0, 'c', 'u')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m-keep', 'keep', 'stdio', 'c')",
            "INSERT INTO mcp_servers (id, name, transport, created_at) VALUES ('m-extra', 'extra', 'stdio', 'c')",
            "INSERT INTO servers (id, label, host, username, auth_method, created_at) VALUES ('v1', 'prod', 'h', 'u', 'agent', 'c')",
            "INSERT INTO servers (id, label, host, username, auth_method, created_at) VALUES ('v2', 'old', 'h', 'u', 'agent', 'c')",
            "INSERT INTO github_accounts (id, label, created_at) VALUES ('gh1', 'work', 'c')",
            "INSERT INTO github_accounts (id, label, created_at) VALUES ('gh2', 'old', 'c')",
            "INSERT INTO gitlab_accounts (id, label, created_at) VALUES ('gl1', 'lab', 'c')",
        ]).await;
        // An older source that didn't send todos; notes sent but empty.
        let sent = SyncCategories {
            notes: true,
            mcp_servers: true,
            vps_servers: true,
            accounts: true,
            ..Default::default()
        };
        let mcp = |id: &str, name: &str| {
            json!({
                "id": id, "name": name, "description": "", "transport": "stdio", "command": null, "args": [],
                "url": null, "env_keys": [], "header_keys": [], "enabled": true, "created_at": "c",
            })
        };
        let p = payload(
            sent,
            json!({
                "notes": [],
                "mcp_servers": [mcp("m-src", "keep")],
                "vps_servers": [{ "id": "v-src", "label": "prod", "host": "h", "port": 22, "username": "u", "auth_method": "agent" }],
                "github_accounts": [{ "id": "gh-src", "label": "work" }],
            }),
        );
        let cats = SyncCategories {
            todos: true,
            notes: true,
            mcp_servers: true,
            vps_servers: true,
            accounts: true,
            mirror: true,
            ..Default::default()
        };
        let mut warnings = Vec::new();
        // Only planned: deleting these goes through the Keychain.
        let plan = plan_mirror(&db, &p, &cats, &mut warnings).await;
        let planned: Vec<(&str, &str)> = plan.iter().map(|d| (d.section, d.id.as_str())).collect();
        assert_eq!(
            planned,
            vec![
                ("mcpServers", "m-extra"),
                ("vpsServers", "v2"),
                ("githubAccounts", "gh2")
            ]
        );
        for section in ["todos", "notes", "gitlabAccounts"] {
            assert!(
                warnings
                    .iter()
                    .any(|w| w.starts_with(&format!("{section}:"))),
                "{section}: {warnings:?}"
            );
        }

        // Applying todos + notes only: nothing goes.
        let cats = SyncCategories {
            todos: true,
            notes: true,
            mirror: true,
            ..Default::default()
        };
        let dir = temp_dir();
        let report = apply_payload(&env(&dir), &db, p, &cats).await;
        assert_eq!(count(&db, "SELECT COUNT(*) FROM todos").await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM notes").await, 1);
        assert!(report.counts.values().all(|c| c.deleted == 0), "{report:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
