use crate::db::Db;
use crate::commands::skills::{remove_skill_artifacts, write_skill_artifacts};
use crate::commands::rules::{ApplyAllOutcome, ApplyFailure};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub path: String,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
    pub created_at: String,
    pub github_account_id: Option<String>,
    /// GĐ6: linked GitLab account (mirrors `github_account_id`). `None` when the
    /// project has no GitLab account attached (backward-compatible default).
    #[serde(default)]
    pub gitlab_account_id: Option<String>,
    /// Number of runs recorded against this project (usage frequency).
    #[serde(default)]
    pub run_count: i64,
    /// Timestamp of the most recent run, if any (recency).
    #[serde(default)]
    pub last_used_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AppliedSkill {
    pub skill_id: String,
    pub skill_name: String,
    pub skill_description: String,
    pub target: String,
    pub has_claude: bool,
    pub has_codex: bool,
    pub applied_at: String,
}

#[derive(Debug, Deserialize)]
pub struct AddRepoPayload {
    pub name: String,
    pub path: String,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddProjectPayload {
    pub path: String,
    pub name: Option<String>,
    pub repos: Option<Vec<AddRepoPayload>>,
}

fn detect_github_remote(project_path: &str) -> Option<(String, String)> {
    let output = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(project_path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // Parse github.com/owner/repo from https or git URL
    let url = url.trim_end_matches(".git");
    if let Some(path) = url.strip_prefix("https://github.com/") {
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    if let Some(path) = url.strip_prefix("git@github.com:") {
        let parts: Vec<&str> = path.splitn(2, '/').collect();
        if parts.len() == 2 {
            return Some((parts[0].to_string(), parts[1].to_string()));
        }
    }
    None
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Repo {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub path: String,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct DetectedRepo {
    pub name: String,
    pub path: String,
    pub github_owner: Option<String>,
    pub github_repo: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DetectedProjectInfo {
    pub name: String,
    pub path: String,
    pub repos: Vec<DetectedRepo>,
}

#[tauri::command]
pub fn open_in_vscode(path: String, file: Option<String>) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    // When a file is provided, open the project folder AND reveal that file
    // (e.g. the most recently fetched issue/PR markdown).
    let file = file.filter(|f| Path::new(f).exists());

    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.args(["-a", "Visual Studio Code", &path]);
        if let Some(ref f) = file {
            cmd.arg(f);
        }
        cmd.spawn()
            .map_err(|e| format!("Failed to open VS Code: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "code", &path]);
        if let Some(ref f) = file {
            cmd.arg(f);
        }
        cmd.spawn()
            .map_err(|e| format!("Failed to open VS Code: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        let mut cmd = std::process::Command::new("code");
        cmd.arg(&path);
        if let Some(ref f) = file {
            cmd.arg(f);
        }
        cmd.spawn()
            .map_err(|e| format!("Failed to open VS Code: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub fn open_in_folder(path: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

#[tauri::command]
pub fn open_in_terminal(path: String, terminal_app: String) -> Result<(), String> {
    if !Path::new(&path).exists() {
        return Err(format!("Path does not exist: {}", path));
    }

    #[cfg(target_os = "macos")]
    {
        // `open -a <App> <path>` launches the app with a new window/tab cd'd into `path`.
        let app = if terminal_app == "iterm" {
            "iTerm"
        } else {
            "Terminal"
        };
        std::process::Command::new("open")
            .args(["-a", app, &path])
            .spawn()
            .map_err(|e| format!("Failed to open {}: {}", app, e))?;
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = terminal_app;
        return Err("Open in terminal is only supported on macOS".to_string());
    }

    Ok(())
}

#[tauri::command]
pub async fn detect_project_info(path: String) -> Result<DetectedProjectInfo, String> {
    if !Path::new(&path).exists() {
        return Err("Path does not exist".to_string());
    }
    let project_name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "Project".to_string());

    let mut repos: Vec<DetectedRepo> = Vec::new();

    // Check root for .git
    if Path::new(&path).join(".git").exists() {
        let github = detect_github_remote(&path);
        repos.push(DetectedRepo {
            name: project_name.clone(),
            path: path.clone(),
            github_owner: github.as_ref().map(|(o, _)| o.clone()),
            github_repo: github.as_ref().map(|(_, r)| r.clone()),
        });
    }

    // Scan immediate subdirectories for .git
    if let Ok(entries) = std::fs::read_dir(&path) {
        let mut subdirs: Vec<std::path::PathBuf> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| e.path())
            .collect();
        subdirs.sort();
        for subdir in subdirs {
            if subdir.join(".git").exists() {
                let subdir_str = subdir.to_string_lossy().to_string();
                let subdir_name = subdir
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| subdir_str.clone());
                let github = detect_github_remote(&subdir_str);
                repos.push(DetectedRepo {
                    name: subdir_name,
                    path: subdir_str,
                    github_owner: github.as_ref().map(|(o, _)| o.clone()),
                    github_repo: github.as_ref().map(|(_, r)| r.clone()),
                });
            }
        }
    }

    Ok(DetectedProjectInfo {
        name: project_name,
        path,
        repos,
    })
}

#[tauri::command]
pub async fn list_projects(db: State<'_, Db>) -> Result<Vec<Project>, String> {
    use sqlx::Row;
    // Sort by usage frequency (number of runs), then recency, then name.
    let rows = sqlx::query(
        "SELECT p.id, p.name, p.path, p.created_at, p.github_account_id, p.gitlab_account_id, \
                COUNT(r.id) AS run_count, MAX(r.created_at) AS last_used_at \
         FROM projects p \
         LEFT JOIN runs r ON r.project_id = p.id \
         GROUP BY p.id \
         ORDER BY run_count DESC, last_used_at DESC, p.name COLLATE NOCASE ASC"
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let projects = rows.iter().map(|row| {
        Project {
            id: row.get("id"),
            name: row.get("name"),
            path: row.get("path"),
            github_owner: None,
            github_repo: None,
            created_at: row.get("created_at"),
            github_account_id: row.get("github_account_id"),
            gitlab_account_id: row.get("gitlab_account_id"),
            run_count: row.get("run_count"),
            last_used_at: row.get("last_used_at"),
        }
    }).collect();

    Ok(projects)
}

#[tauri::command]
pub async fn add_project(
    db: State<'_, Db>,
    payload: AddProjectPayload,
) -> Result<Project, String> {
    if !Path::new(&payload.path).exists() {
        return Err("Project path does not exist".to_string());
    }

    let name = payload.name.unwrap_or_else(|| {
        Path::new(&payload.path)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Project".to_string())
    });

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&name)
    .bind(&payload.path)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    if let Some(repos) = payload.repos {
        for repo in repos {
            let repo_id = Uuid::new_v4().to_string();
            sqlx::query(
                "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
            )
            .bind(&repo_id)
            .bind(&id)
            .bind(&repo.name)
            .bind(&repo.path)
            .bind(&repo.github_owner)
            .bind(&repo.github_repo)
            .bind(&now)
            .execute(db.inner())
            .await
            .map_err(|e| e.to_string())?;
        }
    }

    Ok(Project {
        id,
        name,
        path: payload.path,
        github_owner: None,
        github_repo: None,
        created_at: now,
        github_account_id: None,
        gitlab_account_id: None,
        run_count: 0,
        last_used_at: None,
    })
}

#[tauri::command]
pub async fn remove_project(
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM projects WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn update_project(
    db: State<'_, Db>,
    id: String,
    name: String,
) -> Result<Project, String> {
    use sqlx::Row;
    sqlx::query(
        "UPDATE projects SET name = ? WHERE id = ?"
    )
    .bind(&name)
    .bind(&id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query(
        "SELECT p.id, p.name, p.path, p.created_at, p.github_account_id, p.gitlab_account_id, \
                COUNT(r.id) AS run_count, MAX(r.created_at) AS last_used_at \
         FROM projects p LEFT JOIN runs r ON r.project_id = p.id WHERE p.id = ? GROUP BY p.id"
    )
        .bind(&id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(Project {
        id: row.get("id"),
        name: row.get("name"),
        path: row.get("path"),
        github_owner: None,
        github_repo: None,
        created_at: row.get("created_at"),
        github_account_id: row.get("github_account_id"),
        gitlab_account_id: row.get("gitlab_account_id"),
        run_count: row.get("run_count"),
        last_used_at: row.get("last_used_at"),
    })
}

#[tauri::command]
pub async fn list_repos(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<Repo>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT id, project_id, name, path, github_owner, github_repo, created_at FROM repos WHERE project_id = ? ORDER BY name"
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| Repo {
        id: row.get("id"),
        project_id: row.get("project_id"),
        name: row.get("name"),
        path: row.get("path"),
        github_owner: row.get("github_owner"),
        github_repo: row.get("github_repo"),
        created_at: row.get("created_at"),
    }).collect())
}

#[tauri::command]
pub async fn add_repo(
    db: State<'_, Db>,
    project_id: String,
    name: String,
    path: String,
    github_owner: Option<String>,
    github_repo: Option<String>,
) -> Result<Repo, String> {
    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&id)
    .bind(&project_id)
    .bind(&name)
    .bind(&path)
    .bind(&github_owner)
    .bind(&github_repo)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(Repo { id, project_id, name, path, github_owner, github_repo, created_at: now })
}

#[tauri::command]
pub async fn update_repo(
    db: State<'_, Db>,
    id: String,
    name: String,
    github_owner: Option<String>,
    github_repo: Option<String>,
) -> Result<Repo, String> {
    use sqlx::Row;
    sqlx::query(
        "UPDATE repos SET name = ?, github_owner = ?, github_repo = ? WHERE id = ?"
    )
    .bind(&name)
    .bind(&github_owner)
    .bind(&github_repo)
    .bind(&id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    let row = sqlx::query(
        "SELECT id, project_id, name, path, github_owner, github_repo, created_at FROM repos WHERE id = ?"
    )
    .bind(&id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(Repo {
        id: row.get("id"),
        project_id: row.get("project_id"),
        name: row.get("name"),
        path: row.get("path"),
        github_owner: row.get("github_owner"),
        github_repo: row.get("github_repo"),
        created_at: row.get("created_at"),
    })
}

#[tauri::command]
pub async fn remove_repo(
    db: State<'_, Db>,
    id: String,
) -> Result<(), String> {
    sqlx::query("DELETE FROM repos WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_applied_skills(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<AppliedSkill>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT ps.skill_id, s.name as skill_name, s.description as skill_description,
                ps.target, ps.synced_hash_claude, ps.synced_hash_codex, ps.applied_at
         FROM project_skills ps
         JOIN skills s ON s.id = ps.skill_id
         WHERE ps.project_id = ?
         ORDER BY s.name"
    )
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| {
        let claude: Option<String> = row.get("synced_hash_claude");
        let codex: Option<String> = row.get("synced_hash_codex");
        AppliedSkill {
            skill_id: row.get("skill_id"),
            skill_name: row.get("skill_name"),
            skill_description: row.get("skill_description"),
            target: row.get("target"),
            has_claude: claude.is_some(),
            has_codex: codex.is_some(),
            applied_at: row.get("applied_at"),
        }
    }).collect())
}

#[tauri::command]
pub async fn apply_skill(
    db: State<'_, Db>,
    project_id: String,
    skill_id: String,
) -> Result<(), String> {
    apply_skill_to_project(db.inner(), &project_id, &skill_id).await
}

#[tauri::command]
pub async fn apply_skill_to_all_projects(
    db: State<'_, Db>,
    skill_id: String,
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
        match apply_skill_to_project(db.inner(), &project_id, &skill_id).await {
            Ok(()) => applied += 1,
            Err(error) => failures.push(ApplyFailure { project_id, project_name, error }),
        }
    }
    Ok(ApplyAllOutcome { applied, failures })
}

/// Core apply logic shared by `apply_skill` and `apply_skill_to_all_projects`.
async fn apply_skill_to_project(
    db: &Db,
    project_id: &str,
    skill_id: &str,
) -> Result<(), String> {
    use sqlx::Row;

    let skill_row = sqlx::query("SELECT name, description, target, source_path FROM skills WHERE id = ?")
        .bind(skill_id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("Skill not found: {}", e))?;

    let skill_name: String = skill_row.get("name");
    let description: String = skill_row.get("description");
    let target: String = skill_row.get("target");
    let source_path: String = skill_row.get("source_path");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await
        .map_err(|e| format!("Project not found: {}", e))?;

    let project_path: String = project_row.get("path");

    let (hash_claude, hash_codex) =
        write_skill_artifacts(&project_path, &skill_name, &description, &target, &source_path)?;
    let now = chrono::Utc::now().to_rfc3339();

    sqlx::query(
        "INSERT OR REPLACE INTO project_skills
         (project_id, skill_id, target, synced_hash_claude, synced_hash_codex, applied_at)
         VALUES (?, ?, ?, ?, ?, ?)"
    )
    .bind(project_id)
    .bind(skill_id)
    .bind(&target)
    .bind(&hash_claude)
    .bind(&hash_codex)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    Ok(())
}

#[tauri::command]
pub async fn remove_skill_from_project(
    db: State<'_, Db>,
    project_id: String,
    skill_id: String,
) -> Result<(), String> {
    use sqlx::Row;

    let skill_row = sqlx::query("SELECT name FROM skills WHERE id = ?")
        .bind(&skill_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let skill_name: String = skill_row.get("name");

    let project_row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    let project_path: String = project_row.get("path");

    remove_skill_artifacts(&project_path, &skill_name);

    sqlx::query("DELETE FROM project_skills WHERE project_id = ? AND skill_id = ?")
        .bind(&project_id)
        .bind(&skill_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}

#[derive(Debug, Serialize)]
pub struct SyncConflict {
    pub id: String,
    pub project_id: String,
    pub skill_id: String,
    pub skill_name: String,
    pub project_name: String,
    pub engine: String,
    pub detected_at: String,
    pub local_hash: String,
    pub source_hash: String,
    pub resolved: bool,
}

#[tauri::command]
pub async fn list_sync_conflicts(
    db: State<'_, Db>,
) -> Result<Vec<SyncConflict>, String> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT sc.id, sc.project_id, sc.skill_id, s.name as skill_name, p.name as project_name,
                sc.engine, sc.detected_at, sc.local_hash, sc.source_hash, sc.resolved
         FROM sync_conflicts sc
         JOIN skills s ON s.id = sc.skill_id
         JOIN projects p ON p.id = sc.project_id
         WHERE sc.resolved = 0
         ORDER BY sc.detected_at DESC"
    )
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows.iter().map(|row| {
        let resolved: i64 = row.get("resolved");
        SyncConflict {
            id: row.get("id"),
            project_id: row.get("project_id"),
            skill_id: row.get("skill_id"),
            skill_name: row.get("skill_name"),
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
pub async fn resolve_sync_conflict(
    db: State<'_, Db>,
    conflict_id: String,
    overwrite: bool,
) -> Result<(), String> {
    use sqlx::Row;
    if overwrite {
        // Get conflict details, then re-apply skill from source for the affected engine.
        let row = sqlx::query(
            "SELECT sc.project_id, sc.skill_id, sc.engine, s.source_path, s.name as skill_name,
                    s.description as skill_description, p.path as project_path
             FROM sync_conflicts sc
             JOIN skills s ON s.id = sc.skill_id
             JOIN projects p ON p.id = sc.project_id
             WHERE sc.id = ?"
        )
        .bind(&conflict_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| e.to_string())?;

        let project_path: String = row.get("project_path");
        let source_path: String = row.get("source_path");
        let skill_name: String = row.get("skill_name");
        let description: String = row.get("skill_description");
        let project_id: String = row.get("project_id");
        let skill_id: String = row.get("skill_id");
        let engine: String = row.get("engine");

        // Re-apply only the conflicting engine (pass the engine as target).
        let (hash_claude, hash_codex) =
            write_skill_artifacts(&project_path, &skill_name, &description, &engine, &source_path)?;

        if engine == "codex" {
            sqlx::query("UPDATE project_skills SET synced_hash_codex = ? WHERE project_id = ? AND skill_id = ?")
                .bind(&hash_codex)
                .bind(&project_id)
                .bind(&skill_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        } else {
            sqlx::query("UPDATE project_skills SET synced_hash_claude = ? WHERE project_id = ? AND skill_id = ?")
                .bind(&hash_claude)
                .bind(&project_id)
                .bind(&skill_id)
                .execute(db.inner())
                .await
                .map_err(|e| e.to_string())?;
        }
    }

    sqlx::query("UPDATE sync_conflicts SET resolved = 1 WHERE id = ?")
        .bind(&conflict_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;

    Ok(())
}
