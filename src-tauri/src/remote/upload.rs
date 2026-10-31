//! Remote desktop uploads: files the controlling Devdy (A) drops or pastes into
//! a remote-desktop window are copied onto this machine (B) before the UI uses
//! them, because every path the UI hands to a command must be a path on B.
//!
//! Two halves, both plain Tauri commands:
//!   - B side (reached through the IPC bridge like any other command):
//!     `remote_upload_begin` → `remote_upload_chunk`… → `remote_upload_finish`
//!     (or `remote_upload_abort`). Each upload lands in its own directory
//!     `<app cache>/remote-uploads/<upload_id>/<sanitized name>`, so the original
//!     file name survives and two drops of `a.png` never collide. Directories
//!     older than [`KEEP_UPLOADS`] are pruned on the next upload.
//!   - A side (called with the REAL local invoke, see `src/lib/remoteUpload.ts`):
//!     `stat_local_files` / `read_local_file_chunk` read the dropped files.
//!
//! Bytes travel as base64 in JSON, at most [`MAX_CHUNK_BYTES`] per call, and are
//! written strictly in order (each chunk's `offset` must equal what was written).
//!
//! The File Transfer window (`transfer.rs`) also uploads into a chosen folder
//! (`dest_dir`): the bytes go to a hidden temp file there, are hashed as they
//! arrive, and the file is renamed into place by `remote_upload_finish` once
//! complete (and matching the source's SHA-256 when given). The same
//! [`Uploads`] backs the controlling side's `local_fs_write_*`.
//!
//! Partial files never outlive their copy for long: [`spawn_sweeper`] drops
//! writes left idle for [`STALE_UPLOAD`] (their temp file with them) every
//! [`SWEEP_EVERY`], and removes our own temp files (`.devdy-part-<id>`, nothing
//! else) older than [`PART_MAX_AGE`] from every folder a copy wrote into. Those
//! folders are remembered in the app cache, so leftovers of a crash or quit are
//! found after a restart too.

use base64::Engine;
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{AppHandle, Manager};

/// Largest single upload into the upload dir.
pub const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;
/// Largest single file copied into a chosen folder.
pub const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024 * 1024;
/// Largest decoded chunk per `remote_upload_chunk` / `read_local_file_chunk`.
pub const MAX_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// Uploads that may be in progress at once.
pub const MAX_OPEN_UPLOADS: usize = 32;
/// An unfinished upload untouched for this long is dropped (and its file removed).
const STALE_UPLOAD: Duration = Duration::from_secs(10 * 60);
/// Finished uploads are kept this long: the agent reads them after the prompt.
const KEEP_UPLOADS: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_NAME_CHARS: usize = 120;
const UPLOADS_DIR: &str = "remote-uploads";
/// Prefix of the temp file a copy into a chosen folder is written to; the
/// rest of its name is the upload id (32 hex digits).
const PART_PREFIX: &str = ".devdy-part-";
/// How often [`spawn_sweeper`] runs.
pub const SWEEP_EVERY: Duration = Duration::from_secs(3 * 60);
/// A temp file of ours untouched this long is a leftover (an active write
/// appends at least every [`STALE_UPLOAD`]).
pub const PART_MAX_AGE: Duration = Duration::from_secs(60 * 60);
/// Folders that may hold temp files of ours, one per line (app cache).
const PART_DIRS_FILE: &str = "transfer-part-dirs.txt";

/// What to do when a copy's name is already taken in its folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Conflict {
    /// Replace the existing file (never a folder).
    Overwrite,
    /// Keep both: `name (1).ext`, `name (2).ext`…
    Rename,
    /// Refuse.
    Fail,
}

impl Conflict {
    pub fn parse(s: Option<&str>) -> Result<Self, String> {
        match s.unwrap_or("fail") {
            "overwrite" => Ok(Conflict::Overwrite),
            "rename" => Ok(Conflict::Rename),
            "fail" => Ok(Conflict::Fail),
            other => Err(format!("unknown conflict mode: {other}")),
        }
    }
}

enum Target {
    /// `<uploads root>/<id>/<name>`: `path` is the file itself, `dir` is removed on abort.
    Cache { dir: PathBuf },
    /// `<dir>/<name>`: `path` is a temp file in `dir`, renamed on finish.
    /// `mode`: the source's permission bits (Unix), carried to the copy.
    Dest { dir: PathBuf, name: String, conflict: Conflict, mode: Option<u32> },
}

