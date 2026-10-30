//! File sync (design §4, §9b): library skill/rule files, Devdy run logs, Claude
//! transcripts, Codex rollouts and Devdy-managed SSH keys.
//!
//! Local -> relay (`stage`): tracked files are found from the synced rows
//! (skills, rules, runs, servers). A quick scan (files of running sessions and
//! [`kick`]ed paths) runs every 5 s, a full scan every 60 s; a file is only
//! read when its size or mtime changed, and only queued when its sha256 did.
//! Content is cut into 1 MiB plaintext chunks, each sealed and stored as a
//! relay blob. `sync_blobs` remembers which plaintext chunk is already up, so a
//! growing append-only log re-uploads only its last partial chunk + new ones.
//! The `file` op itself goes through `sync_outbox` like a row change.
//!
//! Relay -> local (`apply_op`): last-writer-wins by HLC (via `sync_rows`),
//! missing chunks downloaded and verified, written atomically (tmp + rename)
//! at this device's path. A local file with content sync never saw is not
//! overwritten when it is newer (or, for append-only files, diverged): the
//! remote version goes to a `<name>.conflict-<device>` sibling instead.
//! Library files authored (or edited) here are moved to the legacy backups
//! before a remote version replaces them. Files written here are recorded with
//! origin = 'remote' so the session watcher does not import them again.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(not(test))]
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::clock::{wall_ms, Hlc};
use super::codec::{expand_portable, map_in_project, to_portable, PathCtx};
use super::op::{canonical_json, Op, OpKind};
use super::store::BlobIo;
use crate::db::Db;

pub const TBL: &str = "file";
pub const CHUNK_SIZE: usize = 1 << 20;
/// Bigger files are not synced (a transcript this large is pathological).
const MAX_FILE: u64 = 512 << 20;
/// Marker of the temporary files written before the atomic rename.
const TMP_MARK: &str = ".devdy-sync-";
const CONFLICT_MARK: &str = ".conflict-";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FileKind {
    Library,
    RunLog,
    ClaudeTranscript,
    CodexRollout,
    /// Devdy-managed private key under `<app data>/server-keys`; travels only
    /// when secrets sync is on, always written 0600.
    SshKey,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::Library => "library",
            FileKind::RunLog => "run_log",
            FileKind::ClaudeTranscript => "claude_transcript",
            FileKind::CodexRollout => "codex_rollout",
            FileKind::SshKey => "ssh_key",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "library" => FileKind::Library,
            "run_log" => FileKind::RunLog,
            "claude_transcript" => FileKind::ClaudeTranscript,
            "codex_rollout" => FileKind::CodexRollout,
            "ssh_key" => FileKind::SshKey,
            _ => return None,
        })
    }

    /// Logs and transcripts only ever grow.
    fn append_only(self) -> bool {
        matches!(self, FileKind::RunLog | FileKind::ClaudeTranscript | FileKind::CodexRollout)
    }
}

/// The op pk of a file.
pub fn file_pk(kind: FileKind, portable: &str) -> Value {
    json!({ "kind": kind.as_str(), "path": portable })
}

fn parse_pk(pk: &Value) -> Result<(FileKind, String), String> {
    let kind = pk.get("kind").and_then(Value::as_str).and_then(FileKind::parse);
    let path = pk.get("path").and_then(Value::as_str).filter(|p| !p.is_empty());
    match (kind, path) {
        (Some(k), Some(p)) => Ok((k, p.to_string())),
        _ => Err(format!("malformed file pk {pk}")),
    }
}

/// A file this device keeps in sync.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Tracked {
    pub kind: FileKind,
    pub local: PathBuf,
    pub portable: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ChunkRef {
    /// sha256 of the plaintext chunk.
    sha: String,
    /// sha of the sealed blob on the relay.
    blob: String,
    len: u64,
}

/// A `sync_files` row.
#[derive(Debug, Clone)]
struct Record {
    local_path: String,
    size: u64,
    sha256: String,
    chunks: Vec<ChunkRef>,
    mtime: i64,
    mode: Option<i64>,
    deleted: bool,
    /// 'local' (authored here) or 'remote' (written by sync).
    origin: String,
}

// ---- Scan cadence ------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Timing {
    /// Files of running sessions + kicked paths.
    pub quick: Duration,
    /// Everything (and Keychain items).
    pub full: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Timing { quick: Duration::from_secs(5), full: Duration::from_secs(60) }
    }
}

#[derive(Default)]
pub struct ScanState {
    last_quick: Option<Instant>,
    last_full: Option<Instant>,
    /// Codex thread id -> rollout, rebuilt on every full scan (a Devdy-native
    /// Codex run does not record its rollout path).
    codex_index: HashMap<String, PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanMode {
    Quick,
    Full,
}

impl ScanState {
    /// Which scan is due now, if any. `kicked` forces at least a quick one.
    pub fn due(&mut self, timing: &Timing, kicked: bool) -> Option<ScanMode> {
        let now = Instant::now();
        let elapsed = |t: Option<Instant>, every: Duration| t.is_none_or(|t| now.duration_since(t) >= every);
        if elapsed(self.last_full, timing.full) {
            self.last_full = Some(now);
            self.last_quick = Some(now);
            return Some(ScanMode::Full);
        }
        if kicked || elapsed(self.last_quick, timing.quick) {
            self.last_quick = Some(now);
            return Some(ScanMode::Quick);
        }
        None
    }
}

#[cfg(not(test))]
static KICKED: StdMutex<BTreeSet<PathBuf>> = StdMutex::new(BTreeSet::new());

#[cfg(not(test))]
fn with_kicks<R>(f: impl FnOnce(&mut BTreeSet<PathBuf>) -> R) -> Option<R> {
    KICKED.lock().ok().map(|mut k| f(&mut k))
}

// Tests run in parallel, each on its own runtime thread.
#[cfg(test)]
thread_local! {
    static KICKED: std::cell::RefCell<BTreeSet<PathBuf>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}

#[cfg(test)]
fn with_kicks<R>(f: impl FnOnce(&mut BTreeSet<PathBuf>) -> R) -> Option<R> {
    Some(KICKED.with(|k| f(&mut k.borrow_mut())))
}

/// A tracked file changed (or may have): pick it up on the next quick scan
/// instead of waiting for the full one.
pub fn kick(path: impl Into<PathBuf>) {
    let path = path.into();
    with_kicks(|k| k.insert(path));
}

pub fn has_kicks() -> bool {
    with_kicks(|k| !k.is_empty()).unwrap_or(false)
}

fn take_kicks() -> BTreeSet<PathBuf> {
    with_kicks(std::mem::take).unwrap_or_default()
}

// ---- Tracked files -----------------------------------------------------------

fn err(e: sqlx::Error) -> String {
    e.to_string()
}

fn is_sync_artifact(name: &str) -> bool {
    name.contains(TMP_MARK) || name.contains(CONFLICT_MARK) || name == ".DS_Store"
}

fn under_app_data(paths: &PathCtx, p: &Path) -> bool {
    paths.app_data.as_deref().is_some_and(|d| !d.as_os_str().is_empty() && p.starts_with(d))
}

fn walk_files(root: &Path, out: &mut Vec<PathBuf>) {
    if root.is_file() {
        out.push(root.to_path_buf());
        return;
    }
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let name = entry.file_name().to_string_lossy();
        if entry.file_type().is_file() && !is_sync_artifact(&name) {
            out.push(entry.into_path());
        }
    }
}

