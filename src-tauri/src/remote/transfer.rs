//! Remote desktop file transfer: copy files and folders between the
//! controlling Devdy (A, the machine the user sits at) and the controlled one
//! (B), both ways (`src/views/FileTransferWindow.vue`).
//!
//! - The file-system helpers here ([`walk`], [`hash_file`], [`make_dir`],
//!   [`rename_entry`], [`delete_entry`]) are shared by both ends.
//! - B side: `remote_fs_mkdir` / `rename` / `delete` / `walk` / `hash`
//!   (`fs_bridge.rs`) and `remote_upload_*` with a destination folder
//!   (`upload.rs`), reached through the IPC bridge and held to its
//!   [`crate::remote::fs_policy::RemoteFs`].
//! - A side: the `local_fs_*` commands below, called with the REAL local
//!   invoke and refused to the bridge (`guard::LOCAL_ONLY_COMMANDS`): run on B
//!   they would let a controller touch any file there.
//!
//! Names and relative paths of a copy come from the OTHER machine (its
//! listing, its walk), so a compromised B could hand A `../../Library/…`. The
//! A-side writers therefore take the folder the user picked (`root`) plus a
//! `/`-separated relative path, and refuse anything that resolves outside
//! `root` (`..`, absolute parts, links pointing elsewhere): [`contained`].
//!
//! A file is written to a hidden temp file next to its destination, hashed
//! (SHA-256) as the chunks arrive, and renamed into place only once complete
//! and matching the source's hash ([`crate::remote::upload::Uploads`]).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use base64::Engine;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use tauri::Manager;

use crate::remote::guard::FileId;
use crate::remote::upload::{Conflict, UploadBegin, Uploads};

/// Most entries one [`walk`] returns; a bigger tree is reported `truncated`
/// (the transfer refuses it rather than copying part of it).
pub const MAX_WALK: usize = 10_000;
/// Longest file name accepted for a new / renamed entry (bytes, as most file
/// systems count).
const MAX_NAME_BYTES: usize = 255;

/// One entry of a [`walk`], relative to the walked folder.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WalkEntry {
    /// `/`-separated whatever the platform (the other end joins it with its own).
    pub rel: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix permission bits, carried to the copy.
    pub mode: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct Walk {
    /// Folders before their contents, siblings by name.
    pub entries: Vec<WalkEntry>,
    pub total_bytes: u64,
    /// More than [`MAX_WALK`] entries: `entries` is incomplete.
    pub truncated: bool,
    /// Entries left out: links to folders (never followed), dangling links,
    /// entries the caller may not see.
    pub skipped: u32,
}

/// Every entry under the folder `root` (not `root` itself), depth first.
/// Links to files count as files; links to folders are skipped so a cycle
/// cannot loop. `hide` filters entries (and their contents) out.
pub fn walk(root: &Path, max: usize, hide: impl Fn(&Path) -> bool) -> Result<Walk, String> {
    let shown = root.to_string_lossy();
    if !fs::metadata(root).map_err(|e| io_err(&shown, e))?.is_dir() {
        return Err(format!("not a directory: {shown}"));
    }
    let mut out = Walk { entries: Vec::new(), total_bytes: 0, truncated: false, skipped: 0 };
    // (absolute path, relative path); popped in order, so pushed reversed.
    let mut stack: Vec<(PathBuf, String)> = Vec::new();
    let push_children = |stack: &mut Vec<(PathBuf, String)>, dir: &Path, rel: &str| -> Result<(), String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .map_err(|e| io_err(&dir.to_string_lossy(), e))?
            .flatten()
            .map(|e| e.file_name())
            .collect();
        names.sort();
        for name in names.into_iter().rev() {
            let child_rel = if rel.is_empty() {
                name.to_string_lossy().into_owned()
            } else {
                format!("{rel}/{}", name.to_string_lossy())
            };
            stack.push((dir.join(&name), child_rel));
        }
        Ok(())
    };
    push_children(&mut stack, root, "")?;
    while let Some((path, rel)) = stack.pop() {
        if hide(&path) {
            out.skipped += 1;
            continue;
        }
        let Ok(own) = fs::symlink_metadata(&path) else {
            out.skipped += 1;
            continue;
        };
        let meta = if own.file_type().is_symlink() {
            match fs::metadata(&path) {
                Ok(m) if !m.is_dir() => m,
                _ => {
                    out.skipped += 1;
                    continue;
                }
            }
        } else {
            own
        };
        if out.entries.len() >= max {
            out.truncated = true;
            break;
        }
        let size = if meta.is_dir() { 0 } else { meta.len() };
        out.total_bytes += size;
        let mode = crate::remote::fs_bridge::mode_of(&meta);
        out.entries.push(WalkEntry { rel: rel.clone(), is_dir: meta.is_dir(), size, mode });
        if meta.is_dir() {
            // An unreadable sub-folder is copied empty rather than failing the lot.
            if push_children(&mut stack, &path, &rel).is_err() {
                out.skipped += 1;
            }
        }
    }
    Ok(out)
}

