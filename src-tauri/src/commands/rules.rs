use crate::commands::project_links;
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
    /// The user switched this rule on for the project directly.
    pub manual: bool,
    /// Some group the project has enabled contains this rule. Independent of `manual`: either one
    /// on its own keeps the rule applied.
    pub from_group: bool,
    /// Codex gets this rule as a skill (`.agents/skills`) rather than an AGENTS.md block.
    pub codex_skill: bool,
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

// ---- Codex: inline block vs. skill --------------------------------------------
//
// A rule reaches Codex either inlined into AGENTS.md (default; always in context, counts towards
// Codex's AGENTS.md size cap) or, with `codex_mode: skill` in its frontmatter, as a skill Codex
// loads on demand. The skill form is generated from the rule into the library
// (`<rules dir>/.codex-skills/<name>/SKILL.md`) and projects link to it.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexMode {
    Inline,
    Skill,
}

/// Read `codex_mode:` from a rule's frontmatter; anything but `skill` means inline.
pub fn parse_codex_mode(content: &str) -> CodexMode {
    let (_, _, _, mode) = parse_rule_frontmatter_full(content);
    if mode == "skill" { CodexMode::Skill } else { CodexMode::Inline }
}

fn codex_skill_source(source_path: &str, name: &str) -> PathBuf {
    let rules_dir = Path::new(source_path).parent().unwrap_or(Path::new("."));
    rules_dir.join(".codex-skills").join(name)
}

fn codex_skill_dest(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path).join(".agents").join("skills").join(name)
}

/// (Re)generate the skill form of a rule in the library.
fn write_codex_skill_source(source_path: &str, name: &str, description: &str, body: &str) -> Result<PathBuf, String> {
    let dir = codex_skill_source(source_path, name);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // A JSON string is a valid YAML double-quoted scalar, so quoting/escaping is handled.
    let desc = serde_json::to_string(description.trim()).map_err(|e| e.to_string())?;
    let skill_md = format!("---\nname: {}\ndescription: {}\n---\n\n{}", name, desc, body);
    fs::write(dir.join("SKILL.md"), skill_md).map_err(|e| e.to_string())?;
    Ok(dir)
}

