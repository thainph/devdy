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
/// Paths are tiny (just relative strings), so this can be generous — the old
/// 8k cap silently dropped files in larger monorepos, making the `@`-mention
/// picker look like it "couldn't find" perfectly real files.
const MAX_ENTRIES: usize = 50000;

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

/// True when `path` lives under one of the shared AI transcript stores
/// (`~/.claude` or `~/.codex`). These hold the session logs Devdy mirrors, so
/// the file viewer is allowed to read them even though they sit outside any
/// project directory.
fn transcript_root_allows(path: &Path) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    let home = Path::new(&home);
    [".claude", ".codex"].iter().any(|sub| {
        home.join(sub)
            .canonicalize()
            .map(|root| path.starts_with(&root))
            .unwrap_or(false)
    })
}

/// True when `path` lives anywhere under the user's HOME directory. Files
/// dragged into the composer (e.g. from `~/Downloads`) sit outside any project,
/// but they're the user's own files and the viewer is a read-only preview, so we
/// allow previewing them. Keeps system directories (`/etc`, `/var`, …) off-limits.
fn home_allows(path: &Path) -> bool {
    let Ok(home) = std::env::var("HOME") else {
        return false;
    };
    Path::new(&home)
        .canonicalize()
        .map(|root| path.starts_with(&root))
        .unwrap_or(false)
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

    // Allow the project root plus the shared AI transcript stores. Session logs
    // Devdy mirrors live under `~/.claude` / `~/.codex`, so the file viewer must
    // be able to read them even though they sit outside any project.
    if !canonical.starts_with(&root)
        && !transcript_root_allows(&canonical)
        && !home_allows(&canonical)
    {
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

/// An image read from anywhere on disk, encoded as raw base64 for the composer's
/// drag-and-drop flow. Non-image files are referenced by path (no read), so this
/// is used only for image attachments dropped from outside the project.
#[derive(Debug, Serialize, Clone)]
pub struct FileBase64 {
    /// e.g. "image/png". Derived from the file extension.
    pub media_type: String,
    /// Raw base64 (no `data:` prefix), matching the image attachment format the
    /// sidecars expect.
    pub data: String,
}

/// Ceiling for a single dropped image — mirrors the frontend's 10 MiB limit and
/// keeps the base64 payload off the IPC channel from getting unbounded.
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

/// Read an image at an arbitrary absolute path and return it as base64.
///
/// The frontend `plugin-fs` scope is deliberately narrow (app dirs only), so
/// reading an image dropped from Finder/Desktop must go through the backend,
/// which has full filesystem access. We validate that the path is an image by
/// extension and cap the size.
#[tauri::command]
pub async fn read_file_base64(path: String) -> Result<FileBase64, String> {
    tokio::task::spawn_blocking(move || read_file_base64_inner(&path))
        .await
        .map_err(|e| format!("join error: {e}"))?
}

fn read_file_base64_inner(path: &str) -> Result<FileBase64, String> {
    let p = Path::new(path);
    let media_type = image_media_type(p)
        .ok_or_else(|| "Not a supported image file".to_string())?;
    let meta = std::fs::metadata(p).map_err(|e| format!("stat {path}: {e}"))?;
    if meta.len() as usize > MAX_IMAGE_BYTES {
        return Err(format!(
            "Ảnh quá lớn (tối đa {}MB).",
            MAX_IMAGE_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(p).map_err(|e| format!("read {path}: {e}"))?;
    Ok(FileBase64 { media_type, data: base64_encode(&bytes) })
}

/// Map an image file extension to its MIME type. Returns `None` for anything we
/// don't treat as an inline image (those go through the path-reference flow).
fn image_media_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return None,
    };
    Some(mime.to_string())
}

/// Minimal standard-alphabet base64 encoder (no padding-free / URL-safe quirks),
/// avoiding a new crate dependency for this single use.
fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 { TABLE[((n >> 6) & 0x3f) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[(n & 0x3f) as usize] as char } else { '=' });
    }
    out
}