fn track(paths: &PathCtx, kind: FileKind, local: PathBuf, out: &mut Vec<Tracked>) {
    let portable = to_portable(paths, &local.to_string_lossy());
    out.push(Tracked { kind, local, portable });
}

/// Library files: every file of every skill folder, every rule file and the
/// Codex skill form of each rule. Only inside this device's app data dir.
async fn library_files(db: &Db, paths: &PathCtx, out: &mut Vec<Tracked>) -> Result<(), String> {
    if paths.app_data.is_none() {
        return Ok(());
    }
    let mut files = Vec::new();
    for src in sqlx::query_scalar::<_, String>("SELECT source_path FROM skills").fetch_all(db).await.map_err(err)? {
        let p = PathBuf::from(src);
        if under_app_data(paths, &p) {
            walk_files(&p, &mut files);
        }
    }
    for row in sqlx::query("SELECT name, source_path FROM rules").fetch_all(db).await.map_err(err)? {
        let name: String = row.get("name");
        let p = PathBuf::from(row.get::<String, _>("source_path"));
        if !under_app_data(paths, &p) {
            continue;
        }
        if p.is_file() {
            files.push(p.clone());
        }
        if let Some(dir) = p.parent() {
            let codex = dir.join(".codex-skills").join(&name);
            if codex.is_dir() {
                walk_files(&codex, &mut files);
            }
        }
    }
    for f in files {
        track(paths, FileKind::Library, f, out);
    }
    Ok(())
}

async fn ssh_key_files(db: &Db, paths: &PathCtx, out: &mut Vec<Tracked>) -> Result<(), String> {
    let Some(keys_root) = paths.app_data.as_ref().map(|d| d.join("server-keys")) else { return Ok(()) };
    let rows: Vec<String> =
        sqlx::query_scalar("SELECT private_key_path FROM servers WHERE private_key_path IS NOT NULL")
            .fetch_all(db)
            .await
            .map_err(err)?;
    for p in rows.into_iter().map(PathBuf::from) {
        if p.starts_with(&keys_root) && p.is_file() {
            track(paths, FileKind::SshKey, p, out);
        }
    }
    Ok(())
}

fn codex_index(home: &Path) -> HashMap<String, PathBuf> {
    let mut idx = HashMap::new();
    let root = home.join(".codex").join("sessions");
    for entry in walkdir::WalkDir::new(root).follow_links(false).into_iter().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_prefix("rollout-").and_then(|n| n.strip_suffix(".jsonl")) else { continue };
        // rollout-<YYYY-MM-DDThh-mm-ss>-<thread id>: the id is everything after
        // the 19-char timestamp and its dash.
        if let Some(id) = stem.get(20..) {
            idx.insert(id.to_string(), entry.into_path());
        }
    }
    idx
}

