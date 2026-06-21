use crate::db::Db;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub description: String,
    pub target: String,
    pub source_path: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RuleContent {
    pub rule: Rule,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateRulePayload {
    pub name: String,
    pub description: String,
    pub target: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRulePayload {
    pub id: String,
    pub name: String,
    pub description: String,
    pub target: String,
    pub content: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct AppliedRule {
    pub rule_id: String,
    pub rule_name: String,
    pub rule_description: String,
    pub target: String,
    pub has_claude: bool,
    pub has_codex: bool,
    pub applied_at: String,
}

fn rules_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap().join("rules")
}

pub fn content_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn validate_rule_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Rule name cannot be empty".to_string());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err("Rule name must only contain letters, numbers, hyphens, and underscores".to_string());
    }
    Ok(())
}

fn validate_target(target: &str) -> Result<(), String> {
    match target {
        "claude" | "codex" | "both" => Ok(()),
        _ => Err("target must be one of: claude, codex, both".to_string()),
    }
}

fn applies_claude(target: &str) -> bool {
    target == "claude" || target == "both"
}
fn applies_codex(target: &str) -> bool {
    target == "codex" || target == "both"
}

/// Strip YAML frontmatter, returning only the markdown body (for Codex AGENTS.md, which has no
/// frontmatter / path-scoping support). Claude keeps the whole file as-is.
fn strip_frontmatter(content: &str) -> String {
    if content.starts_with("---") {
        let rest = &content[3..];
        if let Some(end) = rest.find("\n---") {
            // skip past the closing '---' line
            let after = &rest[end + 4..];
            return after.trim_start_matches('\n').to_string();
        }
    }
    content.to_string()
}

// ---- Managed block helpers (Codex AGENTS.md) ---------------------------------
// Block insert/extract/remove logic is shared with skills; see `agents_block`.

use crate::commands::agents_block;

fn upsert_agents_block(agents_path: &Path, name: &str, body: &str) -> Result<(), String> {
    agents_block::upsert(agents_path, "rule", name, body)
}

fn extract_agents_block(agents_path: &Path, name: &str) -> Option<String> {
    agents_block::extract(agents_path, "rule", name)
}

fn remove_agents_block(agents_path: &Path, name: &str) {
    agents_block::remove(agents_path, "rule", name)
}

// ---- Claude rule file helpers ------------------------------------------------

fn claude_rule_path(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path)
        .join(".claude")
        .join("rules")
        .join(format!("{}.md", name))
}

fn agents_path(project_path: &str) -> PathBuf {
    Path::new(project_path).join("AGENTS.md")
}

// ---- CRUD --------------------------------------------------------------------

#[tauri::command]
pub async fn list_rules(db: State<'_, Db>) -> Result<Vec<Rule>, String> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT id, name, description, target, source_path, updated_at FROM rules ORDER BY name")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| Rule {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get("description"),
        target: row.get("target"),
        source_path: row.get("source_path"),
        updated_at: row.get("updated_at"),
    }).collect())
}

#[tauri::command]
pub async fn get_rule(db: State<'_, Db>, id: String) -> Result<RuleContent, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT id, name, description, target, source_path, updated_at FROM rules WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let source_path: String = row.get("source_path");
    let content = fs::read_to_string(&source_path).unwrap_or_default();

    Ok(RuleContent {
        rule: Rule {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            target: row.get("target"),
            source_path,
            updated_at: row.get("updated_at"),
        },
        content,
    })
}

