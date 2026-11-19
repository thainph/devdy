//! Disk-usage statistics for session/run data.
//!
//! `get_storage_stats` measures how much disk space Devdy's per-project data and
//! the mirrored Claude/Codex CLI sessions occupy, grouped into categories. It is
//! read-only: space is reclaimed by deleting sessions in History, which removes
//! a session's Devdy log, fetched task files and engine transcripts together.
//! Sweeping a category wholesale here used to leave runs pointing at
//! transcripts that no longer existed, so they could not resume.
//!
//! The Devdy row counts every file under each project's `.devdy/` (run logs,
//! fetched issue/PR files, worktrees, …), split by top-level folder in `parts`.
//! Claude/Codex categories cover the CLI's own session stores
//! (`~/.claude/projects/**/*.jsonl` plus every managed account,
//! `~/.codex/sessions/**/*.jsonl`).

use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use std::path::{Path, PathBuf};
use tauri::State;
use walkdir::WalkDir;

/// Stable category identifiers shared with the frontend.
const CAT_DEVDY: &str = "devdy_data";
const CAT_CLAUDE: &str = "claude_sessions";
const CAT_CODEX: &str = "codex_sessions";

#[derive(Debug, Serialize)]
pub struct StorageCategory {
    /// One of `devdy_data` | `claude_sessions` | `codex_sessions`.
    pub id: String,
    pub label: String,
    pub description: String,
    /// Representative location shown in the UI (may stand for several roots).
    pub path: String,
    pub size_bytes: u64,
    pub file_count: u64,
    /// Breakdown by top-level folder, largest first (only the Devdy row).
    pub parts: Vec<StoragePart>,
}

#[derive(Debug, Serialize)]
pub struct StoragePart {
    /// Top-level entry under `.devdy/` (`runs`, `worktrees`, …); loose files
    /// directly in `.devdy/` are grouped as `(files)`.
    pub name: String,
    pub size_bytes: u64,
    pub file_count: u64,
}

#[derive(Debug, Serialize)]
pub struct StorageStats {
    pub categories: Vec<StorageCategory>,
    pub total_bytes: u64,
    pub total_files: u64,
}

fn home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

/// `.jsonl` transcript predicate used by both Claude and Codex session stores.
fn is_jsonl(p: &Path) -> bool {
    p.extension().map(|e| e == "jsonl").unwrap_or(false)
}

/// Walk `root`, summing size and count of regular files matching `pred`.
/// Missing roots contribute nothing. Symlinks are not followed.
fn scan<P: Fn(&Path) -> bool>(root: &Path, pred: &P) -> (u64, u64) {
    let mut bytes = 0u64;
    let mut count = 0u64;
    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if entry.file_type().is_file() && pred(entry.path()) {
            if let Ok(meta) = entry.metadata() {
                bytes += meta.len();
                count += 1;
            }
        }
    }
    (bytes, count)
}

/// Every project's `.devdy` directory, each counted once: duplicates and
/// directories nested inside another one (a project that lives in a sibling's
/// `.devdy/worktrees/…`) are dropped so their bytes aren't summed twice.
async fn devdy_roots(db: &Db) -> Vec<PathBuf> {
    let rows = match sqlx::query("SELECT path FROM projects").fetch_all(db).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    dedupe_roots(
        rows.into_iter()
            .filter_map(|row| row.try_get::<String, _>("path").ok())
            .map(|p| Path::new(&p).join(".devdy"))
            .collect(),
    )
}

fn dedupe_roots(mut dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    // Shorter paths first, so a parent is kept before anything inside it.
    dirs.sort_by_key(|d| d.components().count());
    let mut kept: Vec<PathBuf> = Vec::new();
    for d in dirs {
        if !kept.iter().any(|k| d.starts_with(k)) {
            kept.push(d);
        }
    }
    kept
}

/// Every regular file under `root`, summed per top-level entry.
fn scan_parts(root: &Path, into: &mut std::collections::BTreeMap<String, (u64, u64)>) {
    for entry in WalkDir::new(root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let rel = entry.path().strip_prefix(root).unwrap_or(entry.path());
        let mut comps = rel.components();
        let first = comps.next();
        let name = match (first, comps.next()) {
            (Some(c), Some(_)) => c.as_os_str().to_string_lossy().into_owned(),
            _ => "(files)".to_string(),
        };
        let slot = into.entry(name).or_insert((0, 0));
        slot.0 += meta.len();
        slot.1 += 1;
    }
}

