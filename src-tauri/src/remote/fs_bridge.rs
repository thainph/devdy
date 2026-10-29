//! Remote desktop: this machine's file system as seen by a remote-desktop
//! window on another Devdy.
//!
//! The remote window's UI holds this machine's paths (project dirs, Slack
//! downloads, images in a run…), but native features act on the disk of the
//! machine it is rendered on. These commands let it browse and read files
//! here instead: its file dialogs become an in-app browser built on
//! [`remote_fs_list`], and previews / "open file" pull bytes with
//! [`remote_fs_read`]. They are ordinary app commands, so a remote window
//! reaches them through the IPC bridge like any other `invoke`.
//!
//! Full parity: nothing is hidden or filtered — the local UI can already read
//! any path through the asset protocol (scope `**`).

use std::fs::{self, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::UNIX_EPOCH;

use base64::Engine;
use serde::Serialize;
use tauri::Manager;

/// Most bytes one [`remote_fs_read`] returns (binary crosses the relay as
/// base64 in JSON; callers chunk above this).
pub const MAX_READ: u64 = 4 * 1024 * 1024;
/// Most entries one [`remote_fs_list`] returns. A huge directory would
/// otherwise stat every entry and ship one giant frame over the relay.
pub const MAX_LIST: usize = 5000;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FsEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Last modification, Unix milliseconds (`None` when unavailable).
    pub modified: Option<u64>,
}

/// A directory listing; `truncated` when it held more than [`MAX_LIST`]
/// entries and only the first ones read are returned.
#[derive(Debug, Serialize)]
pub struct FsListing {
    pub entries: Vec<FsEntry>,
    pub truncated: bool,
}

#[derive(Debug, Serialize)]
pub struct FsChunk {
    pub data_base64: String,
    pub total_size: u64,
}

fn modified_ms(meta: &Metadata) -> Option<u64> {
    let d = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    u64::try_from(d.as_millis()).ok()
}

/// Metadata of `path`, following symlinks; a dangling link is described by
/// the link itself rather than failing.
fn metadata(path: &Path) -> std::io::Result<Metadata> {
    fs::metadata(path).or_else(|_| fs::symlink_metadata(path))
}

fn entry(path: &Path, meta: &Metadata) -> FsEntry {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned());
    FsEntry {
        name,
        path: path.to_string_lossy().into_owned(),
        is_dir: meta.is_dir(),
        size: if meta.is_dir() { 0 } else { meta.len() },
        modified: modified_ms(meta),
    }
}

/// Directories first, then by name (case-insensitive, ties broken exactly).
pub fn sort_entries(entries: &mut [FsEntry]) {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.name.cmp(&b.name))
    });
}

/// The byte window a read returns: `offset` clamped to the file, `len`
/// capped at [`MAX_READ`] and at the end of the file.
pub fn read_window(total: u64, offset: u64, len: u64) -> (u64, u64) {
    let start = offset.min(total);
    (start, len.min(MAX_READ).min(total - start))
}

fn io_err(path: &str, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("not found: {path}"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path}"),
        _ => format!("{path}: {e}"),
    }
}

pub fn list_dir(path: &str, max: usize) -> Result<FsListing, String> {
    let mut out = Vec::new();
    let mut truncated = false;
    for item in fs::read_dir(path).map_err(|e| io_err(path, e))? {
        let Ok(item) = item else { continue };
        if out.len() >= max {
            truncated = true;
            break;
        }
        let p = item.path();
        // An entry that vanished or cannot be stat'ed is still listed (parity
        // with Finder), as a plain zero-size file.
        match metadata(&p) {
            Ok(meta) => out.push(entry(&p, &meta)),
            Err(_) => out.push(FsEntry {
                name: item.file_name().to_string_lossy().into_owned(),
                path: p.to_string_lossy().into_owned(),
                is_dir: false,
                size: 0,
                modified: None,
            }),
        }
    }
    sort_entries(&mut out);
    Ok(FsListing { entries: out, truncated })
}

pub fn stat(path: &str) -> Result<FsEntry, String> {
    let meta = metadata(Path::new(path)).map_err(|e| io_err(path, e))?;
    Ok(entry(Path::new(path), &meta))
}

