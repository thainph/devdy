//! File sync (design §4, §9b; on-demand-sync.md §P2): library skill/rule
//! files, Devdy run logs, Claude transcripts, Codex rollouts and Devdy-managed
//! SSH keys.
//!
//! Local side ([`scan`]): tracked files are found from the synced rows
//! (skills, rules, runs, servers) when the other device asks for changes; a
//! file is only read when its size or mtime changed, and only queued (a
//! `file` op through `sync_outbox`, stamped into the change feed like a row
//! change) when its sha256 did. Nothing is uploaded: the op carries `{size,
//! sha256, mode}` and the content stays here until the other device reads
//! it ([`read_range`], `file_read`). A grown append-only file is hashed from
//! the SHA-256 state recorded after its last full chunk (`hash_state`), not
//! from the start.
//!
//! Remote side ([`apply_op`]): last-writer-wins by HLC (via `sync_rows`); the
//! content is read from the source device chunk by chunk ([`FileSource`],
//! [`CHUNK_WINDOW`] requests in flight), written to a `.part` file next to the
//! target (kept on failure, so the next try resumes after its whole chunks),
//! checked against the op's sha256 and renamed into place, or appended in
//! place when the incoming version of an append-only file extends the local
//! one. A local file with content sync never saw is not overwritten when it is
//! newer (or, for append-only files, unless it is a prefix of the incoming
//! version): the remote version goes to a `<name>.conflict-<device>` sibling
//! instead, and the local version keeps its place in the outbox. File IO and
//! hashing run on the blocking pool. Library files authored (or edited) here
//! are moved to the legacy backups before a remote version replaces them.
//! Files written here are recorded with origin = 'remote' so the session
//! watcher does not import them again. Transcripts and Codex rollouts get the
//! sender's cwd rewritten to this device's project path; the record then keeps
//! the synced sha as the base and the rewritten file's size/sha as
//! `local_size` / `local_sha256`.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
#[cfg(not(test))]
use std::sync::Mutex as StdMutex;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use sqlx::{Row, SqliteConnection};

use super::clock::{wall_ms, Hlc};
use super::codec::{expand_portable, map_in_project, portable_path, PathCtx};
#[cfg(test)]
use super::codec::to_portable;
use super::feed::{FileReadReq, FILE_CHANGED, FILE_GONE};
use super::op::{canonical_json, Op, OpKind};
use super::peer::CHUNK_WINDOW;
use crate::db::Db;

pub const TBL: &str = "file";
/// Content travels in pieces of this size (`peer::CHUNK_SIZE`); records keep
/// one sha256 per piece of the version they describe.
pub const CHUNK_SIZE: usize = super::peer::CHUNK_SIZE;
/// Bigger files are not synced (a transcript this large is pathological).
const MAX_FILE: u64 = 512 << 20;
/// Marker of the partial downloads written before the atomic rename.
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
    /// Session files: run logs and CLI transcripts (the bulk of the data).
    pub fn is_session(self) -> bool {
        matches!(self, FileKind::RunLog | FileKind::ClaudeTranscript | FileKind::CodexRollout)
    }

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
    pub fn append_only(self) -> bool {
        matches!(self, FileKind::RunLog | FileKind::ClaudeTranscript | FileKind::CodexRollout)
    }

    /// Files whose cwd is rewritten to this device's project path.
    fn rewrites_cwd(self) -> bool {
        matches!(self, FileKind::ClaudeTranscript | FileKind::CodexRollout)
    }

    /// The only modes a file of this kind is written with: library files 0644
    /// (0755 when the sender's copy was executable), keys 0600, the rest the
    /// default.
    fn mode(self, sent: Option<u32>) -> Option<u32> {
        match self {
            FileKind::SshKey => Some(0o600),
            FileKind::Library => Some(if sent.unwrap_or(0) & 0o111 != 0 { 0o755 } else { 0o644 }),
            _ => None,
        }
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

/// One `CHUNK_SIZE` piece of a recorded version (older records also carry
/// the relay blob sha of the piece, ignored now).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ChunkRef {
    /// sha256 of the piece.
    sha: String,
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
    /// Size / sha256 of the file on disk when sync rewrote it (transcript cwd),
    /// else `None`: the file is the synced content.
    local_size: Option<u64>,
    local_sha256: Option<String>,
    hash_state: Option<HashState>,
}

impl Record {
    /// Size of the recorded version as it is on disk here.
    fn disk_size(&self) -> u64 {
        self.local_size.unwrap_or(self.size)
    }

    fn disk_sha(&self) -> &str {
        self.local_sha256.as_deref().unwrap_or(&self.sha256)
    }

    /// The synced content's SHA-256 state after its full chunks, when it can
    /// be resumed for the file on disk (same inode).
    fn synced_mid(&self, ino: u64) -> Option<Mid> {
        let hs = self.hash_state.as_ref().filter(|h| h.ino != 0 && h.ino == ino)?;
        let mid = hs.synced?;
        let full = (mid.off / CHUNK_SIZE as u64) as usize;
        (mid.off % CHUNK_SIZE as u64 == 0 && mid.off <= self.size && full <= self.chunks.len()).then_some(mid)
    }
}

// ---- Resumable SHA-256 ---------------------------------------------------------

const SHA256_IV: [u32; 8] =
    [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
    0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
    0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
    0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
    0xc67178f2,
];

/// The SHA-256 compression function over one 64-byte block (FIPS 180-4),
/// which `sha2` only exposes behind a feature.
fn compress(h: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
        w[i] = u32::from_be_bytes(*word);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(SHA256_K[i]).wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (x, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
        *x = x.wrapping_add(v);
    }
}

/// SHA-256 state after the first `off` bytes of some content (`off` a
/// multiple of 64), so content that grew is hashed from there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Mid {
    off: u64,
    h: [u32; 8],
}

impl Default for Mid {
    fn default() -> Self {
        Mid { off: 0, h: SHA256_IV }
    }
}

impl Mid {
    /// Absorb the whole 64-byte blocks of `bytes` (the content from `off` on);
    /// returns how many bytes that was.
    fn absorb(&mut self, bytes: &[u8]) -> usize {
        let n = bytes.len() / 64 * 64;
        for block in bytes[..n].as_chunks::<64>().0 {
            compress(&mut self.h, block);
        }
        self.off += n as u64;
        n
    }

    /// Hex sha256 of the content absorbed so far followed by `rest`.
    fn finish(mut self, rest: &[u8]) -> String {
        let n = self.absorb(rest);
        let rest = &rest[n..];
        let bits = (self.off + rest.len() as u64) * 8;
        let mut last = Vec::with_capacity(128);
        last.extend_from_slice(rest);
        last.push(0x80);
        while last.len() % 64 != 56 {
            last.push(0);
        }
        last.extend_from_slice(&bits.to_be_bytes());
        self.absorb(&last);
        hex::encode(self.h.iter().flat_map(|w| w.to_be_bytes()).collect::<Vec<u8>>())
    }
}

/// `sync_files.hash_state`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct HashState {
    /// Inode of the file on disk the states were taken from (0 = unknown: a
    /// replaced file is re-read from the start).
    ino: u64,
    /// Synced content after its full chunks (`off` = full chunks × CHUNK_SIZE).
    synced: Option<Mid>,
    /// The file on disk, when it differs from the synced content (cwd rewrite).
    disk: Option<Mid>,
}

fn inode(meta: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.ino()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0
    }
}

/// Run blocking file IO / hashing off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(f).await.map_err(|e| e.to_string())
}

// ---- Scan state ----------------------------------------------------------------

/// What a scan keeps between runs.
#[derive(Default)]
pub struct ScanState {
    /// Codex thread id -> rollout, rebuilt on every scan (a Devdy-native
    /// Codex run does not record its rollout path).
    codex_index: std::sync::Arc<HashMap<String, PathBuf>>,
    /// This scan leaves run logs and CLI transcripts out (the asking device
    /// does not take sessions); the next scan without it picks them up.
    pub skip_sessions: bool,
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

#[cfg(test)]
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
    let portable = portable_path(paths, &local.to_string_lossy());
    out.push(Tracked { kind, local, portable });
}