#[tauri::command]
pub async fn get_storage_stats(db: State<'_, Db>) -> Result<StorageStats, String> {
    // Everything under every project's .devdy, by top-level folder.
    // Worktrees can hold whole checkouts (node_modules, target/), so walk them
    // off the async runtime.
    let roots = devdy_roots(db.inner()).await;
    let by_part = tauri::async_runtime::spawn_blocking(move || {
        let mut m = std::collections::BTreeMap::new();
        for root in roots {
            scan_parts(&root, &mut m);
        }
        m
    })
    .await
    .map_err(|e| e.to_string())?;
    let mut parts: Vec<StoragePart> = by_part
        .into_iter()
        .map(|(name, (size_bytes, file_count))| StoragePart { name, size_bytes, file_count })
        .collect();
    parts.sort_by(|a, b| b.size_bytes.cmp(&a.size_bytes).then_with(|| a.name.cmp(&b.name)));
    let devdy_bytes = parts.iter().map(|p| p.size_bytes).sum();
    let devdy_files = parts.iter().map(|p| p.file_count).sum();

    // Claude/Codex CLI session transcripts. Claude includes the global
    // `~/.claude` store plus every managed multi-account config dir (AC-012).
    let (mut claude_bytes, mut claude_files) = match home() {
        Some(h) => scan(&h.join(".claude").join("projects"), &is_jsonl),
        None => (0, 0),
    };
    for dir in crate::commands::claude_accounts::account_config_dirs(db.inner()).await {
        let (b, c) = scan(&Path::new(&dir).join("projects"), &is_jsonl);
        claude_bytes += b;
        claude_files += c;
    }
    let (codex_bytes, codex_files) = match home() {
        Some(h) => scan(&h.join(".codex").join("sessions"), &is_jsonl),
        None => (0, 0),
    };

    let categories = vec![
        StorageCategory {
            id: CAT_DEVDY.into(),
            label: "Devdy data".into(),
            description: "Everything in every project's .devdy folder: run logs, \
                          fetched issue/PR files, worktrees and the rest. A \
                          session's log and fetched files are removed with it \
                          in History."
                .into(),
            path: "<project>/.devdy".into(),
            size_bytes: devdy_bytes,
            file_count: devdy_files,
            parts,
        },
        StorageCategory {
            id: CAT_CLAUDE.into(),
            label: "Claude CLI sessions".into(),
            description: "Claude Code session transcripts (~/.claude/projects/**/*.jsonl, \
                          plus every managed account). Shared with the CLI/VS Code; \
                          a session's transcripts are removed when it is deleted \
                          in History."
                .into(),
            path: "~/.claude/projects".into(),
            size_bytes: claude_bytes,
            file_count: claude_files,
            parts: Vec::new(),
        },
        StorageCategory {
            id: CAT_CODEX.into(),
            label: "Codex CLI sessions".into(),
            description: "Codex rollout session logs (~/.codex/sessions/**/*.jsonl). \
                          Shared with the Codex CLI; a session's rollouts are \
                          removed when it is deleted in History."
                .into(),
            path: "~/.codex/sessions".into(),
            size_bytes: codex_bytes,
            file_count: codex_files,
            parts: Vec::new(),
        },
    ];

    let total_bytes = categories.iter().map(|c| c.size_bytes).sum();
    let total_files = categories.iter().map(|c| c.file_count).sum();

    Ok(StorageStats {
        categories,
        total_bytes,
        total_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_and_duplicate_roots_are_counted_once() {
        let roots = dedupe_roots(vec![
            PathBuf::from("/a/.devdy/worktrees/x/.devdy"),
            PathBuf::from("/a/.devdy"),
            PathBuf::from("/b/.devdy"),
            PathBuf::from("/a/.devdy"),
        ]);
        assert_eq!(roots, vec![PathBuf::from("/a/.devdy"), PathBuf::from("/b/.devdy")]);
    }

    #[test]
    fn files_are_summed_per_top_level_folder() {
        let dir = std::env::temp_dir().join(format!("devdy-storage-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("runs")).unwrap();
        std::fs::create_dir_all(dir.join("worktrees/feat/src")).unwrap();
        std::fs::write(dir.join("runs/a.log"), b"12345").unwrap();
        std::fs::write(dir.join("runs/b.log"), b"123").unwrap();
        std::fs::write(dir.join("worktrees/feat/src/main.rs"), b"1234567890").unwrap();
        std::fs::write(dir.join("config.json"), b"12").unwrap();

        let mut m = std::collections::BTreeMap::new();
        scan_parts(&dir, &mut m);
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(m.get("runs"), Some(&(8, 2)));
        assert_eq!(m.get("worktrees"), Some(&(10, 1)));
        assert_eq!(m.get("(files)"), Some(&(2, 1)));
    }
}