fn remove_codex_skill_source(source_path: &str, name: &str) {
    let _ = fs::remove_dir_all(codex_skill_source(source_path, name));
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
    let renamed = old_name != payload.name;
    let new_path = rules_dir(&app).join(format!("{}.md", payload.name));

    // Check before touching any file: renaming onto another rule's file would overwrite its
    // source — and through the links, every project using it.
    if renamed {
        let taken = sqlx::query("SELECT id FROM rules WHERE name = ? AND id != ?")
            .bind(&payload.name)
            .bind(&payload.id)
            .fetch_optional(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        let case_only = project_links::same_file(&new_path, Path::new(&old_path));
        if taken.is_some() || (new_path.exists() && !case_only) {
            return Err(format!("Rule '{}' already exists", payload.name));
        }
    }

    // If renamed, drop old artifacts from every applied project first.
    if renamed {
        let projects = sqlx::query(
            "SELECT p.path, pr.synced_hash_claude, pr.synced_hash_codex
             FROM project_rules pr JOIN projects p ON p.id = pr.project_id WHERE pr.rule_id = ?"
        )
        .bind(&payload.id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
        let mut errors = Vec::new();
        for prow in &projects {
            let ppath: String = prow.get("path");
            let prev_claude: Option<String> = prow.get("synced_hash_claude");
            let had_codex = prow.get::<Option<String>, _>("synced_hash_codex").is_some();
            if let Err(e) = remove_rule_artifacts(&ppath, &old_name, &old_path, prev_claude.as_deref(), had_codex) {
                errors.push(e);
            }
        }
        if !errors.is_empty() {
            return Err(format!("Not renamed — clean these up first:\n{}", errors.join("\n")));
        }
        remove_codex_skill_source(&old_path, &old_name);
    }

    let new_source_path = if renamed {
        fs::rename(&old_path, &new_path).map_err(|e| e.to_string())?;
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

    // Claude links to the source, so its content is already live; sync covers the AGENTS.md
    // block / Codex skill, target changes and renames. Failures surface rather than vanish.
    let failures = sync_rule_to_projects(&db, &payload.id)
        .await
        .map_err(|e| e.to_string())?;
    if !(applies_codex(&payload.target) && parse_codex_mode(&payload.content) == CodexMode::Skill) {
        remove_codex_skill_source(&new_source_path, &payload.name);
    }
    if !failures.is_empty() {
        return Err(format!("Saved, but syncing to projects failed:\n{}", failures.join("\n")));
    }

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

    // Remove applied artifacts from every project — before the source goes, or links dangle.
    let projects = sqlx::query(
        "SELECT p.path, pr.synced_hash_claude, pr.synced_hash_codex
         FROM project_rules pr JOIN projects p ON p.id = pr.project_id WHERE pr.rule_id = ?"
    )
    .bind(&id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for prow in &projects {
        let ppath: String = prow.get("path");
        let prev_claude: Option<String> = prow.get("synced_hash_claude");
        let had_codex = prow.get::<Option<String>, _>("synced_hash_codex").is_some();
        if let Err(e) = remove_rule_artifacts(&ppath, &name, &source_path, prev_claude.as_deref(), had_codex) {
            errors.push(e);
        }
    }
    if !errors.is_empty() {
        return Err(format!("Not deleted — clean these up first:\n{}", errors.join("\n")));
    }

    remove_codex_skill_source(&source_path, &name);
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
    let (name, description, target, _) = parse_rule_frontmatter_full(content);
    (name, description, target)
}

/// (name, description, target, codex_mode) from a rule's frontmatter.
fn parse_rule_frontmatter_full(content: &str) -> (String, String, String, String) {
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
            return (pick("name:"), pick("description:"), pick("target:"), pick("codex_mode:"));
        }
    }
    (String::new(), String::new(), String::new(), String::new())
}

// ---- Apply / remove per project ----------------------------------------------

#[tauri::command]
pub async fn get_applied_rules(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<AppliedRule>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT pr.rule_id, r.name as rule_name, r.description as rule_description, r.source_path,
                pr.target, pr.synced_hash_claude, pr.synced_hash_codex, pr.applied_at, pr.manual,
                (SELECT COUNT(*) FROM rule_group_members m
                   JOIN project_rule_groups pg
                     ON pg.group_id = m.group_id AND pg.project_id = pr.project_id
                  WHERE m.rule_id = pr.rule_id) as group_count
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
        let manual: i64 = row.get("manual");
        let group_count: i64 = row.get("group_count");
        let source_path: String = row.get("source_path");
        let codex_skill = fs::read_to_string(&source_path)
            .is_ok_and(|c| parse_codex_mode(&c) == CodexMode::Skill);
        AppliedRule {
            rule_id: row.get("rule_id"),
            rule_name: row.get("rule_name"),
            rule_description: row.get("rule_description"),
            target: row.get("target"),
            has_claude: claude.is_some(),
            has_codex: codex.is_some(),
            applied_at: row.get("applied_at"),
            manual: manual != 0,
            from_group: group_count > 0,
            codex_skill,
        }
    }).collect())
}

#[tauri::command]
pub async fn apply_rule(
    db: State<'_, Db>,
    project_id: String,
    rule_id: String,
) -> Result<(), String> {
    apply_rule_to_project(db.inner(), &project_id, &rule_id, true).await
}

/// Summary returned by "apply to all projects" bulk operations (rules & skills).
#[derive(Debug, Serialize)]
pub struct ApplyAllOutcome {
    pub applied: usize,
    pub failures: Vec<ApplyFailure>,
}

#[derive(Debug, Serialize)]
pub struct ApplyFailure {
    pub project_id: String,
    pub project_name: String,
    pub error: String,
}

#[tauri::command]
pub async fn apply_rule_to_all_projects(
    db: State<'_, Db>,
    rule_id: String,
) -> Result<ApplyAllOutcome, String> {
    use sqlx::Row;
    let rows = sqlx::query("SELECT id, name FROM projects ORDER BY name")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let mut applied = 0;
    let mut failures = Vec::new();
    for row in &rows {
        let project_id: String = row.get("id");
        let project_name: String = row.get("name");
        match apply_rule_to_project(db.inner(), &project_id, &rule_id, true).await {
            Ok(()) => applied += 1,
            Err(error) => failures.push(ApplyFailure { project_id, project_name, error }),
        }
    }
    Ok(ApplyAllOutcome { applied, failures })
}