/// Run logs and CLI transcripts of runs (all, or only running ones).
async fn run_files(
    db: &Db,
    paths: &PathCtx,
    running_only: bool,
    codex: &HashMap<String, PathBuf>,
    out: &mut Vec<Tracked>,
) -> Result<(), String> {
    let sql = format!(
        "SELECT r.id, r.engine, r.session_id, r.output_path, r.transcript_path, p.path AS project_path, \
                a.config_dir \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         LEFT JOIN claude_accounts a ON a.id = r.claude_account_id {}",
        if running_only { "WHERE r.status = 'running'" } else { "" }
    );
    let rows = sqlx::query(&sql).fetch_all(db).await.map_err(err)?;
    for row in rows {
        let id: String = row.get("id");
        let engine: Option<String> = row.get("engine");
        let session: Option<String> = row.get("session_id");
        let output: Option<String> = row.get("output_path");
        let transcript: Option<String> = row.get("transcript_path");
        let project: String = row.get("project_path");
        let config_dir: Option<String> = row.get("config_dir");

        let conventional = Path::new(&project).join(".devdy").join("runs").join(format!("{id}.log"));
        let mut logs: Vec<PathBuf> = output.map(PathBuf::from).into_iter().collect();
        if !logs.contains(&conventional) {
            logs.push(conventional);
        }
        for log in logs.into_iter().filter(|p| p.is_file()) {
            track(paths, FileKind::RunLog, log, out);
        }

        let recorded = transcript.map(PathBuf::from).filter(|p| p.is_file());
        match (engine.as_deref(), session.as_deref()) {
            (Some("claude"), Some(sid)) => {
                let file = recorded.or_else(|| {
                    let root = match config_dir.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                        Some(d) => PathBuf::from(d),
                        None => paths.home.join(".claude"),
                    };
                    let enc = crate::commands::sessions::encode_project_dir(&project);
                    Some(root.join("projects").join(enc).join(format!("{sid}.jsonl"))).filter(|p| p.is_file())
                });
                if let Some(f) = file {
                    track(paths, FileKind::ClaudeTranscript, f, out);
                }
            }
            (Some("codex"), Some(sid)) => {
                if let Some(f) = recorded.or_else(|| codex.get(sid).cloned().filter(|p| p.is_file())) {
                    track(paths, FileKind::CodexRollout, f, out);
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Every file this device currently tracks.
async fn collect(
    db: &Db,
    paths: &PathCtx,
    mode: ScanMode,
    state: &mut ScanState,
    with_keys: bool,
) -> Result<Vec<Tracked>, String> {
    let mut out = Vec::new();
    if mode == ScanMode::Full {
        state.codex_index = codex_index(&paths.home);
        library_files(db, paths, &mut out).await?;
        if with_keys {
            ssh_key_files(db, paths, &mut out).await?;
        }
    }
    run_files(db, paths, mode == ScanMode::Quick, &state.codex_index, &mut out).await?;
    out.sort();
    out.dedup();
    Ok(out)
}

// ---- Records -----------------------------------------------------------------

async fn load_record(conn: &mut SqliteConnection, kind: FileKind, portable: &str) -> Result<Option<Record>, String> {
    let row = sqlx::query(
        "SELECT local_path, size, sha256, chunks_json, mtime, mode, deleted, origin FROM sync_files WHERE kind = ? AND path = ?",
    )
    .bind(kind.as_str())
    .bind(portable)
    .fetch_optional(&mut *conn)
    .await
    .map_err(err)?;
    Ok(row.map(|r| Record {
        local_path: r.get("local_path"),
        size: r.get::<i64, _>("size").max(0) as u64,
        sha256: r.get("sha256"),
        chunks: serde_json::from_str(&r.get::<String, _>("chunks_json")).unwrap_or_default(),
        mtime: r.get("mtime"),
        mode: r.get("mode"),
        deleted: r.get::<i64, _>("deleted") != 0,
        origin: r.get("origin"),
    }))
}

#[allow(clippy::too_many_arguments)]
async fn save_record(
    conn: &mut SqliteConnection,
    kind: FileKind,
    portable: &str,
    local: &Path,
    content: Option<(&Content, i64)>,
    mode: Option<u32>,
    hlc: Option<&Hlc>,
    origin: &str,
) -> Result<(), String> {
    let (size, sha, chunks, mtime, deleted) = match content {
        Some((c, mtime)) => (c.size as i64, c.sha256.clone(), serde_json::to_string(&c.chunks).unwrap_or_default(), mtime, 0),
        None => (0, String::new(), "[]".to_string(), 0, 1),
    };
    sqlx::query(
        "INSERT INTO sync_files (kind, path, local_path, size, sha256, chunks_json, mtime, mode, hlc, deleted, origin) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(kind, path) DO UPDATE SET local_path = excluded.local_path, size = excluded.size, \
           sha256 = excluded.sha256, chunks_json = excluded.chunks_json, mtime = excluded.mtime, \
           mode = excluded.mode, hlc = COALESCE(excluded.hlc, sync_files.hlc), deleted = excluded.deleted, \
           origin = excluded.origin",
    )
    .bind(kind.as_str())
    .bind(portable)
    .bind(local.to_string_lossy().as_ref())
    .bind(size)
    .bind(sha)
    .bind(chunks)
    .bind(mtime)
    .bind(mode.map(i64::from))
    .bind(hlc.map(|h| h.to_string()))
    .bind(deleted)
    .bind(origin)
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok(())
}

async fn queue(conn: &mut SqliteConnection, kind: FileKind, portable: &str, op: &str) -> Result<(), String> {
    sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES (?, ?, ?, ?)")
        .bind(TBL)
        .bind(canonical_json(&file_pk(kind, portable)))
        .bind(op)
        .bind(wall_ms() as i64)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

fn mtime_ms(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn file_mode(meta: &fs::Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

// ---- Chunking ----------------------------------------------------------------

/// A file's content as sync describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Content {
    size: u64,
    sha256: String,
    chunks: Vec<ChunkRef>,
}

fn read_full(f: &mut fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Hash `path` in `CHUNK_SIZE` chunks, uploading every chunk the relay does
/// not have yet (per `sync_blobs`).
async fn upload_file(conn: &mut SqliteConnection, blobs: &dyn BlobIo, path: &Path) -> Result<Content, StageError> {
    let local = |e: std::io::Error| StageError::Local(format!("{}: {e}", path.display()));
    let abort = |e: sqlx::Error| StageError::Abort(e.to_string());
    let mut f = fs::File::open(path).map_err(local)?;
    let mut whole = Sha256::new();
    let mut chunks = Vec::new();
    let mut size = 0u64;
    let mut buf = vec![0u8; CHUNK_SIZE];
    loop {
        let n = read_full(&mut f, &mut buf).map_err(local)?;
        if n == 0 {
            break;
        }
        let bytes = &buf[..n];
        whole.update(bytes);
        size += n as u64;
        let sha = hex::encode(Sha256::digest(bytes));
        let known: Option<String> = sqlx::query_scalar("SELECT blob_sha FROM sync_blobs WHERE chunk_sha = ?")
            .bind(&sha)
            .fetch_optional(&mut *conn)
            .await
            .map_err(abort)?;
        let blob = match known {
            Some(b) => b,
            None => {
                let b = blobs.put(bytes).await.map_err(StageError::Abort)?;
                sqlx::query(
                    "INSERT OR REPLACE INTO sync_blobs (chunk_sha, blob_sha, len, uploaded_at) VALUES (?, ?, ?, ?)",
                )
                .bind(&sha)
                .bind(&b)
                .bind(n as i64)
                .bind(wall_ms() as i64)
                .execute(&mut *conn)
                .await
                .map_err(abort)?;
                b
            }
        };
        chunks.push(ChunkRef { sha, blob, len: n as u64 });
        if n < CHUNK_SIZE {
            break;
        }
    }
    Ok(Content { size, sha256: hex::encode(whole.finalize()), chunks })
}

fn sha_of_prefix(path: &Path, len: u64) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    let mut take = f.take(len);
    let copied = std::io::copy(&mut take, &mut HashWriter(&mut h)).ok()?;
    (copied == len).then(|| hex::encode(h.finalize()))
}

struct HashWriter<'a>(&'a mut Sha256);
impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---- Local -> outbox -----------------------------------------------------------

/// Scan what is due and queue `file` ops for changed files. Returns how many
/// ops were queued.
pub async fn stage(
    db: &Db,
    paths: &PathCtx,
    blobs: &dyn BlobIo,
    state: &mut ScanState,
    mode: ScanMode,
    with_keys: bool,
) -> Result<usize, String> {
    let kicked = take_kicks();
    let mut tracked = collect(db, paths, mode, state, with_keys).await?;
    if !kicked.is_empty() {
        let mut conn = db.acquire().await.map_err(err)?;
        for p in &kicked {
            let row: Option<(String, String)> =
                sqlx::query_as("SELECT kind, path FROM sync_files WHERE local_path = ? AND deleted = 0")
                    .bind(p.to_string_lossy().as_ref())
                    .fetch_optional(&mut *conn)
                    .await
                    .map_err(err)?;
            if let Some((kind, portable)) = row {
                if let Some(kind) = FileKind::parse(&kind) {
                    tracked.push(Tracked { kind, local: p.clone(), portable });
                }
            }
        }
        tracked.sort();
        tracked.dedup();
    }
    let mut queued = 0;
    for t in &tracked {
        match stage_one(db, blobs, t).await {
            Ok(true) => queued += 1,
            Ok(false) => {}
            Err(StageError::Abort(e)) => return Err(e),
            Err(StageError::Local(e)) => tracing::debug!(event = "sync_file_skipped", error = %e),
        }
    }
    if mode == ScanMode::Full {
        queued += stage_deletions(db, with_keys).await?;
    }
    Ok(queued)
}

enum StageError {
    /// The relay (or the DB) is unusable: stop this scan.
    Abort(String),
    /// This file only (vanished, unreadable...).
    Local(String),
}

async fn stage_one(db: &Db, blobs: &dyn BlobIo, t: &Tracked) -> Result<bool, StageError> {
    let local = StageError::Local;
    let meta = match fs::metadata(&t.local) {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(false),
        Err(e) => return Err(local(format!("{}: {e}", t.local.display()))),
    };
    if meta.len() > MAX_FILE {
        return Err(local(format!("{}: too large to sync", t.local.display())));
    }
    let mtime = mtime_ms(&meta);
    let mut conn = db.acquire().await.map_err(|e| local(e.to_string()))?;
    let rec = load_record(&mut conn, t.kind, &t.portable).await.map_err(local)?;
    let local_str = t.local.to_string_lossy();
    if let Some(r) = &rec {
        if !r.deleted && r.size == meta.len() && r.mtime == mtime && r.local_path == local_str {
            return Ok(false);
        }
    }
    let content = upload_file(&mut conn, blobs, &t.local).await?;
    let mode = match t.kind {
        FileKind::SshKey => Some(0o600),
        FileKind::Library => file_mode(&meta),
        _ => None,
    };
    let unchanged = rec.as_ref().is_some_and(|r| !r.deleted && r.sha256 == content.sha256);
    let mut tx = sqlx::Connection::begin(&mut *conn).await.map_err(|e| local(e.to_string()))?;
    if unchanged {
        // Touched but identical (or moved): remember the new stat, keep origin.
        sqlx::query("UPDATE sync_files SET local_path = ?, mtime = ? WHERE kind = ? AND path = ?")
            .bind(local_str.as_ref())
            .bind(mtime)
            .bind(t.kind.as_str())
            .bind(&t.portable)
            .execute(&mut *tx)
            .await
            .map_err(|e| local(e.to_string()))?;
    } else {
        save_record(&mut tx, t.kind, &t.portable, &t.local, Some((&content, mtime)), mode, None, "local")
            .await
            .map_err(local)?;
        queue(&mut tx, t.kind, &t.portable, "U").await.map_err(local)?;
    }
    tx.commit().await.map_err(|e| local(e.to_string()))?;
    Ok(!unchanged)
}

/// Library files / keys removed on their own (their folder is still there)
/// become Delete ops; for anything else the row op carries the removal and
/// the record is simply forgotten.
async fn stage_deletions(db: &Db, with_keys: bool) -> Result<usize, String> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT kind, path, local_path FROM sync_files WHERE deleted = 0 AND kind IN ('library', 'ssh_key')")
            .fetch_all(db)
            .await
            .map_err(err)?;
    let mut queued = 0;
    for (kind, portable, local) in rows {
        let Some(kind) = FileKind::parse(&kind) else { continue };
        if kind == FileKind::SshKey && !with_keys {
            continue;
        }
        let p = Path::new(&local);
        if fs::symlink_metadata(p).is_ok() {
            continue;
        }
        let mut tx = db.begin().await.map_err(err)?;
        if p.parent().is_some_and(Path::is_dir) {
            save_record(&mut tx, kind, &portable, p, None, None, None, "local").await?;
            queue(&mut tx, kind, &portable, "D").await?;
            queued += 1;
        } else {
            sqlx::query("DELETE FROM sync_files WHERE kind = ? AND path = ?")
                .bind(kind.as_str())
                .bind(&portable)
                .execute(&mut *tx)
                .await
                .map_err(err)?;
        }
        tx.commit().await.map_err(err)?;
    }
    Ok(queued)
}

/// The op row for an outbox entry: `None` drops the entry (no record).
pub async fn pending_row(
    conn: &mut SqliteConnection,
    pk: &Value,
    with_keys: bool,
) -> Result<Option<(OpKind, Option<Map<String, Value>>)>, String> {
    let Ok((kind, portable)) = parse_pk(pk) else { return Ok(None) };
    if kind == FileKind::SshKey && !with_keys {
        return Ok(None);
    }
    let Some(rec) = load_record(conn, kind, &portable).await? else { return Ok(None) };
    if rec.deleted {
        return Ok(Some((OpKind::Delete, None)));
    }
    let mode = rec.mode;
    let row = json!({
        "size": rec.size,
        "sha256": rec.sha256,
        "chunks": rec.chunks.iter().map(|c| c.blob.clone()).collect::<Vec<_>>(),
        "chunk_size": CHUNK_SIZE,
        "mode": mode,
    });
    Ok(Some((OpKind::Upsert, row.as_object().cloned())))
}

// ---- Relay -> local --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Applied,
    Skipped,
    /// The remote version was kept next to a diverged local file.
    Conflict,
}

/// Where a portable file path lives on this device, refusing anything that
/// would land outside the place its kind belongs to.
pub fn localize(paths: &PathCtx, kind: FileKind, portable: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(match kind {
        FileKind::RunLog => map_in_project(paths, portable),
        _ => expand_portable(paths, portable),
    });
    let bad = || format!("cannot place {} file {portable} on this device", kind.as_str());
    if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(bad());
    }
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if name.is_empty() || is_sync_artifact(&name) {
        return Err(bad());
    }
    let ok = match kind {
        FileKind::Library => {
            let data = paths.app_data.as_deref();
            data.is_some_and(|d| p.starts_with(d.join("skills")) || p.starts_with(d.join("rules")))
        }
        FileKind::SshKey => paths.app_data.as_deref().is_some_and(|d| p.starts_with(d.join("server-keys"))),
        FileKind::RunLog => {
            let parts: Vec<_> = p.components().collect();
            parts.windows(2).any(|w| w[0].as_os_str() == ".devdy" && w[1].as_os_str() == "runs")
        }
        FileKind::ClaudeTranscript => {
            p.extension().is_some_and(|e| e == "jsonl") && p.components().any(|c| c.as_os_str() == "projects")
        }
        FileKind::CodexRollout => p.starts_with(paths.home.join(".codex").join("sessions")) && name.starts_with("rollout-"),
    };
    if ok {
        Ok(p)
    } else {
        Err(bad())
    }
}

struct Incoming {
    size: u64,
    sha256: String,
    chunks: Vec<String>,
    mode: Option<u32>,
}

fn parse_row(row: &Map<String, Value>) -> Result<Incoming, String> {
    let size = row.get("size").and_then(Value::as_u64).ok_or("file op without size")?;
    let sha256 = row.get("sha256").and_then(Value::as_str).ok_or("file op without sha256")?.to_string();
    let chunks: Vec<String> = row
        .get("chunks")
        .and_then(Value::as_array)
        .ok_or("file op without chunks")?
        .iter()
        .map(|c| c.as_str().map(str::to_string).ok_or("bad chunk ref"))
        .collect::<Result<_, _>>()?;
    let chunk_size = row.get("chunk_size").and_then(Value::as_u64).unwrap_or(CHUNK_SIZE as u64);
    if chunk_size != CHUNK_SIZE as u64 {
        return Err(format!("unsupported chunk size {chunk_size}"));
    }
    if size > MAX_FILE || chunks.len() as u64 != size.div_ceil(CHUNK_SIZE as u64) {
        return Err("file op size and chunks disagree".into());
    }
    let mode = row.get("mode").and_then(Value::as_u64).map(|m| (m as u32) & 0o777);
    Ok(Incoming { size, sha256, chunks, mode })
}

/// Apply one remote `file` op. Errors mean "retry later" (the caller defers).
pub async fn apply_op(db: &Db, paths: &PathCtx, blobs: &dyn BlobIo, op: &Op) -> Result<Outcome, String> {
    let (kind, portable) = parse_pk(&op.pk)?;
    let key = op.pk_key();
    let mut conn = db.acquire().await.map_err(err)?;
    let known: Option<String> = sqlx::query_scalar("SELECT hlc FROM sync_rows WHERE tbl = ? AND pk = ?")
        .bind(TBL)
        .bind(&key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
    if known.and_then(|s| s.parse::<Hlc>().ok()).is_some_and(|h| h >= op.hlc) {
        return Ok(Outcome::Skipped);
    }
    let local_changed: Option<i64> =
        sqlx::query_scalar("SELECT MAX(changed_at) FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(TBL)
            .bind(&key)
            .fetch_one(&mut *conn)
            .await
            .map_err(err)?;
    if local_changed.is_some_and(|c| c.max(0) as u64 >= op.hlc.millis) {
        // Our newer local version will be pushed and win on the other side.
        return Ok(Outcome::Skipped);
    }
    let path = localize(paths, kind, &portable)?;
    let rec = load_record(&mut conn, kind, &portable).await?;
    drop(conn);

    let outcome = match op.kind {
        OpKind::Upsert => {
            let row = op.row.as_ref().ok_or("upsert without row")?;
            write_remote(db, blobs, kind, &portable, &path, rec.as_ref(), &parse_row(row)?, op).await?
        }
        OpKind::Delete => remove_remote(db, kind, &portable, &path, rec.as_ref()).await?,
    };

    let mut tx = db.begin().await.map_err(err)?;
    if outcome != Outcome::Skipped {
        // Whatever local change was pending lost (or survives as a conflict copy).
        sqlx::query("DELETE FROM sync_outbox WHERE tbl = ? AND pk = ?")
            .bind(TBL)
            .bind(&key)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    }
    sqlx::query(
        "INSERT INTO sync_rows (tbl, pk, hlc, deleted) VALUES (?, ?, ?, ?) \
         ON CONFLICT(tbl, pk) DO UPDATE SET hlc = excluded.hlc, deleted = excluded.deleted",
    )
    .bind(TBL)
    .bind(&key)
    .bind(op.hlc.to_string())
    .bind((op.kind == OpKind::Delete) as i64)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(outcome)
}

/// Whether the file at `path` is exactly the version sync last recorded.
fn matches_record(path: &Path, rec: Option<&Record>) -> bool {
    let Some(r) = rec.filter(|r| !r.deleted && r.local_path == path.to_string_lossy()) else { return false };
    fs::metadata(path).is_ok_and(|m| m.len() == r.size && mtime_ms(&m) == r.mtime)
}

enum Placement {
    /// Replace the file.
    Replace,
    /// Replace, but first move the unseen local version to the backups.
    BackupThenReplace,
    /// Keep the local file, write the remote one next to it.
    Sibling,
    /// Keep the local file, drop the remote version (local is ahead).
    Keep,
    /// Already identical.
    Same,
}

fn decide(kind: FileKind, path: &Path, rec: Option<&Record>, inc: &Incoming, op_millis: u64) -> Placement {
    let Ok(meta) = fs::metadata(path) else { return Placement::Replace };
    if meta.len() == inc.size && sha_of_prefix(path, inc.size).as_deref() == Some(inc.sha256.as_str()) {
        return Placement::Same;
    }
    if matches_record(path, rec) {
        // A library version authored here is kept in the backups when a
        // remote one wins; one that came from sync exists elsewhere anyway.
        let authored_here = rec.is_some_and(|r| r.origin == "local");
        return if kind == FileKind::Library && authored_here { Placement::BackupThenReplace } else { Placement::Replace };
    }
    // The local file holds content sync never saw.
    if kind.append_only() {
        let local_len = meta.len();
        if local_len > inc.size && sha_of_prefix(path, inc.size).as_deref() == Some(inc.sha256.as_str()) {
            return Placement::Keep;
        }
        // Local being a prefix of the incoming version is checked while
        // assembling (`prefix_ok`); anything else diverged.
        return Placement::Sibling;
    }
    if mtime_ms(&meta).max(0) as u64 > op_millis {
        Placement::Sibling
    } else if kind == FileKind::Library {
        Placement::BackupThenReplace
    } else {
        Placement::Replace
    }
}

fn sibling_path(path: &Path, device: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let device: String = device.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(32).collect();
    path.with_file_name(format!("{name}{CONFLICT_MARK}{device}"))
}

fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}{TMP_MARK}{}.tmp", uuid::Uuid::new_v4().simple()))
}

/// Write the incoming content to `tmp`, reusing chunks the local copy already
/// has (only when the local file is the version sync recorded).
async fn assemble(
    blobs: &dyn BlobIo,
    path: &Path,
    rec: Option<&Record>,
    inc: &Incoming,
    tmp: &Path,
) -> Result<Content, String> {
    let reusable: HashMap<usize, (u64, u64)> = match rec.filter(|_| matches_record(path, rec)) {
        Some(r) => {
            let mut off = 0u64;
            r.chunks
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let at = off;
                    off += c.len;
                    (i, c, at)
                })
                .filter(|(i, c, _)| inc.chunks.get(*i) == Some(&c.blob))
                .map(|(i, c, at)| (i, (at, c.len)))
                .collect()
        }
        None => HashMap::new(),
    };
    let io = |e: std::io::Error| format!("{}: {e}", tmp.display());
    let mut out = fs::File::create(tmp).map_err(io)?;
    let mut whole = Sha256::new();
    let mut chunks = Vec::with_capacity(inc.chunks.len());
    let mut size = 0u64;
    for (i, blob) in inc.chunks.iter().enumerate() {
        let bytes = match reusable.get(&i) {
            Some((at, len)) => {
                let mut f = fs::File::open(path).map_err(io)?;
                f.seek(SeekFrom::Start(*at)).map_err(io)?;
                let mut b = vec![0u8; *len as usize];
                f.read_exact(&mut b).map_err(io)?;
                b
            }
            None => blobs.get(blob).await?,
        };
        if bytes.len() > CHUNK_SIZE || (i + 1 < inc.chunks.len() && bytes.len() != CHUNK_SIZE) {
            return Err("chunk has the wrong size".into());
        }
        whole.update(&bytes);
        size += bytes.len() as u64;
        out.write_all(&bytes).map_err(io)?;
        chunks.push(ChunkRef { sha: hex::encode(Sha256::digest(&bytes)), blob: blob.clone(), len: bytes.len() as u64 });
    }
    out.sync_all().map_err(io)?;
    let sha256 = hex::encode(whole.finalize());
    if size != inc.size || sha256 != inc.sha256 {
        return Err("downloaded content does not match the file op".into());
    }
    Ok(Content { size, sha256, chunks })
}