struct Upload {
    target: Target,
    path: PathBuf,
    size: u64,
    written: u64,
    hasher: Sha256,
    touched: Instant,
    /// Webview that started it, when it should end with that webview.
    owner: Option<String>,
}

/// Uploads in progress, keyed by upload id.
#[derive(Default)]
pub struct Uploads {
    map: HashMap<String, Upload>,
    /// Folders temp files were created in since the last sweep.
    part_dirs: BTreeSet<PathBuf>,
}

static UPLOADS: LazyLock<Mutex<Uploads>> = LazyLock::new(|| Mutex::new(Uploads::default()));

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct UploadBegin {
    pub upload_id: String,
    pub path: String,
}

/// A file name that is safe to create inside the upload directory: the last
/// path component only, no control or separator characters, no leading dots,
/// bounded length (the extension is kept when trimming).
pub fn sanitize_name(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_control() || matches!(c, ':' | '/' | '\\') { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim();
    if cleaned.is_empty() {
        return "upload".to_string();
    }
    if cleaned.chars().count() <= MAX_NAME_CHARS {
        return cleaned.to_string();
    }
    let (stem, ext) = match cleaned.rfind('.') {
        Some(i) if i > 0 && cleaned.len() - i <= 16 => (&cleaned[..i], &cleaned[i..]),
        _ => (cleaned, ""),
    };
    let keep = MAX_NAME_CHARS - ext.chars().count();
    format!("{}{}", stem.chars().take(keep).collect::<String>(), ext)
}

impl Uploads {
    /// Start an upload of `size` bytes named `name` under `root`.
    pub fn begin(&mut self, root: &Path, name: &str, size: u64) -> Result<UploadBegin, String> {
        if size > MAX_UPLOAD_BYTES {
            return Err(format!("File too large (max {} MiB)", MAX_UPLOAD_BYTES / 1024 / 1024));
        }
        self.drop_stale(STALE_UPLOAD);
        if self.map.len() >= MAX_OPEN_UPLOADS {
            return Err("Too many uploads in progress".to_string());
        }
        prune_old(root, KEEP_UPLOADS);
        let id = uuid::Uuid::new_v4().simple().to_string();
        let dir = root.join(&id);
        fs::create_dir_all(&dir).map_err(|e| format!("create upload dir: {e}"))?;
        let path = dir.join(sanitize_name(name));
        if let Err(e) = OpenOptions::new().write(true).create_new(true).open(&path) {
            let _ = fs::remove_dir_all(&dir);
            return Err(format!("create upload file: {e}"));
        }
        let shown = path.to_string_lossy().into_owned();
        self.map.insert(
            id.clone(),
            Upload {
                target: Target::Cache { dir },
                path,
                size,
                written: 0,
                hasher: Sha256::new(),
                touched: Instant::now(),
                owner: None,
            },
        );
        Ok(UploadBegin { upload_id: id, path: shown })
    }

    /// Start a copy of `size` bytes named `name` into the existing folder
    /// `dir`. The returned path is where it lands unless `conflict` is
    /// [`Conflict::Rename`] and the name is taken by then. `mode` (Unix
    /// permission bits of the source) is applied to the copy; without it an
    /// overwritten file keeps its own.
    pub fn begin_to(
        &mut self,
        dir: &Path,
        name: &str,
        size: u64,
        conflict: Conflict,
        mode: Option<u32>,
    ) -> Result<UploadBegin, String> {
        let name = crate::remote::transfer::valid_name(name)?;
        if size > MAX_TRANSFER_BYTES {
            return Err(format!("File too large (max {} GiB)", MAX_TRANSFER_BYTES / 1024 / 1024 / 1024));
        }
        if !dir.is_dir() {
            return Err(format!("not a directory: {}", dir.display()));
        }
        let dest = dir.join(name);
        if let Ok(meta) = fs::symlink_metadata(&dest) {
            if conflict == Conflict::Fail {
                return Err(format!("already exists: {}", dest.display()));
            }
            if conflict == Conflict::Overwrite && meta.is_dir() {
                return Err(format!("is a directory: {}", dest.display()));
            }
        }
        self.drop_stale(STALE_UPLOAD);
        if self.map.len() >= MAX_OPEN_UPLOADS {
            return Err("Too many uploads in progress".to_string());
        }
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = dir.join(format!("{PART_PREFIX}{id}"));
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|e| format!("create temp file in {}: {e}", dir.display()))?;
        self.part_dirs.insert(dir.to_path_buf());
        self.map.insert(
            id.clone(),
            Upload {
                target: Target::Dest { dir: dir.to_path_buf(), name: name.to_string(), conflict, mode },
                path,
                size,
                written: 0,
                hasher: Sha256::new(),
                touched: Instant::now(),
                owner: None,
            },
        );
        Ok(UploadBegin { upload_id: id, path: dest.to_string_lossy().into_owned() })
    }

    /// Append `bytes` at `offset` (must equal the bytes written so far). A failed
    /// write aborts the upload.
    pub fn chunk(&mut self, id: &str, offset: u64, bytes: &[u8]) -> Result<u64, String> {
        let up = self.map.get_mut(id).ok_or_else(|| "Unknown upload".to_string())?;
        if bytes.len() > MAX_CHUNK_BYTES {
            return Err("Chunk too large".to_string());
        }
        if offset != up.written {
            return Err(format!("Unexpected offset {offset} (expected {})", up.written));
        }
        if up.written + bytes.len() as u64 > up.size {
            return Err("Upload exceeds its declared size".to_string());
        }
        up.hasher.update(bytes);
        let res = OpenOptions::new()
            .append(true)
            .open(&up.path)
            .and_then(|mut f| f.write_all(bytes));
        match res {
            Ok(()) => {
                up.written += bytes.len() as u64;
                up.touched = Instant::now();
                Ok(up.written)
            }
            Err(e) => {
                self.abort(id);
                Err(format!("write upload: {e}"))
            }
        }
    }

    /// Complete an upload; returns its path on this machine. With `sha256`
    /// (hex), the bytes received must hash to it or the upload is dropped.
    pub fn finish(&mut self, id: &str, sha256: Option<&str>) -> Result<String, String> {
        let up = self.map.get(id).ok_or_else(|| "Unknown upload".to_string())?;
        if up.written != up.size {
            return Err(format!("Upload incomplete ({} of {} bytes)", up.written, up.size));
        }
        let up = self.map.remove(id).expect("checked above");
        let digest = hex::encode(up.hasher.clone().finalize());
        if sha256.is_some_and(|want| !want.eq_ignore_ascii_case(&digest)) {
            discard(&up);
            return Err("integrity check failed: SHA-256 mismatch".to_string());
        }
        match &up.target {
            Target::Cache { .. } => Ok(up.path.to_string_lossy().into_owned()),
            Target::Dest { dir, name, conflict, mode } => match place(&up.path, dir, name, *conflict, *mode) {
                Ok(dest) => Ok(dest.to_string_lossy().into_owned()),
                Err(e) => {
                    discard(&up);
                    Err(e)
                }
            },
        }
    }

    /// Drop an upload and its partial file (no-op when unknown).
    pub fn abort(&mut self, id: &str) {
        if let Some(up) = self.map.remove(id) {
            discard(&up);
        }
    }

    /// Tie an upload to the webview that started it (see [`Self::abort_owned_by`]).
    pub fn set_owner(&mut self, id: &str, label: &str) {
        if let Some(up) = self.map.get_mut(id) {
            up.owner = Some(label.to_string());
        }
    }

    /// Drop every upload started by webview `label` (its window is gone).
    pub fn abort_owned_by(&mut self, label: &str) {
        let ids: Vec<String> = self
            .map
            .iter()
            .filter(|(_, u)| u.owner.as_deref() == Some(label))
            .map(|(id, _)| id.clone())
            .collect();
        for id in ids {
            self.abort(&id);
        }
    }

    /// Drop every upload (the app is quitting).
    pub fn abort_all(&mut self) {
        let ids: Vec<String> = self.map.keys().cloned().collect();
        for id in ids {
            self.abort(&id);
        }
    }

    /// Temp files of the writes in progress.
    fn active_parts(&self) -> HashSet<PathBuf> {
        self.map
            .values()
            .filter(|u| matches!(u.target, Target::Dest { .. }))
            .map(|u| u.path.clone())
            .collect()
    }

    fn drop_stale(&mut self, max_idle: Duration) {
        let stale: Vec<String> = self
            .map
            .iter()
            .filter(|(_, u)| u.touched.elapsed() >= max_idle)
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.abort(&id);
        }
    }
}