/// SHA-256 (lowercase hex) of a file, streamed.
pub fn hash_file(path: &Path) -> Result<String, String> {
    let shown = path.to_string_lossy();
    let mut file = fs::File::open(path).map_err(|e| io_err(&shown, e))?;
    if file.metadata().map_err(|e| io_err(&shown, e))?.is_dir() {
        return Err(format!("is a directory: {shown}"));
    }
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 1024];
    loop {
        let n = file.read(&mut buf).map_err(|e| io_err(&shown, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

/// `name` when it can name a new entry in a folder: one path component, no
/// control characters, not `.` / `..`, at most [`MAX_NAME_BYTES`].
pub fn valid_name(name: &str) -> Result<&str, String> {
    let bad = name.is_empty()
        || name == "."
        || name == ".."
        || name.len() > MAX_NAME_BYTES
        || name.chars().any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || (cfg!(windows) && name.chars().any(|c| matches!(c, ':' | '*' | '?' | '"' | '<' | '>' | '|')));
    if bad {
        return Err(format!("invalid name: {name:?}"));
    }
    Ok(name)
}

/// The folder `root` joined with the `/`-separated `rel` (`""` = `root`
/// itself), when it exists and resolves inside `root`: every part must be a
/// [`valid_name`] (no `..`, no separators), and links along the way must not
/// lead out. Returns the resolved path.
pub fn contained(root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.starts_with('/') {
        return Err(format!("not a relative path: {rel}"));
    }
    let root = root.canonicalize().map_err(|e| io_err(&root.to_string_lossy(), e))?;
    let mut path = root.clone();
    for part in rel.split('/').filter(|p| !p.is_empty()) {
        path.push(valid_name(part)?);
    }
    let resolved = path.canonicalize().map_err(|e| io_err(&path.to_string_lossy(), e))?;
    if !resolved.starts_with(&root) {
        return Err(format!("outside the destination folder: {rel}"));
    }
    Ok(resolved)
}

/// Create the folder `rel` under `root` (see [`contained`]); its parent must
/// exist. Returns the folder's path.
pub fn make_dir_in(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let (parent, name) = match rel.trim_end_matches('/').rsplit_once('/') {
        Some((parent, name)) => (parent, name),
        None => ("", rel.trim_end_matches('/')),
    };
    if rel.starts_with('/') {
        return Err(format!("not a relative path: {rel}"));
    }
    let dir = contained(root, parent)?.join(valid_name(name)?);
    make_dir(&dir)?;
    // An existing link by that name must not lead out either.
    contained(root, rel)
}

/// Create the folder `path` (its parent must exist); an existing folder is fine.
pub fn make_dir(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        Err(e) => Err(io_err(&path.to_string_lossy(), e)),
    }
}

/// Rename the entry `path` (a link itself, not its target) to `new_name` in
/// the same folder; refuses to replace an existing entry. Returns the new path.
pub fn rename_entry(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = valid_name(new_name)?;
    let shown = path.to_string_lossy();
    fs::symlink_metadata(path).map_err(|e| io_err(&shown, e))?;
    let parent = path.parent().ok_or_else(|| format!("cannot rename {shown}"))?;
    let dest = parent.join(new_name);
    if fs::symlink_metadata(&dest).is_ok() {
        // A case-only rename on a case-insensitive volume finds itself.
        let same = FileId::of(&dest).is_some() && FileId::of(&dest) == FileId::of(path);
        if !same {
            return Err(format!("already exists: {}", dest.to_string_lossy()));
        }
    }
    fs::rename(path, &dest).map_err(|e| io_err(&shown, e))?;
    Ok(dest)
}

/// Remove the entry `path`: a file, a link (not its target) or a folder with
/// everything in it.
pub fn delete_entry(path: &Path) -> Result<(), String> {
    let shown = path.to_string_lossy();
    let meta = fs::symlink_metadata(path).map_err(|e| io_err(&shown, e))?;
    let res = if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    res.map_err(|e| io_err(&shown, e))
}

fn io_err(path: &str, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("not found: {path}"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path}"),
        _ => format!("{path}: {e}"),
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())?
}

// ---- Tauri commands, CONTROLLING side (local only: called with the REAL invoke) ----

/// Files being written on this machine (downloads from the controlled one).
static LOCAL_WRITES: LazyLock<Mutex<Uploads>> = LazyLock::new(|| Mutex::new(Uploads::default()));

/// For the sweeper ([`crate::remote::upload::spawn_sweeper`]).
pub(crate) fn local_writes_map() -> &'static Mutex<Uploads> {
    &LOCAL_WRITES
}

