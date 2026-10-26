//! Read-only checks on how Claude Code and Codex will load a project's instructions.
//!
//! Devdy installs Claude artifacts under `.claude/` and Codex artifacts under `AGENTS.md` /
//! `.agents/skills/`. These checks flag setups where one engine also reads the other's files
//! (duplicated context), where Codex silently truncates AGENTS.md, or where Devdy's symlinks
//! could be committed. They only warn — nothing here changes the project.

use crate::db::Db;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tauri::State;

/// Codex's default `project_doc_max_bytes`; AGENTS.md content past it is dropped.
const CODEX_DEFAULT_MAX_BYTES: u64 = 32 * 1024;

/// File names Claude Code treats as a project's own CLAUDE.md.
const CLAUDE_MD_NAMES: [&str; 3] = ["CLAUDE.md", ".claude/CLAUDE.md", "CLAUDE.local.md"];

/// Directories Devdy links skills/rules into.
const LINK_DIRS: [&str; 3] = [".claude/skills", ".claude/rules", ".agents/skills"];

#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum InstructionWarning {
    /// AGENTS.md is bigger than Codex reads; the tail is ignored.
    AgentsMdTooLarge { size: u64, limit: u64 },
    /// No CLAUDE.md, so Claude Code falls back to reading AGENTS.md (Codex's file).
    ClaudeReadsAgentsMd,
    /// A CLAUDE.md imports AGENTS.md, so Claude reads Codex's file.
    ClaudeMdImportsAgentsMd { path: String },
    /// Codex config lists CLAUDE.md as a fallback instructions file.
    CodexReadsClaudeMd { config: String },
    /// Devdy's symlinks in this directory are not git-ignored; committed, they break elsewhere.
    LinksNotIgnored { dir: String },
    /// Old copies Devdy couldn't vouch for were moved here instead of being deleted. Shown until
    /// the user reviews and removes the folder.
    LegacyCopiesBackedUp { dir: String, batches: usize },
}

#[tauri::command]
pub async fn get_instruction_warnings(
    db: State<'_, Db>,
    project_id: String,
) -> Result<Vec<InstructionWarning>, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT path FROM projects WHERE id = ?")
        .bind(&project_id)
        .fetch_one(db.inner())
        .await
        .map_err(|e| format!("Project not found: {}", e))?;
    let path: String = row.get("path");
    let codex_home = codex_home();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    Ok(check_project(Path::new(&path), codex_home.as_deref(), home.as_deref()))
}

fn codex_home() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CODEX_HOME") {
        return Some(PathBuf::from(dir));
    }
    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".codex"))
}

pub fn check_project(project: &Path, codex_home: Option<&Path>, home: Option<&Path>) -> Vec<InstructionWarning> {
    let mut warnings = Vec::new();
    // Project config overrides the user config for the same key.
    let configs: Vec<PathBuf> = [
        codex_home.map(|h| h.join("config.toml")),
        Some(project.join(".codex").join("config.toml")),
    ]
    .into_iter()
    .flatten()
    .collect();

    let agents = project.join("AGENTS.md");
    let agents_size = fs::metadata(&agents).map(|m| m.len()).unwrap_or(0);
    if agents_size > 0 {
        let limit = configs
            .iter()
            .filter_map(|c| read_toml(c))
            .filter_map(|t| t.get("project_doc_max_bytes").and_then(|v| v.as_integer()))
            .next_back()
            .map(|v| v.max(0) as u64)
            .unwrap_or(CODEX_DEFAULT_MAX_BYTES);
        if agents_size > limit {
            warnings.push(InstructionWarning::AgentsMdTooLarge { size: agents_size, limit });
        }
        if !has_claude_md(project, home) {
            warnings.push(InstructionWarning::ClaudeReadsAgentsMd);
        }
    }

    for name in CLAUDE_MD_NAMES {
        let file = project.join(name);
        if fs::read_to_string(&file).is_ok_and(|c| imports_agents_md(&c)) {
            warnings.push(InstructionWarning::ClaudeMdImportsAgentsMd { path: name.to_string() });
        }
    }

    for config in &configs {
        let lists_claude_md = read_toml(config)
            .and_then(|t| t.get("project_doc_fallback_filenames").cloned())
            .and_then(|v| v.as_array().cloned())
            .is_some_and(|names| {
                names.iter().any(|n| n.as_str().is_some_and(|s| s.eq_ignore_ascii_case("CLAUDE.md")))
            });
        if lists_claude_md {
            warnings.push(InstructionWarning::CodexReadsClaudeMd {
                config: config.to_string_lossy().to_string(),
            });
        }
    }

    for dir in LINK_DIRS {
        if let Some(link) = first_symlink(&project.join(dir)) {
            if git_tracks_path(project, &Path::new(dir).join(link.file_name().unwrap_or_default())) {
                warnings.push(InstructionWarning::LinksNotIgnored { dir: dir.to_string() });
            }
        }
    }

    let backups = crate::commands::project_links::project_backup_dir(project);
    let batches = fs::read_dir(&backups).map(|d| d.flatten().count()).unwrap_or(0);
    if batches > 0 {
        warnings.push(InstructionWarning::LegacyCopiesBackedUp {
            dir: backups.to_string_lossy().to_string(),
            batches,
        });
    }

    warnings
}

