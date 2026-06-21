//! Project file listing for the chat composer's `@`-mention autocomplete.
//!
//! Walks the project directory honoring `.gitignore` (via the `ignore` crate),
//! returning relative POSIX-style paths the frontend can fuzzy-search. Claude
//! reads any mentioned path through its Read tool since the run's cwd is the
//! project root, so we only need to surface paths — not file contents.

use ignore::WalkBuilder;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Hard ceiling so huge repos can't flood the IPC channel or the picker.
const MAX_ENTRIES: usize = 8000;

/// Cap a single file read so a huge/binary file can't flood the IPC channel.
const MAX_FILE_BYTES: usize = 2 * 1024 * 1024; // 2 MiB

#[derive(Debug, Serialize, Clone)]
pub struct ProjectEntry {
    /// Relative POSIX path from the project root.
    pub path: String,
    /// True for directories (mentionable too — Claude can ls/glob inside).
    pub is_dir: bool,
}

#[tauri::command]
pub async fn list_project_files(project_path: String) -> Result<Vec<ProjectEntry>, String> {
    let root = project_path.clone();
    // Walking the tree is blocking IO; keep it off the async runtime threads.
    tokio::task::spawn_blocking(move || walk(&root))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

/// Contents of a single project file, for the RunView file viewer.
#[derive(Debug, Serialize, Clone)]
pub struct FileContent {
    /// Relative POSIX path from the project root (what the UI shows).
    pub path: String,
    pub content: String,
    /// True when the file was longer than MAX_FILE_BYTES and got cut off.
    pub truncated: bool,
}

/// Read a file referenced by the AI (tool call or prose mention) so the UI can
/// preview it. `file_path` may be absolute or relative to the project root, but
/// the resolved path must stay inside the project — we refuse anything that
/// escapes the root (path traversal / absolute paths elsewhere on disk).
#[tauri::command]
pub async fn read_project_file(
    project_path: String,
    file_path: String,
) -> Result<FileContent, String> {
    tokio::task::spawn_blocking(move || read_within(&project_path, &file_path))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

fn read_within(project_path: &str, file_path: &str) -> Result<FileContent, String> {
    let root = Path::new(project_path)
        .canonicalize()
        .map_err(|e| format!("invalid project path: {e}"))?;

    // Resolve the requested path: absolute as-is, relative against the root.
    let requested = Path::new(file_path);
    let joined: PathBuf = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };

    let canonical = joined
        .canonicalize()
        .map_err(|_| format!("File not found: {file_path}"))?;

    // Refuse anything outside the project root.
    if !canonical.starts_with(&root) {
        return Err("Refusing to read a file outside the project".to_string());
    }
    if canonical.is_dir() {
        return Err(format!("{file_path} is a directory, not a file"));
    }

    let bytes = std::fs::read(&canonical).map_err(|e| format!("read {file_path}: {e}"))?;
    // Reject binary files (NUL byte heuristic) — the viewer renders text only.
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err("Binary file — cannot preview".to_string());
    }
    let truncated = bytes.len() > MAX_FILE_BYTES;
    let slice = if truncated { &bytes[..MAX_FILE_BYTES] } else { &bytes[..] };
    let content = String::from_utf8_lossy(slice).into_owned();

    // Display path relative to the root when possible, else the original input.
    let rel = canonical
        .strip_prefix(&root)
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| file_path.to_string());

    Ok(FileContent { path: rel, content, truncated })
}

fn walk(root: &str) -> Result<Vec<ProjectEntry>, String> {
    let root_path = Path::new(root);
    if !root_path.is_dir() {
        return Err(format!("not a directory: {root}"));
    }

    let mut entries: Vec<ProjectEntry> = Vec::new();

    let walker = WalkBuilder::new(root_path)
        .hidden(false) // surface dotfiles; .gitignore still applies
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build();

    for result in walker {
        if entries.len() >= MAX_ENTRIES {
            break;
        }
        let entry = match result {
            Ok(e) => e,
            Err(_) => continue,
        };
        // Skip the root itself.
        let rel = match entry.path().strip_prefix(root_path) {
            Ok(r) if !r.as_os_str().is_empty() => r,
            _ => continue,
        };
        let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let path = rel.to_string_lossy().replace('\\', "/");
        entries.push(ProjectEntry { path, is_dir });
    }

    // Files before dirs is unnecessary; sort by path for a stable picker order.
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}