/// Library files: every file of every skill folder, every rule file and the
/// Codex skill form of each rule. Only inside this device's app data dir.
async fn library_files(db: &Db, paths: &PathCtx, out: &mut Vec<Tracked>) -> Result<(), String> {
    if paths.app_data.is_none() {
        return Ok(());
    }
    let skills: Vec<String> = sqlx::query_scalar("SELECT source_path FROM skills").fetch_all(db).await.map_err(err)?;
    let rules: Vec<(String, String)> = sqlx::query_as("SELECT name, source_path FROM rules").fetch_all(db).await.map_err(err)?;
    let ctx = paths.clone();
    let files = blocking(move || {
        let mut files = Vec::new();
        for p in skills.into_iter().map(PathBuf::from) {
            if under_app_data(&ctx, &p) {
                walk_files(&p, &mut files);
            }
        }
        for (name, src) in rules {
            let p = PathBuf::from(src);
            if !under_app_data(&ctx, &p) {
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
        files
    })
    .await?;
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
    let keys = blocking(move || {
        rows.into_iter().map(PathBuf::from).filter(|p| p.starts_with(&keys_root) && p.is_file()).collect::<Vec<_>>()
    })
    .await?;
    for p in keys {
        track(paths, FileKind::SshKey, p, out);
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

/// Run logs and CLI transcripts of every run.
async fn run_files(
    db: &Db,
    paths: &PathCtx,
    codex: std::sync::Arc<HashMap<String, PathBuf>>,
    out: &mut Vec<Tracked>,
) -> Result<(), String> {
    let sql = "SELECT r.id, r.engine, r.session_id, r.output_path, r.transcript_path, p.path AS project_path, \
                a.config_dir \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         LEFT JOIN claude_accounts a ON a.id = r.claude_account_id";
    let rows: Vec<RunFileRow> = sqlx::query_as(sql).fetch_all(db).await.map_err(err)?;
    let ctx = paths.clone();
    let found = blocking(move || {
        let mut out = Vec::new();
        for row in rows {
            run_row_files(&ctx, row, &codex, &mut out);
        }
        out
    })
    .await?;
    out.extend(found);
    Ok(())
}

type RunFileRow = (String, Option<String>, Option<String>, Option<String>, Option<String>, String, Option<String>);

fn run_row_files(paths: &PathCtx, row: RunFileRow, codex: &HashMap<String, PathBuf>, out: &mut Vec<Tracked>) {
    let (id, engine, session, output, transcript, project, config_dir) = row;
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

/// The run log / transcript `path` is, if it belongs to a run (matched by
/// recorded path, log name or session id). A Codex rollout joins the index.
async fn kicked_run_file(
    conn: &mut SqliteConnection,
    paths: &PathCtx,
    state: &mut ScanState,
    path: &Path,
) -> Result<Vec<Tracked>, String> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else { return Ok(Vec::new()) };
    let stem = name.strip_suffix(".log").or_else(|| name.strip_suffix(".jsonl")).unwrap_or(name);
    let thread = stem.strip_prefix("rollout-").and_then(|s| s.get(20..));
    let local = path.to_string_lossy();
    let rows: Vec<RunFileRow> = sqlx::query_as(
        "SELECT r.id, r.engine, r.session_id, r.output_path, r.transcript_path, p.path AS project_path, \
                a.config_dir \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         LEFT JOIN claude_accounts a ON a.id = r.claude_account_id \
         WHERE r.output_path = ?1 OR r.transcript_path = ?1 OR r.id = ?2 OR r.session_id IN (?2, ?3)",
    )
    .bind(local.as_ref())
    .bind(stem)
    .bind(thread.unwrap_or(stem))
    .fetch_all(&mut *conn)
    .await
    .map_err(err)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(id) = thread {
        let mut index = (*state.codex_index).clone();
        index.insert(id.to_string(), path.to_path_buf());
        state.codex_index = std::sync::Arc::new(index);
    }
    let (ctx, codex, want) = (paths.clone(), state.codex_index.clone(), path.to_path_buf());
    blocking(move || {
        let mut out = Vec::new();
        for row in rows {
            run_row_files(&ctx, row, &codex, &mut out);
        }
        out.retain(|t| t.local == want);
        out
    })
    .await
}

/// Every file this device currently tracks.
async fn collect(db: &Db, paths: &PathCtx, state: &mut ScanState, with_keys: bool) -> Result<Vec<Tracked>, String> {
    let mut out = Vec::new();
    let home = paths.home.clone();
    state.codex_index = std::sync::Arc::new(blocking(move || codex_index(&home)).await?);
    library_files(db, paths, &mut out).await?;
    if with_keys {
        ssh_key_files(db, paths, &mut out).await?;
    }
    if !state.skip_sessions {
        run_files(db, paths, state.codex_index.clone(), &mut out).await?;
    }
    Ok(dedup_tracked(out))
}

/// One entry per file.
fn dedup_tracked(mut list: Vec<Tracked>) -> Vec<Tracked> {
    list.sort();
    list.dedup();
    list
}

// ---- Records -----------------------------------------------------------------

async fn load_record(conn: &mut SqliteConnection, kind: FileKind, portable: &str) -> Result<Option<Record>, String> {
    let row = sqlx::query(
        "SELECT local_path, size, sha256, chunks_json, mtime, mode, deleted, origin, local_size, local_sha256, hash_state \
         FROM sync_files WHERE kind = ? AND path = ?",
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
        local_size: r.get::<Option<i64>, _>("local_size").map(|n| n.max(0) as u64),
        local_sha256: r.get("local_sha256"),
        hash_state: r.get::<Option<String>, _>("hash_state").and_then(|s| serde_json::from_str(&s).ok()),
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
           origin = excluded.origin, local_size = NULL, local_sha256 = NULL, hash_state = NULL",
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

/// Record that the file on disk is a rewrite of the synced content (after
/// [`save_record`], which clears it).
async fn set_local_view(conn: &mut SqliteConnection, kind: FileKind, portable: &str, size: u64, sha: &str) -> Result<(), String> {
    sqlx::query("UPDATE sync_files SET local_size = ?, local_sha256 = ? WHERE kind = ? AND path = ?")
        .bind(size as i64)
        .bind(sha)
        .bind(kind.as_str())
        .bind(portable)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

async fn set_hash_state(conn: &mut SqliteConnection, kind: FileKind, portable: &str, hs: &HashState) -> Result<(), String> {
    sqlx::query("UPDATE sync_files SET hash_state = ? WHERE kind = ? AND path = ?")
        .bind(serde_json::to_string(hs).map_err(|e| e.to_string())?)
        .bind(kind.as_str())
        .bind(portable)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(())
}

/// Queue a file op.
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

/// A per-id app-data folder (`old_seg` = `/<dir>/<old id>`) is being renamed
/// to `new_seg` (a re-keyed row, `bootstrap::run_dir_moves`): its file
/// records follow, so the next scan finds them unchanged instead of hashing
/// everything again, and are queued under their new paths (the other device
/// only knows them under the old ones). A record whose new path is already
/// tracked is dropped.
pub async fn move_dir_records(conn: &mut SqliteConnection, old_seg: &str, new_seg: &str) -> Result<(), String> {
    use super::bootstrap::{moved_path_sql, under_dir_sql};
    let (in_path, in_local) = (under_dir_sql("path"), under_dir_sql("local_path"));
    let sql = format!(
        "UPDATE OR IGNORE sync_files SET path = {}, local_path = CASE WHEN {in_local} THEN {} ELSE local_path END WHERE {in_path}",
        moved_path_sql("path"),
        moved_path_sql("local_path"),
    );
    sqlx::query(&sql).bind(old_seg).bind(new_seg).execute(&mut *conn).await.map_err(err)?;
    sqlx::query(&format!("DELETE FROM sync_files WHERE {in_path}")).bind(old_seg).execute(&mut *conn).await.map_err(err)?;
    let sql = format!("UPDATE sync_files SET local_path = {} WHERE {in_local}", moved_path_sql("local_path"));
    sqlx::query(&sql).bind(old_seg).bind(new_seg).execute(&mut *conn).await.map_err(err)?;
    // `?1` is the new folder now.
    let moved: Vec<(String, String)> = sqlx::query_as(&format!("SELECT kind, path FROM sync_files WHERE deleted = 0 AND {in_path}"))
        .bind(new_seg)
        .fetch_all(&mut *conn)
        .await
        .map_err(err)?;
    for (kind, path) in moved {
        if let Some(kind) = FileKind::parse(&kind) {
            queue(conn, kind, &path, "U").await?;
        }
    }
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

/// Up to `len` bytes of `path` from `at`.
fn read_at(path: &Path, at: u64, len: usize) -> std::io::Result<Vec<u8>> {
    let mut f = fs::File::open(path)?;
    f.seek(SeekFrom::Start(at))?;
    let mut buf = vec![0u8; len];
    let n = read_full(&mut f, &mut buf)?;
    buf.truncate(n);
    Ok(buf)
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Where [`hash_file`] starts: the full chunks before `mid.off` are the
/// recorded ones.
struct Resume {
    mid: Mid,
    chunks: Vec<ChunkRef>,
}

/// Hash `path` in `CHUNK_SIZE` chunks (from `resume` when given). Returns the
/// content and the SHA-256 state after its full chunks.
async fn hash_file(path: &Path, resume: Option<Resume>) -> Result<(Content, Mid), String> {
    let p = path.to_path_buf();
    blocking(move || -> Result<(Content, Mid), String> {
        let local = |e: std::io::Error| format!("{}: {e}", p.display());
        let (mut mid, mut chunks) = match resume {
            Some(r) => (r.mid, r.chunks),
            None => (Mid::default(), Vec::new()),
        };
        let mut f = fs::File::open(&p).map_err(local)?;
        f.seek(SeekFrom::Start(mid.off)).map_err(local)?;
        let mut buf = vec![0u8; CHUNK_SIZE];
        loop {
            let n = read_full(&mut f, &mut buf).map_err(local)?;
            if n == 0 {
                return Ok((Content { size: mid.off, sha256: mid.finish(&[]), chunks }, mid));
            }
            chunks.push(ChunkRef { sha: sha_hex(&buf[..n]), len: n as u64 });
            if n < CHUNK_SIZE {
                let size = mid.off + n as u64;
                return Ok((Content { size, sha256: mid.finish(&buf[..n]), chunks }, mid));
            }
            mid.absorb(&buf);
        }
    })
    .await?
}

/// Where hashing a grown append-only file can resume: after the recorded full
/// chunks, if the file still starts with them (same inode, the last recorded
/// full chunk and the recorded partial tail read back unchanged).
fn resume_point(kind: FileKind, rec: Option<&Record>, path: &Path, meta: &fs::Metadata) -> Option<Resume> {
    let r = rec.filter(|r| kind.append_only() && !r.deleted && r.local_size.is_none())?;
    if r.local_path != path.to_string_lossy() || meta.len() < r.size {
        return None;
    }
    let mid = r.synced_mid(inode(meta))?;
    let full = (mid.off / CHUNK_SIZE as u64) as usize;
    if full > 0 {
        let last = &r.chunks[full - 1];
        let bytes = read_at(path, mid.off - CHUNK_SIZE as u64, CHUNK_SIZE).ok()?;
        if sha_hex(&bytes) != last.sha {
            return None;
        }
    }
    if r.size > mid.off {
        let tail = r.chunks.get(full)?;
        let bytes = read_at(path, mid.off, (r.size - mid.off) as usize).ok()?;
        if bytes.len() as u64 != tail.len || sha_hex(&bytes) != tail.sha {
            return None;
        }
    }
    Some(Resume { mid, chunks: r.chunks[..full].to_vec() })
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

/// Scan every tracked file (and the kicked ones) and queue `file` ops for the
/// changed ones. Returns how many ops were queued.
pub async fn scan(db: &Db, paths: &PathCtx, state: &mut ScanState, with_keys: bool) -> Result<usize, String> {
    // Kicks are all session files: keep them for a scan that takes those.
    let kicked = if state.skip_sessions { Default::default() } else { take_kicks() };
    let mut tracked = collect(db, paths, state, with_keys).await?;
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
            } else {
                // Not tracked yet (a new Codex rollout the index has not
                // seen, a run whose row points elsewhere): find its run.
                tracked.extend(kicked_run_file(&mut conn, paths, state, p).await?);
            }
        }
        tracked = dedup_tracked(tracked);
    }
    let mut queued = 0;
    for t in &tracked {
        match scan_one(db, t).await {
            Ok(changed) => queued += changed as usize,
            Err(e) => tracing::debug!(event = "sync_file_skipped", error = %e),
        }
    }
    queued += scan_deletions(db, with_keys).await?;
    Ok(queued)
}

/// Record `t` and queue it when its content changed. Errors concern this file
/// only (vanished, unreadable...).
async fn scan_one(db: &Db, t: &Tracked) -> Result<bool, String> {
    let meta = match tokio::fs::metadata(&t.local).await {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(false),
        Err(e) => return Err(format!("{}: {e}", t.local.display())),
    };
    if meta.len() > MAX_FILE {
        return Err(format!("{}: too large to sync", t.local.display()));
    }
    let mtime = mtime_ms(&meta);
    let mut conn = db.acquire().await.map_err(err)?;
    let rec = load_record(&mut conn, t.kind, &t.portable).await?;
    let local_str = t.local.to_string_lossy();
    if let Some(r) = &rec {
        if !r.deleted && r.disk_size() == meta.len() && r.mtime == mtime && r.local_path == local_str {
            return Ok(false);
        }
    }
    let resume = {
        let (kind, rec, path, meta) = (t.kind, rec.clone(), t.local.clone(), meta.clone());
        blocking(move || resume_point(kind, rec.as_ref(), &path, &meta)).await?
    };
    let (content, mid) = hash_file(&t.local, resume).await?;
    let mode = t.kind.mode(file_mode(&meta));
    // A rewritten transcript compares by what sync wrote to disk: identical =
    // not a local change, even though it differs from the synced content.
    let unchanged = rec.as_ref().is_some_and(|r| !r.deleted && r.disk_sha() == content.sha256);
    let mut tx = sqlx::Connection::begin(&mut *conn).await.map_err(err)?;
    if unchanged {
        // Touched but identical (or moved): remember the new stat, keep origin.
        sqlx::query("UPDATE sync_files SET local_path = ?, mtime = ? WHERE kind = ? AND path = ?")
            .bind(local_str.as_ref())
            .bind(mtime)
            .bind(t.kind.as_str())
            .bind(&t.portable)
            .execute(&mut *tx)
            .await
            .map_err(err)?;
    } else {
        save_record(&mut tx, t.kind, &t.portable, &t.local, Some((&content, mtime)), mode, None, "local").await?;
        let hs = HashState { ino: inode(&meta), synced: Some(mid), disk: None };
        set_hash_state(&mut tx, t.kind, &t.portable, &hs).await?;
        queue(&mut tx, t.kind, &t.portable, "U").await?;
    }
    tx.commit().await.map_err(err)?;
    Ok(!unchanged)
}

/// Library files / keys removed on their own (their folder is still there)
/// become Delete ops; for anything else the row op carries the removal and
/// the record is simply forgotten.
async fn scan_deletions(db: &Db, with_keys: bool) -> Result<usize, String> {
    let rows: Vec<(String, String, String)> =
        sqlx::query_as("SELECT kind, path, local_path FROM sync_files WHERE deleted = 0 AND kind IN ('library', 'ssh_key')")
            .fetch_all(db)
            .await
            .map_err(err)?;
    // (kind, portable, local, folder still there) of the files gone from disk.
    let gone = blocking(move || {
        rows.into_iter()
            .filter_map(|(kind, portable, local)| {
                let kind = FileKind::parse(&kind)?;
                if kind == FileKind::SshKey && !with_keys {
                    return None;
                }
                let p = PathBuf::from(&local);
                if fs::symlink_metadata(&p).is_ok() {
                    return None;
                }
                let folder = p.parent().is_some_and(Path::is_dir);
                Some((kind, portable, p, folder))
            })
            .collect::<Vec<_>>()
    })
    .await?;
    let mut queued = 0;
    for (kind, portable, p, folder) in gone {
        let p = p.as_path();
        let mut tx = db.begin().await.map_err(err)?;
        if folder {
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
    // The synced content: what the other device gets (and already has when
    // this version came from it).
    let row = json!({ "size": rec.size, "sha256": rec.sha256, "mode": rec.mode });
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
    let bad = || format!("cannot place {} file {portable} on this device", kind.as_str());
    // `~//x` / `@data//x` would expand to `/x`.
    if portable.contains("//") {
        return Err(bad());
    }
    let p = PathBuf::from(match kind {
        FileKind::RunLog => map_in_project(paths, portable),
        _ => expand_portable(paths, portable),
    });
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
            // `<config dir>/projects/<project folder>/.../<session>.jsonl`, the
            // config dir being `~/.claude` or a Claude account's.
            let home = (!paths.home.as_os_str().is_empty()).then(|| paths.home.join(".claude"));
            p.extension().is_some_and(|e| e == "jsonl")
                && home.iter().chain(&paths.claude_dirs).any(|root| {
                    p.strip_prefix(root.join("projects")).is_ok_and(|rel| rel.components().count() >= 2)
                })
        }
        FileKind::CodexRollout => p.starts_with(paths.home.join(".codex").join("sessions")) && name.starts_with("rollout-"),
    };
    if ok {
        Ok(p)
    } else {
        Err(bad())
    }
}

#[derive(Debug, Clone)]
struct Incoming {
    size: u64,
    sha256: String,
    mode: Option<u32>,
}

fn parse_row(row: &Map<String, Value>) -> Result<Incoming, String> {
    let size = row.get("size").and_then(Value::as_u64).ok_or("file op without size")?;
    let sha256 = row.get("sha256").and_then(Value::as_str).ok_or("file op without sha256")?.to_string();
    if size > MAX_FILE || sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("malformed file op".into());
    }
    let mode = row.get("mode").and_then(Value::as_u64).map(|m| (m as u32) & 0o777);
    Ok(Incoming { size, sha256, mode })
}

/// Where the content of a remote `file` op comes from: the device that
/// served the op (`file_read` requests), or a fake in tests.
// `#[async_trait]` marks the boxed futures `#[must_use]` a second time.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait FileSource: Send + Sync {
    /// Bytes `[offset, offset + len)` of the version `(kind, path, sha256,
    /// size)` on the source. [`FILE_CHANGED`] / [`FILE_GONE`]: that version
    /// is no longer there (a newer op follows); anything else: retry later.
    async fn read(&self, req: FileReadReq) -> Result<Vec<u8>, String>;
}

/// The incoming version from `from` (a multiple of `CHUNK_SIZE`) to its end,
/// piece by piece in order, with [`CHUNK_WINDOW`] requests in flight.
fn fetch_chunks<'a>(
    src: &'a dyn FileSource,
    kind: FileKind,
    portable: &'a str,
    inc: &'a Incoming,
    from: u64,
) -> BoxStream<'a, Result<Vec<u8>, String>> {
    let size = inc.size;
    stream::iter((from..size).step_by(CHUNK_SIZE))
        .map(move |offset| {
            let len = (size - offset).min(CHUNK_SIZE as u64);
            let req = FileReadReq {
                kind: kind.as_str().to_string(),
                path: portable.to_string(),
                sha256: inc.sha256.clone(),
                size,
                offset,
                len,
            };
            async move {
                let bytes = src.read(req).await?;
                if bytes.len() as u64 != len {
                    return Err(format!("wrong chunk size at {offset}"));
                }
                Ok(bytes)
            }
        })
        .buffered(CHUNK_WINDOW)
        .boxed()
}

/// Whether an apply error means the op's version is gone on the source: the
/// op is dropped (the newer one arrives with the next sync), not retried.
pub fn is_superseded(e: &str) -> bool {
    e == FILE_CHANGED || e == FILE_GONE
}

/// Apply one remote `file` op, reading its content from `src`. Errors mean
/// "retry later" (the caller defers), except [`is_superseded`] ones.
/// `with_keys`: local secrets sync is on (SSH keys are only written then).
pub async fn apply_op(db: &Db, paths: &PathCtx, src: &dyn FileSource, op: &Op, with_keys: bool) -> Result<Outcome, String> {
    let (kind, portable) = parse_pk(&op.pk)?;
    if kind == FileKind::SshKey && !with_keys {
        return Ok(Outcome::Skipped);
    }
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
        // Our newer local version wins when the other device takes it.
        return Ok(Outcome::Skipped);
    }
    let path = localize(paths, kind, &portable)?;
    let rec = load_record(&mut conn, kind, &portable).await?;
    drop(conn);

    let outcome = match op.kind {
        OpKind::Upsert => {
            let row = op.row.as_ref().ok_or("upsert without row")?;
            let roots = if kind.rewrites_cwd() { project_roots(db, paths).await? } else { Vec::new() };
            write_remote(db, src, kind, &portable, &path, rec, &parse_row(row)?, op, &roots).await?
        }
        OpKind::Delete => remove_remote(db, kind, &portable, &path, rec).await?,
    };

    let mut tx = db.begin().await.map_err(err)?;
    // Whatever local change was pending lost (or survives as a conflict copy),
    // except a diverged append-only file: the local version stays in place
    // and must still be offered to the other device.
    let local_lost = match outcome {
        Outcome::Applied => true,
        Outcome::Conflict => !kind.append_only(),
        Outcome::Skipped => false,
    };
    if local_lost {
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
    fs::metadata(path).is_ok_and(|m| m.len() == r.disk_size() && mtime_ms(&m) == r.mtime)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Placement {
    /// Replace the file.
    Replace,
    /// Replace, but first move the unseen local version to the backups.
    BackupThenReplace,
    /// Append-only file with content sync never saw: replaced only when it is
    /// a prefix of the incoming version, kept when the incoming version is a
    /// prefix of it, else the incoming version goes next to it.
    Extend,
    /// Keep the local file, write the remote one next to it.
    Sibling,
    /// Keep the local file, drop the remote version (local is ahead).
    Keep,
    /// Already identical.
    Same,
}

fn decide(kind: FileKind, path: &Path, rec: Option<&Record>, inc: &Incoming, op_millis: u64) -> Placement {
    let Ok(meta) = fs::metadata(path) else { return Placement::Replace };
    if rec.is_some_and(|r| r.sha256 == inc.sha256 && r.local_sha256.is_some()) && matches_record(path, rec) {
        return Placement::Same;
    }
    if meta.len() == inc.size && sha_of_prefix(path, inc.size).as_deref() == Some(inc.sha256.as_str()) {
        return Placement::Same;
    }
    if kind.append_only() {
        // Exactly what sync wrote here: nothing local to lose.
        if matches_record(path, rec) && rec.is_some_and(|r| r.origin == "remote") {
            return Placement::Replace;
        }
        if meta.len() > inc.size && sha_of_prefix(path, inc.size).as_deref() == Some(inc.sha256.as_str()) {
            return Placement::Keep;
        }
        return Placement::Extend;
    }
    if matches_record(path, rec) {
        // A library version authored here is kept in the backups when a
        // remote one wins; one that came from sync exists elsewhere anyway.
        let authored_here = rec.is_some_and(|r| r.origin == "local");
        return if kind == FileKind::Library && authored_here { Placement::BackupThenReplace } else { Placement::Replace };
    }
    // The local file holds content sync never saw.
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

/// Where the incoming version `sha256` of `path` is downloaded: named after
/// the version, so an interrupted download is resumed by the next try (and
/// never mixed with another version's bytes).
fn part_path(path: &Path, sha256: &str) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tag = sha256.get(..16).unwrap_or(sha256);
    path.with_file_name(format!(".{name}{TMP_MARK}{tag}.part"))
}

/// Remove the partial downloads of `path` (other versions it never finished).
fn clear_parts(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else { return };
    let prefix = format!(".{}{TMP_MARK}", name.to_string_lossy());
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = entry.file_name().to_string_lossy().into_owned();
        if n.starts_with(&prefix) && n.ends_with(".part") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Mismatch between the downloaded bytes and the op (the source's file
/// changed under the request, or a broken source).
fn content_mismatch() -> String {
    FILE_CHANGED.to_string()
}

/// Download the incoming version into `part`, after the whole chunks an
/// earlier, interrupted try left there. Returns the content and the SHA-256
/// state after its full chunks. A version that does not match the op removes
/// the part file and fails with [`FILE_CHANGED`].
async fn assemble(
    src: &dyn FileSource,
    kind: FileKind,
    portable: &str,
    inc: &Incoming,
    part: &Path,
) -> Result<(Content, Mid), String> {
    let io = |e: std::io::Error| format!("{}: {e}", part.display());
    let (p, size) = (part.to_path_buf(), inc.size);
    let (f, mut mid, mut chunks) = blocking(move || -> std::io::Result<_> {
        let mut f = fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&p)?;
        let keep = f.metadata()?.len().min(size) / CHUNK_SIZE as u64 * CHUNK_SIZE as u64;
        f.set_len(keep)?;
        f.seek(SeekFrom::Start(0))?;
        let (mut mid, mut chunks) = (Mid::default(), Vec::new());
        let mut buf = vec![0u8; CHUNK_SIZE];
        while mid.off < keep {
            f.read_exact(&mut buf)?;
            chunks.push(ChunkRef { sha: sha_hex(&buf), len: CHUNK_SIZE as u64 });
            mid.absorb(&buf);
        }
        f.seek(SeekFrom::End(0))?;
        Ok((f, mid, chunks))
    })
    .await?
    .map_err(io)?;
    if mid.off > 0 {
        tracing::debug!(event = "sync_file_resumed", kind = kind.as_str(), from = mid.off);
    }
    let mut out = Some(f);
    let mut written = mid.off;
    let mut sha256 = None;
    let mut pieces = fetch_chunks(src, kind, portable, inc, mid.off);
    while let Some(piece) = pieces.next().await {
        let bytes = piece?;
        let mut f = out.take().ok_or("file closed")?;
        let len = bytes.len() as u64;
        let last = written + len == size;
        let (f, sha, next, whole) = blocking(move || {
            let r = f.write_all(&bytes);
            let sha = sha_hex(&bytes);
            let mut next = mid;
            let whole = if bytes.len() == CHUNK_SIZE {
                next.absorb(&bytes);
                last.then(|| next.finish(&[]))
            } else {
                Some(mid.finish(&bytes))
            };
            r.map(|_| (f, sha, next, whole))
        })
        .await?
        .map_err(io)?;
        out = Some(f);
        mid = next;
        written += len;
        sha256 = whole;
        chunks.push(ChunkRef { sha, len });
    }
    drop(pieces);
    let f = out.take().ok_or("file closed")?;
    blocking(move || f.sync_all()).await?.map_err(io)?;
    // Nothing fetched: the part held the whole (chunk-aligned) version.
    let sha256 = sha256.unwrap_or_else(|| mid.finish(&[]));
    if written != inc.size || sha256 != inc.sha256 {
        let _ = fs::remove_file(part);
        return Err(content_mismatch());
    }
    Ok((Content { size: written, sha256, chunks }, mid))
}

/// The new bytes of an append-only file whose incoming version extends the
/// version recorded here, appended in place (no full rewrite, no re-download
/// of the chunks it already has). `None` = not applicable: take the full path.
#[allow(clippy::too_many_arguments)]
async fn append_in_place(
    db: &Db,
    src: &dyn FileSource,
    kind: FileKind,
    portable: &str,
    path: &Path,
    rec: Option<&Record>,
    inc: &Incoming,
    op: &Op,
    roots: &[(String, String)],
) -> Result<Option<Outcome>, String> {
    let Some(r) = rec.filter(|r| kind.append_only() && inc.size > r.size).cloned() else { return Ok(None) };
    let (p, rr) = (path.to_path_buf(), r.clone());
    let probe = blocking(move || {
        let meta = fs::metadata(&p).ok()?;
        if !matches_record(&p, Some(&rr)) {
            return None;
        }
        // Line-wise cwd rewrite: only safe to rewrite the appended part on its
        // own when the file ends at a line break.
        let ends_line = meta.len() == 0 || read_at(&p, meta.len() - 1, 1).ok()?.as_slice() == b"\n";
        Some((inode(&meta), ends_line))
    })
    .await?;
    let Some((ino, ends_line)) = probe else { return Ok(None) };
    let Some(synced) = r.synced_mid(ino) else { return Ok(None) };
    if r.local_sha256.is_some() && r.hash_state.as_ref().is_none_or(|h| h.disk.is_none()) {
        return Ok(None);
    }
    let rewrite = kind.rewrites_cwd() && !roots.is_empty();
    if rewrite && !ends_line {
        return Ok(None);
    }
    let full = (synced.off / CHUNK_SIZE as u64) as usize;
    if r.chunks.len() < full {
        return Ok(None);
    }
    // From our last full chunk on (append-only sources serve it from their
    // possibly longer current file); the rest is ours already.
    let mut data = Vec::with_capacity((inc.size - synced.off) as usize);
    let mut pieces = fetch_chunks(src, kind, portable, inc, synced.off);
    while let Some(piece) = pieces.next().await {
        data.extend_from_slice(&piece?);
    }
    drop(pieces);
    let kept = (r.size - synced.off) as usize;
    if kept > 0 && r.chunks.get(full).is_none_or(|c| c.sha != sha_hex(&data[..kept])) {
        // The incoming version does not extend ours after all.
        return Ok(None);
    }
    let roots = roots.to_vec();
    let (p, disk_mid, old_disk) = (path.to_path_buf(), r.hash_state.as_ref().and_then(|h| h.disk).unwrap_or(synced), r.disk_size());
    let prepared = blocking(move || -> Result<_, String> {
        let sha256 = synced.finish(&data);
        let mut next_synced = synced;
        next_synced.absorb(&data[..data.len() / CHUNK_SIZE * CHUNK_SIZE]);
        let mut chunk_refs = Vec::new();
        for piece in data.chunks(CHUNK_SIZE) {
            chunk_refs.push((sha_hex(piece), piece.len() as u64));
        }
        let appended = &data[kept..];
        let rewritten = if rewrite { rewrite_cwds(appended, &roots) } else { None };
        let changed = rewritten.is_some();
        let disk_append = rewritten.unwrap_or_else(|| appended.to_vec());
        // The disk view: its recorded state + the bytes after it on disk + the new ones.
        let mut disk_bytes = read_at(&p, disk_mid.off, (old_disk - disk_mid.off) as usize).map_err(|e| e.to_string())?;
        if disk_bytes.len() as u64 != old_disk - disk_mid.off {
            return Err(format!("{}: changed while reading", p.display()));
        }
        disk_bytes.extend_from_slice(&disk_append);
        let disk_sha = disk_mid.finish(&disk_bytes);
        let mut next_disk = disk_mid;
        next_disk.absorb(&disk_bytes);
        Ok((sha256, next_synced, chunk_refs, disk_append, changed, disk_sha, next_disk))
    })
    .await??;
    let (sha256, next_synced, refs, disk_append, rewritten, disk_sha, next_disk) = prepared;
    if sha256 != inc.sha256 {
        // Our full chunks are not the start of the incoming version: take
        // the full path (which decides between replacing and a sibling).
        return Ok(None);
    }
    let mut chunks: Vec<ChunkRef> = r.chunks[..full].to_vec();
    chunks.extend(refs.into_iter().map(|(sha, len)| ChunkRef { sha, len }));
    let content = Content { size: inc.size, sha256, chunks };
    let new_disk = old_disk + disk_append.len() as u64;
    let local_view = r.local_sha256.is_some() || rewritten;
    let hs = HashState { ino, synced: Some(next_synced), disk: local_view.then_some(next_disk) };
    // Recorded before the write so the session watcher, woken by it, already
    // knows this version came from sync.
    let mut conn = db.acquire().await.map_err(err)?;
    save_record(&mut conn, kind, portable, path, Some((&content, 0)), None, Some(&op.hlc), "remote").await?;
    if local_view {
        set_local_view(&mut conn, kind, portable, new_disk, &disk_sha).await?;
    }
    set_hash_state(&mut conn, kind, portable, &hs).await?;
    let p = path.to_path_buf();
    let written = blocking(move || -> std::io::Result<i64> {
        let mut f = fs::OpenOptions::new().append(true).open(&p)?;
        if let Err(e) = f.write_all(&disk_append).and_then(|_| f.sync_data()) {
            let _ = f.set_len(old_disk);
            return Err(e);
        }
        Ok(mtime_ms(&f.metadata()?))
    })
    .await?;
    let mtime = match written {
        Ok(m) => m,
        Err(e) => {
            // Back to the version on disk: forget the record (re-read next scan).
            let _ = sqlx::query("DELETE FROM sync_files WHERE kind = ? AND path = ?")
                .bind(kind.as_str())
                .bind(portable)
                .execute(&mut *conn)
                .await;
            return Err(format!("{}: {e}", path.display()));
        }
    };
    sqlx::query("UPDATE sync_files SET mtime = ? WHERE kind = ? AND path = ?")
        .bind(mtime)
        .bind(kind.as_str())
        .bind(portable)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    if kind.rewrites_cwd() {
        mark_transcript_mirrored(&mut conn, path, new_disk).await?;
    }
    Ok(Some(Outcome::Applied))
}

#[allow(clippy::too_many_arguments)]
async fn write_remote(
    db: &Db,
    src: &dyn FileSource,
    kind: FileKind,
    portable: &str,
    path: &Path,
    rec: Option<Record>,
    inc: &Incoming,
    op: &Op,
    roots: &[(String, String)],
) -> Result<Outcome, String> {
    let mut placement = {
        let (p, r, i, millis) = (path.to_path_buf(), rec.clone(), inc.clone(), op.hlc.millis);
        blocking(move || decide(kind, &p, r.as_ref(), &i, millis)).await?
    };
    let rec = rec.as_ref();
    let mode = kind.mode(inc.mode);
    if let Placement::Keep = placement {
        return Ok(Outcome::Skipped);
    }
    if let Placement::Same = placement {
        let mut conn = db.acquire().await.map_err(err)?;
        if let Some(r) = rec.filter(|r| r.local_sha256.is_some()) {
            // Already written (cwd-rewritten) by sync: only the version moves.
            sqlx::query("UPDATE sync_files SET hlc = ? WHERE kind = ? AND path = ?")
                .bind(op.hlc.to_string())
                .bind(kind.as_str())
                .bind(portable)
                .execute(&mut *conn)
                .await
                .map_err(err)?;
            debug_assert_eq!(r.sha256, inc.sha256);
            return Ok(Outcome::Applied);
        }
        let (p, i) = (path.to_path_buf(), inc.clone());
        let (meta, chunks, mid) = blocking(move || -> Result<_, String> {
            let meta = fs::metadata(&p).map_err(|e| e.to_string())?;
            let (chunks, mid) = chunk_refs_of(&p, &i)?;
            Ok((meta, chunks, mid))
        })
        .await??;
        let content = Content { size: inc.size, sha256: inc.sha256.clone(), chunks };
        save_record(&mut conn, kind, portable, path, Some((&content, mtime_ms(&meta))), mode, Some(&op.hlc), "remote").await?;
        set_hash_state(&mut conn, kind, portable, &HashState { ino: inode(&meta), synced: Some(mid), disk: None }).await?;
        return Ok(Outcome::Applied);
    }
    if matches!(placement, Placement::Replace | Placement::Extend) {
        if let Some(outcome) = append_in_place(db, src, kind, portable, path, rec, inc, op, roots).await? {
            return Ok(outcome);
        }
    }
    let parent = path.parent().ok_or("file without a parent folder")?.to_path_buf();
    let is_key = kind == FileKind::SshKey;
    blocking(move || -> Result<(), String> {
        fs::create_dir_all(&parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        if is_key {
            set_mode(&parent, 0o700);
        }
        Ok(())
    })
    .await??;
    // Kept when the transfer fails midway: the next try resumes after it.
    let tmp = part_path(path, &inc.sha256);
    let (content, mid) = match assemble(src, kind, portable, inc, &tmp).await {
        Ok(c) => c,
        Err(e) => {
            if is_superseded(&e) {
                let _ = fs::remove_file(&tmp);
            }
            return Err(e);
        }
    };
    let finish = |e: String| {
        let _ = fs::remove_file(&tmp);
        e
    };
    // The sender's cwd -> this device's project path. From here on `tmp` is
    // what lands on disk; `content` stays the synced version.
    let local_view = if kind.rewrites_cwd() {
        let (t, r) = (tmp.clone(), roots.to_vec());
        blocking(move || rewrite_file_cwds(&t, &r)).await.and_then(|r| r).map_err(finish)?
    } else {
        None
    };
    let disk_size = local_view.as_ref().map_or(content.size, |(n, _, _)| *n);
    if placement == Placement::Extend {
        // Local behind (a prefix of the incoming version): replaced. Local
        // ahead (the incoming version is a prefix of it): kept. Else diverged.
        let (p, t) = (path.to_path_buf(), tmp.clone());
        let (local_len, same_start) = blocking(move || {
            let local_len = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            let common = local_len.min(disk_size);
            (local_len, sha_of_prefix(&p, common).is_some_and(|a| sha_of_prefix(&t, common) == Some(a)))
        })
        .await
        .map_err(finish)?;
        placement = match (same_start, local_len > disk_size) {
            (true, true) => {
                let _ = fs::remove_file(&tmp);
                return Ok(Outcome::Skipped);
            }
            (true, false) => Placement::Replace,
            (false, _) => Placement::Sibling,
        };
    }
    if let Some(m) = mode {
        set_mode(&tmp, m);
    }
    match placement {
        Placement::Sibling => {
            let sib = sibling_path(path, &op.hlc.device);
            fs::rename(&tmp, &sib).map_err(|e| finish(format!("{}: {e}", sib.display())))?;
            tracing::info!(event = "sync_file_conflict", kind = kind.as_str());
            Ok(Outcome::Conflict)
        }
        _ => {
            if matches!(placement, Placement::BackupThenReplace) && path.exists() {
                let p = path.to_path_buf();
                blocking(move || crate::commands::project_links::back_up_library_orphan(&p, "sync"))
                    .await
                    .and_then(|r| r)
                    .map_err(finish)?;
            }
            let meta = fs::metadata(&tmp).map_err(|e| finish(e.to_string()))?;
            let hs = HashState { ino: inode(&meta), synced: Some(mid), disk: local_view.as_ref().map(|(_, _, m)| *m) };
            // Recorded before the rename so the session watcher, woken by the
            // rename, already knows this version came from sync.
            let mut conn = db.acquire().await.map_err(|e| finish(err(e)))?;
            save_record(&mut conn, kind, portable, path, Some((&content, mtime_ms(&meta))), mode, Some(&op.hlc), "remote")
                .await
                .map_err(finish)?;
            if let Some((n, sha, _)) = &local_view {
                set_local_view(&mut conn, kind, portable, *n, sha).await.map_err(finish)?;
            }
            set_hash_state(&mut conn, kind, portable, &hs).await.map_err(finish)?;
            if let Err(e) = fs::rename(&tmp, path) {
                let _ = sqlx::query("DELETE FROM sync_files WHERE kind = ? AND path = ?")
                    .bind(kind.as_str())
                    .bind(portable)
                    .execute(&mut *conn)
                    .await;
                return Err(finish(format!("{}: {e}", path.display())));
            }
            if kind.rewrites_cwd() {
                mark_transcript_mirrored(&mut conn, path, disk_size).await?;
            }
            let p = path.to_path_buf();
            let _ = blocking(move || clear_parts(&p)).await;
            Ok(Outcome::Applied)
        }
    }
}

// ---- cwd rewrite -------------------------------------------------------------------

/// JSON keys holding the session's working directory in Claude transcripts
/// (`cwd` on every line, `attachment.snapshot.workingDirectory`) and Codex
/// rollouts (`session_meta` / `turn_context` / `world_state` /
/// `thread_settings` `cwd`, plus the `runtime_workspace_roots` arrays).
const CWD_KEYS: &[&str] = &["cwd", "workingDirectory"];
const CWD_LIST_KEYS: &[&str] = &["runtime_workspace_roots"];

/// Local project roots with their synced form: `(synced, local)`.
async fn project_roots(db: &Db, paths: &PathCtx) -> Result<Vec<(String, String)>, String> {
    let rows: Vec<String> = sqlx::query_scalar("SELECT path FROM projects").fetch_all(db).await.map_err(err)?;
    let mut roots: Vec<(String, String)> = rows
        .into_iter()
        .filter(|p| !p.is_empty())
        .map(|p| (portable_path(paths, &p), p))
        .collect();
    roots.sort_by_key(|(synced, _)| std::cmp::Reverse(synced.len()));
    Ok(roots)
}

/// Where a working directory of another device lives here: under one of the
/// project roots, matched by the synced form: `~/w/p` matches `<X>/w/p` for
/// the leftmost absolute `<X>` (the sender's home, which we don't know),
/// longest root first. `None` = leave it (already local, or unknown).
pub(crate) fn map_foreign_dir(v: &str, roots: &[(String, String)]) -> Option<String> {
    let under = |v: &str, root: &str| v == root || v.strip_prefix(root).is_some_and(|r| r.starts_with('/'));
    if roots.iter().any(|(_, local)| under(v, local)) {
        return None;
    }
    for (synced, local) in roots {
        if let Some(tail) = synced.strip_prefix('~').filter(|t| t.starts_with('/')) {
            let mut from = 0;
            while let Some(i) = v[from..].find(tail).map(|i| i + from) {
                let rest = &v[i + tail.len()..];
                if (rest.is_empty() || rest.starts_with('/')) && i > 0 && v.starts_with('/') {
                    return Some(format!("{local}{rest}"));
                }
                from = i + 1;
            }
        } else if synced.starts_with('/') && under(v, synced) {
            return Some(format!("{local}{}", &v[synced.len()..]));
        }
    }
    None
}

/// End (exclusive) of the JSON string literal starting at `at` (a `"`).
fn string_end(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            b'\n' => return None,
            _ => i += 1,
        }
    }
    None
}

fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t') {
        i += 1;
    }
    i
}

/// Rewrite the cwd values of a JSONL text in place, touching only those string
/// literals (key order, spacing and every other byte stay as they are, so a
/// prefix of a file rewrites to a prefix of the rewritten file).
pub(crate) fn rewrite_cwds(text: &[u8], roots: &[(String, String)]) -> Option<Vec<u8>> {
    if roots.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(text.len());
    let mut changed = false;
    let mut copied = 0;
    let mut i = 0;
    let mut replace = |out: &mut Vec<u8>, copied: &mut usize, start: usize, end: usize| {
        let Ok(old) = serde_json::from_slice::<String>(&text[start..end]) else { return };
        let Some(new) = map_foreign_dir(&old, roots) else { return };
        let Ok(lit) = serde_json::to_string(&new) else { return };
        out.extend_from_slice(&text[*copied..start]);
        out.extend_from_slice(lit.as_bytes());
        *copied = end;
        changed = true;
    };
    while i < text.len() {
        if text[i] != b'"' {
            i += 1;
            continue;
        }
        let Some(key_end) = string_end(text, i) else {
            i += 1;
            continue;
        };
        let key = &text[i + 1..key_end - 1];
        let colon = skip_ws(text, key_end);
        if colon >= text.len() || text[colon] != b':' {
            i = key_end;
            continue;
        }
        let v = skip_ws(text, colon + 1);
        let is_cwd = CWD_KEYS.iter().any(|k| k.as_bytes() == key);
        let is_list = CWD_LIST_KEYS.iter().any(|k| k.as_bytes() == key);
        if is_cwd && text.get(v) == Some(&b'"') {
            match string_end(text, v) {
                Some(end) => {
                    replace(&mut out, &mut copied, v, end);
                    i = end;
                }
                None => i = v + 1,
            }
        } else if is_list && text.get(v) == Some(&b'[') {
            let mut j = skip_ws(text, v + 1);
            while text.get(j) == Some(&b'"') {
                let Some(end) = string_end(text, j) else { break };
                replace(&mut out, &mut copied, j, end);
                j = skip_ws(text, end);
                if text.get(j) == Some(&b',') {
                    j = skip_ws(text, j + 1);
                }
            }
            i = j.max(v + 1);
        } else {
            i = colon + 1;
        }
    }
    if !changed {
        return None;
    }
    out.extend_from_slice(&text[copied..]);
    Some(out)
}

/// Rewrite `tmp` in place; returns the new size, sha256 and SHA-256 state
/// when it changed.
fn rewrite_file_cwds(tmp: &Path, roots: &[(String, String)]) -> Result<Option<(u64, String, Mid)>, String> {
    if roots.is_empty() {
        return Ok(None);
    }
    let io = |e: std::io::Error| format!("{}: {e}", tmp.display());
    let text = fs::read(tmp).map_err(io)?;
    let Some(new) = rewrite_cwds(&text, roots) else { return Ok(None) };
    let mut f = fs::File::create(tmp).map_err(io)?;
    f.write_all(&new).map_err(io)?;
    f.sync_all().map_err(io)?;
    let mut mid = Mid::default();
    mid.absorb(&new);
    Ok(Some((new.len() as u64, hex::encode(Sha256::digest(&new)), mid)))
}

/// Chunk refs (and the SHA-256 state after the full chunks) for a local file
/// already identical to the incoming version.
fn chunk_refs_of(path: &Path, inc: &Incoming) -> Result<(Vec<ChunkRef>, Mid), String> {
    let mut f = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut out = Vec::new();
    let mut mid = Mid::default();
    let mut left = inc.size;
    while left > 0 {
        let n = read_full(&mut f, &mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        if n == CHUNK_SIZE {
            mid.absorb(&buf);
        }
        out.push(ChunkRef { sha: sha_hex(&buf[..n]), len: n as u64 });
        left = left.saturating_sub(n as u64);
    }
    Ok((out, mid))
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
    rec: Option<Record>,
) -> Result<Outcome, String> {
    let p = path.to_path_buf();
    let removed = blocking(move || -> Result<Outcome, String> {
        if fs::symlink_metadata(&p).is_err() {
            return Ok(Outcome::Applied);
        }
        if !matches_record(&p, rec.as_ref()) {
            // Edited here since the last sync: keep it (it will be pushed back).
            return Ok(Outcome::Conflict);
        }
        match kind {
            FileKind::Library | FileKind::SshKey => drop(crate::commands::project_links::back_up_library_orphan(&p, "sync")?),
            _ => fs::remove_file(&p).map_err(|e| format!("{}: {e}", p.display()))?,
        }
        Ok(Outcome::Applied)
    })
    .await??;
    if removed == Outcome::Applied {
        let mut conn = db.acquire().await.map_err(err)?;
        save_record(&mut conn, kind, portable, path, None, None, None, "remote").await?;
    }
    Ok(removed)
}

/// The run whose log or transcript a `file` op wrote, if any (for the
/// `sync://run-log-updated` event).
pub async fn run_of_op(db: &Db, paths: &PathCtx, op: &Op) -> Option<String> {
    let (kind, portable) = parse_pk(&op.pk).ok()?;
    let path = localize(paths, kind, &portable).ok()?;
    let p = path.to_string_lossy().into_owned();
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let (sql, key) = match kind {
        // `<project>/.devdy/runs/<run id>.log`, or a recorded output path.
        FileKind::RunLog => ("SELECT id FROM runs WHERE id = ?2 OR output_path = ?1 LIMIT 1", stem),
        FileKind::ClaudeTranscript => (
            "SELECT id FROM runs WHERE transcript_path = ?1 OR (engine = 'claude' AND session_id = ?2) \
             ORDER BY created_at DESC LIMIT 1",
            stem,
        ),
        // rollout-<YYYY-MM-DDThh-mm-ss>-<thread id>.jsonl
        FileKind::CodexRollout => (
            "SELECT id FROM runs WHERE transcript_path = ?1 OR (engine = 'codex' AND session_id = ?2) \
             ORDER BY created_at DESC LIMIT 1",
            stem.strip_prefix("rollout-").and_then(|s| s.get(20..)).unwrap_or_default().to_string(),
        ),
        FileKind::Library | FileKind::SshKey => return None,
    };
    sqlx::query_scalar(sql).bind(&p).bind(&key).fetch_optional(db).await.ok().flatten()
}

/// Size of `path` as the other devices get it, for the final sizes a run
/// records on release: a file still exactly as sync wrote it here with a
/// rewritten cwd counts with the synced content's size (it is never pushed
/// again, so its size here would never arrive anywhere); anything else with
/// its size on disk, which is what the next scan uploads.
pub async fn wire_size(db: &Db, path: &Path) -> Option<u64> {
    let meta = tokio::fs::metadata(path).await.ok().filter(|m| m.is_file())?;
    let rec: Option<(i64, Option<i64>, Option<String>)> = sqlx::query_as(
        "SELECT size, local_size, local_sha256 FROM sync_files WHERE local_path = ? AND deleted = 0",
    )
    .bind(path.to_string_lossy().as_ref())
    .fetch_optional(db)
    .await
    .ok()
    .flatten();
    if let Some((size, Some(local_size), Some(local_sha))) = rec {
        if local_size.max(0) as u64 == meta.len() {
            let (p, len) = (path.to_path_buf(), meta.len());
            let sha = blocking(move || sha_of_prefix(&p, len)).await.ok().flatten();
            if sha.as_deref() == Some(local_sha.as_str()) {
                return Some(size.max(0) as u64);
            }
        }
    }
    Some(meta.len())
}

// ---- Serving file_read ---------------------------------------------------------------

/// Serve `file_read` (on-demand-sync.md §P2): bytes `[offset, offset + len)`
/// of a tracked file's version `(sha256, size)`. Only files this device
/// recorded (and so offered in its change feed) are served, at their place
/// on this device. Append-only kinds are served from the current file as
/// long as the range lies within `size` (the asker checks the whole sha256 at
/// the end); rewritten kinds only while the file is still that version.
/// Errors: [`FILE_GONE`] (no such file / record), [`FILE_CHANGED`] (another
/// version now), or a bad request.
pub async fn read_range(db: &Db, paths: &PathCtx, req: &FileReadReq, with_keys: bool) -> Result<Vec<u8>, String> {
    let kind = FileKind::parse(&req.kind).ok_or("bad_request: unknown file kind")?;
    let end = req.offset.checked_add(req.len).ok_or("bad_request: range")?;
    if req.len == 0 || req.len > CHUNK_SIZE as u64 || end > req.size {
        return Err("bad_request: range".into());
    }
    if kind == FileKind::SshKey && !with_keys {
        return Err(FILE_GONE.into());
    }
    let rec = {
        let mut conn = db.acquire().await.map_err(err)?;
        load_record(&mut conn, kind, &req.path).await?
    };
    let Some(rec) = rec.filter(|r| !r.deleted) else { return Err(FILE_GONE.into()) };
    let path = localize(paths, kind, &req.path).map_err(|_| FILE_GONE.to_string())?;
    if rec.local_path != path.to_string_lossy() {
        return Err(FILE_GONE.into());
    }
    let req = req.clone();
    blocking(move || -> Result<Vec<u8>, String> {
        let meta = match fs::metadata(&path) {
            Ok(m) if m.is_file() => m,
            _ => return Err(FILE_GONE.into()),
        };
        // On disk is a rewrite of the synced content (transcript cwd): the
        // synced bytes are not here to serve.
        if rec.local_size.is_some() {
            return Err(FILE_CHANGED.into());
        }
        if kind.append_only() {
            if meta.len() < end {
                return Err(FILE_CHANGED.into());
            }
        } else {
            let unchanged = meta.len() == rec.size && mtime_ms(&meta) == rec.mtime;
            let sha = if unchanged { Some(rec.sha256.clone()) } else { sha_of_prefix(&path, meta.len()) };
            if meta.len() != req.size || sha.as_deref() != Some(req.sha256.as_str()) {
                return Err(FILE_CHANGED.into());
            }
        }
        let bytes = read_at(&path, req.offset, req.len as usize).map_err(|_| FILE_GONE.to_string())?;
        if bytes.len() as u64 != req.len {
            return Err(FILE_CHANGED.into());
        }
        Ok(bytes)
    })
    .await?
}

// ---- Session watcher guard ---------------------------------------------------------

/// Whether `path` is a transcript sync wrote and nobody has touched since: the
/// session watcher must not import it as a new external session.
pub async fn written_by_sync(db: &Db, path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else { return false };
    let size: Option<i64> = sqlx::query_scalar(
        "SELECT COALESCE(local_size, size) FROM sync_files WHERE local_path = ? AND origin = 'remote' AND deleted = 0",
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
    use sqlx::sqlite::SqlitePoolOptions;
    use std::sync::Mutex;
    use std::time::Duration;

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

    pub async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        crate::sync::install(&pool).await.unwrap();
        pool
    }

    /// The other device's side of `file_read`: served from its DB and disk
    /// exactly like a peer would. Counts what it served; can fail from some
    /// offset on (an interrupted transfer) or corrupt what it sends.
    pub struct DiskSource {
        db: Db,
        paths: PathCtx,
        pub bytes: Mutex<u64>,
        pub fail_from: Mutex<Option<u64>>,
        pub corrupt: Mutex<bool>,
    }

    pub fn source(db: &Db, paths: &PathCtx) -> DiskSource {
        DiskSource { db: db.clone(), paths: paths.clone(), bytes: Mutex::new(0), fail_from: Mutex::new(None), corrupt: Mutex::new(false) }
    }

    impl DiskSource {
        fn served(&self) -> u64 {
            *self.bytes.lock().unwrap()
        }
    }

    #[async_trait]
    impl FileSource for DiskSource {
        async fn read(&self, req: FileReadReq) -> Result<Vec<u8>, String> {
            if self.fail_from.lock().unwrap().is_some_and(|f| req.offset >= f) {
                return Err("peer_timeout".into());
            }
            let mut bytes = read_range(&self.db, &self.paths, &req, true).await?;
            *self.bytes.lock().unwrap() += bytes.len() as u64;
            if *self.corrupt.lock().unwrap() {
                if let Some(b) = bytes.last_mut() {
                    *b ^= 1;
                }
            }
            Ok(bytes)
        }
    }

    async fn scan_all(db: &Db, paths: &PathCtx, with_keys: bool) -> usize {
        scan(db, paths, &mut ScanState::default(), with_keys).await.unwrap()
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

    /// What the feed would serve for `t` on device `db`.
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

    /// A scan for a device that does not take sessions leaves run logs for
    /// a later one.
    #[tokio::test]
    async fn a_scan_without_sessions_leaves_run_logs() {
        let h = tempdir();
        let db = mem_db().await;
        let p = ctx(h.path());
        let log = add_run(&db, h.path(), "r1", "done").await;
        fs::write(&log, b"turn 1\n").unwrap();
        let mut st = ScanState { skip_sessions: true, ..Default::default() };
        assert_eq!(scan(&db, &p, &mut st, false).await.unwrap(), 0);
        st.skip_sessions = false;
        assert_eq!(scan(&db, &p, &mut st, false).await.unwrap(), 1);
    }

    fn mb(n: usize, byte: u8) -> Vec<u8> {
        vec![byte; n]
    }

    #[tokio::test]
    async fn scan_records_chunks_and_append_growth() {
        let home = tempdir();
        let db = mem_db().await;
        let paths = ctx(home.path());
        let log = add_run(&db, home.path(), "r1", "running").await;
        let mut body = mb(CHUNK_SIZE, b'a');
        body.extend(mb(CHUNK_SIZE / 2, b'b'));
        fs::write(&log, &body).unwrap();

        assert_eq!(scan_all(&db, &paths, false).await, 1);
        let ob = outbox_files(&db).await;
        assert_eq!(ob.len(), 1);
        assert_eq!(ob[0].0, r#"{"kind":"run_log","path":"~/w/p/.devdy/runs/r1.log"}"#);

        // Unchanged: nothing read, nothing queued.
        assert_eq!(scan_all(&db, &paths, false).await, 0);

        // Append: queued again with the new size and sha, nothing uploaded.
        body.extend(mb(CHUNK_SIZE, b'c'));
        fs::write(&log, &body).unwrap();
        assert_eq!(scan_all(&db, &paths, false).await, 1);
        let mut conn = db.acquire().await.unwrap();
        let pk = file_pk(FileKind::RunLog, "~/w/p/.devdy/runs/r1.log");
        let (kind, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
        let row = row.unwrap();
        assert_eq!(kind, OpKind::Upsert);
        assert_eq!(row["size"], body.len() as u64);
        assert_eq!(row["sha256"], hex::encode(Sha256::digest(&body)));
        assert!(row.get("chunks").is_none(), "no relay chunks any more");
        drop(conn);
        assert_eq!(record(&db, FileKind::RunLog, "~/w/p/.devdy/runs/r1.log").await.chunks.len(), 3);

        // A touch without a content change re-reads but queues nothing.
        let f = fs::File::options().append(true).open(&log).unwrap();
        f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5)).unwrap();
        let before = outbox_files(&db).await.len();
        assert_eq!(scan_all(&db, &paths, false).await, 0);
        assert_eq!(outbox_files(&db).await.len(), before);
    }

    /// L3: a kicked file no scan has recorded yet (a short run that ended
    /// before the next scan, a Codex rollout created after it) is picked up;
    /// a kick for an unrelated file is not.
    #[tokio::test]
    async fn scan_takes_kicked_files_it_never_saw() {
        let home = tempdir();
        let db = mem_db().await;
        let paths = ctx(home.path());
        let mut st = ScanState::default();
        assert_eq!(scan(&db, &paths, &mut st, false).await.unwrap(), 0);
        let short = add_run(&db, home.path(), "short", "done").await;
        fs::write(&short, b"done already").unwrap();
        sqlx::query("INSERT INTO runs (id, project_id, type, status, engine, session_id, created_at) VALUES ('cx', 'p1', 'session', 'running', 'codex', 'th-1', 'x')")
            .execute(&db)
            .await
            .unwrap();
        let rollout = home.path().join(".codex/sessions/2026/10/30/rollout-2026-10-30T10-00-00-th-1.jsonl");
        fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        fs::write(&rollout, b"{}\n").unwrap();
        let stray = home.path().join("elsewhere.jsonl");
        fs::write(&stray, b"{}\n").unwrap();
        for p in [&short, &rollout, &stray] {
            kick(p);
        }
        assert!(has_kicks());
        assert_eq!(scan(&db, &paths, &mut st, false).await.unwrap(), 2);
        assert!(!has_kicks());
        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM sync_files ORDER BY kind").fetch_all(&db).await.unwrap();
        assert_eq!(kinds, vec!["codex_rollout", "run_log"]);
    }

    #[tokio::test]
    async fn apply_reads_from_the_source_and_appends_only_the_growth() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let log = add_run(&db_a, ha.path(), "r1", "running").await;
        let mut body = mb(CHUNK_SIZE, b'x');
        body.extend(b"tail");
        fs::write(&log, &body).unwrap();
        scan_all(&db_a, &pa, false).await;
        let t = Tracked { kind: FileKind::RunLog, local: log.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;
        let src = source(&db_a, &pa);

        assert_eq!(apply_op(&db_b, &pb, &src, &op, true).await.unwrap(), Outcome::Applied);
        let local_b = hb.path().join("w/p/.devdy/runs/r1.log");
        assert_eq!(fs::read(&local_b).unwrap(), body);
        assert_eq!(src.served(), body.len() as u64);
        assert_eq!(run_of_op(&db_b, &pb, &op).await, None, "B has no runs row yet");
        add_run(&db_b, hb.path(), "r1", "running").await;
        assert_eq!(run_of_op(&db_b, &pb, &op).await.as_deref(), Some("r1"));
        let leftovers: Vec<_> = fs::read_dir(local_b.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no part file left behind");
        // Re-applying the same op is a no-op.
        assert_eq!(apply_op(&db_b, &pb, &src, &op, true).await.unwrap(), Outcome::Skipped);
        // Applied files are not queued back.
        assert!(outbox_files(&db_b).await.is_empty());

        // Growth: B reads from its last full chunk on, appends in place.
        let ino = inode(&fs::metadata(&local_b).unwrap());
        body.extend(mb(CHUNK_SIZE, b'y'));
        fs::write(&log, &body).unwrap();
        scan_all(&db_a, &pa, false).await;
        let op2 = op_for(&db_a, &t, 2_000, "dev-a").await;
        let src = source(&db_a, &pa);
        assert_eq!(apply_op(&db_b, &pb, &src, &op2, true).await.unwrap(), Outcome::Applied);
        assert_eq!(src.served(), (body.len() - CHUNK_SIZE) as u64, "the first chunk stays");
        assert_eq!(fs::read(&local_b).unwrap(), body);
        assert_eq!(inode(&fs::metadata(&local_b).unwrap()), ino, "appended in place");

        // Corrupted content is refused (as a changed version) and leaves the
        // file untouched.
        body.extend(b"more");
        fs::write(&log, &body).unwrap();
        scan_all(&db_a, &pa, false).await;
        let op3 = op_for(&db_a, &t, 3_000, "dev-a").await;
        let bad = source(&db_a, &pa);
        *bad.corrupt.lock().unwrap() = true;
        let e = apply_op(&db_b, &pb, &bad, &op3, true).await.unwrap_err();
        assert!(is_superseded(&e), "{e}");
        assert_eq!(fs::read(&local_b).unwrap().len(), 2 * CHUNK_SIZE + 4);
        let leftovers: Vec<_> = fs::read_dir(local_b.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "a bad version's part file is removed");
    }

    /// A transfer cut midway leaves its whole chunks in the part file: the
    /// next try reads only the rest, and the result checks out.
    #[tokio::test]
    async fn an_interrupted_transfer_resumes_after_its_whole_chunks() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let log = add_run(&db_a, ha.path(), "r1", "done").await;
        let body: Vec<u8> = (0..(3 * CHUNK_SIZE + CHUNK_SIZE / 2)).map(|i| (i % 251) as u8).collect();
        fs::write(&log, &body).unwrap();
        scan_all(&db_a, &pa, false).await;
        let t = Tracked { kind: FileKind::RunLog, local: log.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        let cut = source(&db_a, &pa);
        *cut.fail_from.lock().unwrap() = Some(2 * CHUNK_SIZE as u64);
        let e = apply_op(&db_b, &pb, &cut, &op, true).await.unwrap_err();
        assert!(!is_superseded(&e), "a network failure is retried: {e}");
        let local_b = hb.path().join("w/p/.devdy/runs/r1.log");
        assert!(!local_b.exists());
        let part = part_path(&local_b, op.row.as_ref().unwrap()["sha256"].as_str().unwrap());
        assert!(fs::metadata(&part).unwrap().len() >= 2 * CHUNK_SIZE as u64);

        let src = source(&db_a, &pa);
        assert_eq!(apply_op(&db_b, &pb, &src, &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(src.served(), (body.len() - 2 * CHUNK_SIZE) as u64, "resumed after 2 chunks");
        assert_eq!(fs::read(&local_b).unwrap(), body);
        assert!(!part.exists());

        // A part file of some other content under the same name is not
        // trusted blindly: the whole sha decides, and a bad one starts over.
        let hc = tempdir();
        let (db_c, pc) = (mem_db().await, ctx(hc.path()));
        let local_c = hc.path().join("w/p/.devdy/runs/r1.log");
        fs::create_dir_all(local_c.parent().unwrap()).unwrap();
        let part_c = part_path(&local_c, op.row.as_ref().unwrap()["sha256"].as_str().unwrap());
        fs::write(&part_c, mb(2 * CHUNK_SIZE, b'?')).unwrap();
        let e = apply_op(&db_c, &pc, &src, &op, true).await.unwrap_err();
        assert!(is_superseded(&e) && !part_c.exists(), "{e}");
        assert_eq!(apply_op(&db_c, &pc, &src, &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read(&local_c).unwrap(), body);
    }

    /// Rewritten kinds are only served while the file is still the version
    /// asked for; append-only kinds are served from a longer current file.
    #[tokio::test]
    async fn served_versions_and_file_changed() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let rule = pa.app_data.as_ref().unwrap().join("rules/git.md");
        fs::create_dir_all(rule.parent().unwrap()).unwrap();
        fs::write(&rule, "v1").unwrap();
        sqlx::query("INSERT INTO rules (id, name, description, source_path, updated_at) VALUES ('r', 'git', '', ?, 'x')")
            .bind(rule.to_string_lossy().as_ref())
            .execute(&db_a)
            .await
            .unwrap();
        let log = add_run(&db_a, ha.path(), "r1", "running").await;
        fs::write(&log, "l1\n").unwrap();
        scan_all(&db_a, &pa, false).await;
        let tr = Tracked { kind: FileKind::Library, local: rule.clone(), portable: "@data/rules/git.md".into() };
        let tl = Tracked { kind: FileKind::RunLog, local: log.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        let (op_rule, op_log) = (op_for(&db_a, &tr, 1_000, "dev-a").await, op_for(&db_a, &tl, 1_000, "dev-a").await);

        // Changed on A since: the rule is refused, the log still served (the
        // asked range is inside the version's size).
        fs::write(&rule, "v2 edited").unwrap();
        fs::write(&log, "l1\nl2\n").unwrap();
        let src = source(&db_a, &pa);
        let e = apply_op(&db_b, &pb, &src, &op_rule, true).await.unwrap_err();
        assert_eq!(e, FILE_CHANGED);
        let rule_b = pb.app_data.as_ref().unwrap().join("rules/git.md");
        assert!(!rule_b.exists());
        assert!(fs::read_dir(rule_b.parent().unwrap()).unwrap().next().is_none(), "no part file kept");
        assert_eq!(apply_op(&db_b, &pb, &src, &op_log, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(hb.path().join("w/p/.devdy/runs/r1.log")).unwrap(), "l1\n");

        // Never more than asked, never what A does not track.
        let req = |path: &str, size: u64, offset: u64, len: u64| FileReadReq {
            kind: "run_log".into(),
            path: path.into(),
            sha256: "0".repeat(64),
            size,
            offset,
            len,
        };
        let p = "~/w/p/.devdy/runs/r1.log";
        assert!(read_range(&db_a, &pa, &req(p, 3, 2, 2), true).await.unwrap_err().starts_with("bad_request"));
        assert!(read_range(&db_a, &pa, &req(p, 3, 0, 0), true).await.unwrap_err().starts_with("bad_request"));
        assert_eq!(read_range(&db_a, &pa, &req(p, 3, 0, 3), true).await.unwrap(), b"l1\n");
        assert_eq!(read_range(&db_a, &pa, &req(p, 99, 0, 99), true).await.unwrap_err(), FILE_CHANGED, "shorter than asked");
        assert_eq!(read_range(&db_a, &pa, &req("~/w/p/.devdy/runs/other.log", 3, 0, 3), true).await.unwrap_err(), FILE_GONE);
        fs::remove_file(&log).unwrap();
        assert_eq!(read_range(&db_a, &pa, &req(p, 3, 0, 3), true).await.unwrap_err(), FILE_GONE);
    }

    #[tokio::test]
    async fn diverged_files_get_a_conflict_sibling() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));

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
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: fa.clone(), portable: "@data/rules/git.md".into() };
        fs::write(&fb, "edited on b").unwrap();
        let op = op_for(&db_a, &t, 1_000, "dev-a").await; // older than B's edit
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "edited on b");
        assert_eq!(fs::read_to_string(fb.with_file_name("git.md.conflict-dev-a")).unwrap(), "from a");

        // A newer remote version replaces an older unseen local edit, which is
        // moved to the legacy backups.
        let far = wall_ms() + 60_000;
        let op = op_for(&db_a, &t, far, "dev-a").await;
        let r = apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap();
        assert_eq!(r, Outcome::Applied);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "from a");

        // Append-only: both sides continued the transcript -> sibling; local
        // ahead of remote -> kept.
        let enc = crate::commands::sessions::encode_project_dir(&ha.path().join("w/p").to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, "l1\nl2-a\n").unwrap();
        let mut conn = db_a.acquire().await.unwrap();
        let (content, _) = hash_file(&ta, None).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        assert_eq!(portable, "~/.claude/projects/~-w-p/s1.jsonl");
        save_record(&mut conn, FileKind::ClaudeTranscript, &portable, &ta, Some((&content, 0)), None, None, "local").await.unwrap();
        drop(conn);
        let tt = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone() };
        let op = op_for(&db_a, &tt, 1_000, "dev-a").await;
        let tb = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        fs::create_dir_all(tb.parent().unwrap()).unwrap();
        fs::write(&tb, "l1\nl2-b\n").unwrap();
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-b\n");
        assert!(tb.with_file_name("s1.jsonl.conflict-dev-a").is_file());

        fs::write(&tb, "l1\nl2-a\nl3-b\n").unwrap();
        let op = op_for(&db_a, &tt, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Skipped);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-a\nl3-b\n");

        // Local behind remote (a strict prefix) is just updated.
        fs::write(&tb, "l1\n").unwrap();
        let op = op_for(&db_a, &tt, 3_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
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
        let enc = crate::commands::sessions::encode_project_dir(&ha.path().join("w/p").to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s-r1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, "{}\n").unwrap();
        add_run(&db_a, ha.path(), "r1", "done").await;
        let mut st = ScanState::default();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        let t = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        add_run(&db_b, hb.path(), "r1", "done").await;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
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

    fn transcript_lines(cwd: &str, turns: usize) -> String {
        let cwd = serde_json::to_string(cwd).unwrap();
        let mut out = String::new();
        for n in 0..turns {
            out += &format!(
                "{{\"type\":\"user\",\"cwd\":{cwd},\"sessionId\":\"s-r1\",\"message\":{{\"role\":\"user\",\"content\":\"q{n} {{\\\"cwd\\\":\\\"/x\\\"}}\"}}}}\n\
                 {{\"type\":\"assistant\",\"cwd\":{cwd},\"message\":{{\"role\":\"assistant\",\"model\":\"m\",\"content\":[{{\"type\":\"text\",\"text\":\"a{n}\"}}]}}}}\n"
            );
        }
        out
    }

    #[test]
    fn cwd_rewrite_touches_only_cwd_literals() {
        let roots = vec![("~/w/p".to_string(), "/Users/bob/code/p".to_string())];
        assert_eq!(map_foreign_dir("/Users/alice/w/p", &roots).as_deref(), Some("/Users/bob/code/p"));
        assert_eq!(map_foreign_dir("/Users/alice/w/p/sub", &roots).as_deref(), Some("/Users/bob/code/p/sub"));
        assert_eq!(map_foreign_dir("/Users/alice/w/pp", &roots), None);
        assert_eq!(map_foreign_dir("/Users/bob/code/p", &roots), None, "already local");
        assert_eq!(map_foreign_dir("w/p", &roots), None);
        let abs = vec![("/opt/w/p".to_string(), "/srv/p".to_string())];
        assert_eq!(map_foreign_dir("/opt/w/p/x", &abs).as_deref(), Some("/srv/p/x"));
        assert_eq!(map_foreign_dir("/mnt/opt/w/p", &abs), None);

        // Claude: top-level cwd + snapshot.workingDirectory; escaped JSON in content is left alone.
        let claude = "{\"type\":\"user\",\"cwd\":\"/Users/alice/w/p\",\"message\":{\"content\":\"{\\\"cwd\\\":\\\"/Users/alice/w/p\\\"}\"}}\n\
                      {\"attachment\":{\"snapshot\":{\"workingDirectory\": \"/Users/alice/w/p/a\"}},\"path\":\"/Users/alice/w/p/f.rs\"}\n";
        let out = String::from_utf8(rewrite_cwds(claude.as_bytes(), &roots).unwrap()).unwrap();
        assert_eq!(
            out,
            "{\"type\":\"user\",\"cwd\":\"/Users/bob/code/p\",\"message\":{\"content\":\"{\\\"cwd\\\":\\\"/Users/alice/w/p\\\"}\"}}\n\
             {\"attachment\":{\"snapshot\":{\"workingDirectory\": \"/Users/bob/code/p/a\"}},\"path\":\"/Users/alice/w/p/f.rs\"}\n"
        );
        // Codex: session_meta / turn_context / thread_settings cwd and the roots arrays.
        let codex = "{\"type\":\"session_meta\",\"payload\":{\"id\":\"t\",\"cwd\":\"/Users/alice/w/p\",\"runtime_workspace_roots\":[\"/Users/alice/w/p\", \"/tmp\"]}}\n\
                     {\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/Users/alice/w/p\",\"model\":\"m\"}}\n\
                     {\"type\":\"event_msg\",\"payload\":{\"type\":\"thread_settings_applied\",\"thread_settings\":{\"cwd\":\"/Users/alice/w/p\",\"runtime_workspace_roots\":[]}}}\n";
        let out = String::from_utf8(rewrite_cwds(codex.as_bytes(), &roots).unwrap()).unwrap();
        assert_eq!(out, codex.replace("/Users/alice/w/p", "/Users/bob/code/p"));
        // Nothing to map: untouched.
        assert!(rewrite_cwds(b"{\"cwd\":\"/elsewhere\"}\n", &roots).is_none());
        assert!(rewrite_cwds(claude.as_bytes(), &[]).is_none());
        // A prefix of the file rewrites to a prefix of the rewritten file.
        let full = rewrite_cwds(codex.as_bytes(), &roots).unwrap();
        let cut = codex.find("{\"type\":\"turn_context\"").unwrap();
        assert!(full.starts_with(&rewrite_cwds(&codex.as_bytes()[..cut], &roots).unwrap()));
    }

    #[tokio::test]
    async fn transcript_cwd_is_rewritten_and_not_pushed_back() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let project_a = ha.path().join("w/p");
        let project_b = hb.path().join("w/p");
        let enc = crate::commands::sessions::encode_project_dir(&project_a.to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s-r1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, transcript_lines(&project_a.to_string_lossy(), 1)).unwrap();
        add_run(&db_a, ha.path(), "r1", "done").await;
        let mut st = ScanState::default();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        let t = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone() };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        // B has the project (at its own path) but not the runs row yet.
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
            .bind(project_b.to_string_lossy().as_ref())
            .execute(&db_b)
            .await
            .unwrap();
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
        let tb = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        let on_b = fs::read_to_string(&tb).unwrap();
        assert_eq!(on_b, transcript_lines(&project_b.to_string_lossy(), 1), "cwd follows B's project path");
        assert!(written_by_sync(&db_b, &tb).await);

        // The on-open reconcile / get_run_log path must not import it as a new
        // session (it would: the rewritten cwd matches B's project).
        let pb_path = project_b.to_string_lossy().into_owned();
        assert!(crate::commands::sessions::parse_transcript(&tb, &pb_path).is_some());
        let out = crate::commands::sessions::upsert_session_run_core(
            &db_b, "p1", "p", &pb_path, "s-r1", "claude", &tb, crate::commands::sessions::parse_transcript, true,
        )
        .await
        .unwrap();
        assert!(matches!(out, crate::commands::sessions::SyncOutcome::Skipped));
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs").fetch_one(&db_b).await.unwrap();
        assert_eq!(runs, 0, "no duplicate session imported");

        // The rewritten file is not a local change: nothing is pushed back.
        add_run(&db_b, hb.path(), "r1", "done").await;
        let mut st_b = ScanState::default();
        assert_eq!(scan(&db_b, &pb, &mut st_b, false).await.unwrap(), 0);
        assert!(outbox_files(&db_b).await.is_empty());

        // A appends: B replaces (no conflict copy) and rewrites again.
        fs::write(&ta, transcript_lines(&project_a.to_string_lossy(), 2)).unwrap();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        let op2 = op_for(&db_a, &t, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op2, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&tb).unwrap(), transcript_lines(&project_b.to_string_lossy(), 2));
        let siblings = fs::read_dir(tb.parent().unwrap()).unwrap().flatten().count();
        assert_eq!(siblings, 1, "no conflict sibling");
        assert_eq!(scan(&db_b, &pb, &mut st_b, false).await.unwrap(), 0);
        let mirrored: Option<i64> =
            sqlx::query_scalar("SELECT transcript_synced_size FROM runs WHERE id = 'r1'").fetch_one(&db_b).await.unwrap();
        assert_eq!(mirrored, Some(fs::metadata(&tb).unwrap().len() as i64), "local size counts as mirrored");

        // Continued on B: B's version (with B's cwd) is pushed as a change.
        let mut more = fs::read_to_string(&tb).unwrap();
        more += &transcript_lines(&pb_path, 1);
        fs::write(&tb, more).unwrap();
        assert_eq!(scan(&db_b, &pb, &mut st_b, false).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn library_file_delete_round_trip() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
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
        assert_eq!(scan(&db_a, &pa, &mut st, false).await.unwrap(), 2);
        let mut ops = Vec::new();
        for (pk, _) in outbox_files(&db_a).await {
            let pk: Value = serde_json::from_str(&pk).unwrap();
            let mut conn = db_a.acquire().await.unwrap();
            let (_, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
            ops.push(upsert_op(pk, row.unwrap(), 1_000, "dev-a"));
        }
        for op in &ops {
            assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), op, true).await.unwrap(), Outcome::Applied);
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
        assert_eq!(scan(&db_b, &pb, &mut st_b, false).await.unwrap(), 1);
        fs::write(dir_a.join("SKILL.md"), "skill v2").unwrap();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: dir_a.join("SKILL.md"), portable: "@data/skills/review/SKILL.md".into() };
        let op = op_for(&db_a, &t, wall_ms() + 60_000, "dev-a").await;
        sqlx::query("DELETE FROM sync_outbox").execute(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&skill_b).unwrap(), "skill v2");
        let root = crate::commands::project_links::backup_root();
        let kept = walkdir::WalkDir::new(&root).into_iter().flatten().any(|e| {
            e.file_type().is_file() && fs::read_to_string(e.path()).is_ok_and(|c| c == "zz-sync-b-version")
        });
        assert!(kept, "b's own version is in the backups");

        // Removed on A (folder still there) -> Delete op -> moved away on B.
        fs::remove_file(dir_a.join("references/zz-sync-ref.md")).unwrap();
        sqlx::query("DELETE FROM sync_outbox").execute(&db_a).await.unwrap();
        assert_eq!(scan(&db_a, &pa, &mut st, false).await.unwrap(), 1);
        let (pk, op) = outbox_files(&db_a).await.pop().unwrap();
        assert_eq!(op, "D");
        let pk: Value = serde_json::from_str(&pk).unwrap();
        let mut conn = db_a.acquire().await.unwrap();
        let (kind, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
        assert_eq!((kind, row), (OpKind::Delete, None));
        let del = Op { v: OP_VERSION, schema: SCHEMA_VERSION, hlc: Hlc::new(2_000, 0, "dev-a"), tbl: TBL.into(), pk, kind: OpKind::Delete, row: None };
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &del, true).await.unwrap(), Outcome::Applied);
        assert!(!ref_b.exists());
        let root = crate::commands::project_links::backup_root();
        assert!(walkdir::WalkDir::new(root).into_iter().flatten().any(|e| e.file_name() == "zz-sync-ref.md"));
    }
    #[test]
    fn resumable_sha256_matches_sha2() {
        for len in [0usize, 1, 55, 56, 63, 64, 65, 127, 128, 1000, CHUNK_SIZE + 3] {
            let data: Vec<u8> = (0..len).map(|i| (i * 7 % 251) as u8).collect();
            assert_eq!(Mid::default().finish(&data), sha_hex(&data), "len {len}");
            // Resumed from any 64-byte boundary.
            let mut mid = Mid::default();
            let cut = len / 2 / 64 * 64;
            mid.absorb(&data[..cut]);
            assert_eq!(mid.off as usize, cut);
            assert_eq!(mid.finish(&data[cut..]), sha_hex(&data), "len {len} resumed at {cut}");
        }
    }

    async fn record(db: &Db, kind: FileKind, portable: &str) -> Record {
        let mut conn = db.acquire().await.unwrap();
        load_record(&mut conn, kind, portable).await.unwrap().unwrap()
    }

    #[tokio::test]
    async fn grown_log_is_hashed_from_the_recorded_state() {
        let h = tempdir();
        let db = mem_db().await;
        let p = ctx(h.path());
        let log = add_run(&db, h.path(), "r1", "done").await;
        let portable = "~/w/p/.devdy/runs/r1.log";
        let mut body = mb(2 * CHUNK_SIZE, b'a');
        body.extend(b"tail");
        fs::write(&log, &body).unwrap();
        let mut st = ScanState::default();
        scan(&db, &p, &mut st, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        let meta = fs::metadata(&log).unwrap();
        assert_eq!(rec.synced_mid(inode(&meta)).unwrap().off, 2 * CHUNK_SIZE as u64);

        // Appended: resumes after the full chunks, same result as a full read.
        body.extend(mb(CHUNK_SIZE, b'b'));
        fs::write(&log, &body).unwrap();
        let meta = fs::metadata(&log).unwrap();
        let resume = resume_point(FileKind::RunLog, Some(&rec), &log, &meta).expect("resumable");
        assert_eq!((resume.mid.off, resume.chunks.len()), (2 * CHUNK_SIZE as u64, 2));
        scan(&db, &p, &mut st, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!((rec.size, rec.sha256.clone()), (body.len() as u64, sha_hex(&body)));
        assert_eq!(rec.chunks.len(), 4);

        // Rewritten in place (not an append): detected, re-read from the start.
        body[CHUNK_SIZE * 3 - 1] = b'X';
        body.extend(b"more");
        fs::write(&log, &body).unwrap();
        let meta = fs::metadata(&log).unwrap();
        assert!(resume_point(FileKind::RunLog, Some(&rec), &log, &meta).is_none());
        scan(&db, &p, &mut st, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!(rec.sha256, sha_hex(&body));
        // A library file never resumes.
        assert!(resume_point(FileKind::Library, Some(&rec), &log, &meta).is_none());
    }

    #[tokio::test]
    async fn unpushed_local_appends_survive_a_diverging_remote_version() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let la = add_run(&db_a, ha.path(), "r1", "done").await;
        let lb = add_run(&db_b, hb.path(), "r1", "done").await;
        let t = Tracked { kind: FileKind::RunLog, local: la.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        fs::write(&la, "l1\n").unwrap();
        let (mut sa, mut sb) = (ScanState::default(), ScanState::default());
        scan(&db_a, &pa, &mut sa, false).await.unwrap();
        apply_op(&db_b, &pb, &source(&db_a, &pa), &op_for(&db_a, &t, 1_000, "dev-a").await, true).await.unwrap();

        // B continues the log and stages it (record = B's own version), A
        // continues differently; A's op is newer than B's change.
        fs::write(&lb, "l1\nl2-b\n").unwrap();
        scan(&db_b, &pb, &mut sb, false).await.unwrap();
        sqlx::query("UPDATE sync_outbox SET changed_at = 1").execute(&db_b).await.unwrap();
        fs::write(&la, "l1\nl2-a\n").unwrap();
        scan(&db_a, &pa, &mut sa, false).await.unwrap();
        let op = op_for(&db_a, &t, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&lb).unwrap(), "l1\nl2-b\n", "B's turn is kept");
        assert_eq!(fs::read_to_string(lb.with_file_name("r1.log.conflict-dev-a")).unwrap(), "l1\nl2-a\n");
        assert_eq!(outbox_files(&db_b).await.len(), 1, "B's unpushed append is still pushed");

        // A version that extends B's: replaced (appended in place).
        let ino = inode(&fs::metadata(&lb).unwrap());
        fs::write(&la, "l1\nl2-b\nl3-a\n").unwrap();
        scan(&db_a, &pa, &mut sa, false).await.unwrap();
        let op = op_for(&db_a, &t, 3_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&lb).unwrap(), "l1\nl2-b\nl3-a\n");
        assert_eq!(inode(&fs::metadata(&lb).unwrap()), ino, "appended in place");
        assert!(outbox_files(&db_b).await.is_empty(), "contained in the applied version");
    }

    #[tokio::test]
    async fn peers_append_in_place() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let la = add_run(&db_a, ha.path(), "r1", "done").await;
        let t = Tracked { kind: FileKind::RunLog, local: la.clone(), portable: "~/w/p/.devdy/runs/r1.log".into() };
        let mut body = mb(CHUNK_SIZE, b'x');
        body.extend(b"tail\n");
        fs::write(&la, &body).unwrap();
        let mut st = ScanState::default();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        apply_op(&db_b, &pb, &source(&db_a, &pa), &op_for(&db_a, &t, 1_000, "dev-a").await, true).await.unwrap();
        let lb = hb.path().join("w/p/.devdy/runs/r1.log");
        let ino = inode(&fs::metadata(&lb).unwrap());

        for (n, millis) in [(1usize, 2_000u64), (2, 3_000)] {
            body.extend(mb(CHUNK_SIZE / 2 * n, b'0' + n as u8));
            fs::write(&la, &body).unwrap();
            scan(&db_a, &pa, &mut st, false).await.unwrap();
            let op = op_for(&db_a, &t, millis, "dev-a").await;
            let src = source(&db_a, &pa);
            assert_eq!(apply_op(&db_b, &pb, &src, &op, true).await.unwrap(), Outcome::Applied);
            assert_eq!(fs::read(&lb).unwrap(), body);
            assert_eq!(inode(&fs::metadata(&lb).unwrap()), ino, "appended, not replaced");
            assert_eq!(src.served(), (body.len() - CHUNK_SIZE) as u64, "only from the last full chunk on");
        }
        let rec = record(&db_b, FileKind::RunLog, "~/w/p/.devdy/runs/r1.log").await;
        assert_eq!((rec.size, rec.sha256.clone(), rec.origin.as_str()), (body.len() as u64, sha_hex(&body), "remote"));
        assert!(matches_record(&lb, Some(&rec)));
        // Nothing to push back.
        let mut sb = ScanState::default();
        assert_eq!(scan(&db_b, &pb, &mut sb, false).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn modes_are_masked_and_keys_need_local_secrets_sync() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let skill = pa.app_data.as_ref().unwrap().join("skills/s");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("run.sh"), "#!/bin/sh").unwrap();
        fs::write(skill.join("SKILL.md"), "md").unwrap();
        sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES ('s', 's', '', 'claude', ?, 'x')")
            .bind(skill.to_string_lossy().as_ref())
            .execute(&db_a)
            .await
            .unwrap();
        let mut st = ScanState::default();
        scan(&db_a, &pa, &mut st, false).await.unwrap();
        for (name, sent, want) in [("run.sh", 0o777, 0o755), ("SKILL.md", 0o666, 0o644)] {
            let t = Tracked { kind: FileKind::Library, local: skill.join(name), portable: format!("@data/skills/s/{name}") };
            let mut op = op_for(&db_a, &t, 1_000, "dev-a").await;
            op.row.as_mut().unwrap().insert("mode".into(), json!(sent));
            assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
            let got = file_mode(&fs::metadata(pb.app_data.as_ref().unwrap().join("skills/s").join(name)).unwrap()).unwrap();
            assert_eq!(got, want, "{name}");
        }

        // SSH key: never written while local secrets sync is off.
        let key = pa.app_data.as_ref().unwrap().join("server-keys/k1");
        fs::create_dir_all(key.parent().unwrap()).unwrap();
        fs::write(&key, "-----KEY-----").unwrap();
        sqlx::query("INSERT INTO servers (id, label, host, username, auth_method, private_key_path, created_at) VALUES ('sv', 'v', 'h', 'u', 'key', ?, 'x')")
            .bind(key.to_string_lossy().as_ref())
            .execute(&db_a)
            .await
            .unwrap();
        scan(&db_a, &pa, &mut st, true).await.unwrap();
        let t = Tracked { kind: FileKind::SshKey, local: key.clone(), portable: "@data/server-keys/k1".into() };
        let op = op_for(&db_a, &t, 2_000, "dev-a").await;
        let key_b = pb.app_data.as_ref().unwrap().join("server-keys/k1");
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, false).await.unwrap(), Outcome::Skipped);
        assert!(!key_b.exists());
        assert_eq!(apply_op(&db_b, &pb, &source(&db_a, &pa), &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(file_mode(&fs::metadata(&key_b).unwrap()), Some(0o600));
    }

    #[test]
    fn transcripts_only_land_under_claude_config_dirs() {
        let home = PathBuf::from("/Users/bob");
        let p = ctx(&home).with_claude_dirs([PathBuf::from("/Users/bob/.claude-max"), PathBuf::from("relative")]);
        let ok = |s: &str| localize(&p, FileKind::ClaudeTranscript, s);
        assert!(ok("~/.claude/projects/~-w-p/s.jsonl").is_ok());
        assert!(ok("~/.claude-max/projects/~-w-p/s.jsonl").is_ok());
        assert!(ok("~/.claude-other/projects/~-w-p/s.jsonl").is_err(), "not a configured account");
        assert!(ok("~/.claude/projects/s.jsonl").is_err(), "no project folder");
        assert!(ok("~/w/projects/x/s.jsonl").is_err());
        assert!(ok("~//tmp/projects/x/s.jsonl").is_err());
        assert!(ok("/tmp/.claude/projects/x/s.jsonl").is_err());
        assert!(ok("~/.claude/projects/x/../../../.zshrc.jsonl").is_err());
        assert_eq!(expand_portable(&p, "~//etc/passwd"), "~//etc/passwd", "never escapes the home dir");
        assert!(localize(&p, FileKind::RunLog, "~//w/.devdy/runs/r.log").is_err());
    }

}