fn discard(up: &Upload) {
    let _ = match &up.target {
        Target::Cache { dir } => fs::remove_dir_all(dir),
        Target::Dest { .. } => fs::remove_file(&up.path),
    };
}

/// Permission bits for a copy: the source's `mode` without group / other
/// write (scripts keep their `+x`, private files stay private); with no source
/// mode, those of the file it replaces. Hard links are not preserved.
#[cfg(unix)]
fn carry_mode(part: &Path, replaced: &Path, mode: Option<u32>) {
    use std::os::unix::fs::PermissionsExt;
    let bits = match mode {
        Some(m) => Some(m & 0o755),
        None => fs::metadata(replaced).ok().filter(|m| m.is_file()).map(|m| m.permissions().mode() & 0o7777),
    };
    if let Some(bits) = bits {
        let _ = fs::set_permissions(part, fs::Permissions::from_mode(bits));
    }
}

#[cfg(not(unix))]
fn carry_mode(_part: &Path, _replaced: &Path, _mode: Option<u32>) {}

/// Move the finished temp file `part` to `dir/name` as `conflict` says.
fn place(part: &Path, dir: &Path, name: &str, conflict: Conflict, mode: Option<u32>) -> Result<PathBuf, String> {
    let taken = |p: &Path| fs::symlink_metadata(p).is_ok();
    let dest = match conflict {
        Conflict::Overwrite => {
            let dest = dir.join(name);
            if dest.is_dir() {
                return Err(format!("is a directory: {}", dest.display()));
            }
            dest
        }
        Conflict::Fail => {
            let dest = dir.join(name);
            if taken(&dest) {
                return Err(format!("already exists: {}", dest.display()));
            }
            dest
        }
        Conflict::Rename => (0..10_000)
            .map(|n| dir.join(crate::remote::file_proxy::numbered_name(name, n)))
            .find(|p| !taken(p))
            .ok_or_else(|| format!("no free name for {name} in {}", dir.display()))?,
    };
    carry_mode(part, &dest, mode);
    // Same folder, so a rename: the file appears whole or not at all.
    fs::rename(part, &dest).map_err(|e| format!("move into place {}: {e}", dest.display()))?;
    Ok(dest)
}