/// Claude Code skips AGENTS.md when the project (or a folder above it) has a CLAUDE.md of its
/// own. `~/.claude/CLAUDE.md` is the user's memory, not a project file, so it doesn't count.
fn has_claude_md(project: &Path, home: Option<&Path>) -> bool {
    let user_memory = home.map(|h| h.join(".claude").join("CLAUDE.md"));
    project.ancestors().any(|dir| {
        CLAUDE_MD_NAMES.iter().any(|name| {
            let file = dir.join(name);
            Some(&file) != user_memory.as_ref() && file.is_file()
        })
    })
}

/// `@AGENTS.md` / `@./AGENTS.md` import lines in a CLAUDE.md.
fn imports_agents_md(content: &str) -> bool {
    content.split_whitespace().any(|token| {
        let token = token.trim_end_matches([',', '.', ')', ';']);
        matches!(token, "@AGENTS.md" | "@./AGENTS.md")
    })
}

fn read_toml(path: &Path) -> Option<toml::Table> {
    fs::read_to_string(path).ok()?.parse::<toml::Table>().ok()
}

fn first_symlink(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
}

/// True when `rel` (relative to `project`) sits in a git work tree and is not ignored, i.e. could
/// be committed. Relative so a project path reached through a symlink still resolves.
fn git_tracks_path(project: &Path, rel: &Path) -> bool {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(["check-ignore", "-q", "--"])
        .arg(rel)
        .output();
    // 0 = ignored, 1 = not ignored, 128 = not a repo / error.
    matches!(output, Ok(o) if o.status.code() == Some(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-checks-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn flags_large_agents_md_and_missing_claude_md() {
        let project = tmp("large");
        fs::write(project.join("AGENTS.md"), "x".repeat(40 * 1024)).unwrap();
        let w = check_project(&project, None, None);
        assert!(w.contains(&InstructionWarning::AgentsMdTooLarge { size: 40 * 1024, limit: CODEX_DEFAULT_MAX_BYTES }));
        assert!(w.contains(&InstructionWarning::ClaudeReadsAgentsMd));

        fs::write(project.join("CLAUDE.md"), "# Project\n").unwrap();
        assert!(!check_project(&project, None, None).contains(&InstructionWarning::ClaudeReadsAgentsMd));
    }

    #[test]
    fn user_memory_claude_md_does_not_count_as_project_claude_md() {
        let home = tmp("home");
        let project = home.join("work/app");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(home.join(".claude/CLAUDE.md"), "# me\n").unwrap();
        fs::write(project.join("AGENTS.md"), "rules\n").unwrap();
        assert!(check_project(&project, None, Some(&home)).contains(&InstructionWarning::ClaudeReadsAgentsMd));
    }

    #[test]
    fn honours_codex_max_bytes_override() {
        let project = tmp("limit");
        let home = tmp("limit-home");
        fs::write(project.join("AGENTS.md"), "x".repeat(40 * 1024)).unwrap();
        fs::write(home.join("config.toml"), "project_doc_max_bytes = 65536\n").unwrap();
        let w = check_project(&project, Some(&home), None);
        assert!(!w.iter().any(|w| matches!(w, InstructionWarning::AgentsMdTooLarge { .. })));
    }

    #[test]
    fn flags_cross_reads() {
        let project = tmp("cross");
        let home = tmp("cross-home");
        fs::write(project.join("CLAUDE.md"), "See @AGENTS.md for shared rules.\n").unwrap();
        fs::write(
            home.join("config.toml"),
            "project_doc_fallback_filenames = [\"TEAM.md\", \"CLAUDE.md\"]\n",
        )
        .unwrap();
        let w = check_project(&project, Some(&home), None);
        assert!(w.contains(&InstructionWarning::ClaudeMdImportsAgentsMd { path: "CLAUDE.md".into() }));
        assert!(w.iter().any(|w| matches!(w, InstructionWarning::CodexReadsClaudeMd { .. })));
    }

    #[test]
    fn plain_mention_of_agents_md_is_not_an_import() {
        assert!(!imports_agents_md("Codex reads AGENTS.md, Claude does not."));
        assert!(imports_agents_md("@./AGENTS.md"));
    }
}
