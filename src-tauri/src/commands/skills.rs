use crate::commands::agents_block;
use crate::db::Db;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use tauri::{AppHandle, Manager, State};
use uuid::Uuid;
use walkdir::WalkDir;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Skill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub target: String,
    pub source_path: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SkillContent {
    pub skill: Skill,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateSkillPayload {
    pub name: String,
    pub description: String,
    pub target: String,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSkillPayload {
    pub id: String,
    pub name: String,
    pub description: String,
    pub target: String,
    pub content: String,
}

fn skills_dir(app: &AppHandle) -> PathBuf {
    app.path().app_data_dir().unwrap().join("skills")
}

pub fn skill_hash(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("Skill name cannot be empty".to_string());
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_') {
        return Err("Skill name must only contain letters, numbers, hyphens, and underscores".to_string());
    }
    Ok(())
}

fn parse_frontmatter(content: &str) -> (String, String) {
    // Extract name and description from YAML frontmatter
    if content.starts_with("---") {
        let rest = &content[3..];
        if let Some(end) = rest.find("---") {
            let fm = &rest[..end];
            let name = fm.lines()
                .find(|l| l.starts_with("name:"))
                .map(|l| l["name:".len()..].trim().trim_matches('"').to_string())
                .unwrap_or_default();
            let description = fm.lines()
                .find(|l| l.starts_with("description:"))
                .map(|l| l["description:".len()..].trim().trim_matches('"').to_string())
                .unwrap_or_default();
            return (name, description);
        }
    }
    (String::new(), String::new())
}

/// Read the `target:` field from a skill's SKILL.md frontmatter (defaults to "claude").
fn parse_target(content: &str) -> String {
    if content.starts_with("---") {
        let rest = &content[3..];
        if let Some(end) = rest.find("\n---") {
            let fm = &rest[..end];
            if let Some(t) = fm.lines().find(|l| l.trim_start().starts_with("target:")) {
                let v = t.trim_start()["target:".len()..]
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !v.is_empty() {
                    return v;
                }
            }
        }
    }
    "claude".to_string()
}

fn validate_target(target: &str) -> Result<(), String> {
    match target {
        "claude" | "codex" | "both" => Ok(()),
        _ => Err("target must be one of: claude, codex, both".to_string()),
    }
}

pub fn applies_claude(target: &str) -> bool {
    target == "claude" || target == "both"
}
pub fn applies_codex(target: &str) -> bool {
    target == "codex" || target == "both"
}

// ---- Per-engine destinations -------------------------------------------------

/// Claude reads skills from `<project>/.claude/skills/<name>/`.
pub fn claude_skill_dir(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path).join(".claude").join("skills").join(name)
}

/// Codex has no native skills folder; files live in `<project>/.codex/skills/<name>/`
/// and a pointer block in AGENTS.md tells Codex the skill exists.
pub fn codex_skill_dir(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path).join(".codex").join("skills").join(name)
}

pub fn agents_path(project_path: &str) -> PathBuf {
    Path::new(project_path).join("AGENTS.md")
}

/// Pointer block injected into AGENTS.md for a Codex-targeted skill.
pub fn codex_pointer_body(name: &str, description: &str) -> String {
    let desc = description.trim();
    if desc.is_empty() {
        format!(
            "## Skill: {name}\n\nFull instructions: .codex/skills/{name}/SKILL.md",
            name = name
        )
    } else {
        format!(
            "## Skill: {name}\n\n{desc}\n\nFull instructions: .codex/skills/{name}/SKILL.md",
            name = name,
            desc = desc
        )
    }
}

