use crate::commands::{agents_block, project_links};
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

/// Codex discovers skills natively in `<project>/.agents/skills/<name>/` (Claude never reads
/// `.agents/`, so the two engines don't see each other's skills).
pub fn codex_skill_dir(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path).join(".agents").join("skills").join(name)
}

/// Pre-`.agents/skills` location: a copy in `.codex/skills/<name>/` plus a pointer block in
/// AGENTS.md. Only cleaned up now.
pub fn legacy_codex_skill_dir(project_path: &str, name: &str) -> PathBuf {
    Path::new(project_path).join(".codex").join("skills").join(name)
}

pub fn agents_path(project_path: &str) -> PathBuf {
    Path::new(project_path).join("AGENTS.md")
}

/// Read a skill's source SKILL.md content (empty string if missing).
pub fn read_skill_md(source_path: &str) -> String {
    fs::read_to_string(Path::new(source_path).join("SKILL.md")).unwrap_or_default()
}

/// Link a skill into a project for `target` and drop what no longer applies, returning
/// (claude_hash, codex_hash) — the source SKILL.md hash per engine it is installed for.
///
/// `prev_*` are the SKILL.md hashes recorded at the last sync (None = not installed for that
/// engine); only an engine Devdy installed before is cleaned up. A copy-era folder is deleted when
/// swapped for a link (or removed) only if it is unedited: SKILL.md matches the source or the last
/// sync, and every other file matches the source or was not written after that SKILL.md. Any
/// other copy is moved to the legacy backups, never deleted.
pub fn write_skill_artifacts(
    project_path: &str,
    name: &str,
    target: &str,
    source_path: &str,
    prev_claude: Option<&str>,
    prev_codex: Option<&str>,
) -> Result<(Option<String>, Option<String>), String> {
    let hash = skill_hash(&read_skill_md(source_path));
    let root = Path::new(project_path);
    let source = Path::new(source_path);
    let unedited_for = |prev: Option<&str>| {
        let hash = hash.clone();
        let prev = prev.map(str::to_string);
        move |copy: &Path| {
            let skill_md = copy.join("SKILL.md");
            let Ok(md) = fs::read_to_string(&skill_md) else { return false };
            let md = skill_hash(&md);
            // Other files: identical to the source, or not written since the sync pass that wrote
            // this SKILL.md (a stale copy whose source moved on is still unedited).
            (md == hash || Some(&md) == prev.as_ref())
                && (project_links::same_tree_except(copy, source, Some(Path::new("SKILL.md")))
                    || project_links::untouched_since(copy, &skill_md))
        }
    };
    let had_claude = prev_claude.is_some();
    let had_codex = prev_codex.is_some();

    let claude_dest = claude_skill_dir(project_path, name);
    let hash_claude = if applies_claude(target) {
        project_links::ensure_symlink(root, &claude_dest, source, unedited_for(prev_claude))?;
        Some(hash.clone())
    } else {
        if had_claude {
            project_links::remove_installed(root, &claude_dest, source, unedited_for(prev_claude))?;
        }
        None
    };

    let codex_dest = codex_skill_dir(project_path, name);
    let legacy = legacy_codex_skill_dir(project_path, name);
    let unedited = unedited_for(prev_codex);
    let hash_codex = if applies_codex(target) {
        project_links::ensure_symlink(root, &codex_dest, source, &unedited)?;
        project_links::remove_installed(root, &legacy, source, &unedited)?;
        Some(hash)
    } else {
        if had_codex {
            project_links::remove_installed(root, &codex_dest, source, &unedited)?;
            project_links::remove_installed(root, &legacy, source, &unedited)?;
        }
        None
    };
    // The copy era's AGENTS.md pointer; `.agents/skills` needs none.
    if applies_codex(target) || had_codex {
        agents_block::remove(&agents_path(project_path), "skill", name);
    }
    Ok((hash_claude, hash_codex))
}

