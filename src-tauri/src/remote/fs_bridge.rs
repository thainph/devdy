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
//! Listing shows names anywhere except inside credential stores (Devdy's app
//! data, `~/.ssh`, cloud CLI logins, …, [`crate::remote::guard::SecretPaths`]).
//! Reading is held to the remote desktop's allowlist (HOME, mounted volumes,
//! projects, uploads, [`crate::remote::fs_policy::RemoteFs`]): never a
//! credential store, nor system files outside it. The File Transfer window's
//! commands (`remote_fs_mkdir` / `rename` / `delete` / `walk` / `hash`, see
//! `transfer.rs`) are held to the same [`RemoteFs`]: reads to its allowlist,
//! changes to its writable roots.

use std::fs::{self, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use base64::Engine;
use serde::Serialize;
use tauri::Manager;

use crate::remote::fs_policy::{self, RemoteFs};
use crate::remote::guard::SecretPaths;
use crate::remote::transfer;

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
    /// Unix permission bits (`None` elsewhere), carried to a transferred copy.
    pub mode: Option<u32>,
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

/// Permission bits of `meta` (`rwx` for user / group / other plus
/// setuid / setgid / sticky); `None` off Unix.
pub fn mode_of(meta: &Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o7777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
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
        mode: mode_of(meta),
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
                mode: None,
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
/// Browsing (the remote file dialog picks folders to add as projects, files
/// to attach) sees names anywhere but inside a credential store.
#[tauri::command]
pub async fn remote_fs_list(app: tauri::AppHandle, path: String) -> Result<FsListing, String> {
    let secrets = crate::remote::guard::SecretPaths::for_app(&app);
    blocking(move || {
        let canonical = secrets.check(&path)?;
        list_dir(&canonical.to_string_lossy(), MAX_LIST)
    })
    .await
}

#[tauri::command]
pub async fn remote_fs_stat(app: tauri::AppHandle, path: String) -> Result<FsEntry, String> {
    let secrets = crate::remote::guard::SecretPaths::for_app(&app);
    blocking(move || {
        secrets.check(&path)?;
        stat(&path)
    })
    .await
}

/// Up to `len` bytes (capped at 4 MiB) of a file from `offset`. From the
/// remote-desktop bridge: only inside its allowlist
/// ([`crate::remote::fs_policy::RemoteFs`]); never a credential store.
#[tauri::command]
pub async fn remote_fs_read(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    db: tauri::State<'_, crate::db::Db>,
    path: String,
    offset: u64,
    len: u64,
) -> Result<FsChunk, String> {
    let policy = crate::remote::fs_policy::RemoteFs::for_caller(&webview, db.inner()).await?;
    let secrets = crate::remote::guard::SecretPaths::for_app(&app);
    blocking(move || {
        let canonical = match policy {
            Some(p) => p.read(Path::new(&path))?,
            None => secrets.check(&path)?,
        };
        read_chunk(&canonical.to_string_lossy(), offset, len)
    })
    .await
}

// ---- File Transfer window (see `transfer.rs`) ----

/// An existing path the caller may read: the bridge's allowlist, else only
/// never a credential store.
fn readable(policy: Option<&RemoteFs>, secrets: &SecretPaths, path: &str) -> Result<PathBuf, String> {
    match policy {
        Some(p) => p.read(Path::new(path)),
        None => secrets.check(path),
    }
}

/// The entry `raw` itself (a link is not followed), when the caller may
/// change or remove it: its folder must be writable, and the entry movable
/// ([`RemoteFs::change`]: not a root, nor a folder holding credentials).
fn changeable(policy: Option<&RemoteFs>, raw: &Path) -> Result<PathBuf, String> {
    let name = raw.file_name().ok_or_else(|| format!("cannot change {}", raw.display()))?;
    let parent = raw.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let own = fs_policy::write(policy, parent)?.join(name);
    let meta = fs::symlink_metadata(&own).map_err(|e| io_err(&raw.to_string_lossy(), e))?;
    // A link is changed in its (checked) folder; its target is not touched.
    if !meta.file_type().is_symlink() {
        fs_policy::change(policy, &own)?;
    }
    Ok(own)
}

async fn policy_for(webview: &tauri::Webview, db: &crate::db::Db) -> Result<Option<RemoteFs>, String> {
    RemoteFs::for_caller(webview, db).await
}

/// Create the folder `path` (its parent must exist); returns its path.
#[tauri::command]
pub async fn remote_fs_mkdir(webview: tauri::Webview, db: tauri::State<'_, crate::db::Db>, path: String) -> Result<String, String> {
    let policy = policy_for(&webview, db.inner()).await?;
    blocking(move || {
        let target = fs_policy::write(policy.as_ref(), Path::new(&path))?;
        transfer::make_dir(&target)?;
        Ok(target.to_string_lossy().into_owned())
    })
    .await
}

/// Rename a file or folder in place (never replacing another); returns the new path.
#[tauri::command]
pub async fn remote_fs_rename(
    webview: tauri::Webview,
    db: tauri::State<'_, crate::db::Db>,
    path: String,
    new_name: String,
) -> Result<String, String> {
    let policy = policy_for(&webview, db.inner()).await?;
    blocking(move || {
        let own = changeable(policy.as_ref(), Path::new(&path))?;
        let new_name = transfer::valid_name(&new_name)?;
        let parent = own.parent().unwrap_or(Path::new("."));
        fs_policy::write(policy.as_ref(), &parent.join(new_name))?;
        transfer::rename_entry(&own, new_name).map(|p| p.to_string_lossy().into_owned())
    })
    .await
}

/// Delete a file, a link or a folder with everything in it.
#[tauri::command]
pub async fn remote_fs_delete(webview: tauri::Webview, db: tauri::State<'_, crate::db::Db>, path: String) -> Result<(), String> {
    let policy = policy_for(&webview, db.inner()).await?;
    blocking(move || {
        let own = changeable(policy.as_ref(), Path::new(&path))?;
        transfer::delete_entry(&own)
    })
    .await
}

/// Everything under a folder (relative paths and sizes), to copy it; at most
/// [`transfer::MAX_WALK`] entries. Credential stores inside are left out.
#[tauri::command]
pub async fn remote_fs_walk(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    db: tauri::State<'_, crate::db::Db>,
    path: String,
) -> Result<transfer::Walk, String> {
    let policy = policy_for(&webview, db.inner()).await?;
    let secrets = SecretPaths::for_app(&app);
    blocking(move || {
        let root = readable(policy.as_ref(), &secrets, &path)?;
        transfer::walk(&root, transfer::MAX_WALK, |p| secrets.denies(p))
    })
    .await
}

/// SHA-256 (hex) of a file, to check a copy of it.
#[tauri::command]
pub async fn remote_fs_hash(
    app: tauri::AppHandle,
    webview: tauri::Webview,
    db: tauri::State<'_, crate::db::Db>,
    path: String,
) -> Result<String, String> {
    let policy = policy_for(&webview, db.inner()).await?;
    let secrets = SecretPaths::for_app(&app);
    blocking(move || transfer::hash_file(&readable(policy.as_ref(), &secrets, &path)?)).await
}

#[tauri::command]
pub fn remote_fs_home(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

/// Diagnostics from a remote-desktop window on the CONTROLLING machine: its
/// console errors and remote-IPC traces land in this machine's log, the one
/// place both ends can read after the fact. Truncated; never interpreted.
#[tauri::command]
pub fn remote_client_log(level: String, source: String, message: String) {
    const MAX: usize = 2000;
    let mut message = message;
    if message.len() > MAX {
        let mut cut = MAX;
        while !message.is_char_boundary(cut) {
            cut -= 1;
        }
        message.truncate(cut);
        message.push('…');
    }
    let source: String = source.chars().take(80).collect();
    match level.as_str() {
        "error" => tracing::error!(target: "devdy::remote_client", source = %source, "{message}"),
        "warn" => tracing::warn!(target: "devdy::remote_client", source = %source, "{message}"),
        _ => tracing::info!(target: "devdy::remote_client", source = %source, "{message}"),
    }
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
        FsEntry { name: name.into(), path: format!("/x/{name}"), is_dir, size: 0, modified: None, mode: None }
    }

    #[test]
    fn sorts_dirs_first_then_name_case_insensitive() {
        let mut v = vec![e("b.txt", false), e("Zeta", true), e("A.txt", false), e("alpha", true), e(".hidden", false)];
        sort_entries(&mut v);
        let names: Vec<_> = v.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["alpha", "Zeta", ".hidden", "A.txt", "b.txt"]);
    }

    #[cfg(unix)]
    #[test]
    fn transfer_changes_stay_inside_the_writable_roots() {
        use crate::remote::fs_policy::Roots;
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("devdy-fsb-{}", uuid::Uuid::new_v4()));
        let (home, proj) = (base.join("home"), base.join("home/proj"));
        fs::create_dir_all(proj.join(".git")).unwrap();
        fs::create_dir_all(home.join("other")).unwrap();
        fs::write(proj.join("a.txt"), b"x").unwrap();
        fs::write(home.join("other/keep"), b"x").unwrap();
        std::os::unix::fs::symlink(home.join("other"), proj.join("link")).unwrap();
        let roots = Roots { writable: vec![proj.clone()], ..Roots::default() };
        let policy = RemoteFs::new(roots, Some(&home), SecretPaths::new(Some(&home), None));
        let p = Some(&policy);

        assert_eq!(changeable(p, &proj.join("a.txt")).unwrap(), proj.join("a.txt"));
        assert!(changeable(p, &home.join("other/keep")).unwrap_err().starts_with("permission denied"));
        // Full control: tool config inside a writable root is the user's to change.
        assert!(changeable(p, &proj.join(".git")).is_ok());
        assert!(changeable(p, &proj.join("missing")).unwrap_err().starts_with("not found"));
        // A link inside the project is changed itself, never its target outside.
        let link = changeable(p, &proj.join("link")).unwrap();
        transfer::delete_entry(&link).unwrap();
        assert!(home.join("other/keep").exists());
        assert!(readable(p, &SecretPaths::new(Some(&home), None), &home.join("other/keep").to_string_lossy()).is_err());
        // A local caller is not restricted.
        assert!(changeable(None, &home.join("other/keep")).is_ok());
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn renaming_or_deleting_a_folder_holding_credentials_is_refused() {
        use crate::remote::fs_policy::Roots;
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("devdy-fsb-{}", uuid::Uuid::new_v4()));
        let home = base.join("home");
        let app = home.join("Library/Application Support/vn.papay.devdy");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::create_dir_all(app.join("server-keys")).unwrap();
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::write(home.join(".claude/.credentials.json"), b"x").unwrap();
        let roots = Roots { writable: vec![home.clone()], ..Roots::default() };
        let policy = RemoteFs::new(roots, Some(&home), SecretPaths::new(Some(&home), Some(&app)));
        let p = Some(&policy);
        for dir in [home.join(".claude"), home.join("Library"), home.join("Library/Application Support"), app.clone()] {
            assert!(changeable(p, &dir).unwrap_err().starts_with("permission denied"), "{}", dir.display());
        }
        assert!(changeable(p, &home.join("Documents")).is_ok());
        fs::remove_dir_all(base).unwrap();
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