pub fn read_chunk(path: &str, offset: u64, len: u64) -> Result<FsChunk, String> {
    let mut file = fs::File::open(path).map_err(|e| io_err(path, e))?;
    let meta = file.metadata().map_err(|e| io_err(path, e))?;
    if meta.is_dir() {
        return Err(format!("is a directory: {path}"));
    }
    let total = meta.len();
    let (start, n) = read_window(total, offset, len);
    file.seek(SeekFrom::Start(start)).map_err(|e| io_err(path, e))?;
    let mut buf = Vec::with_capacity(n as usize);
    file.take(n).read_to_end(&mut buf).map_err(|e| io_err(path, e))?;
    Ok(FsChunk {
        data_base64: base64::engine::general_purpose::STANDARD.encode(&buf),
        total_size: total,
    })
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

// ---- Tauri commands (called by a remote-desktop window over the bridge) ----

/// The entries of a directory (nothing hidden), directories first; at most
/// [`MAX_LIST`] of them.
#[tauri::command]
pub async fn remote_fs_list(path: String) -> Result<FsListing, String> {
    blocking(move || list_dir(&path, MAX_LIST)).await
}

#[tauri::command]
pub async fn remote_fs_stat(path: String) -> Result<FsEntry, String> {
    blocking(move || stat(&path)).await
}

/// Up to `len` bytes (capped at 4 MiB) of a file from `offset`.
#[tauri::command]
pub async fn remote_fs_read(path: String, offset: u64, len: u64) -> Result<FsChunk, String> {
    blocking(move || read_chunk(&path, offset, len)).await
}

#[tauri::command]
pub fn remote_fs_home(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// This machine's log folder (where `lib.rs` points the tracing file
/// appender), so a remote window's "Open logs" shows the controlled machine's.
#[tauri::command]
pub fn remote_fs_log_dir(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .app_data_dir()
        .map(|p| p.join("logs").to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, is_dir: bool) -> FsEntry {
        FsEntry { name: name.into(), path: format!("/x/{name}"), is_dir, size: 0, modified: None }
    }

    #[test]
    fn sorts_dirs_first_then_name_case_insensitive() {
        let mut v = vec![e("b.txt", false), e("Zeta", true), e("A.txt", false), e("alpha", true), e(".hidden", false)];
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta", ".hidden", "A.txt", "b.txt"]);
    }

    #[test]
    fn read_window_clamps_and_caps() {
        assert_eq!(read_window(10, 0, 100), (0, 10));
        assert_eq!(read_window(10, 4, 3), (4, 3));
        assert_eq!(read_window(10, 20, 5), (10, 0));
        assert_eq!(read_window(u64::MAX, 0, u64::MAX), (0, MAX_READ));
    }

    #[test]
    fn lists_stats_and_reads_a_real_dir() {
        let dir = std::env::temp_dir().join(format!("devdy-fsb-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join(".dot"), b"hello world").unwrap();
        let d = dir.to_string_lossy().into_owned();

        let listing = list_dir(&d, MAX_LIST).unwrap();
        assert!(!listing.truncated);
        let list = listing.entries;
        assert_eq!(list.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["sub", ".dot"]);
        assert!(list[0].is_dir);
        assert_eq!(list[1].size, 11);
        assert!(list[1].modified.is_some());

        let file = dir.join(".dot").to_string_lossy().into_owned();
        assert_eq!(stat(&file).unwrap().name, ".dot");
        let chunk = read_chunk(&file, 6, 100).unwrap();
        assert_eq!(chunk.total_size, 11);
        let bytes = base64::engine::general_purpose::STANDARD.decode(chunk.data_base64).unwrap();
        assert_eq!(bytes, b"world");

        assert!(read_chunk(&d, 0, 1).unwrap_err().starts_with("is a directory"));
        let capped = list_dir(&d, 1).unwrap();
        assert!(capped.truncated);
        assert_eq!(capped.entries.len(), 1);
        assert!(!list_dir(&d, 2).unwrap().truncated);
        assert!(stat(&format!("{d}/missing")).unwrap_err().starts_with("not found"));
        fs::remove_dir_all(dir).unwrap();
    }
}