/// Copy every file from a skill's source directory into `dest_dir`.
pub fn copy_skill_files(source_path: &str, dest_dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dest_dir).map_err(|e| e.to_string())?;
    let source_dir = Path::new(source_path);
    for entry in WalkDir::new(source_dir) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_file() {
            let rel = path.strip_prefix(source_dir).map_err(|e| e.to_string())?;
            let dest = dest_dir.join(rel);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            fs::copy(path, &dest).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Read a skill's source SKILL.md content (empty string if missing).
pub fn read_skill_md(source_path: &str) -> String {
    fs::read_to_string(Path::new(source_path).join("SKILL.md")).unwrap_or_default()
}

/// Write a skill to a project for the given `target`, returning (claude_hash, codex_hash).
/// Each hash is the source SKILL.md hash for the engine it was synced to (None otherwise).
pub fn write_skill_artifacts(
    project_path: &str,
    name: &str,
    description: &str,
    target: &str,
    source_path: &str,
) -> Result<(Option<String>, Option<String>), String> {
    let content = read_skill_md(source_path);
    let hash = skill_hash(&content);
    let mut hash_claude = None;
    let mut hash_codex = None;

    if applies_claude(target) {
        copy_skill_files(source_path, &claude_skill_dir(project_path, name))?;
        hash_claude = Some(hash.clone());
    }
    if applies_codex(target) {
        copy_skill_files(source_path, &codex_skill_dir(project_path, name))?;
        agents_block::upsert(
            &agents_path(project_path),
            "skill",
            name,
            &codex_pointer_body(name, description),
        )?;
        hash_codex = Some(hash);
    }
    Ok((hash_claude, hash_codex))
}

/// Remove all of a skill's artifacts (both engines) from a project.
pub fn remove_skill_artifacts(project_path: &str, name: &str) {
    let _ = fs::remove_dir_all(claude_skill_dir(project_path, name));
    let _ = fs::remove_dir_all(codex_skill_dir(project_path, name));
    agents_block::remove(&agents_path(project_path), "skill", name);
}

#[tauri::command]
pub async fn list_skills(db: State<'_, Db>) -> Result<Vec<Skill>, String> {
    let rows = sqlx::query("SELECT id, name, description, target, source_path, updated_at FROM skills ORDER BY name")
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let skills = rows.iter().map(|row| {
        use sqlx::Row;
        Skill {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            target: row.get("target"),
            source_path: row.get("source_path"),
            updated_at: row.get("updated_at"),
        }
    }).collect();

    Ok(skills)
}

#[tauri::command]
pub async fn get_skill(db: State<'_, Db>, id: String) -> Result<SkillContent, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT id, name, description, target, source_path, updated_at FROM skills WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let source_path: String = row.get("source_path");
    let skill_md = Path::new(&source_path).join("SKILL.md");
    let content = fs::read_to_string(&skill_md).unwrap_or_default();

    Ok(SkillContent {
        skill: Skill {
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
pub async fn create_skill(
    app: AppHandle,
    db: State<'_, Db>,
    payload: CreateSkillPayload,
) -> Result<Skill, String> {
    validate_skill_name(&payload.name)?;
    validate_target(&payload.target)?;

    if payload.description.is_empty() {
        return Err("Description is required".to_string());
    }

    // Check name uniqueness
    let existing = sqlx::query("SELECT id FROM skills WHERE name = ?")
        .bind(&payload.name)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    if existing.is_some() {
        return Err(format!("Skill '{}' already exists", payload.name));
    }

    let id = Uuid::new_v4().to_string();
    let skill_dir = skills_dir(&app).join(&payload.name);
    fs::create_dir_all(&skill_dir).map_err(|e| e.to_string())?;
    fs::write(skill_dir.join("SKILL.md"), &payload.content).map_err(|e| e.to_string())?;

    let source_path = skill_dir.to_string_lossy().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(&id)
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.target)
        .bind(&source_path)
        .bind(&now)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(Skill { id, name: payload.name, description: payload.description, target: payload.target, source_path, updated_at: now })
}

#[tauri::command]
pub async fn update_skill(
    app: AppHandle,
    db: State<'_, Db>,
    payload: UpdateSkillPayload,
) -> Result<Skill, String> {
    validate_skill_name(&payload.name)?;
    validate_target(&payload.target)?;

    if payload.description.is_empty() {
        return Err("Description is required".to_string());
    }

    // Get current skill
    use sqlx::Row;
    let row = sqlx::query("SELECT name, source_path FROM skills WHERE id = ?")
        .bind(&payload.id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let old_name: String = row.get("name");
    let old_path: String = row.get("source_path");

    // If renamed, drop old artifacts from every applied project first.
    if old_name != payload.name {
        let projects = sqlx::query(
            "SELECT p.path FROM project_skills ps JOIN projects p ON p.id = ps.project_id WHERE ps.skill_id = ?"
        )
        .bind(&payload.id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
        for prow in &projects {
            let ppath: String = prow.get("path");
            remove_skill_artifacts(&ppath, &old_name);
        }
    }

    let new_source_path = if old_name != payload.name {
        // Rename directory
        let new_dir = skills_dir(&app).join(&payload.name);
        fs::rename(&old_path, &new_dir).map_err(|e| e.to_string())?;
        new_dir.to_string_lossy().to_string()
    } else {
        old_path
    };

    fs::write(Path::new(&new_source_path).join("SKILL.md"), &payload.content)
        .map_err(|e| e.to_string())?;

    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query("UPDATE skills SET name = ?, description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?")
        .bind(&payload.name)
        .bind(&payload.description)
        .bind(&payload.target)
        .bind(&new_source_path)
        .bind(&now)
        .bind(&payload.id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    // Trigger auto-sync for all projects that have this skill applied
    let _ = sync_skill_to_projects(&db, &payload.id).await;

    Ok(Skill {
        id: payload.id,
        name: payload.name,
        description: payload.description,
        target: payload.target,
        source_path: new_source_path,
        updated_at: now,
    })
}

#[tauri::command]
pub async fn delete_skill(
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT source_path FROM skills WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let source_path: String = row.get("source_path");
    let _ = fs::remove_dir_all(&source_path);

    sqlx::query("DELETE FROM skills WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn export_skill_zip(
    db: State<'_, Db>,
    id: String,
    dest_path: String,
) -> Result<(), String> {
    use sqlx::Row;
    use std::io::Write;
    use zip::ZipWriter;
    use zip::write::SimpleFileOptions;

    let row = sqlx::query("SELECT name, source_path FROM skills WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    let name: String = row.get("name");
    let source_path: String = row.get("source_path");

    let file = fs::File::create(&dest_path).map_err(|e| e.to_string())?;
    let mut zip = ZipWriter::new(file);
    let options = SimpleFileOptions::default();

    for entry in WalkDir::new(&source_path) {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.is_file() {
            let rel = path.strip_prefix(&source_path).map_err(|e| e.to_string())?;
            let zip_path = format!("{}/{}", name, rel.to_string_lossy());
            zip.start_file(&zip_path, options).map_err(|e| e.to_string())?;
            let data = fs::read(path).map_err(|e| e.to_string())?;
            zip.write_all(&data).map_err(|e| e.to_string())?;
        }
    }

    zip.finish().map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn import_skill_zip(
    app: AppHandle,
    db: State<'_, Db>,
    zip_path: String,
) -> Result<Skill, String> {
    use std::io::Read;
    use zip::ZipArchive;

    let file = fs::File::open(&zip_path).map_err(|e| e.to_string())?;
    let mut archive = ZipArchive::new(file).map_err(|e| e.to_string())?;

    // Detect skill name from first path component
    let skill_name = {
        let first = archive.by_index(0).map_err(|e| e.to_string())?;
        first.name().split('/').next().unwrap_or("imported").to_string()
    };

    validate_skill_name(&skill_name)?;

    let dest_dir = skills_dir(&app).join(&skill_name);
    fs::create_dir_all(&dest_dir).map_err(|e| e.to_string())?;

    // Extract files
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let name = entry.name().to_string();
        // Strip first component (skill name prefix)
        let rel: PathBuf = name.split('/').skip(1).collect();
        if rel.to_string_lossy().is_empty() {
            continue;
        }
        let out = dest_dir.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&out).map_err(|e| e.to_string())?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            fs::write(&out, &data).map_err(|e| e.to_string())?;
        }
    }

    // Read SKILL.md to get description
    let skill_md_path = dest_dir.join("SKILL.md");
    let content = fs::read_to_string(&skill_md_path).unwrap_or_default();
    let (_, description) = parse_frontmatter(&content);
    let description = if description.is_empty() { skill_name.clone() } else { description };
    let target = parse_target(&content);

    // Upsert into DB
    let existing = sqlx::query("SELECT id FROM skills WHERE name = ?")
        .bind(&skill_name)
        .fetch_optional(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    use sqlx::Row;
    let id = if let Some(row) = existing {
        let id: String = row.get("id");
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("UPDATE skills SET description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?")
            .bind(&description)
            .bind(&target)
            .bind(dest_dir.to_string_lossy().as_ref())
            .bind(&now)
            .bind(&id)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        id
    } else {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind(&skill_name)
            .bind(&description)
            .bind(&target)
            .bind(dest_dir.to_string_lossy().as_ref())
            .bind(&now)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        id
    };

    let now = chrono::Utc::now().to_rfc3339();
    Ok(Skill {
        id,
        name: skill_name,
        description,
        target,
        source_path: dest_dir.to_string_lossy().to_string(),
        updated_at: now,
    })
}

#[tauri::command]
pub async fn open_skill_folder(db: State<'_, Db>, id: String) -> Result<(), String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT source_path FROM skills WHERE id = ?")
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let source_path: String = row.get("source_path");

    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg(&source_path)
        .spawn()
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "windows")]
    std::process::Command::new("explorer")
        .arg(&source_path)
        .spawn()
        .map_err(|e| e.to_string())?;

    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open")
        .arg(&source_path)
        .spawn()
        .map_err(|e| e.to_string())?;

    Ok(())
}

/// Sync a skill to every project that has it applied. Handles target changes (removing artifacts
/// for engines no longer targeted) and per-engine conflict detection — mirrors rules' sync.
pub async fn sync_skill_to_projects(db: &Db, skill_id: &str) -> Result<()> {
    use sqlx::Row;

    let skill = sqlx::query("SELECT name, description, target, source_path FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(db)
        .await?;
    let name: String = skill.get("name");
    let description: String = skill.get("description");
    let target: String = skill.get("target");
    let source_path: String = skill.get("source_path");
    let new_hash = skill_hash(&read_skill_md(&source_path));

    let rows = sqlx::query(
        "SELECT ps.project_id, ps.synced_hash_claude, ps.synced_hash_codex, p.path FROM project_skills ps
         JOIN projects p ON p.id = ps.project_id
         WHERE ps.skill_id = ?"
    )
    .bind(skill_id)
    .fetch_all(db)
    .await?;

    for row in rows {
        let project_id: String = row.get("project_id");
        let prev_claude: Option<String> = row.get("synced_hash_claude");
        let prev_codex: Option<String> = row.get("synced_hash_codex");
        let project_path: String = row.get("path");

        // ---- Claude (.claude/skills/<name>/) ----
        let mut next_claude = prev_claude.clone();
        if applies_claude(&target) {
            let local = local_skill_hash(&claude_skill_dir(&project_path, &name));
            match (&local, &prev_claude) {
                (Some(lh), Some(ph)) if lh != ph => {
                    record_skill_conflict(db, &project_id, skill_id, "claude", lh, &new_hash).await;
                }
                _ => {
                    let _ = copy_skill_files(&source_path, &claude_skill_dir(&project_path, &name));
                    next_claude = Some(new_hash.clone());
                }
            }
        } else if prev_claude.is_some() {
            let _ = fs::remove_dir_all(claude_skill_dir(&project_path, &name));
            next_claude = None;
        }

        // ---- Codex (.codex/skills/<name>/ + AGENTS.md pointer) ----
        let mut next_codex = prev_codex.clone();
        if applies_codex(&target) {
            let local = local_skill_hash(&codex_skill_dir(&project_path, &name));
            match (&local, &prev_codex) {
                (Some(lh), Some(ph)) if lh != ph => {
                    record_skill_conflict(db, &project_id, skill_id, "codex", lh, &new_hash).await;
                }
                _ => {
                    let _ = copy_skill_files(&source_path, &codex_skill_dir(&project_path, &name));
                    let _ = agents_block::upsert(
                        &agents_path(&project_path),
                        "skill",
                        &name,
                        &codex_pointer_body(&name, &description),
                    );
                    next_codex = Some(new_hash.clone());
                }
            }
        } else if prev_codex.is_some() {
            let _ = fs::remove_dir_all(codex_skill_dir(&project_path, &name));
            agents_block::remove(&agents_path(&project_path), "skill", &name);
            next_codex = None;
        }

        let _ = sqlx::query(
            "UPDATE project_skills SET target = ?, synced_hash_claude = ?, synced_hash_codex = ? WHERE project_id = ? AND skill_id = ?"
        )
        .bind(&target)
        .bind(&next_claude)
        .bind(&next_codex)
        .bind(&project_id)
        .bind(skill_id)
        .execute(db)
        .await;
    }

    Ok(())
}

/// Hash of a destination skill's local SKILL.md, or None if it doesn't exist.
fn local_skill_hash(dest_dir: &Path) -> Option<String> {
    let md = dest_dir.join("SKILL.md");
    if md.exists() {
        Some(skill_hash(&fs::read_to_string(&md).unwrap_or_default()))
    } else {
        None
    }
}

async fn record_skill_conflict(
    db: &Db,
    project_id: &str,
    skill_id: &str,
    engine: &str,
    local_hash: &str,
    source_hash: &str,
) {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let _ = sqlx::query(
        "INSERT OR REPLACE INTO sync_conflicts (id, project_id, skill_id, engine, detected_at, local_hash, source_hash, resolved) VALUES (?, ?, ?, ?, ?, ?, ?, 0)"
    )
    .bind(&id)
    .bind(project_id)
    .bind(skill_id)
    .bind(engine)
    .bind(&now)
    .bind(local_hash)
    .bind(source_hash)
    .execute(db)
    .await;
}