/// Whether `name` is one of our temp files: [`PART_PREFIX`] + an upload id.
pub fn is_part_name(name: &str) -> bool {
    name.strip_prefix(PART_PREFIX)
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Remove our temp files in `dir` untouched for `max_age` (never one in
/// `active`, never anything else). Returns whether temp files of ours remain.
pub fn prune_parts(dir: &Path, max_age: Duration, active: &HashSet<PathBuf>) -> bool {
    let Ok(entries) = fs::read_dir(dir) else { return false };
    let now = SystemTime::now();
    let mut remain = false;
    for entry in entries.flatten() {
        if !is_part_name(&entry.file_name().to_string_lossy()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else { continue };
        let old = meta
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if meta.is_file() && old && !active.contains(&path) && fs::remove_file(&path).is_ok() {
            continue;
        }
        remain = true;
    }
    remain
}

fn load_part_dirs(file: &Path) -> BTreeSet<PathBuf> {
    fs::read_to_string(file)
        .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
        .unwrap_or_default()
}

fn save_part_dirs(file: &Path, dirs: &BTreeSet<PathBuf>) {
    if dirs.is_empty() {
        let _ = fs::remove_file(file);
        return;
    }
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let body: String = dirs.iter().map(|d| format!("{}\n", d.to_string_lossy())).collect();
    let _ = fs::write(file, body);
}

/// One sweep over `writes` (the upload and local-write maps): drop idle
/// writes, then prune old temp files of ours from every folder recorded in
/// `file` or used since the last sweep; folders left without any are forgotten.
pub fn sweep(writes: &[&Mutex<Uploads>], file: Option<&Path>, max_age: Duration) {
    let mut dirs = file.map(load_part_dirs).unwrap_or_default();
    let mut active = HashSet::new();
    for m in writes {
        let mut ups = m.lock().unwrap_or_else(|e| e.into_inner());
        ups.drop_stale(STALE_UPLOAD);
        dirs.append(&mut ups.part_dirs);
        active.extend(ups.active_parts());
    }
    dirs.retain(|d| prune_parts(d, max_age, &active));
    if let Some(file) = file {
        save_part_dirs(file, &dirs);
    }
}

fn all_writes() -> [&'static Mutex<Uploads>; 2] {
    [&*UPLOADS, crate::remote::transfer::local_writes_map()]
}

/// Sweep both write maps of this machine now and every [`SWEEP_EVERY`] (the
/// first run cleans what a previous run left behind).
pub fn spawn_sweeper(app: AppHandle) {
    let file = app.path().app_cache_dir().ok().map(|d| d.join(PART_DIRS_FILE));
    tauri::async_runtime::spawn(async move {
        let mut tick = tokio::time::interval(SWEEP_EVERY);
        loop {
            tick.tick().await;
            let file = file.clone();
            let _ = tokio::task::spawn_blocking(move || sweep(&all_writes(), file.as_deref(), PART_MAX_AGE)).await;
        }
    });
}

/// The app is quitting: drop every unfinished write and its temp file.
pub fn abort_all_writes() {
    for m in all_writes() {
        m.lock().unwrap_or_else(|e| e.into_inner()).abort_all();
    }
}

/// Remove upload directories under `root` last modified more than `max_age` ago.
fn prune_old(root: &Path, max_age: Duration) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let now = SystemTime::now();
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age >= max_age);
        if old && entry.path().is_dir() {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

pub fn uploads_root<R: tauri::Runtime>(app: &AppHandle<R>) -> Result<PathBuf, String> {
    let dir = app.path().app_cache_dir().map_err(|e| e.to_string())?;
    Ok(dir.join(UPLOADS_DIR))
}

fn lock() -> std::sync::MutexGuard<'static, Uploads> {
    UPLOADS.lock().unwrap_or_else(|e| e.into_inner())
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| format!("join error: {e}"))?
}

/// Start an upload into the upload dir, or, with `dest_dir`, into that folder
/// (`conflict`: `overwrite` / `rename` / `fail`, the default). From the
/// remote-desktop bridge the folder must be writable under its
/// [`crate::remote::fs_policy::RemoteFs`].
#[tauri::command]
pub async fn remote_upload_begin(
    app: AppHandle,
    webview: tauri::Webview,
    db: tauri::State<'_, crate::db::Db>,
    name: String,
    size: u64,
    dest_dir: Option<String>,
    conflict: Option<String>,
    mode: Option<u32>,
) -> Result<UploadBegin, String> {
    let Some(dest_dir) = dest_dir else {
        let root = uploads_root(&app)?;
        return blocking(move || lock().begin(&root, &name, size)).await;
    };
    let conflict = Conflict::parse(conflict.as_deref())?;
    let policy = crate::remote::fs_policy::RemoteFs::for_caller(&webview, db.inner()).await?;
    blocking(move || {
        use crate::remote::fs_policy;
        let name = crate::remote::transfer::valid_name(&name)?;
        // The folder must be writable, and so must the name in it (no hook /
        // tool config planted through a copy).
        let dir = fs_policy::write(policy.as_ref(), Path::new(&dest_dir))?;
        fs_policy::write(policy.as_ref(), &dir.join(name))?;
        lock().begin_to(&dir, name, size, conflict, mode)
    })
    .await
}

#[tauri::command]
pub async fn remote_upload_chunk(upload_id: String, offset: u64, data_base64: String) -> Result<u64, String> {
    blocking(move || {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data_base64.as_bytes())
            .map_err(|e| format!("bad chunk: {e}"))?;
        lock().chunk(&upload_id, offset, &bytes)
    })
    .await
}