/// Re-write a rule (or every rule when `rule_id` is None) into each project that already has it
/// applied. Unlike auto-sync this overwrites local edits, so any open conflicts for the re-applied
/// pairs are marked resolved. `manual` / group flags are left untouched.
#[tauri::command]
pub async fn reapply_rules(
    db: State<'_, Db>,
    rule_id: Option<String>,
) -> Result<ApplyAllOutcome, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT pr.project_id, pr.rule_id, p.name as project_name, p.path, r.name as rule_name
         FROM project_rules pr
         JOIN projects p ON p.id = pr.project_id
         JOIN rules r ON r.id = pr.rule_id
         WHERE ?1 IS NULL OR pr.rule_id = ?1
         ORDER BY p.name, r.name"
    )
    .bind(&rule_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let mut applied = 0;
    let mut failures = Vec::new();
    for row in &rows {
        let project_id: String = row.get("project_id");
        let rid: String = row.get("rule_id");
        let project_name: String = row.get("project_name");
        let project_path: String = row.get("path");
        let rule_name: String = row.get("rule_name");
        // Don't recreate a project folder that was moved or deleted on disk.
        let result = if !Path::new(&project_path).is_dir() {
            Err(format!("Project folder not found: {}", project_path))
        } else {
            apply_rule_to_project(db.inner(), &project_id, &rid, false).await
        };
        match result {
            Ok(()) => {
                applied += 1;
                let _ = sqlx::query(
                    "UPDATE rule_sync_conflicts SET resolved = 1 WHERE project_id = ? AND rule_id = ? AND resolved = 0"
                )
                .bind(&project_id)
                .bind(&rid)
                .execute(db.inner())
                .await;
            }
            Err(error) => failures.push(ApplyFailure {
                project_id,
                project_name,
                error: format!("{}: {}", rule_name, error),
            }),
        }
    }
    Ok(ApplyAllOutcome { applied, failures })
}