#[allow(clippy::too_many_arguments)]
async fn write_remote(
    db: &Db,
    blobs: &dyn BlobIo,
    kind: FileKind,
    portable: &str,
    path: &Path,
    rec: Option<&Record>,
    inc: &Incoming,
    op: &Op,
) -> Result<Outcome, String> {
    let mut placement = decide(kind, path, rec, inc, op.hlc.millis);
    let mode = match kind {
        FileKind::SshKey => Some(0o600),
        _ => inc.mode,
    };
    if let Placement::Keep = placement {
        return Ok(Outcome::Skipped);
    }
    if let Placement::Same = placement {
        let meta = fs::metadata(path).map_err(|e| e.to_string())?;
        let content = Content { size: inc.size, sha256: inc.sha256.clone(), chunks: chunk_refs_of(path, inc)? };
        let mut conn = db.acquire().await.map_err(err)?;
        save_record(&mut conn, kind, portable, path, Some((&content, mtime_ms(&meta))), mode, Some(&op.hlc), "remote").await?;
        return Ok(Outcome::Applied);
    }
    let parent = path.parent().ok_or("file without a parent folder")?;
    fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if kind == FileKind::SshKey {
        set_mode(parent, 0o700);
    }
    let tmp = tmp_path(path);
    let content = match assemble(blobs, path, rec, inc, &tmp).await {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e);
        }
    };
    // A diverged append-only file whose content is a prefix of the incoming
    // one was simply behind.
    if matches!(placement, Placement::Sibling) && kind.append_only() {
        let local_len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if local_len < inc.size && sha_of_prefix(path, local_len) == sha_of_prefix(&tmp, local_len) {
            placement = Placement::Replace;
        }
    }
    if let Some(m) = mode {
        set_mode(&tmp, m);
    }
    let finish = |e: String| {
        let _ = fs::remove_file(&tmp);
        e
    };
    match placement {
        Placement::Sibling => {
            let sib = sibling_path(path, &op.hlc.device);
            fs::rename(&tmp, &sib).map_err(|e| finish(format!("{}: {e}", sib.display())))?;
            tracing::info!(event = "sync_file_conflict", kind = kind.as_str());
            Ok(Outcome::Conflict)
        }
        _ => {
            if matches!(placement, Placement::BackupThenReplace) && path.exists() {
                crate::commands::project_links::back_up_library_orphan(path, "sync").map_err(finish)?;
            }
            let mtime = fs::metadata(&tmp).map(|m| mtime_ms(&m)).map_err(|e| finish(e.to_string()))?;
            // Recorded before the rename so the session watcher, woken by the
            // rename, already knows this version came from sync.
            let mut conn = db.acquire().await.map_err(|e| finish(err(e)))?;
            save_record(&mut conn, kind, portable, path, Some((&content, mtime)), mode, Some(&op.hlc), "remote")
                .await
                .map_err(finish)?;
            if let Err(e) = fs::rename(&tmp, path) {
                let _ = sqlx::query("DELETE FROM sync_files WHERE kind = ? AND path = ?")
                    .bind(kind.as_str())
                    .bind(portable)
                    .execute(&mut *conn)
                    .await;
                return Err(finish(format!("{}: {e}", path.display())));
            }
            if matches!(kind, FileKind::ClaudeTranscript | FileKind::CodexRollout) {
                mark_transcript_mirrored(&mut conn, path, content.size).await?;
            }
            Ok(Outcome::Applied)
        }
    }
}