#[tauri::command]
pub async fn create_rule(
    app: AppHandle,
    db: State<'_, Db>,
    payload: CreateRulePayload,
) -> Result<Rule, String> {
    validate_rule_name(&payload.name)?;
    validate_target(&payload.target)?;
    if payload.description.is_empty() {
        return Err("Description is required".to_string());
    }

    let existing = sqlx::query("SELECT id FROM rules WHERE name = ?")
        .bind(&payload.name)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    if existing.is_some() {
        return Err(format!("Rule '{}' already exists", payload.name));
    }

    let id = Uuid::new_v4().to_string();
    let dir = rules_dir(&app);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let source_path = dir.join(format!("{}.md", payload.name));
    fs::write(&source_path, &payload.content).map_err(|e| e.to_string())?;

    let source_path = source_path.to_string_lossy().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query("INSERT INTO rules (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.target)
        .bind(&source_path)
        .bind(&now)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(Rule {
        id,
        name: payload.name,
        description: payload.description,
        target: payload.target,
        source_path,
        updated_at: now,
    })
}

#[tauri::command]
pub async fn update_rule(
    app: AppHandle,
    db: State<'_, Db>,
    payload: UpdateRulePayload,
) -> Result<Rule, String> {
    use sqlx::Row;
    validate_rule_name(&payload.name)?;
    validate_target(&payload.target)?;
    if payload.description.is_empty() {
        return Err("Description is required".to_string());
    }

    let row = sqlx::query("SELECT name, source_path FROM rules WHERE id = ?")
        .bind(&payload.id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Rule not found: {}", e))?;
    let old_name: String = row.get("name");
    let old_path: String = row.get("source_path");

    // If renamed, drop old artifacts from every applied project first.
    if old_name != payload.name {
        let projects = sqlx::query(
            "SELECT p.path FROM project_rules pr JOIN projects p ON p.id = pr.project_id WHERE pr.rule_id = ?"
        )
        .bind(&payload.id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
        for prow in &projects {
            let ppath: String = prow.get("path");
            let _ = fs::remove_file(claude_rule_path(&ppath, &old_name));
            remove_agents_block(&agents_path(&ppath), &old_name);
        }
    }

    let new_source_path = if old_name != payload.name {
        let new_path = rules_dir(&app).join(format!("{}.md", payload.name));
        let _ = fs::rename(&old_path, &new_path);
        new_path.to_string_lossy().to_string()
    } else {
        old_path
    };

    fs::write(&new_source_path, &payload.content).map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query("UPDATE rules SET name = ?, description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?")
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.target)
        .bind(&new_source_path)
        .bind(&now)
        .bind(&payload.id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let _ = sync_rule_to_projects(&db, &payload.id).await;

    Ok(Rule {
        id: payload.id,
        name: payload.name,
        description: payload.description,
        target: payload.target,
        source_path: new_source_path,
        updated_at: now,
    })
}

#[tauri::command]
pub async fn delete_rule(db: State<'_, Db>, id: String) -> Result<(), String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT name, source_path FROM rules WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Rule not found: {}", e))?;
    let name: String = row.get("name");
    let source_path: String = row.get("source_path");

    // Remove applied artifacts from every project.
    let projects = sqlx::query(
        "SELECT p.path FROM project_rules pr JOIN projects p ON p.id = pr.project_id WHERE pr.rule_id = ?"
    )
    .bind(&id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    for prow in &projects {
        let ppath: String = prow.get("path");
        let _ = fs::remove_file(claude_rule_path(&ppath, &name));
        remove_agents_block(&agents_path(&ppath), &name);
    }

    let _ = fs::remove_file(&source_path);

    sqlx::query("DELETE FROM rules WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn export_rule(db: State<'_, Db>, id: String, dest_path: String) -> Result<(), String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT source_path FROM rules WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let source_path: String = row.get("source_path");
    fs::copy(&source_path, &dest_path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn import_rule(
    app: AppHandle,
    db: State<'_, Db>,
    src_path: String,
) -> Result<Rule, String> {
    use sqlx::Row;
    let content = fs::read_to_string(&src_path).map_err(|e| e.to_string())?;
    let (name, description, target) = parse_rule_frontmatter(&content);

    let name = if name.is_empty() {
        Path::new(&src_path)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "imported".to_string())
    } else {
        name
    };
    validate_rule_name(&name)?;
    let description = if description.is_empty() { name.clone() } else { description };
    let target = if target.is_empty() { "both".to_string() } else { target };

    let dir = rules_dir(&app);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let source_path = dir.join(format!("{}.md", name));
    fs::write(&source_path, &content).map_err(|e| e.to_string())?;
    let source_path = source_path.to_string_lossy().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let existing = sqlx::query("SELECT id FROM rules WHERE name = ?")
        .bind(&name)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let id = if let Some(row) = existing {
        let id: String = row.get("id");
        sqlx::query("UPDATE rules SET description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?")
            .bind(&description)
            .bind(&target)
            .bind(&source_path)
            .bind(&now)
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        id
    } else {
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO rules (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(&name)
            .bind(&description)
            .bind(&target)
            .bind(&source_path)
            .bind(&now)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        id
    };

    Ok(Rule { id, name, description, target, source_path, updated_at: now })
}

#[tauri::command]
pub async fn open_rules_folder(app: AppHandle) -> Result<(), String> {
    let dir = rules_dir(&app);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.to_string_lossy().to_string();

    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(&path).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer").arg(&path).spawn().map_err(|e| e.to_string())?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?;

    Ok(())
}

fn parse_rule_frontmatter(content: &str) -> (String, String, String) {
    if content.starts_with("---") {
        let rest = &content[3..];
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            let pick = |key: &str| -> String {
                fm.lines()
                    .find(|l| l.trim_start().starts_with(key))
                    .map(|l| {
                        let after = &l.trim_start()[key.len()..];
                        after.trim().trim_matches('"').trim_matches('\'').to_string()
                    })
                    .unwrap_or_default()
            };
            return (pick("name:"), pick("description:"), pick("target:"));
        }
    }
    (String::new(), String::new(), String::new())
}

// ---- Apply / remove per project ----------------------------------------------

#[tauri::command]
pub async fn get_applied_rules(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<AppliedRule>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT pr.rule_id, r.name as rule_name, r.description as rule_description,
                pr.target, pr.synced_hash_claude, pr.synced_hash_codex, pr.applied_at
         FROM project_rules pr JOIN rules r ON r.id = pr.rule_id
         WHERE pr.project_id = ? ORDER BY r.name"
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| {
        let claude: Option<String> = row.get("synced_hash_claude");
        let codex: Option<String> = row.get("synced_hash_codex");
        AppliedRule {
            rule_id: row.get("rule_id"),
            rule_name: row.get("rule_name"),
            rule_description: row.get("rule_description"),
            target: row.get("target"),
            has_claude: claude.is_some(),
            has_codex: codex.is_some(),
            applied_at: row.get("applied_at"),
        }
    }).collect())
}

#[tauri::command]
pub async fn apply_rule(
    db: State<'_, Db>,
    project_id: String,
    rule_id: String,
) -> Result<(), String> {
    use sqlx::Row;
    let rule_row = sqlx::query("SELECT name, target, source_path FROM rules WHERE id = ?")
        .bind(&rule_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Rule not found: {}", e))?;
    let name: String = rule_row.get("name");
    let target: String = rule_row.get("target");
    let source_path: String = rule_row.get("source_path");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Project not found: {}", e))?;
    let project_path: String = project_row.get("path");

    let content = fs::read_to_string(&source_path).unwrap_or_default();
    let (hash_claude, hash_codex) =
        write_rule_artifacts(&project_path, &name, &target, &content)?;
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT OR REPLACE INTO project_rules
         (project_id, rule_id, target, synced_hash_claude, synced_hash_codex, applied_at)
         VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(&project_id)
    .bind(&rule_id)
    .bind(&target)
    .bind(&hash_claude)
    .bind(&hash_codex)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Write Claude file + Codex block per `target`; returns (claude_hash, codex_hash) as applicable.
fn write_rule_artifacts(
    project_path: &str,
    name: &str,
    target: &str,
    content: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let mut hash_claude = None;
    let mut hash_codex = None;

    if applies_claude(target) {
        let dest = claude_rule_path(project_path, name);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        fs::write(&dest, content).map_err(|e| e.to_string())?;
        hash_claude = Some(content_hash(content));
    }
    if applies_codex(target) {
        let body = strip_frontmatter(content);
        upsert_agents_block(&agents_path(project_path), name, &body)?;
        hash_codex = Some(content_hash(body.trim_end_matches('\n')));
    }
    Ok((hash_claude, hash_codex))
}

#[tauri::command]
pub async fn remove_rule_from_project(
    db: State<'_, Db>,
    project_id: String,
    rule_id: String,
) -> Result<(), String> {
    use sqlx::Row;
    let rule_row = sqlx::query("SELECT name FROM rules WHERE id = ?")
        .bind(&rule_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let name: String = rule_row.get("name");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let project_path: String = project_row.get("path");

    let _ = fs::remove_file(claude_rule_path(&project_path, &name));
    remove_agents_block(&agents_path(&project_path), &name);

    sqlx::query("DELETE FROM project_rules WHERE project_id = ? AND rule_id = ?")
        .bind(&project_id)
        .bind(&rule_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Conflicts ----------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RuleSyncConflict {
    pub id: String,
    pub project_id: String,
    pub rule_id: String,
    pub rule_name: String,
    pub project_name: String,
    pub engine: String,
    pub detected_at: String,
    pub local_hash: String,
    pub source_hash: String,
    pub resolved: bool,
}

#[tauri::command]
pub async fn list_rule_sync_conflicts(db: State<'_, Db>) -> Result<Vec<RuleSyncConflict>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT rc.id, rc.project_id, rc.rule_id, r.name as rule_name, p.name as project_name,
                rc.engine, rc.detected_at, rc.local_hash, rc.source_hash, rc.resolved
         FROM rule_sync_conflicts rc
         JOIN rules r ON r.id = rc.rule_id
         JOIN projects p ON p.id = rc.project_id
         WHERE rc.resolved = 0
         ORDER BY rc.detected_at DESC"
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| {
        let resolved: i64 = row.get("resolved");
        RuleSyncConflict {
            id: row.get("id"),
            project_id: row.get("project_id"),
            rule_id: row.get("rule_id"),
            rule_name: row.get("rule_name"),
            project_name: row.get("project_name"),
            engine: row.get("engine"),
            detected_at: row.get("detected_at"),
            local_hash: row.get("local_hash"),
            source_hash: row.get("source_hash"),
            resolved: resolved != 0,
        }
    }).collect())
}

#[tauri::command]
pub async fn resolve_rule_sync_conflict(
    db: State<'_, Db>,
    conflict_id: String,
    overwrite: bool,
) -> Result<(), String> {
    use sqlx::Row;
    if overwrite {
        let row = sqlx::query(
            "SELECT rc.project_id, rc.rule_id, rc.engine, r.name, r.source_path, p.path as project_path
             FROM rule_sync_conflicts rc
             JOIN rules r ON r.id = rc.rule_id
             JOIN projects p ON p.id = rc.project_id
             WHERE rc.id = ?"
        )
        .bind(&conflict_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

        let project_id: String = row.get("project_id");
        let rule_id: String = row.get("rule_id");
        let engine: String = row.get("engine");
        let name: String = row.get("name");
        let source_path: String = row.get("source_path");
        let project_path: String = row.get("project_path");

        let content = fs::read_to_string(&source_path).unwrap_or_default();
        if engine == "claude" {
            let (h, _) = write_rule_artifacts(&project_path, &name, "claude", &content)?;
            sqlx::query("UPDATE project_rules SET synced_hash_claude = ? WHERE project_id = ? AND rule_id = ?")
                .bind(&h)
                .bind(&project_id)
                .bind(&rule_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        } else {
            let (_, h) = write_rule_artifacts(&project_path, &name, "codex", &content)?;
            sqlx::query("UPDATE project_rules SET synced_hash_codex = ? WHERE project_id = ? AND rule_id = ?")
                .bind(&h)
                .bind(&project_id)
                .bind(&rule_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    sqlx::query("UPDATE rule_sync_conflicts SET resolved = 1 WHERE id = ?")
        .bind(&conflict_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---- Auto-sync ----------------------------------------------------------------

/// Sync a rule to every project that has it applied. Handles target changes (removing artifacts
/// for engines no longer targeted) and per-engine conflict detection.
pub async fn sync_rule_to_projects(db: &Db, rule_id: &str) -> Result<()> {
    use sqlx::Row;

    let rule = sqlx::query("SELECT name, target, source_path FROM rules WHERE id = ?")
        .bind(rule_id)
        .fetch_one(db)
        .await?;
    let name: String = rule.get("name");
    let target: String = rule.get("target");
    let source_path: String = rule.get("source_path");
    let content = fs::read_to_string(&source_path).unwrap_or_default();
    let body = strip_frontmatter(&content);
    let new_claude_hash = content_hash(&content);
    let new_codex_hash = content_hash(body.trim_end_matches('\n'));

    let rows = sqlx::query(
        "SELECT pr.project_id, pr.synced_hash_claude, pr.synced_hash_codex, p.path
         FROM project_rules pr JOIN projects p ON p.id = pr.project_id
         WHERE pr.rule_id = ?"
    )
    .bind(rule_id)
    .fetch_all(db)
    .await?;

    for row in rows {
        let project_id: String = row.get("project_id");
        let prev_claude: Option<String> = row.get("synced_hash_claude");
        let prev_codex: Option<String> = row.get("synced_hash_codex");
        let project_path: String = row.get("path");

        // ---- Claude ----
        let mut next_claude = prev_claude.clone();
        if applies_claude(&target) {
            let dest = claude_rule_path(&project_path, &name);
            let local_hash = fs::read_to_string(&dest).ok().map(|c| content_hash(&c));
            match (&local_hash, &prev_claude) {
                (Some(lh), Some(ph)) if lh != ph => {
                    record_conflict(db, &project_id, rule_id, "claude", lh, &new_claude_hash).await;
                }
                _ => {
                    let _ = write_rule_artifacts(&project_path, &name, "claude", &content);
                    next_claude = Some(new_claude_hash.clone());
                }
            }
        } else if prev_claude.is_some() {
            // No longer targets claude — remove artifact.
            let _ = fs::remove_file(claude_rule_path(&project_path, &name));
            next_claude = None;
        }

        // ---- Codex ----
        let mut next_codex = prev_codex.clone();
        if applies_codex(&target) {
            let local_hash = extract_agents_block(&agents_path(&project_path), &name)
                .map(|b| content_hash(b.trim_end_matches('\n')));
            match (&local_hash, &prev_codex) {
                (Some(lh), Some(ph)) if lh != ph => {
                    record_conflict(db, &project_id, rule_id, "codex", lh, &new_codex_hash).await;
                }
                _ => {
                    let _ = upsert_agents_block(&agents_path(&project_path), &name, &body);
                    next_codex = Some(new_codex_hash.clone());
                }
            }
        } else if prev_codex.is_some() {
            remove_agents_block(&agents_path(&project_path), &name);
            next_codex = None;
        }

        let _ = sqlx::query(
            "UPDATE project_rules SET target = ?, synced_hash_claude = ?, synced_hash_codex = ? WHERE project_id = ? AND rule_id = ?"
        )
        .bind(&target)
        .bind(&next_claude)
        .bind(&next_codex)
        .bind(&project_id)
        .bind(rule_id)
        .execute(db)
        .await;
    }

    Ok(())
}

async fn record_conflict(
    db: &Db,
    project_id: &str,
    rule_id: &str,
    engine: &str,
    local_hash: &str,
    source_hash: &str,
) {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let _ = sqlx::query(
        "INSERT INTO rule_sync_conflicts (id, project_id, rule_id, engine, detected_at, local_hash, source_hash, resolved)
         VALUES (?, ?, ?, ?, ?, ?, ?, 0)"
    )
    .bind(&id)
    .bind(project_id)
    .bind(rule_id)
    .bind(engine)
    .bind(&now)
    .bind(local_hash)
    .bind(source_hash)
    .execute(db)
    .await;
}