/// Core apply logic shared by `apply_rule`, `apply_rule_to_all_projects` and group enablement.
///
/// See `apply_skill_to_project` for what `manual` means: hand-picked rules (`true`) survive a
/// group being switched off, rules a group pulled in (`false`) do not.
pub async fn apply_rule_to_project(
    db: &Db,
    project_id: &str,
    rule_id: &str,
    manual: bool,
) -> Result<(), String> {
    use sqlx::Row;
    let rule_row = sqlx::query("SELECT name, description, target, source_path FROM rules WHERE id = ?")
        .bind(rule_id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("Rule not found: {}", e))?;
    let name: String = rule_row.get("name");
    let description: String = rule_row.get("description");
    let target: String = rule_row.get("target");
    let source_path: String = rule_row.get("source_path");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("Project not found: {}", e))?;
    let project_path: String = project_row.get("path");

    let prev = sqlx::query(
        "SELECT synced_hash_claude, synced_hash_codex FROM project_rules WHERE project_id = ? AND rule_id = ?",
    )
    .bind(project_id)
    .bind(rule_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let prev_claude: Option<String> = prev.as_ref().and_then(|r| r.get("synced_hash_claude"));
    let prev_codex: Option<String> = prev.as_ref().and_then(|r| r.get("synced_hash_codex"));

    let (hash_claude, hash_codex) = write_rule_artifacts(
        &project_path,
        &name,
        &description,
        &target,
        &source_path,
        prev_claude.as_deref(),
        prev_codex.as_deref(),
    )?;
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO project_rules
         (project_id, rule_id, target, synced_hash_claude, synced_hash_codex, applied_at, manual)
         VALUES (?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(project_id, rule_id) DO UPDATE SET
            target = excluded.target,
            synced_hash_claude = excluded.synced_hash_claude,
            synced_hash_codex = excluded.synced_hash_codex,
            applied_at = excluded.applied_at,
            manual = CASE WHEN excluded.manual = 1 THEN 1 ELSE project_rules.manual END"
    )
    .bind(project_id)
    .bind(rule_id)
    .bind(&target)
    .bind(&hash_claude)
    .bind(&hash_codex)
    .bind(&now)
    .bind(if manual { 1 } else { 0 })
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

/// Install a rule into a project for `target` and drop what no longer applies; returns
/// (claude_hash, codex_hash) for the engines it is installed for. `prev_*` are the hashes recorded
/// at the last sync (None = not installed for that engine). Overwrites an edited AGENTS.md block
/// (explicit apply); auto-sync checks for that first, see `sync_rule_to_projects`.
fn write_rule_artifacts(
    project_path: &str,
    name: &str,
    description: &str,
    target: &str,
    source_path: &str,
    prev_claude: Option<&str>,
    prev_codex: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    let content = read_rule_source(source_path)?;
    let hash_claude = if applies_claude(target) {
        Some(link_claude_rule(project_path, name, source_path, &content, prev_claude)?)
    } else {
        if prev_claude.is_some() {
            unlink_claude_rule(project_path, name, source_path, prev_claude)?;
        }
        None
    };
    let hash_codex = if applies_codex(target) {
        Some(install_codex_rule(project_path, name, description, source_path, &content)?)
    } else {
        if prev_codex.is_some() {
            remove_codex_rule(project_path, name, source_path)?;
        }
        None
    };
    Ok((hash_claude, hash_codex))
}

fn read_rule_source(source_path: &str) -> Result<String, String> {
    fs::read_to_string(source_path).map_err(|e| format!("Cannot read rule source {}: {}", source_path, e))
}

/// A copy-era `.claude/rules/<name>.md` is unedited when it matches the source or the last sync.
fn claude_copy_known<'a>(source_path: &'a str, prev_claude: Option<&'a str>) -> impl Fn(&Path) -> bool + 'a {
    move |copy| {
        let Ok(local) = fs::read_to_string(copy) else { return false };
        let local = content_hash(&local);
        Some(local.as_str()) == prev_claude
            || fs::read_to_string(source_path).is_ok_and(|src| content_hash(&src) == local)
    }
}

/// Link `.claude/rules/<name>.md` to the source file; returns the source content hash.
fn link_claude_rule(
    project_path: &str,
    name: &str,
    source_path: &str,
    content: &str,
    prev_claude: Option<&str>,
) -> Result<String, String> {
    project_links::ensure_symlink(
        Path::new(project_path),
        &claude_rule_path(project_path, name),
        Path::new(source_path),
        claude_copy_known(source_path, prev_claude),
    )?;
    Ok(content_hash(content))
}

fn unlink_claude_rule(project_path: &str, name: &str, source_path: &str, prev_claude: Option<&str>) -> Result<(), String> {
    project_links::remove_installed(
        Path::new(project_path),
        &claude_rule_path(project_path, name),
        Path::new(source_path),
        claude_copy_known(source_path, prev_claude),
    )
}

/// Put a rule in front of Codex per its `codex_mode`, clearing the other form; returns the hash
/// of the body Codex sees.
fn install_codex_rule(
    project_path: &str,
    name: &str,
    description: &str,
    source_path: &str,
    content: &str,
) -> Result<String, String> {
    let root = Path::new(project_path);
    let body = strip_frontmatter(content);
    let agents = agents_path(project_path);
    let dest = codex_skill_dest(project_path, name);
    match parse_codex_mode(content) {
        CodexMode::Inline => {
            project_links::remove_installed(root, &dest, &codex_skill_source(source_path, name), project_links::no_copies)?;
            upsert_agents_block(&agents, name, &body)?;
        }
        CodexMode::Skill => {
            let dir = write_codex_skill_source(source_path, name, description, &body)?;
            project_links::ensure_symlink(root, &dest, &dir, project_links::no_copies)?;
            remove_agents_block(&agents, name);
        }
    }
    Ok(content_hash(body.trim_end_matches('\n')))
}

fn remove_codex_rule(project_path: &str, name: &str, source_path: &str) -> Result<(), String> {
    remove_agents_block(&agents_path(project_path), name);
    project_links::remove_installed(
        Path::new(project_path),
        &codex_skill_dest(project_path, name),
        &codex_skill_source(source_path, name),
        project_links::no_copies,
    )
}

/// Remove a rule's artifacts from a project for the engines it was installed for. An edited
/// copy-era `.claude/rules` file is kept and reported.
fn remove_rule_artifacts(
    project_path: &str,
    name: &str,
    source_path: &str,
    prev_claude: Option<&str>,
    had_codex: bool,
) -> Result<(), String> {
    if prev_claude.is_some() {
        unlink_claude_rule(project_path, name, source_path, prev_claude)?;
    }
    if had_codex {
        remove_codex_rule(project_path, name, source_path)?;
    }
    Ok(())
}

#[tauri::command]
/// Drop the *direct* link between a project and a rule; see `remove_skill_from_project` for why an
/// enabled group containing the rule keeps it applied.
pub async fn remove_rule_from_project(
    db: State<'_, Db>,
    project_id: String,
    rule_id: String,
) -> Result<(), String> {
    unlink_rule_from_project(db.inner(), &project_id, &rule_id).await
}

pub async fn unlink_rule_from_project(
    db: &Db,
    project_id: &str,
    rule_id: &str,
) -> Result<(), String> {
    if crate::commands::groups::claimed_by_enabled_group(
        db,
        crate::commands::groups::GroupKind::Rule,
        project_id,
        rule_id,
    )
    .await?
    {
        sqlx::query("UPDATE project_rules SET manual = 0 WHERE project_id = ? AND rule_id = ?")
            .bind(project_id)
            .bind(rule_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
        return Ok(());
    }
    remove_rule_from_project_inner(db, project_id, rule_id).await
}

/// Core removal logic shared by the command above and group disablement.
pub async fn remove_rule_from_project_inner(
    db: &Db,
    project_id: &str,
    rule_id: &str,
) -> Result<(), String> {
    use sqlx::Row;
    let rule_row = sqlx::query("SELECT name, source_path FROM rules WHERE id = ?")
        .bind(rule_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let name: String = rule_row.get("name");
    let source_path: String = rule_row.get("source_path");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    let project_path: String = project_row.get("path");

    let installed = sqlx::query(
        "SELECT synced_hash_claude, synced_hash_codex FROM project_rules WHERE project_id = ? AND rule_id = ?",
    )
    .bind(project_id)
    .bind(rule_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    let prev_claude: Option<String> = installed.as_ref().and_then(|r| r.get("synced_hash_claude"));
    let had_codex = installed.as_ref().is_some_and(|r| r.get::<Option<String>, _>("synced_hash_codex").is_some());
    remove_rule_artifacts(&project_path, &name, &source_path, prev_claude.as_deref(), had_codex)?;

    sqlx::query("DELETE FROM project_rules WHERE project_id = ? AND rule_id = ?")
        .bind(project_id)
        .bind(rule_id)
        .execute(db)
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
            "SELECT rc.project_id, rc.rule_id, rc.engine, rc.local_hash, r.name, r.description, r.source_path,
                    p.path as project_path
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
        let local_hash: String = row.get("local_hash");
        let name: String = row.get("name");
        let description: String = row.get("description");
        let source_path: String = row.get("source_path");
        let project_path: String = row.get("project_path");

        let content = read_rule_source(&source_path)?;
        if engine == "claude" {
            // The edited copy's hash counts as known, so it is replaced by the link.
            let h = link_claude_rule(&project_path, &name, &source_path, &content, Some(&local_hash))?;
            sqlx::query("UPDATE project_rules SET synced_hash_claude = ? WHERE project_id = ? AND rule_id = ?")
                .bind(&h)
                .bind(&project_id)
                .bind(&rule_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        } else {
            let h = install_codex_rule(&project_path, &name, &description, &source_path, &content)?;
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

/// Sync a rule to every project that has it applied. Claude links to the source, so it only
/// needs (re)linking on target/name changes; Codex gets its AGENTS.md block or generated skill
/// refreshed. An AGENTS.md block edited in the project is kept and recorded as a conflict.
/// Returns one message per project that failed.
pub async fn sync_rule_to_projects(db: &Db, rule_id: &str) -> Result<Vec<String>> {
    use sqlx::Row;

    let rule = sqlx::query("SELECT name, description, target, source_path FROM rules WHERE id = ?")
        .bind(rule_id)
        .fetch_one(db)
        .await?;
    let name: String = rule.get("name");
    let description: String = rule.get("description");
    let target: String = rule.get("target");
    let source_path: String = rule.get("source_path");
    let content = read_rule_source(&source_path).map_err(anyhow::Error::msg)?;
    let new_codex_hash = content_hash(strip_frontmatter(&content).trim_end_matches('\n'));

    let rows = sqlx::query(
        "SELECT pr.project_id, pr.synced_hash_claude, pr.synced_hash_codex, p.path, p.name as project_name
         FROM project_rules pr JOIN projects p ON p.id = pr.project_id
         WHERE pr.rule_id = ?"
    )
    .bind(rule_id)
    .fetch_all(db)
    .await?;

    let mut failures = Vec::new();
    for row in rows {
        let project_id: String = row.get("project_id");
        let prev_claude: Option<String> = row.get("synced_hash_claude");
        let prev_codex: Option<String> = row.get("synced_hash_codex");
        let project_path: String = row.get("path");
        let project_name: String = row.get("project_name");
        let mut errors = Vec::new();

        // ---- Claude ----
        let next_claude = if applies_claude(&target) {
            match link_claude_rule(&project_path, &name, &source_path, &content, prev_claude.as_deref()) {
                Ok(h) => Some(h),
                Err(e) => {
                    errors.push(e);
                    prev_claude.clone()
                }
            }
        } else if prev_claude.is_some() {
            match unlink_claude_rule(&project_path, &name, &source_path, prev_claude.as_deref()) {
                Ok(()) => None,
                Err(e) => {
                    errors.push(e);
                    prev_claude.clone()
                }
            }
        } else {
            None
        };

        // ---- Codex ----
        let local_block = extract_agents_block(&agents_path(&project_path), &name)
            .map(|b| content_hash(b.trim_end_matches('\n')));
        let edited_locally = matches!((&local_block, &prev_codex), (Some(lh), Some(ph)) if lh != ph);
        let next_codex = if edited_locally {
            let lh = local_block.as_deref().unwrap_or_default();
            record_conflict(db, &project_id, rule_id, "codex", lh, &new_codex_hash).await;
            prev_codex.clone()
        } else if applies_codex(&target) {
            match install_codex_rule(&project_path, &name, &description, &source_path, &content) {
                Ok(h) => Some(h),
                Err(e) => {
                    errors.push(e);
                    prev_codex.clone()
                }
            }
        } else if prev_codex.is_some() {
            match remove_codex_rule(&project_path, &name, &source_path) {
                Ok(()) => None,
                Err(e) => {
                    errors.push(e);
                    prev_codex.clone()
                }
            }
        } else {
            None
        };

        if !errors.is_empty() {
            failures.push(format!("{}: {}", project_name, errors.join("; ")));
        }

        sqlx::query(
            "UPDATE project_rules SET target = ?, synced_hash_claude = ?, synced_hash_codex = ? WHERE project_id = ? AND rule_id = ?"
        )
        .bind(&target)
        .bind(&next_claude)
        .bind(&next_codex)
        .bind(&project_id)
        .bind(rule_id)
        .execute(db)
        .await?;
    }

    Ok(failures)
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::commands::project_links::is_symlink;
    use crate::commands::projects::{apply_skill_to_project, remove_skill_from_project_inner};
    use crate::commands::skills::sync_skill_to_projects;
    use sqlx::sqlite::SqlitePoolOptions;

    fn tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-rules-{}-{}", label, Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// In-memory DB with real migrations, one project at `<root>/proj` and a library at `<root>/lib`.
    async fn setup(label: &str) -> (Db, PathBuf, PathBuf) {
        let root = tmp(label);
        let project = root.join("proj");
        let lib = root.join("lib");
        fs::create_dir_all(&project).unwrap();
        fs::create_dir_all(lib.join("rules")).unwrap();
        fs::create_dir_all(lib.join("skills")).unwrap();
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'proj', ?, '2026-01-01')")
            .bind(project.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        (pool, project, lib)
    }

    async fn add_rule(db: &Db, lib: &Path, name: &str, target: &str, content: &str) -> PathBuf {
        let source = lib.join("rules").join(format!("{}.md", name));
        fs::write(&source, content).unwrap();
        sqlx::query(
            "INSERT INTO rules (id, name, description, target, source_path, updated_at)
             VALUES (?, ?, 'Use \"quotes\" carefully', ?, ?, '2026-01-01')",
        )
        .bind(name)
        .bind(name)
        .bind(target)
        .bind(source.to_string_lossy().to_string())
        .execute(db)
        .await
        .unwrap();
        source
    }

    async fn set_target(db: &Db, table: &str, id: &str, target: &str) {
        sqlx::query(&format!("UPDATE {} SET target = ? WHERE id = ?", table))
            .bind(target)
            .bind(id)
            .execute(db)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn inline_rule_links_claude_and_inlines_codex() {
        let (db, project, lib) = setup("inline").await;
        let source = add_rule(&db, &lib, "style", "both", "---\nname: style\n---\n\nUse tabs.\n").await;

        apply_rule_to_project(&db, "p1", "style", true).await.unwrap();

        let claude = project.join(".claude/rules/style.md");
        assert_eq!(fs::read_link(&claude).unwrap(), source);
        let agents = fs::read_to_string(project.join("AGENTS.md")).unwrap();
        assert!(agents.contains("Use tabs."));
        assert!(!agents.contains("name: style"), "frontmatter stays out of AGENTS.md");

        // Editing the source shows up through the link; the AGENTS.md block needs a sync.
        fs::write(&source, "---\nname: style\n---\n\nUse spaces.\n").unwrap();
        assert!(fs::read_to_string(&claude).unwrap().contains("Use spaces."));
        assert!(sync_rule_to_projects(&db, "style").await.unwrap().is_empty());
        assert!(fs::read_to_string(project.join("AGENTS.md")).unwrap().contains("Use spaces."));
    }

    #[tokio::test]
    async fn skill_mode_rule_becomes_a_codex_skill() {
        let (db, project, lib) = setup("skill-mode").await;
        add_rule(&db, &lib, "ci", "codex", "---\nname: ci\ncodex_mode: skill\n---\n\nPin actions.\n").await;

        apply_rule_to_project(&db, "p1", "ci", true).await.unwrap();

        let dest = project.join(".agents/skills/ci");
        assert!(is_symlink(&dest));
        let skill_md = fs::read_to_string(dest.join("SKILL.md")).unwrap();
        assert!(skill_md.starts_with("---\nname: ci\ndescription: \"Use \\\"quotes\\\" carefully\"\n---"));
        assert!(skill_md.contains("Pin actions."));
        assert!(!project.join("AGENTS.md").exists());
        assert!(!project.join(".claude/rules/ci.md").exists());

        // Switching back to inline drops the skill link and writes the block.
        fs::write(lib.join("rules/ci.md"), "---\nname: ci\n---\n\nPin actions.\n").unwrap();
        assert!(sync_rule_to_projects(&db, "ci").await.unwrap().is_empty());
        assert!(!is_symlink(&dest));
        assert!(fs::read_to_string(project.join("AGENTS.md")).unwrap().contains("Pin actions."));
    }

    #[tokio::test]
    async fn legacy_copy_is_replaced_only_when_unedited() {
        let (db, project, lib) = setup("legacy").await;
        let content = "---\nname: git\n---\n\nNo force push.\n";
        add_rule(&db, &lib, "git", "claude", content).await;
        let claude = project.join(".claude/rules/git.md");
        fs::create_dir_all(claude.parent().unwrap()).unwrap();

        // Unedited copy from the copy era → becomes a link.
        fs::write(&claude, content).unwrap();
        apply_rule_to_project(&db, "p1", "git", true).await.unwrap();
        assert!(is_symlink(&claude));

        // Edited copy → error, file untouched.
        fs::remove_file(&claude).unwrap();
        fs::write(&claude, "my local tweak\n").unwrap();
        let err = apply_rule_to_project(&db, "p1", "git", true).await.unwrap_err();
        assert!(err.contains("local copy"), "{err}");
        assert_eq!(fs::read_to_string(&claude).unwrap(), "my local tweak\n");
    }

    #[tokio::test]
    async fn edited_legacy_copy_survives_target_change_and_unapply() {
        let (db, project, lib) = setup("legacy-remove").await;
        let content = "Rule body.\n";
        add_rule(&db, &lib, "doc", "both", content).await;
        let claude = project.join(".claude/rules/doc.md");
        fs::create_dir_all(claude.parent().unwrap()).unwrap();
        fs::write(&claude, "edited by hand\n").unwrap();
        // Copy era: installed for Claude with the original content's hash.
        sqlx::query(
            "INSERT INTO project_rules (project_id, rule_id, target, synced_hash_claude, synced_hash_codex, applied_at, manual)
             VALUES ('p1', 'doc', 'both', ?, NULL, '2026-01-01', 1)",
        )
        .bind(content_hash(content))
        .execute(&db)
        .await
        .unwrap();

        set_target(&db, "rules", "doc", "codex").await;
        let failures = sync_rule_to_projects(&db, "doc").await.unwrap();
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert_eq!(fs::read_to_string(&claude).unwrap(), "edited by hand\n");

        let err = remove_rule_from_project_inner(&db, "p1", "doc").await.unwrap_err();
        assert!(err.contains("local copy"), "{err}");
        assert!(claude.exists());
    }

    #[tokio::test]
    async fn target_change_removes_the_dropped_engine() {
        let (db, project, lib) = setup("target").await;
        add_rule(&db, &lib, "lang", "both", "Speak Vietnamese.\n").await;
        apply_rule_to_project(&db, "p1", "lang", true).await.unwrap();

        set_target(&db, "rules", "lang", "claude").await;
        assert!(sync_rule_to_projects(&db, "lang").await.unwrap().is_empty());
        assert!(is_symlink(&project.join(".claude/rules/lang.md")));
        assert!(!project.join("AGENTS.md").exists());
    }

    #[tokio::test]
    async fn skills_link_per_engine_and_migrate_legacy_codex_copy() {
        let (db, project, lib) = setup("skills").await;
        let source = lib.join("skills/review");
        fs::create_dir_all(source.join("references")).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: review\n---\nReview it.\n").unwrap();
        fs::write(source.join("references/a.md"), "ref").unwrap();
        sqlx::query(
            "INSERT INTO skills (id, name, description, target, source_path, updated_at)
             VALUES ('review', 'review', 'Review PRs', 'both', ?, '2026-01-01')",
        )
        .bind(source.to_string_lossy().to_string())
        .execute(&db)
        .await
        .unwrap();

        // Copy-era leftovers: .codex/skills copy + AGENTS.md pointer block. An edited reference
        // file blocks the switch-over even though SKILL.md is untouched.
        let legacy = project.join(".codex/skills/review");
        fs::create_dir_all(legacy.join("references")).unwrap();
        fs::copy(source.join("SKILL.md"), legacy.join("SKILL.md")).unwrap();
        fs::write(legacy.join("references/a.md"), "edited").unwrap();
        let err = apply_skill_to_project(&db, "p1", "review", true).await.unwrap_err();
        assert!(err.contains("local copy"), "{err}");
        assert_eq!(fs::read_to_string(legacy.join("references/a.md")).unwrap(), "edited");
        fs::write(legacy.join("references/a.md"), "ref").unwrap();
        agents_block::upsert(&project.join("AGENTS.md"), "skill", "review", "## Skill: review").unwrap();

        apply_skill_to_project(&db, "p1", "review", true).await.unwrap();

        assert_eq!(fs::read_link(project.join(".claude/skills/review")).unwrap(), source);
        assert_eq!(fs::read_link(project.join(".agents/skills/review")).unwrap(), source);
        assert!(project.join(".agents/skills/review/references/a.md").exists());
        assert!(!legacy.exists());
        assert!(!project.join("AGENTS.md").exists(), "pointer block removed");

        // A copy-era Claude copy that is merely stale (SKILL.md from the last sync) still migrates.
        let stale = project.join(".claude/skills/review");
        fs::remove_file(&stale).unwrap();
        fs::create_dir_all(stale.join("references")).unwrap();
        fs::write(stale.join("SKILL.md"), "old body\n").unwrap();
        fs::write(stale.join("references/a.md"), "ref").unwrap();
        sqlx::query("UPDATE project_skills SET synced_hash_claude = ? WHERE skill_id = 'review'")
            .bind(crate::commands::skills::skill_hash("old body\n"))
            .execute(&db)
            .await
            .unwrap();
        apply_skill_to_project(&db, "p1", "review", true).await.unwrap();
        assert!(is_symlink(&stale));

        set_target(&db, "skills", "review", "claude").await;
        assert!(sync_skill_to_projects(&db, "review").await.unwrap().is_empty());
        assert!(!is_symlink(&project.join(".agents/skills/review")));

        remove_skill_from_project_inner(&db, "p1", "review").await.unwrap();
        assert!(!is_symlink(&project.join(".claude/skills/review")));
        assert!(source.join("SKILL.md").exists(), "source untouched");
    }
}