/// Chunk refs for a local file already identical to the incoming version.
fn chunk_refs_of(path: &Path, inc: &Incoming) -> Result<Vec<ChunkRef>, String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut out = Vec::with_capacity(inc.chunks.len());
    for blob in &inc.chunks {
        let n = read_full(&mut f, &mut buf).map_err(|e| e.to_string())?;
        out.push(ChunkRef { sha: hex::encode(Sha256::digest(&buf[..n])), blob: blob.clone(), len: n as u64 });
    }
    Ok(out)
}

/// The run log arrived through sync as well, so the transcript counts as
/// mirrored: `upsert_session_run_core` must not rebuild the log from it. The
/// apply guard keeps this machine-local write out of the outbox.
async fn mark_transcript_mirrored(conn: &mut SqliteConnection, path: &Path, size: u64) -> Result<(), String> {
    let p = path.to_string_lossy();
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let mut tx = sqlx::Connection::begin(&mut *conn).await.map_err(err)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 1 WHERE id = 1").execute(&mut *tx).await.map_err(err)?;
    sqlx::query(
        "UPDATE runs SET transcript_synced_size = ? \
         WHERE transcript_path = ? OR (engine = 'claude' AND session_id = ? AND transcript_path IS NULL)",
    )
    .bind(size as i64)
    .bind(p.as_ref())
    .bind(&stem)
    .execute(&mut *tx)
    .await
    .map_err(err)?;
    sqlx::query("UPDATE sync_apply_guard SET active = 0 WHERE id = 1").execute(&mut *tx).await.map_err(err)?;
    tx.commit().await.map_err(err)?;
    Ok(())
}