/// Complete an upload (refused when its SHA-256 is not `sha256`); returns the
/// file's path on this machine.
#[tauri::command]
pub async fn remote_upload_finish(upload_id: String, sha256: Option<String>) -> Result<String, String> {
    blocking(move || lock().finish(&upload_id, sha256.as_deref())).await
}

#[tauri::command]
pub async fn remote_upload_abort(upload_id: String) -> Result<(), String> {
    blocking(move || {
        lock().abort(&upload_id);
        Ok(())
    })
    .await
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalFileStat {
    pub path: String,
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    /// Unix permission bits (carried to a copy).
    pub mode: Option<u32>,
    /// Set when the path can't be read (the other fields are then empty).
    pub error: Option<String>,
}

fn stat_one(path: String) -> LocalFileStat {
    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    match fs::metadata(&path) {
        Ok(m) => {
            let mode = crate::remote::fs_bridge::mode_of(&m);
            LocalFileStat { path, name, size: m.len(), is_dir: m.is_dir(), mode, error: None }
        }
        Err(e) => LocalFileStat { path, name, size: 0, is_dir: false, mode: None, error: Some(e.to_string()) },
    }
}

/// Size / kind of files on THIS machine (the controlling side of a remote
/// desktop reads what the user dropped before uploading it).
#[tauri::command]
pub async fn stat_local_files(paths: Vec<String>) -> Result<Vec<LocalFileStat>, String> {
    blocking(move || Ok(paths.into_iter().map(stat_one).collect())).await
}

fn read_chunk(path: &str, offset: u64, len: usize) -> Result<Vec<u8>, String> {
    let len = len.min(MAX_CHUNK_BYTES);
    let mut f = fs::File::open(path).map_err(|e| format!("open {path}: {e}"))?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| format!("seek {path}: {e}"))?;
    let mut buf = Vec::with_capacity(len);
    f.take(len as u64).read_to_end(&mut buf).map_err(|e| format!("read {path}: {e}"))?;
    Ok(buf)
}