fn local_writes() -> std::sync::MutexGuard<'static, Uploads> {
    LOCAL_WRITES.lock().unwrap_or_else(|e| e.into_inner())
}

/// A window closed: drop the unfinished writes it started (and their temp files).
pub fn on_webview_destroyed(label: &str) {
    local_writes().abort_owned_by(label);
}

#[tauri::command]
pub fn local_fs_home(app: tauri::AppHandle) -> Result<String, String> {
    app.path()
        .home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn local_fs_list(path: String) -> Result<crate::remote::fs_bridge::FsListing, String> {
    blocking(move || crate::remote::fs_bridge::list_dir(&path, crate::remote::fs_bridge::MAX_LIST)).await
}

/// Create the folder `rel` (`/`-separated) inside the folder `root`, never
/// outside it (see [`contained`]).
#[tauri::command]
pub async fn local_fs_mkdir(root: String, rel: String) -> Result<String, String> {
    blocking(move || make_dir_in(Path::new(&root), &rel).map(|p| p.to_string_lossy().into_owned())).await
}

#[tauri::command]
pub async fn local_fs_rename(path: String, new_name: String) -> Result<String, String> {
    blocking(move || rename_entry(Path::new(&path), &new_name).map(|p| p.to_string_lossy().into_owned())).await
}

#[tauri::command]
pub async fn local_fs_delete(path: String) -> Result<(), String> {
    blocking(move || delete_entry(Path::new(&path))).await
}

#[tauri::command]
pub async fn local_fs_walk(path: String) -> Result<Walk, String> {
    blocking(move || walk(Path::new(&path), MAX_WALK, |_| false)).await
}

#[tauri::command]
pub async fn local_fs_hash(path: String) -> Result<String, String> {
    blocking(move || hash_file(Path::new(&path))).await
}

/// Start writing a `size`-byte file named `name` into the folder `rel`
/// (`/`-separated) inside `root`, the destination the user picked — never
/// outside it (see [`contained`]). `conflict` (`overwrite` / `rename` /
/// `fail`) decides what happens when the name is taken, `mode` is the
/// source's permission bits. See [`Uploads::begin_to`]. The write ends with
/// the calling window ([`on_webview_destroyed`]).
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn local_fs_write_begin(
    webview: tauri::Webview,
    root: String,
    rel: String,
    name: String,
    size: u64,
    conflict: Option<String>,
    mode: Option<u32>,
) -> Result<UploadBegin, String> {
    let conflict = Conflict::parse(conflict.as_deref())?;
    let label = webview.label().to_string();
    blocking(move || {
        let dir = contained(Path::new(&root), &rel)?;
        let mut writes = local_writes();
        let begun = writes.begin_to(&dir, &name, size, conflict, mode)?;
        writes.set_owner(&begun.upload_id, &label);
        Ok(begun)
    })
    .await
}

#[tauri::command]
pub async fn local_fs_write_chunk(upload_id: String, offset: u64, data_base64: String) -> Result<u64, String> {
    blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|e| format!("bad chunk: {e}"))?;
        local_writes().chunk(&upload_id, offset, &bytes)
    })
    .await
}

/// Complete a write (refused when its SHA-256 is not `sha256`); returns the
/// file's final path.
#[tauri::command]
pub async fn local_fs_write_finish(upload_id: String, sha256: Option<String>) -> Result<String, String> {
    blocking(move || local_writes().finish(&upload_id, sha256.as_deref())).await
}