async fn remove_remote(
    db: &Db,
    kind: FileKind,
    portable: &str,
    path: &Path,
    rec: Option<&Record>,
) -> Result<Outcome, String> {
    let mut conn = db.acquire().await.map_err(err)?;
    if fs::symlink_metadata(path).is_err() {
        save_record(&mut conn, kind, portable, path, None, None, None, "remote").await?;
        return Ok(Outcome::Applied);
    }
    if !matches_record(path, rec) {
        // Edited here since the last sync: keep it (it will be pushed back).
        return Ok(Outcome::Conflict);
    }
    match kind {
        FileKind::Library | FileKind::SshKey => {
            crate::commands::project_links::back_up_library_orphan(path, "sync")?;
        }
        _ => fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?,
    }
    save_record(&mut conn, kind, portable, path, None, None, None, "remote").await?;
    Ok(Outcome::Applied)
}

// ---- Session watcher guard ---------------------------------------------------------

/// Whether `path` is a transcript sync wrote and nobody has touched since: the
/// session watcher must not import it as a new external session.
pub async fn written_by_sync(db: &Db, path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    let size: Option<i64> = sqlx::query_scalar(
        "SELECT size FROM sync_files WHERE local_path = ? AND origin = 'remote' AND deleted = 0",
    )
    .bind(path.to_string_lossy().as_ref())
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    size.is_some_and(|s| s.max(0) as u64 == meta.len())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::sync::clock::Hlc;
    use crate::sync::op::OP_VERSION;
    use crate::sync::SCHEMA_VERSION;
    use async_trait::async_trait;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Mutex;

    pub struct TempDir(PathBuf);
    impl TempDir {
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    pub fn tempdir() -> TempDir {
        let p = std::env::temp_dir().join(format!("devdy-sync-files-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }

    /// In-memory relay blob store (sealing is the engine's job; not needed here).
    #[derive(Default)]
    pub struct MemBlobs {
        pub blobs: Mutex<HashMap<String, Vec<u8>>>,
        pub puts: Mutex<usize>,
        pub gets: Mutex<usize>,
    }

    #[async_trait]
    impl BlobIo for MemBlobs {
        async fn put(&self, plaintext: &[u8]) -> Result<String, String> {
            *self.puts.lock().unwrap() += 1;
            // Random like a sealed blob: same plaintext, different sha.
            let sha = format!("{}-{}", hex::encode(Sha256::digest(plaintext)), uuid::Uuid::new_v4().simple());
            self.blobs.lock().unwrap().insert(sha.clone(), plaintext.to_vec());
            Ok(sha)
        }
        async fn get(&self, sha: &str) -> Result<Vec<u8>, String> {
            *self.gets.lock().unwrap() += 1;
            self.blobs.lock().unwrap().get(sha).cloned().ok_or_else(|| "blob_not_found".into())
        }
    }

    pub async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    fn ctx(home: &Path) -> PathCtx {
        PathCtx::with_home(home).with_app_data(home.join("Library/Application Support/vn.papay.devdy"))
    }

    async fn outbox_files(db: &Db) -> Vec<(String, String)> {
        sqlx::query_as("SELECT pk, op FROM sync_outbox WHERE tbl = 'file' ORDER BY seq").fetch_all(db).await.unwrap()
    }

    fn upsert_op(pk: Value, row: Map<String, Value>, millis: u64, device: &str) -> Op {
        Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc: Hlc::new(millis, 0, device), tbl: TBL.into(), pk, kind: OpKind::Upsert, row: Some(row) }
    }

    /// What `pending` would send for `t` on device `db`.
    async fn op_for(db: &Db, t: &Tracked, millis: u64, device: &str) -> Op {
        let pk = file_pk(t.kind, &t.portable);
        let mut conn = db.acquire().await.unwrap();
        let (_, row) = pending_row(&mut conn, &pk, true).await.unwrap().unwrap();
        upsert_op(pk, row.unwrap(), millis, device)
    }

    async fn add_run(db: &Db, home: &Path, id: &str, status: &str) -> PathBuf {
        let project = home.join("w/p");
        fs::create_dir_all(project.join(".devdy/runs")).unwrap();
        sqlx::query("INSERT OR IGNORE INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
            .bind(project.to_string_lossy().as_ref())
            .execute(db)
            .await
            .unwrap();
        let log = project.join(format!(".devdy/runs/{id}.log"));
        sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, session_id, output_path, created_at) VALUES (?, 'p1', 'session', ?, 'claude', ?, ?, 'x')")
            .bind(id)
            .bind(status)
            .bind(format!("s-{id}"))
            .bind(log.to_string_lossy().as_ref())
            .execute(db)
            .await
            .unwrap();
        log
    }

    fn mb(n: usize, byte: u8) -> Vec<u8> {
        vec![byte; n]
    }

    #[tokio::test]
    async fn chunking_dedup_and_append_growth() {
        let home = tempdir();
        let db = mem_db().await;
        let paths = ctx(home.path());
        let log = add_run(&db, home.path(), "r1", "running").await;
        let mut body = mb(CHUNK_SIZE, b'a');
        body.extend(mb(CHUNK_SIZE / 2, b'b'));
        fs::write(&log, &body).unwrap();
        let blobs = MemBlobs::default();
        let mut st = ScanState::default();

        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 1);
        assert_eq!(*blobs.puts.lock().unwrap(), 2);
        let ob = outbox_files(&db).await;
        assert_eq!(ob.len(), 1);
        assert_eq!(ob[0].0, r#"{"kind":"run_log","path":"~/w/p/.devdy/runs/r1.log"}"#);

        // Unchanged: nothing read, nothing queued.
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 0);
        assert_eq!(*blobs.puts.lock().unwrap(), 2);

        // Append: the full first chunk is reused, only the grown tail goes up.
        body.extend(mb(CHUNK_SIZE, b'c'));
        fs::write(&log, &body).unwrap();
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 1);
        assert_eq!(*blobs.puts.lock().unwrap(), 4, "2 new chunks (last partial + new)");
        let mut conn = db.acquire().await.unwrap();
        let pk = file_pk(FileKind::RunLog, "~/w/p/.devdy/runs/r1.log");
        let (kind, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
        let row = row.unwrap();
        assert_eq!(kind, OpKind::Upsert);
        assert_eq!(row["size"], body.len() as u64);
        assert_eq!(row["chunks"].as_array().unwrap().len(), 3);
        assert_eq!(row["sha256"], hex::encode(Sha256::digest(&body)));
        drop(conn);

        // A touch without a content change re-reads but queues nothing.
        let f = fs::File::options().append(true).open(&log).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
        let before = outbox_files(&db).await.len();
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 0);
        assert_eq!(outbox_files(&db).await.len(), before);
        assert_eq!(*blobs.puts.lock().unwrap(), 4);
    }

    #[tokio::test]
    async fn quick_scan_covers_running_and_kicked_files_only() {
        let home = tempdir();
        let db = mem_db().await;
        let paths = ctx(home.path());
        let live = add_run(&db, home.path(), "live", "running").await;
        let done = add_run(&db, home.path(), "done", "done").await;
        fs::write(&live, b"1").unwrap();
        fs::write(&done, b"1").unwrap();
        let blobs = MemBlobs::default();
        let mut st = ScanState::default();
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 2);
        fs::write(&live, b"12").unwrap();
        fs::write(&done, b"12").unwrap();
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 1);
        kick(&done);
        assert!(has_kicks());
        assert_eq!(stage(&db, &paths, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 1);

        let t = Timing { quick: Duration::from_secs(5), full: Duration::from_secs(60) };
        let mut fresh = ScanState::default();
        assert_eq!(fresh.due(&t, false), Some(ScanMode::Full));
        assert_eq!(fresh.due(&t, false), None);
        assert_eq!(fresh.due(&t, true), Some(ScanMode::Quick));
    }

    #[tokio::test]
    async fn apply_writes_atomically_and_reuses_local_chunks() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let log = add_run(&db_a, ha.path(), "r1", "running").await;
        let mut body = mb(CHUNK_SIZE, b'x');
        body.extend(b"tail");
        fs::write(&log, &body).unwrap();
        let blobs = MemBlobs::default();
        let mut st = ScanState::default();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::RunLog, local: log.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Applied);
        let local_b = hb.path().join("w/p/.devdy/runs/r1.log");
        assert_eq!(fs::read(&local_b).unwrap(), body);
        let leftovers: Vec<_> = fs::read_dir(local_b.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no tmp file left behind");
        // Re-applying the same op is a no-op.
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Skipped);
        // Applied files are not queued back.
        assert!(outbox_files(&db_b).await.is_empty());

        // Growth: B only downloads the new chunk.
        body.extend(mb(CHUNK_SIZE, b'y'));
        fs::write(&log, &body).unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let op2 = op_for(&db_a, &t, 2_000, "dev-a").await;
        let gets = *blobs.gets.lock().unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op2).await.unwrap(), Outcome::Applied);
        assert_eq!(*blobs.gets.lock().unwrap() - gets, 2, "first chunk reused from disk");
        assert_eq!(fs::read(&local_b).unwrap(), body);

        // A corrupted blob is refused and leaves the file untouched.
        body.extend(b"more");
        fs::write(&log, &body).unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let op3 = op_for(&db_a, &t, 3_000, "dev-a").await;
        let last = op3.row.as_ref().unwrap()["chunks"].as_array().unwrap().last().unwrap().as_str().unwrap().to_string();
        blobs.blobs.lock().unwrap().insert(last, b"evil".to_vec());
        assert!(apply_op(&db_b, &pb, &blobs, &op3).await.is_err());
        assert_eq!(fs::read(&local_b).unwrap().len(), 2 * CHUNK_SIZE + 4);
    }

    #[tokio::test]
    async fn diverged_files_get_a_conflict_sibling() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();

        // Library file: B edited its copy after A's version was made.
        let rel = "Library/Application Support/vn.papay.devdy/rules/git.md";
        let (fa, fb) = (ha.path().join(rel), hb.path().join(rel));
        fs::create_dir_all(fa.parent().unwrap()).unwrap();
        fs::create_dir_all(fb.parent().unwrap()).unwrap();
        fs::write(&fa, "from a").unwrap();
        for (db, f) in [(&db_a, &fa), (&db_b, &fb)] {
            sqlx::query("INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('r', 'git', '', ?, 'x')")
                .bind(f.to_string_lossy().as_ref())
                .execute(db)
                .await
                .unwrap();
        }
        let mut st = ScanState::default();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: fa.clone(), portable: "@data/rules/git.md".into() };
        fs::write(&fb, "edited on b").unwrap();
        let op = op_for(&db_a, &t, 1_000, "dev-a").await; // older than B's edit
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "edited on b");
        assert_eq!(fs::read_to_string(fb.with_file_name("git.md.conflict-dev-a")).unwrap(), "from a");

        // A newer remote version replaces an older unseen local edit, which is
        // moved to the legacy backups.
        let far = wall_ms() + 60_000;
        let op = op_for(&db_a, &t, far, "dev-a").await;
        let r = apply_op(&db_b, &pb, &blobs, &op).await.unwrap();
        assert_eq!(r, Outcome::Applied);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "from a");

        // Append-only: both sides continued the transcript -> sibling; local
        // ahead of remote -> kept.
        let enc = crate::commands::sessions::encode_project_dir(&ha.path().join("w/p").to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, "l1\nl2-a\n").unwrap();
        let mut conn = db_a.acquire().await.unwrap();
        let content = upload_file(&mut conn, &blobs, &ta).await.ok().unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        assert_eq!(portable, "~/.claude/projects/~-w-p/s1.jsonl");
        save_record(&mut conn, FileKind::ClaudeTranscript, &portable, &ta, Some((&content, 0)), None, None, "local").await.unwrap();
        drop(conn);
        let tt = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone() };
        let op = op_for(&db_a, &tt, 1_000, "dev-a").await;
        let tb = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        fs::create_dir_all(tb.parent().unwrap()).unwrap();
        fs::write(&tb, "l1\nl2-b\n").unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-b\n");
        assert!(tb.with_file_name("s1.jsonl.conflict-dev-a").is_file());

        fs::write(&tb, "l1\nl2-a\nl3-b\n").unwrap();
        let op = op_for(&db_a, &tt, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Skipped);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-a\nl3-b\n");

        // Local behind remote (a strict prefix) is just updated.
        fs::write(&tb, "l1\n").unwrap();
        let op = op_for(&db_a, &tt, 3_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-a\n");
    }

    #[tokio::test]
    async fn transcript_path_is_reencoded_for_the_local_home() {
        let (ha, hb) = (PathBuf::from("/Users/alice"), PathBuf::from("/Users/bob"));
        let (pa, pb) = (ctx(&ha), ctx(&hb));
        let a_file = "/Users/alice/.claude/projects/-Users-alice-Workspace-devdy/0b1c.jsonl";
        let portable = to_portable(&pa, a_file);
        assert_eq!(portable, "~/.claude/projects/~-Workspace-devdy/0b1c.jsonl");
        let b_file = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        let expected = PathBuf::from("/Users/bob/.claude/projects")
            .join(crate::commands::sessions::encode_project_dir("/Users/bob/Workspace/devdy"))
            .join("0b1c.jsonl");
        assert_eq!(b_file, expected);
        // The runs row decodes to the same local file.
        let runs = crate::sync::schema::spec("runs").unwrap();
        let wire = crate::sync::codec::encode_row(&pa, runs, json!({"id": "r", "transcript_path": a_file}).as_object().unwrap().clone());
        let local = crate::sync::codec::decode_row(&pb, runs, &wire);
        assert_eq!(local["transcript_path"], b_file.to_string_lossy().as_ref());

        // Paths outside where a kind belongs are refused.
        for (kind, p) in [
            (FileKind::Library, "~/.ssh/id_rsa"),
            (FileKind::Library, "@data/skills/../../../.zshrc"),
            (FileKind::SshKey, "@data/skills/x/key"),
            (FileKind::RunLog, "~/.zshrc"),
            (FileKind::CodexRollout, "~/.codex/sessions/2026/x.jsonl"),
            (FileKind::ClaudeTranscript, "relative/projects/x.jsonl"),
        ] {
            assert!(localize(&pb, kind, p).is_err(), "{p}");
        }
        assert!(localize(&PathCtx::with_home(&hb), FileKind::Library, "@data/skills/x/SKILL.md").is_err());
        assert!(localize(&pb, FileKind::CodexRollout, "~/.codex/sessions/2026/10/30/rollout-2026-10-30T10-00-00-abc.jsonl").is_ok());
    }

    #[tokio::test]
    async fn watcher_guard_and_mirrored_size() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();
        let enc = crate::commands::sessions::encode_project_dir(&ha.path().join("w/p").to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s-r1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, "{}\n").unwrap();
        add_run(&db_a, ha.path(), "r1", "done").await;
        let mut st = ScanState::default();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        let t = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        add_run(&db_b, hb.path(), "r1", "done").await;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Applied);
        let tb = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        assert!(written_by_sync(&db_b, &tb).await, "sync-written transcript is ignored by the watcher");
        let mirrored: Option<i64> = sqlx::query_scalar("SELECT transcript_synced_size FROM runs WHERE id = 'r1'").fetch_one(&db_b).await.unwrap();
        assert_eq!(mirrored, Some(3));
        let ob: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db_b).await.unwrap();
        assert_eq!(ob, before, "machine-local bookkeeping is not captured");
        // Continued locally: the watcher takes over again.
        fs::write(&tb, "{}\n{}\n").unwrap();
        assert!(!written_by_sync(&db_b, &tb).await);
        // A's own scanned file is not "written by sync".
        assert!(!written_by_sync(&db_a, &ta).await);
    }

    #[tokio::test]
    async fn library_file_delete_round_trip() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();
        let dir_a = pa.app_data.as_ref().unwrap().join("skills/review");
        fs::create_dir_all(dir_a.join("references")).unwrap();
        fs::write(dir_a.join("SKILL.md"), "skill").unwrap();
        fs::write(dir_a.join("references/zz-sync-ref.md"), "ref").unwrap();
        fs::write(dir_a.join(".DS_Store"), "junk").unwrap();
        sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES ('s', 'review', '', 'claude', ?, 'x')")
            .bind(dir_a.to_string_lossy().as_ref())
            .execute(&db_a)
            .await
            .unwrap();
        let mut st = ScanState::default();
        assert_eq!(stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 2);
        let mut ops = Vec::new();
        for (pk, _) in outbox_files(&db_a).await {
            let pk: Value = serde_json::from_str(&pk).unwrap();
            let mut conn = db_a.acquire().await.unwrap();
            let (_, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
            ops.push(upsert_op(pk, row.unwrap(), 1_000, "dev-a"));
        }
        for op in &ops {
            assert_eq!(apply_op(&db_b, &pb, &blobs, op).await.unwrap(), Outcome::Applied);
        }
        let ref_b = pb.app_data.as_ref().unwrap().join("skills/review/references/zz-sync-ref.md");
        assert_eq!(fs::read_to_string(&ref_b).unwrap(), "ref");

        // B edits its copy and pushes it; when A's newer version wins, B's
        // authored version goes to the backups.
        let skill_b = pb.app_data.as_ref().unwrap().join("skills/review/SKILL.md");
        sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES ('s', 'review', '', 'claude', ?, 'x')")
            .bind(skill_b.parent().unwrap().to_string_lossy().as_ref())
            .execute(&db_b)
            .await
            .unwrap();
        fs::write(&skill_b, "zz-sync-b-version").unwrap();
        let mut st_b = ScanState::default();
        assert_eq!(stage(&db_b, &pb, &blobs, &mut st_b, ScanMode::Full, false).await.unwrap(), 1);
        fs::write(dir_a.join("SKILL.md"), "skill v2").unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: dir_a.join("SKILL.md"), portable: "@data/skills/review/SKILL.md".into() };
        let op = op_for(&db_a, &t, wall_ms() + 60_000, "dev-a").await;
        sqlx::query("DELETE FROM sync_outbox").execute(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&skill_b).unwrap(), "skill v2");
        let root = crate::commands::project_links::backup_root();
        let kept = walkdir::WalkDir::new(&root).into_iter().flatten().any(|e| {
            e.file_type().is_file() && fs::read_to_string(e.path()).is_ok_and(|c| c == "zz-sync-b-version")
        });
        assert!(kept, "b's own version is in the backups");

        // Removed on A (folder still there) -> Delete op -> moved away on B.
        fs::remove_file(dir_a.join("references/zz-sync-ref.md")).unwrap();
        sqlx::query("DELETE FROM sync_outbox").execute(&db_a).await.unwrap();
        assert_eq!(stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 1);
        let (pk, op) = outbox_files(&db_a).await.pop().unwrap();
        assert_eq!(op, "D");
        let pk: Value = serde_json::from_str(&pk).unwrap();
        let mut conn = db_a.acquire().await.unwrap();
        let (kind, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
        assert_eq!((kind, row), (OpKind::Delete, None));
        let del = Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc: Hlc::new(2_000, 0, "dev-a"), tbl: TBL.into(), pk, kind: OpKind::Delete, row: None };
        assert_eq!(apply_op(&db_b, &pb, &blobs, &del).await.unwrap(), Outcome::Applied);
        assert!(!ref_b.exists());
        let root = crate::commands::project_links::backup_root();
        assert!(walkdir::WalkDir::new(root).into_iter().flatten().any(|e| e.file_name() == "zz-sync-ref.md"));
    }
}