/// Remove a skill's artifacts (legacy layout included) for the engines it was installed for.
/// Legacy copies Devdy can't vouch for are moved to the backups.
pub fn remove_skill_artifacts(
    project_path: &str,
    name: &str,
    source_path: &str,
    prev_claude: Option<&str>,
    prev_codex: Option<&str>,
) -> Result<(), String> {
    write_skill_artifacts(project_path, name, "none", source_path, prev_claude, prev_codex).map(|_| ())
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

    if old_name != payload.name {
        let taken = sqlx::query("SELECT id FROM skills WHERE name = ? AND id != ?")
            .bind(&payload.name)
            .bind(&payload.id)
            .fetch_optional(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        let new_dir = skills_dir(&app).join(&payload.name);
        let case_only = project_links::same_file(&new_dir, Path::new(&old_path));
        if taken.is_some() || (new_dir.exists() && !case_only) {
            return Err(format!("Skill '{}' already exists", payload.name));
        }
    }

    // If renamed, drop old artifacts from every applied project first.
    if old_name != payload.name {
        let projects = sqlx::query(
            "SELECT p.path, ps.synced_hash_claude, ps.synced_hash_codex
             FROM project_skills ps JOIN projects p ON p.id = ps.project_id WHERE ps.skill_id = ?"
        )
        .bind(&payload.id)
        .fetch_all(db.inner())
        .await
        .map_err(|e| e.to_string())?;
        let mut errors = Vec::new();
        for prow in &projects {
            let ppath: String = prow.get("path");
            let prev_claude: Option<String> = prow.get("synced_hash_claude");
            let prev_codex: Option<String> = prow.get("synced_hash_codex");
            if let Err(e) = remove_skill_artifacts(&ppath, &old_name, &old_path, prev_claude.as_deref(), prev_codex.as_deref()) {
                errors.push(e);
            }
        }
        if !errors.is_empty() {
            return Err(format!("Not renamed — clean these up first:\n{}", errors.join("\n")));
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

    // Projects link to the source, so content is already live; sync covers target changes and
    // renames. Failures surface so a broken link is never silent.
    let failures = sync_skill_to_projects(&db, &payload.id)
        .await
        .map_err(|e| e.to_string())?;
    if !failures.is_empty() {
        return Err(format!("Saved, but syncing to projects failed:\n{}", failures.join("\n")));
    }

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

    // Drop the project links first — they would dangle once the source is gone.
    let projects = sqlx::query(
        "SELECT p.path, s.name, ps.synced_hash_claude, ps.synced_hash_codex FROM project_skills ps
         JOIN projects p ON p.id = ps.project_id
         JOIN skills s ON s.id = ps.skill_id
         WHERE ps.skill_id = ?",
    )
    .bind(&id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    let mut errors = Vec::new();
    for prow in &projects {
        let ppath: String = prow.get("path");
        let name: String = prow.get("name");
        let prev_claude: Option<String> = prow.get("synced_hash_claude");
        let prev_codex: Option<String> = prow.get("synced_hash_codex");
        if let Err(e) = remove_skill_artifacts(&ppath, &name, &source_path, prev_claude.as_deref(), prev_codex.as_deref()) {
            errors.push(e);
        }
    }
    if !errors.is_empty() {
        return Err(format!("Not deleted — clean these up first:\n{}", errors.join("\n")));
    }

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

/// Sync a skill to every project that has it applied: (re)create links for the current target
/// and name, drop the ones no longer targeted. Content needs no copying — projects link to the
/// source. Returns one message per project that failed.
pub async fn sync_skill_to_projects(db: &Db, skill_id: &str) -> Result<Vec<String>> {
    use sqlx::Row;

    let skill = sqlx::query("SELECT name, target, source_path FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(db)
        .await?;
    let name: String = skill.get("name");
    let target: String = skill.get("target");
    let source_path: String = skill.get("source_path");

    let rows = sqlx::query(
        "SELECT ps.project_id, ps.synced_hash_claude, ps.synced_hash_codex, p.path, p.name as project_name
         FROM project_skills ps
         JOIN projects p ON p.id = ps.project_id
         WHERE ps.skill_id = ?"
    )
    .bind(skill_id)
    .fetch_all(db)
    .await?;

    let mut failures = Vec::new();
    for row in rows {
        let project_id: String = row.get("project_id");
        let project_path: String = row.get("path");
        let project_name: String = row.get("project_name");

        let prev_claude: Option<String> = row.get("synced_hash_claude");
        let prev_codex: Option<String> = row.get("synced_hash_codex");
        let result = write_skill_artifacts(
            &project_path,
            &name,
            &target,
            &source_path,
            prev_claude.as_deref(),
            prev_codex.as_deref(),
        );
        let (next_claude, next_codex) = match result {
            Ok(hashes) => hashes,
            Err(e) => {
                failures.push(format!("{}: {}", project_name, e));
                continue;
            }
        };

        sqlx::query(
            "UPDATE project_skills SET target = ?, synced_hash_claude = ?, synced_hash_codex = ? WHERE project_id = ? AND skill_id = ?"
        )
        .bind(&target)
        .bind(&next_claude)
        .bind(&next_codex)
        .bind(&project_id)
        .bind(skill_id)
        .execute(db)
        .await?;
    }

    Ok(failures)
}