/// Up to `len` bytes (capped at [`MAX_CHUNK_BYTES`]) of a file on THIS machine
/// from `offset`, base64-encoded. Short (or empty) at end of file.
#[tauri::command]
pub async fn read_local_file_chunk(path: String, offset: u64, len: usize) -> Result<String, String> {
    blocking(move || {
        let bytes = read_chunk(&path, offset, len)?;
        Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-upload-test-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_keeps_only_a_safe_file_name() {
        assert_eq!(sanitize_name("photo.png"), "photo.png");
        assert_eq!(sanitize_name("/Users/me/../../etc/passwd"), "passwd");
        assert_eq!(sanitize_name("C:\\dir\\a.txt"), "a.txt");
        assert_eq!(sanitize_name(".."), "upload");
        assert_eq!(sanitize_name(".env"), "env");
        assert_eq!(sanitize_name(""), "upload");
        assert_eq!(sanitize_name("a\u{0}b:c"), "a_b_c");
        let long = format!("{}.tar.gz", "x".repeat(300));
        let s = sanitize_name(&long);
        assert_eq!(s.chars().count(), MAX_NAME_CHARS);
        assert!(s.ends_with(".gz"));
    }

    #[test]
    fn upload_round_trip_writes_the_file_in_order() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        let b = ups.begin(&root, "../notes.txt", 6).unwrap();
        assert!(b.path.ends_with("notes.txt"));
        assert!(Path::new(&b.path).starts_with(&root));
        assert_eq!(ups.chunk(&b.upload_id, 0, b"abc").unwrap(), 3);
        // Out of order / replayed chunks are refused.
        assert!(ups.chunk(&b.upload_id, 0, b"abc").is_err());
        assert!(ups.finish(&b.upload_id, None).is_err(), "incomplete");
        assert_eq!(ups.chunk(&b.upload_id, 3, b"def").unwrap(), 6);
        let path = ups.finish(&b.upload_id, None).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abcdef");
        assert!(ups.finish(&b.upload_id, None).is_err(), "already finished");
        fs::remove_dir_all(root).unwrap();
    }

    fn sha(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn parts(dir: &Path) -> usize {
        fs::read_dir(dir).unwrap().flatten().filter(|e| is_part_name(&e.file_name().to_string_lossy())).count()
    }

    #[test]
    fn copies_into_a_folder_through_a_temp_file() {
        let dir = tmp_root();
        let mut ups = Uploads::default();
        let b = ups.begin_to(&dir, ".gitignore", 3, Conflict::Fail, None).unwrap();
        assert_eq!(Path::new(&b.path), dir.join(".gitignore"), "leading dots are kept");
        assert!(!dir.join(".gitignore").exists(), "nothing at the destination before finish");
        assert_eq!(parts(&dir), 1);
        ups.chunk(&b.upload_id, 0, b"abc").unwrap();
        assert!(ups.finish(&b.upload_id, Some(&sha(b"abd"))).unwrap_err().contains("integrity"));
        assert_eq!(parts(&dir), 0, "a mismatch drops the temp file");
        assert!(!dir.join(".gitignore").exists());

        let b = ups.begin_to(&dir, ".gitignore", 3, Conflict::Fail, None).unwrap();
        ups.chunk(&b.upload_id, 0, b"abc").unwrap();
        let path = ups.finish(&b.upload_id, Some(&sha(b"abc").to_uppercase())).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
        assert_eq!(parts(&dir), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn conflict_modes() {
        let dir = tmp_root();
        fs::write(dir.join("a.txt"), b"old").unwrap();
        fs::create_dir(dir.join("sub")).unwrap();
        let mut ups = Uploads::default();
        assert!(ups.begin_to(&dir, "a.txt", 1, Conflict::Fail, None).unwrap_err().starts_with("already exists"));
        assert!(ups.begin_to(&dir, "sub", 1, Conflict::Overwrite, None).unwrap_err().starts_with("is a directory"));
        assert!(ups.begin_to(&dir, "../x", 1, Conflict::Fail, None).is_err());
        assert!(ups.begin_to(&dir.join("missing"), "x", 1, Conflict::Fail, None).is_err());

        let mut put = |name: &str, body: &[u8], conflict| {
            let b = ups.begin_to(&dir, name, body.len() as u64, conflict, None).unwrap();
            ups.chunk(&b.upload_id, 0, body).unwrap();
            ups.finish(&b.upload_id, None).unwrap()
        };
        let p = put("a.txt", b"new", Conflict::Overwrite);
        assert_eq!(Path::new(&p), dir.join("a.txt"));
        assert_eq!(fs::read(&p).unwrap(), b"new");
        let p = put("a.txt", b"two", Conflict::Rename);
        assert_eq!(Path::new(&p), dir.join("a (1).txt"));
        let p = put("a.txt", b"three", Conflict::Rename);
        assert_eq!(Path::new(&p), dir.join("a (2).txt"));
        assert_eq!(fs::read(dir.join("a.txt")).unwrap(), b"new", "rename keeps the original");

        // Taken between begin and finish: `fail` refuses and cleans up.
        let b = ups.begin_to(&dir, "late", 1, Conflict::Fail, None).unwrap();
        fs::write(dir.join("late"), b"x").unwrap();
        ups.chunk(&b.upload_id, 0, b"y").unwrap();
        assert!(ups.finish(&b.upload_id, None).unwrap_err().starts_with("already exists"));
        assert_eq!(fs::read(dir.join("late")).unwrap(), b"x");
        assert_eq!(parts(&dir), 0);
        assert_eq!(Conflict::parse(None).unwrap(), Conflict::Fail);
        assert!(Conflict::parse(Some("merge")).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn aborting_a_copy_removes_only_its_temp_file() {
        let dir = tmp_root();
        fs::write(dir.join("keep"), b"1").unwrap();
        let mut ups = Uploads::default();
        let b = ups.begin_to(&dir, "x", 10, Conflict::Fail, None).unwrap();
        ups.chunk(&b.upload_id, 0, b"12345").unwrap();
        ups.abort(&b.upload_id);
        assert_eq!(parts(&dir), 0);
        assert!(dir.join("keep").exists(), "the destination folder is left alone");
        let b = ups.begin_to(&dir, "y", 1, Conflict::Fail, None).unwrap();
        ups.drop_stale(Duration::ZERO);
        assert!(ups.chunk(&b.upload_id, 0, b"a").is_err());
        assert_eq!(parts(&dir), 0, "stale copies are cleaned up too");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_our_temp_files_count_as_parts() {
        assert!(is_part_name(&format!("{PART_PREFIX}{}", "0123456789abcdef0123456789ABCDEF")));
        for name in [".devdy-part-", ".devdy-part-xyz", ".devdy-part-0123", "devdy-part-0123456789abcdef0123456789abcdef", "notes.txt"] {
            assert!(!is_part_name(name), "{name}");
        }
    }

    #[test]
    fn the_sweeper_drops_idle_writes_and_old_leftovers_only() {
        let dir = tmp_root();
        let cache = tmp_root().join("transfer-part-dirs.txt");
        let ups = Mutex::new(Uploads::default());
        let live = ups.lock().unwrap().begin_to(&dir, "live", 4, Conflict::Fail, None).unwrap();
        // A leftover of an earlier run, and files that merely look alike.
        let old = dir.join(format!("{PART_PREFIX}{}", "a".repeat(32)));
        fs::write(&old, b"x").unwrap();
        fs::write(dir.join(".devdy-part-keep"), b"x").unwrap();
        fs::write(dir.join("user.txt"), b"x").unwrap();

        // Young files survive; the folder is remembered on disk for later sweeps.
        sweep(&[&ups], Some(&cache), PART_MAX_AGE);
        assert!(old.exists());
        assert_eq!(load_part_dirs(&cache), BTreeSet::from([dir.clone()]));

        // Old enough (age zero here): the leftover goes, the live write and the
        // look-alikes stay; a fresh Uploads (a restart) still finds the folder.
        let restarted = Mutex::new(Uploads::default());
        sweep(&[&restarted, &ups], Some(&cache), Duration::ZERO);
        assert!(!old.exists());
        assert!(dir.join(".devdy-part-keep").exists() && dir.join("user.txt").exists());
        assert_eq!(parts(&dir), 1, "the live write keeps its temp file");
        assert!(ups.lock().unwrap().chunk(&live.upload_id, 0, b"ab").is_ok());

        ups.lock().unwrap().abort(&live.upload_id);
        sweep(&[&ups], Some(&cache), Duration::ZERO);
        assert!(!cache.exists(), "nothing of ours left: the folder is forgotten");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_end_with_their_window() {
        let dir = tmp_root();
        let mut ups = Uploads::default();
        let a = ups.begin_to(&dir, "a", 1, Conflict::Fail, None).unwrap();
        let b = ups.begin_to(&dir, "b", 1, Conflict::Fail, None).unwrap();
        ups.set_owner(&a.upload_id, "remote-file-transfer");
        ups.abort_owned_by("remote-file-transfer");
        assert!(ups.chunk(&a.upload_id, 0, b"a").is_err());
        assert_eq!(parts(&dir), 1);
        ups.abort_all();
        assert!(ups.chunk(&b.upload_id, 0, b"b").is_err());
        assert_eq!(parts(&dir), 0);
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn copies_carry_permission_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmp_root();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let mut ups = Uploads::default();
        let mut put = |name: &str, conflict, m| {
            let b = ups.begin_to(&dir, name, 1, conflict, m).unwrap();
            ups.chunk(&b.upload_id, 0, b"x").unwrap();
            PathBuf::from(ups.finish(&b.upload_id, None).unwrap())
        };
        assert_eq!(mode(&put("run.sh", Conflict::Fail, Some(0o755))), 0o755);
        assert_eq!(mode(&put("key", Conflict::Fail, Some(0o600))), 0o600);
        assert_eq!(mode(&put("wide", Conflict::Fail, Some(0o777))), 0o755, "no group / other write");
        // Overwriting without a source mode keeps the replaced file's.
        fs::set_permissions(dir.join("run.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(mode(&put("run.sh", Conflict::Overwrite, None)), 0o750);
        assert_eq!(mode(&put("run.sh", Conflict::Overwrite, Some(0o644))), 0o644);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn limits_are_enforced() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        assert!(ups.begin(&root, "big", MAX_UPLOAD_BYTES + 1).is_err());
        let b = ups.begin(&root, "a", 2).unwrap();
        assert!(ups.chunk(&b.upload_id, 0, b"abc").is_err(), "over declared size");
        assert!(ups.chunk("nope", 0, b"a").is_err());
        let dir = Path::new(&b.path).parent().unwrap().to_path_buf();
        ups.abort(&b.upload_id);
        assert!(!dir.exists(), "abort removes the partial file");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stale_uploads_are_dropped() {
        let root = tmp_root();
        let mut ups = Uploads::default();
        let b = ups.begin(&root, "a", 1).unwrap();
        ups.drop_stale(Duration::ZERO);
        assert!(ups.chunk(&b.upload_id, 0, b"a").is_err());
        assert!(!Path::new(&b.path).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_chunks_read_ranges() {
        let root = tmp_root();
        let p = root.join("f.bin");
        fs::write(&p, b"0123456789").unwrap();
        let p = p.to_string_lossy().into_owned();
        assert_eq!(read_chunk(&p, 2, 3).unwrap(), b"234");
        assert_eq!(read_chunk(&p, 8, 10).unwrap(), b"89");
        assert!(read_chunk(&p, 20, 10).unwrap().is_empty());
        let st = stat_one(p.clone());
        assert_eq!((st.size, st.is_dir, st.name.as_str()), (10, false, "f.bin"));
        assert!(stat_one(root.join("missing").to_string_lossy().into_owned()).error.is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