#[tauri::command]
pub async fn local_fs_write_abort(upload_id: String) -> Result<(), String> {
    blocking(move || {
        local_writes().abort(&upload_id);
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-transfer-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn walks_folders_before_their_contents() {
        let root = tmp();
        fs::create_dir_all(root.join("b/inner")).unwrap();
        fs::write(root.join("a.txt"), b"12345").unwrap();
        fs::write(root.join("b/inner/c.bin"), b"xyz").unwrap();
        fs::create_dir(root.join("empty")).unwrap();
        let w = walk(&root, MAX_WALK, |_| false).unwrap();
        let rels: Vec<_> = w.entries.iter().map(|e| (e.rel.as_str(), e.is_dir, e.size)).collect();
        assert_eq!(
            rels,
            [("a.txt", false, 5), ("b", true, 0), ("b/inner", true, 0), ("b/inner/c.bin", false, 3), ("empty", true, 0)]
        );
        assert_eq!((w.total_bytes, w.truncated, w.skipped), (8, false, 0));

        let capped = walk(&root, 2, |_| false).unwrap();
        assert!(capped.truncated);
        assert_eq!(capped.entries.len(), 2);

        let hidden = walk(&root, MAX_WALK, |p| p.ends_with("b")).unwrap();
        assert_eq!(hidden.entries.len(), 2, "b and its contents are left out");
        assert_eq!(hidden.skipped, 1);

        assert!(walk(&root.join("a.txt"), MAX_WALK, |_| false).unwrap_err().starts_with("not a directory"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn walk_follows_file_links_but_not_folder_links() {
        let root = tmp();
        fs::create_dir(root.join("d")).unwrap();
        fs::write(root.join("d/f"), b"hi").unwrap();
        std::os::unix::fs::symlink(root.join("d/f"), root.join("d/link-to-file")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("d/loop")).unwrap();
        std::os::unix::fs::symlink(root.join("gone"), root.join("d/dangling")).unwrap();
        let w = walk(&root.join("d"), MAX_WALK, |_| false).unwrap();
        let rels: Vec<_> = w.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, ["f", "link-to-file"]);
        assert_eq!(w.skipped, 2);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn writes_stay_inside_the_destination_folder() {
        let base = tmp().canonicalize().unwrap();
        let root = base.join("dest");
        fs::create_dir_all(root.join("a")).unwrap();
        fs::create_dir(base.join("outside")).unwrap();
        assert_eq!(contained(&root, "").unwrap(), root);
        assert_eq!(contained(&root, "a/").unwrap(), root.join("a"));
        for bad in ["..", "a/../..", "../outside", "a/./b", "a\\..\\..", "/etc"] {
            assert!(contained(&root, bad).is_err(), "{bad}");
            assert!(make_dir_in(&root, bad).is_err(), "{bad}");
        }
        assert_eq!(make_dir_in(&root, "a/b").unwrap(), root.join("a/b"));
        assert!(root.join("a/b").is_dir());
        assert!(make_dir_in(&root, "x/y").is_err(), "parent must exist");
        assert!(!base.join("outside/..").join("x").exists());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(base.join("outside"), root.join("link")).unwrap();
            assert!(contained(&root, "link").unwrap_err().starts_with("outside the destination"));
            assert!(make_dir_in(&root, "link/sub").is_err());
            assert!(!base.join("outside/sub").exists());
            assert!(make_dir_in(&root, "link").is_err(), "an existing link out is refused");
        }
        fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn hashes_files() {
        let root = tmp();
        let f = root.join("f");
        fs::write(&f, b"abc").unwrap();
        assert_eq!(hash_file(&f).unwrap(), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(hash_file(&root).unwrap_err().starts_with("is a directory"));
        assert!(hash_file(&root.join("missing")).unwrap_err().starts_with("not found"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn validates_names() {
        assert!(valid_name("a.txt").is_ok());
        assert!(valid_name(".gitignore").is_ok());
        for bad in ["", ".", "..", "a/b", "a\\b", "a\u{0}b", &"x".repeat(256)] {
            assert!(valid_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn makes_renames_and_deletes() {
        let root = tmp();
        let d = root.join("new");
        make_dir(&d).unwrap();
        make_dir(&d).unwrap();
        assert!(make_dir(&root.join("no/parent")).is_err());
        fs::write(d.join("f"), b"1").unwrap();
        fs::write(root.join("taken"), b"2").unwrap();
        assert!(make_dir(&root.join("taken")).is_err(), "a file is not a folder");

        assert!(rename_entry(&d, "taken").unwrap_err().starts_with("already exists"));
        assert!(rename_entry(&d, "../escape").is_err());
        let moved = rename_entry(&d, "renamed").unwrap();
        assert_eq!(moved, root.join("renamed"));
        assert!(moved.join("f").exists() && !d.exists());

        delete_entry(&moved).unwrap();
        assert!(!moved.exists());
        delete_entry(&root.join("taken")).unwrap();
        assert!(delete_entry(&root.join("taken")).unwrap_err().starts_with("not found"));
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn deleting_a_link_keeps_its_target() {
        let root = tmp();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/keep"), b"1").unwrap();
        std::os::unix::fs::symlink(root.join("target"), root.join("link")).unwrap();
        delete_entry(&root.join("link")).unwrap();
        assert!(root.join("target/keep").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controlling_side_commands_are_local_only() {
        use crate::remote::guard::is_local_only_command;
        for name in [
            "local_fs_home",
            "local_fs_list",
            "local_fs_mkdir",
            "local_fs_rename",
            "local_fs_delete",
            "local_fs_walk",
            "local_fs_hash",
            "local_fs_write_begin",
            "local_fs_write_chunk",
            "local_fs_write_finish",
            "local_fs_write_abort",
        ] {
            assert!(is_local_only_command(name), "{name}");
        }
        // The controlled side's halves are reached through the bridge.
        for name in ["remote_fs_mkdir", "remote_fs_rename", "remote_fs_delete", "remote_fs_walk", "remote_fs_hash"] {
            assert!(!is_local_only_command(name), "{name}");
        }
    }
}
