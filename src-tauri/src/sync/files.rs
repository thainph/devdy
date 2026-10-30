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
//! A grown append-only file is hashed from the SHA-256 state recorded after its
//! last full chunk (`hash_state`), not from the start. While its run is live,
//! its trailing partial chunk is only published once the file has been idle
//! for `ScanState::tail_idle` (full chunks go at once).
//! The tail blob it replaced is queued in `sync_blob_release` and released on
//! the relay (`blob_release`) once the new manifest was pushed; a release whose
//! manifest is dropped unpushed is cancelled.
//! The `file` op itself goes through `sync_outbox` like a row change.
//!
//! Relay -> local (`apply_op`): last-writer-wins by HLC (via `sync_rows`),
//! missing chunks downloaded and verified, written atomically (tmp + rename)
//! at this device's path, or appended in place when the incoming version of an
//! append-only file extends the local one. A local file with content sync
//! never saw is not overwritten when it is newer (or, for append-only files,
//! unless it is a prefix of the incoming version): the remote version goes to a
//! `<name>.conflict-<device>` sibling instead, and the local version keeps its
//! place in the outbox. File IO and hashing run on the blocking pool.
//! Library files authored (or edited) here are moved to the legacy backups
//! before a remote version replaces them. Files written here are recorded with
//! origin = 'remote' so the session watcher does not import them again.
//! Transcripts and Codex rollouts get the sender's cwd rewritten to this
//! device's project path; the record then keeps the synced sha/chunks as the
//! base and the rewritten file's size/sha as `local_size` / `local_sha256`.

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
use super::codec::{expand_portable, map_in_project, portable_path, PathCtx};
#[cfg(test)]
use super::codec::to_portable;
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
    /// Still being written (its run is running, or it was kicked): an
    /// append-only file's partial tail waits until the file is idle.
    pub live: bool,
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

impl Timing {
    /// How long a live append-only file must stay untouched before its
    /// partial tail goes up: two quick scans, at most [`TAIL_IDLE`] (10 s with
    /// the default cadence).
    pub fn tail_idle(&self) -> Duration {
        TAIL_IDLE.min(self.quick * 2)
    }
}

/// How long a live append-only file must stay untouched before its trailing
/// partial chunk is published.
pub const TAIL_IDLE: Duration = Duration::from_secs(10);

pub struct ScanState {
    last_quick: Option<Instant>,
    last_full: Option<Instant>,
    /// Codex thread id -> rollout, rebuilt on every full scan (a Devdy-native
    /// Codex run does not record its rollout path).
    codex_index: std::sync::Arc<HashMap<String, PathBuf>>,
    /// Live files whose partial tail is not published yet: rechecked on every
    /// quick scan until it is.
    tail_pending: BTreeSet<Tracked>,
    pub tail_idle: Duration,
}

impl Default for ScanState {
    fn default() -> Self {
        ScanState {
            last_quick: None,
            last_full: None,
            codex_index: Default::default(),
            tail_pending: BTreeSet::new(),
            tail_idle: TAIL_IDLE,
        }
    }
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

fn track(paths: &PathCtx, kind: FileKind, local: PathBuf, live: bool, out: &mut Vec<Tracked>) {
    let portable = portable_path(paths, &local.to_string_lossy());
    out.push(Tracked { kind, local, portable, live });
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
        track(paths, FileKind::Library, f, false, out);
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
        track(paths, FileKind::SshKey, p, false, out);
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
    codex: std::sync::Arc<HashMap<String, PathBuf>>,
    out: &mut Vec<Tracked>,
) -> Result<(), String> {
    let sql = format!(
        "SELECT r.id, r.engine, r.session_id, r.output_path, r.transcript_path, p.path AS project_path, \
                a.config_dir, COALESCE(r.status = 'running', 0) AS live \
         FROM runs r JOIN projects p ON p.id = r.project_id \
         LEFT JOIN claude_accounts a ON a.id = r.claude_account_id {}",
        if running_only { "WHERE r.status = 'running'" } else { "" }
    );
    let rows: Vec<RunFileRow> = sqlx::query_as(&sql).fetch_all(db).await.map_err(err)?;
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

type RunFileRow = (String, Option<String>, Option<String>, Option<String>, Option<String>, String, Option<String>, bool);

fn run_row_files(paths: &PathCtx, row: RunFileRow, codex: &HashMap<String, PathBuf>, out: &mut Vec<Tracked>) {
    let (id, engine, session, output, transcript, project, config_dir, live) = row;
    let conventional = Path::new(&project).join(".devdy").join("runs").join(format!("{id}.log"));
    let mut logs: Vec<PathBuf> = output.map(PathBuf::from).into_iter().collect();
    if !logs.contains(&conventional) {
        logs.push(conventional);
    }
    for log in logs.into_iter().filter(|p| p.is_file()) {
        track(paths, FileKind::RunLog, log, live, out);
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
                track(paths, FileKind::ClaudeTranscript, f, live, out);
            }
        }
        (Some("codex"), Some(sid)) => {
            if let Some(f) = recorded.or_else(|| codex.get(sid).cloned().filter(|p| p.is_file())) {
                track(paths, FileKind::CodexRollout, f, live, out);
            }
        }
        _ => {}
    }
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
        let home = paths.home.clone();
        state.codex_index = std::sync::Arc::new(blocking(move || codex_index(&home)).await?);
        library_files(db, paths, &mut out).await?;
        if with_keys {
            ssh_key_files(db, paths, &mut out).await?;
        }
    }
    run_files(db, paths, mode == ScanMode::Quick, state.codex_index.clone(), &mut out).await?;
    out.extend(state.tail_pending.iter().cloned());
    Ok(dedup_tracked(out))
}

/// One entry per file; live if any of its sources says so.
fn dedup_tracked(mut list: Vec<Tracked>) -> Vec<Tracked> {
    list.sort();
    let mut out: Vec<Tracked> = Vec::with_capacity(list.len());
    for t in list {
        match out.last_mut() {
            Some(prev) if prev.kind == t.kind && prev.local == t.local && prev.portable == t.portable => prev.live |= t.live,
            _ => out.push(t),
        }
    }
    out
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

/// Queue a file op; returns its outbox seq.
async fn queue(conn: &mut SqliteConnection, kind: FileKind, portable: &str, op: &str) -> Result<i64, String> {
    let done = sqlx::query("INSERT INTO sync_outbox (tbl, pk, op, changed_at) VALUES (?, ?, ?, ?)")
        .bind(TBL)
        .bind(canonical_json(&file_pk(kind, portable)))
        .bind(op)
        .bind(wall_ms() as i64)
        .execute(&mut *conn)
        .await
        .map_err(err)?;
    Ok(done.last_insert_rowid())
}

/// The new version of a file no longer uses `old` chunks: those this device
/// uploaded, and no other file of it still uses, are forgotten (so dedupe
/// never hands them out again) and released on the relay once the new
/// manifest — outbox entry `seq` — is pushed (it supersedes the old one).
async fn release_superseded(
    conn: &mut SqliteConnection,
    kind: FileKind,
    portable: &str,
    old: &[ChunkRef],
    new: &[ChunkRef],
    seq: i64,
) -> Result<(), String> {
    for c in old.iter().filter(|c| !new.iter().any(|n| n.blob == c.blob)) {
        let ours: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_blobs WHERE blob_sha = ?")
            .bind(&c.blob)
            .fetch_optional(&mut *conn)
            .await
            .map_err(err)?;
        let used: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM sync_files WHERE deleted = 0 AND instr(chunks_json, ?) > 0 \
             AND NOT (kind = ? AND path = ?) LIMIT 1",
        )
        .bind(&c.blob)
        .bind(kind.as_str())
        .bind(portable)
        .fetch_optional(&mut *conn)
        .await
        .map_err(err)?;
        if ours.is_none() || used.is_some() {
            continue;
        }
        sqlx::query("DELETE FROM sync_blobs WHERE blob_sha = ?").bind(&c.blob).execute(&mut *conn).await.map_err(err)?;
        sqlx::query("INSERT OR REPLACE INTO sync_blob_release (blob_sha, after_seq, queued_at) VALUES (?, ?, ?)")
            .bind(&c.blob)
            .bind(seq)
            .bind(wall_ms() as i64)
            .execute(&mut *conn)
            .await
            .map_err(err)?;
    }
    Ok(())
}

/// `sync_blob_release.after_seq` of a release whose manifest was pushed.
const RELEASE_READY: i64 = 0;

/// Called by `mark_pushed(up_to)` before it deletes the outbox entries up to
/// `up_to`: a release becomes due when the newer manifest went out, i.e. the
/// whole outbox group of its file (pending sends the group's newest entry)
/// was pushed. A release whose entry is deleted while the group's newer entry
/// stays (not pushed in this batch) moves to that entry.
pub async fn releases_pushed(conn: &mut SqliteConnection, up_to: i64) -> Result<(), String> {
    let group_max = "(SELECT MAX(o2.seq) FROM sync_outbox o2 WHERE o2.tbl = o.tbl AND o2.pk = o.pk)";
    sqlx::query(&format!(
        "UPDATE sync_blob_release SET after_seq = {RELEASE_READY} WHERE after_seq > {RELEASE_READY} AND after_seq IN \
         (SELECT o.seq FROM sync_outbox o WHERE o.seq <= ?1 AND {group_max} <= ?1)"
    ))
    .bind(up_to)
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    sqlx::query(&format!(
        "UPDATE sync_blob_release SET after_seq = \
           (SELECT {group_max} FROM sync_outbox o WHERE o.seq = sync_blob_release.after_seq) \
         WHERE after_seq > {RELEASE_READY} AND after_seq <= ?1 \
           AND EXISTS (SELECT 1 FROM sync_outbox o WHERE o.seq = sync_blob_release.after_seq)"
    ))
    .bind(up_to)
    .execute(&mut *conn)
    .await
    .map_err(err)?;
    Ok(())
}

/// Blobs whose release is due (their newer manifest was pushed). Releases
/// whose manifest left the outbox unpushed (beaten by a remote op, dropped)
/// are cancelled: the relay may still serve the old manifest, which uses them.
pub async fn releasable_blobs(db: &Db, limit: usize) -> Result<Vec<String>, String> {
    sqlx::query(&format!(
        "DELETE FROM sync_blob_release WHERE after_seq > {RELEASE_READY} \
         AND NOT EXISTS (SELECT 1 FROM sync_outbox o WHERE o.seq = sync_blob_release.after_seq)"
    ))
    .execute(db)
    .await
    .map_err(err)?;
    sqlx::query_scalar(&format!(
        "SELECT blob_sha FROM sync_blob_release WHERE after_seq = {RELEASE_READY} ORDER BY queued_at LIMIT ?"
    ))
    .bind(limit as i64)
    .fetch_all(db)
    .await
    .map_err(err)
}

pub async fn forget_released(db: &Db, shas: &[String]) -> Result<(), String> {
    let mut tx = db.begin().await.map_err(err)?;
    for sha in shas {
        sqlx::query("DELETE FROM sync_blob_release WHERE blob_sha = ?").bind(sha).execute(&mut *tx).await.map_err(err)?;
    }
    tx.commit().await.map_err(err)
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

/// Where `upload_file` starts: the full chunks before `mid.off` are the
/// recorded ones (already up).
struct Resume {
    mid: Mid,
    chunks: Vec<ChunkRef>,
}

/// Hash `path` in `CHUNK_SIZE` chunks (from `resume` when given), uploading
/// every chunk the relay does not have yet (per `sync_blobs`). Without
/// `with_tail` a trailing partial chunk is left out (content = the full
/// chunks). Returns the content and the SHA-256 state after its full chunks.
async fn upload_file(
    conn: &mut SqliteConnection,
    blobs: &dyn BlobIo,
    path: &Path,
    resume: Option<Resume>,
    with_tail: bool,
) -> Result<(Content, Mid), StageError> {
    let local = |e: std::io::Error| StageError::Local(format!("{}: {e}", path.display()));
    let abort = |e: sqlx::Error| StageError::Abort(e.to_string());
    let (mut mid, mut chunks) = match resume {
        Some(r) => (r.mid, r.chunks),
        None => (Mid::default(), Vec::new()),
    };
    loop {
        let (p, at) = (path.to_path_buf(), mid.off);
        let bytes = blocking(move || read_at(&p, at, CHUNK_SIZE)).await.map_err(StageError::Local)?.map_err(local)?;
        let n = bytes.len();
        if n == 0 || (n < CHUNK_SIZE && !with_tail) {
            return Ok((Content { size: mid.off, sha256: mid.finish(&[]), chunks }, mid));
        }
        let (bytes, sha, next) = blocking(move || {
            let sha = sha_hex(&bytes);
            let mut next = mid;
            if bytes.len() == CHUNK_SIZE {
                next.absorb(&bytes);
            }
            (bytes, sha, next)
        })
        .await
        .map_err(StageError::Local)?;
        let known: Option<String> = sqlx::query_scalar("SELECT blob_sha FROM sync_blobs WHERE chunk_sha = ?")
            .bind(&sha)
            .fetch_optional(&mut *conn)
            .await
            .map_err(abort)?;
        let blob = match known {
            Some(b) => b,
            None => {
                let b = blobs.put(&bytes).await.map_err(StageError::Abort)?;
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
            let size = mid.off + n as u64;
            let sha256 = blocking(move || mid.finish(&bytes)).await.map_err(StageError::Local)?;
            return Ok((Content { size, sha256, chunks }, mid));
        }
        mid = next;
    }
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
                    // Kicked by the session watcher: being written right now.
                    tracked.push(Tracked { kind, local: p.clone(), portable, live: true });
                }
            }
        }
        tracked = dedup_tracked(tracked);
    }
    let mut queued = 0;
    let mut tail_pending = BTreeSet::new();
    for t in &tracked {
        match stage_one(db, blobs, t, state.tail_idle).await {
            Ok(staged) => {
                queued += staged.queued as usize;
                if staged.tail_pending {
                    tail_pending.insert(Tracked { live: true, ..t.clone() });
                }
            }
            Err(StageError::Abort(e)) => return Err(e),
            Err(StageError::Local(e)) => tracing::debug!(event = "sync_file_skipped", error = %e),
        }
    }
    state.tail_pending = tail_pending;
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

#[derive(Debug, Default, Clone, Copy)]
struct Staged {
    queued: bool,
    /// A partial tail was held back (the file is live and not idle yet).
    tail_pending: bool,
}

async fn stage_one(db: &Db, blobs: &dyn BlobIo, t: &Tracked, tail_idle: Duration) -> Result<Staged, StageError> {
    let local = StageError::Local;
    let meta = match tokio::fs::metadata(&t.local).await {
        Ok(m) if m.is_file() => m,
        Ok(_) => return Ok(Staged::default()),
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
        if !r.deleted && r.disk_size() == meta.len() && r.mtime == mtime && r.local_path == local_str {
            return Ok(Staged::default());
        }
    }
    let idle = (wall_ms() as i64).saturating_sub(mtime) >= tail_idle.as_millis() as i64;
    let with_tail = !t.kind.append_only() || !t.live || idle || meta.len() % CHUNK_SIZE as u64 == 0;
    if !with_tail {
        // Only full chunks may go now: nothing to do unless one was added.
        let full_end = meta.len() / CHUNK_SIZE as u64 * CHUNK_SIZE as u64;
        let published = rec
            .as_ref()
            .filter(|r| !r.deleted && r.local_path == local_str && r.local_size.is_none())
            .map_or(0, |r| r.size);
        if full_end <= published || full_end == 0 {
            return Ok(Staged { queued: false, tail_pending: true });
        }
    }
    let resume = {
        let (kind, rec, path, meta) = (t.kind, rec.clone(), t.local.clone(), meta.clone());
        blocking(move || resume_point(kind, rec.as_ref(), &path, &meta)).await.map_err(local)?
    };
    let (content, mid) = upload_file(&mut conn, blobs, &t.local, resume, with_tail).await?;
    let mode = t.kind.mode(file_mode(&meta));
    // A rewritten transcript compares by what sync wrote to disk: identical =
    // not a local change, even though it differs from the synced content.
    let unchanged = rec.as_ref().is_some_and(|r| !r.deleted && r.disk_sha() == content.sha256);
    let staged = Staged { queued: !unchanged, tail_pending: !with_tail };
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
        let hs = HashState { ino: inode(&meta), synced: Some(mid), disk: None };
        set_hash_state(&mut tx, t.kind, &t.portable, &hs).await.map_err(local)?;
        let seq = queue(&mut tx, t.kind, &t.portable, "U").await.map_err(local)?;
        if let Some(r) = rec.as_ref().filter(|r| !r.deleted) {
            release_superseded(&mut tx, t.kind, &t.portable, &r.chunks, &content.chunks, seq)
                .await
                .map_err(local)?;
        }
    }
    tx.commit().await.map_err(|e| local(e.to_string()))?;
    Ok(staged)
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
/// `with_keys`: local secrets sync is on (SSH keys are only written then).
pub async fn apply_op(db: &Db, paths: &PathCtx, blobs: &dyn BlobIo, op: &Op, with_keys: bool) -> Result<Outcome, String> {
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
        // Our newer local version will be pushed and win on the other side.
        return Ok(Outcome::Skipped);
    }
    let path = localize(paths, kind, &portable)?;
    let rec = load_record(&mut conn, kind, &portable).await?;
    drop(conn);

    let outcome = match op.kind {
        OpKind::Upsert => {
            let row = op.row.as_ref().ok_or("upsert without row")?;
            let roots = if kind.rewrites_cwd() { project_roots(db, paths).await? } else { Vec::new() };
            write_remote(db, blobs, kind, &portable, &path, rec, &parse_row(row)?, op, &roots).await?
        }
        OpKind::Delete => remove_remote(db, kind, &portable, &path, rec).await?,
    };

    let mut tx = db.begin().await.map_err(err)?;
    // Whatever local change was pending lost (or survives as a conflict copy),
    // except a diverged append-only file: the local version stays in place
    // and must still be pushed.
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

fn tmp_path(path: &Path) -> PathBuf {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    path.with_file_name(format!(".{name}{TMP_MARK}{}.tmp", uuid::Uuid::new_v4().simple()))
}

/// Size check of a downloaded chunk (all but the last are full).
fn check_chunk(bytes: &[u8], i: usize, count: usize) -> Result<(), String> {
    if bytes.len() > CHUNK_SIZE || (i + 1 < count && bytes.len() != CHUNK_SIZE) {
        return Err("chunk has the wrong size".into());
    }
    Ok(())
}

/// Write the incoming content to `tmp`, reusing chunks the local copy already
/// has (only when the local file is the version sync recorded). Returns the
/// content and the SHA-256 state after its full chunks.
async fn assemble(
    blobs: &dyn BlobIo,
    path: &Path,
    rec: Option<&Record>,
    inc: &Incoming,
    tmp: &Path,
) -> Result<(Content, Mid), String> {
    // Chunks are reused from the local copy only when it is the synced content
    // byte for byte (not a cwd-rewritten transcript).
    let local_ok = match rec.filter(|r| r.local_sha256.is_none()).cloned() {
        Some(r) => {
            let p = path.to_path_buf();
            blocking(move || matches_record(&p, Some(&r))).await?
        }
        None => false,
    };
    let reusable: HashMap<usize, (u64, u64)> = match rec.filter(|_| local_ok) {
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
    let t = tmp.to_path_buf();
    let mut out = Some(blocking(move || fs::File::create(&t)).await?.map_err(io)?);
    let mut mid = Mid::default();
    let mut sha256 = None;
    let mut chunks = Vec::with_capacity(inc.chunks.len());
    let mut size = 0u64;
    for (i, blob) in inc.chunks.iter().enumerate() {
        let bytes = match reusable.get(&i) {
            Some((at, len)) => {
                let (p, at, len) = (path.to_path_buf(), *at, *len as usize);
                let b = blocking(move || read_at(&p, at, len)).await?.map_err(io)?;
                if b.len() != len {
                    return Err(format!("{}: changed while reading", path.display()));
                }
                b
            }
            None => blobs.get(blob).await?,
        };
        check_chunk(&bytes, i, inc.chunks.len())?;
        size += bytes.len() as u64;
        let mut f = out.take().ok_or("file closed")?;
        let last = i + 1 == inc.chunks.len();
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
        sha256 = whole;
        chunks.push(ChunkRef { sha, blob: blob.clone(), len: size - chunks.iter().map(|c: &ChunkRef| c.len).sum::<u64>() });
    }
    let f = out.take().ok_or("file closed")?;
    blocking(move || f.sync_all()).await?.map_err(io)?;
    let sha256 = sha256.unwrap_or_else(|| Mid::default().finish(&[]));
    if size != inc.size || sha256 != inc.sha256 {
        return Err("downloaded content does not match the file op".into());
    }
    Ok((Content { size, sha256, chunks }, mid))
}

/// The new bytes of an append-only file whose incoming version extends the
/// version recorded here, appended in place (no full rewrite, no re-download
/// of the chunks it already has). `None` = not applicable: take the full path.
#[allow(clippy::too_many_arguments)]
async fn append_in_place(
    db: &Db,
    blobs: &dyn BlobIo,
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
    if inc.chunks.len() < full || (0..full).any(|i| inc.chunks[i] != r.chunks[i].blob) {
        return Ok(None);
    }
    let mut data = Vec::with_capacity((inc.size - synced.off) as usize);
    for (i, blob) in inc.chunks.iter().enumerate().skip(full) {
        let bytes = blobs.get(blob).await?;
        check_chunk(&bytes, i, inc.chunks.len())?;
        data.extend_from_slice(&bytes);
    }
    if data.len() as u64 != inc.size - synced.off {
        return Err("downloaded content does not match the file op".into());
    }
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
        return Err("downloaded content does not match the file op".into());
    }
    let mut chunks: Vec<ChunkRef> = r.chunks[..full].to_vec();
    for ((sha, len), blob) in refs.into_iter().zip(&inc.chunks[full..]) {
        chunks.push(ChunkRef { sha, blob: blob.clone(), len });
    }
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
    blobs: &dyn BlobIo,
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
        if let Some(outcome) = append_in_place(db, blobs, kind, portable, path, rec, inc, op, roots).await? {
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
    let tmp = tmp_path(path);
    let (content, mid) = match assemble(blobs, path, rec, inc, &tmp).await {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
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
    let mut out = Vec::with_capacity(inc.chunks.len());
    let mut mid = Mid::default();
    for blob in &inc.chunks {
        let n = read_full(&mut f, &mut buf).map_err(|e| e.to_string())?;
        if n == CHUNK_SIZE {
            mid.absorb(&buf);
        }
        out.push(ChunkRef { sha: sha_hex(&buf[..n]), blob: blob.clone(), len: n as u64 });
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

    /// Partial tails go up at once (the deferral has its own test).
    fn eager() -> ScanState {
        ScanState { tail_idle: Duration::ZERO, ..Default::default() }
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
        let mut st = eager();

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
        let mut st = eager();
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
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::RunLog, local: log.clone(), portable: "~/w/p/.devdy/runs/r1.log".into(), live: false };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
        let local_b = hb.path().join("w/p/.devdy/runs/r1.log");
        assert_eq!(fs::read(&local_b).unwrap(), body);
        assert_eq!(run_of_op(&db_b, &pb, &op).await, None, "B has no runs row yet");
        add_run(&db_b, hb.path(), "r1", "running").await;
        assert_eq!(run_of_op(&db_b, &pb, &op).await.as_deref(), Some("r1"));
        let leftovers: Vec<_> = fs::read_dir(local_b.parent().unwrap()).unwrap().flatten().collect();
        assert_eq!(leftovers.len(), 1, "no tmp file left behind");
        // Re-applying the same op is a no-op.
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Skipped);
        // Applied files are not queued back.
        assert!(outbox_files(&db_b).await.is_empty());

        // Growth: B only downloads the new chunk.
        body.extend(mb(CHUNK_SIZE, b'y'));
        fs::write(&log, &body).unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let op2 = op_for(&db_a, &t, 2_000, "dev-a").await;
        let gets = *blobs.gets.lock().unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op2, true).await.unwrap(), Outcome::Applied);
        assert_eq!(*blobs.gets.lock().unwrap() - gets, 2, "first chunk reused from disk");
        assert_eq!(fs::read(&local_b).unwrap(), body);

        // A corrupted blob is refused and leaves the file untouched.
        body.extend(b"more");
        fs::write(&log, &body).unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let op3 = op_for(&db_a, &t, 3_000, "dev-a").await;
        let last = op3.row.as_ref().unwrap()["chunks"].as_array().unwrap().last().unwrap().as_str().unwrap().to_string();
        blobs.blobs.lock().unwrap().insert(last, b"evil".to_vec());
        assert!(apply_op(&db_b, &pb, &blobs, &op3, true).await.is_err());
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
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: fa.clone(), portable: "@data/rules/git.md".into(), live: false };
        fs::write(&fb, "edited on b").unwrap();
        let op = op_for(&db_a, &t, 1_000, "dev-a").await; // older than B's edit
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "edited on b");
        assert_eq!(fs::read_to_string(fb.with_file_name("git.md.conflict-dev-a")).unwrap(), "from a");

        // A newer remote version replaces an older unseen local edit, which is
        // moved to the legacy backups.
        let far = wall_ms() + 60_000;
        let op = op_for(&db_a, &t, far, "dev-a").await;
        let r = apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap();
        assert_eq!(r, Outcome::Applied);
        assert_eq!(fs::read_to_string(&fb).unwrap(), "from a");

        // Append-only: both sides continued the transcript -> sibling; local
        // ahead of remote -> kept.
        let enc = crate::commands::sessions::encode_project_dir(&ha.path().join("w/p").to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, "l1\nl2-a\n").unwrap();
        let mut conn = db_a.acquire().await.unwrap();
        let (content, _) = upload_file(&mut conn, &blobs, &ta, None, true).await.ok().unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        assert_eq!(portable, "~/.claude/projects/~-w-p/s1.jsonl");
        save_record(&mut conn, FileKind::ClaudeTranscript, &portable, &ta, Some((&content, 0)), None, None, "local").await.unwrap();
        drop(conn);
        let tt = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone(), live: false };
        let op = op_for(&db_a, &tt, 1_000, "dev-a").await;
        let tb = localize(&pb, FileKind::ClaudeTranscript, &portable).unwrap();
        fs::create_dir_all(tb.parent().unwrap()).unwrap();
        fs::write(&tb, "l1\nl2-b\n").unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-b\n");
        assert!(tb.with_file_name("s1.jsonl.conflict-dev-a").is_file());

        fs::write(&tb, "l1\nl2-a\nl3-b\n").unwrap();
        let op = op_for(&db_a, &tt, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Skipped);
        assert_eq!(fs::read_to_string(&tb).unwrap(), "l1\nl2-a\nl3-b\n");

        // Local behind remote (a strict prefix) is just updated.
        fs::write(&tb, "l1\n").unwrap();
        let op = op_for(&db_a, &tt, 3_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        let t = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone(), live: false };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        add_run(&db_b, hb.path(), "r1", "done").await;
        let before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_outbox").fetch_one(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
        let blobs = MemBlobs::default();
        let project_a = ha.path().join("w/p");
        let project_b = hb.path().join("w/p");
        let enc = crate::commands::sessions::encode_project_dir(&project_a.to_string_lossy());
        let ta = ha.path().join(".claude/projects").join(enc).join("s-r1.jsonl");
        fs::create_dir_all(ta.parent().unwrap()).unwrap();
        fs::write(&ta, transcript_lines(&project_a.to_string_lossy(), 1)).unwrap();
        add_run(&db_a, ha.path(), "r1", "done").await;
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let portable = to_portable(&pa, &ta.to_string_lossy());
        let t = Tracked { kind: FileKind::ClaudeTranscript, local: ta.clone(), portable: portable.clone(), live: false };
        let op = op_for(&db_a, &t, 1_000, "dev-a").await;

        // B has the project (at its own path) but not the runs row yet.
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', ?, 'x')")
            .bind(project_b.to_string_lossy().as_ref())
            .execute(&db_b)
            .await
            .unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
        let mut st_b = eager();
        assert_eq!(stage(&db_b, &pb, &blobs, &mut st_b, ScanMode::Full, false).await.unwrap(), 0);
        assert!(outbox_files(&db_b).await.is_empty());

        // A appends: B replaces (no conflict copy) and rewrites again.
        fs::write(&ta, transcript_lines(&project_a.to_string_lossy(), 2)).unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let op2 = op_for(&db_a, &t, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op2, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&tb).unwrap(), transcript_lines(&project_b.to_string_lossy(), 2));
        let siblings = fs::read_dir(tb.parent().unwrap()).unwrap().flatten().count();
        assert_eq!(siblings, 1, "no conflict sibling");
        assert_eq!(stage(&db_b, &pb, &blobs, &mut st_b, ScanMode::Full, false).await.unwrap(), 0);
        let mirrored: Option<i64> =
            sqlx::query_scalar("SELECT transcript_synced_size FROM runs WHERE id = 'r1'").fetch_one(&db_b).await.unwrap();
        assert_eq!(mirrored, Some(fs::metadata(&tb).unwrap().len() as i64), "local size counts as mirrored");

        // Continued on B: B's version (with B's cwd) is pushed as a change.
        let mut more = fs::read_to_string(&tb).unwrap();
        more += &transcript_lines(&pb_path, 1);
        fs::write(&tb, more).unwrap();
        assert_eq!(stage(&db_b, &pb, &blobs, &mut st_b, ScanMode::Full, false).await.unwrap(), 1);
    }

    #[tokio::test]
    async fn superseded_tail_chunks_are_released_after_the_push() {
        let h = tempdir();
        let db = mem_db().await;
        let p = ctx(h.path());
        let blobs = MemBlobs::default();
        let log = add_run(&db, h.path(), "r1", "running").await;
        let mut body = mb(CHUNK_SIZE, b'x');
        body.extend(b"tail");
        fs::write(&log, &body).unwrap();
        // Another file whose content is the same as the log's full first chunk.
        let twin = add_run(&db, h.path(), "r2", "running").await;
        fs::write(&twin, mb(CHUNK_SIZE, b'x')).unwrap();
        let mut st = eager();
        stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let chunks = |db: Db| async move {
            let j: String = sqlx::query_scalar("SELECT chunks_json FROM sync_files WHERE path LIKE '%r1.log'").fetch_one(&db).await.unwrap();
            serde_json::from_str::<Vec<ChunkRef>>(&j).unwrap()
        };
        let v1 = chunks(db.clone()).await;
        pushed(&db).await;

        body.extend(b" more");
        fs::write(&log, &body).unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let v2 = chunks(db.clone()).await;
        assert_eq!(v1[0].blob, v2[0].blob, "full chunk reused");
        assert_ne!(v1[1].blob, v2[1].blob);
        // Not before the new manifest left the outbox.
        assert!(releasable_blobs(&db, 10).await.unwrap().is_empty());
        let known: Option<i64> = sqlx::query_scalar("SELECT 1 FROM sync_blobs WHERE blob_sha = ?")
            .bind(&v1[1].blob)
            .fetch_optional(&db)
            .await
            .unwrap();
        assert_eq!(known, None, "a released blob is never deduped again");
        pushed(&db).await;
        assert_eq!(releasable_blobs(&db, 10).await.unwrap(), vec![v1[1].blob.clone()]);
        forget_released(&db, &[v1[1].blob.clone()]).await.unwrap();
        assert!(releasable_blobs(&db, 10).await.unwrap().is_empty());

        // The log shrinks to its first chunk: that chunk is still used by the
        // twin file, so only the tail goes.
        fs::write(&log, mb(CHUNK_SIZE, b'y')).unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        pushed(&db).await;
        assert_eq!(releasable_blobs(&db, 10).await.unwrap(), vec![v2[1].blob.clone()]);
        forget_released(&db, &[v2[1].blob.clone()]).await.unwrap();

        // A manifest dropped unpushed (a remote version won) never releases
        // the chunks the relay may still serve.
        let mut grown = mb(CHUNK_SIZE, b'y');
        grown.extend(b"tail");
        fs::write(&log, &grown).unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        grown.extend(b"+");
        fs::write(&log, &grown).unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_blob_release").fetch_one(&db).await.unwrap();
        assert_eq!(queued, 1, "the unpushed tail is superseded");
        sqlx::query("DELETE FROM sync_outbox").execute(&db).await.unwrap();
        assert!(releasable_blobs(&db, 10).await.unwrap().is_empty());
        let queued: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_blob_release").fetch_one(&db).await.unwrap();
        assert_eq!(queued, 0, "cancelled");
    }

    /// What `mark_pushed` does for everything in the outbox.
    async fn pushed(db: &Db) {
        let up_to: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM sync_outbox").fetch_one(db).await.unwrap();
        let mut conn = db.acquire().await.unwrap();
        releases_pushed(&mut conn, up_to).await.unwrap();
        sqlx::query("DELETE FROM sync_outbox WHERE seq <= ?").bind(up_to).execute(&mut *conn).await.unwrap();
    }

    #[tokio::test]
    async fn release_waits_for_the_files_newest_manifest() {
        let h = tempdir();
        let db = mem_db().await;
        let p = ctx(h.path());
        let blobs = MemBlobs::default();
        let log = add_run(&db, h.path(), "r1", "running").await;
        let mut st = eager();
        fs::write(&log, b"v1").unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        pushed(&db).await;
        fs::write(&log, b"v1 v2").unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let first: i64 = sqlx::query_scalar("SELECT MAX(seq) FROM sync_outbox").fetch_one(&db).await.unwrap();
        // Another row changes, then the file again: a push that stops before
        // the file's newest entry must not release v1's chunk.
        sqlx::query("INSERT INTO todos (id, text, position, created_at) VALUES ('t', 'x', 0, 'x')").execute(&db).await.unwrap();
        fs::write(&log, b"v1 v2 v3").unwrap();
        stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap();
        let todo: i64 = sqlx::query_scalar("SELECT seq FROM sync_outbox WHERE tbl = 'todos'").fetch_one(&db).await.unwrap();
        assert!(first < todo);
        let mut conn = db.acquire().await.unwrap();
        releases_pushed(&mut conn, todo).await.unwrap();
        sqlx::query("DELETE FROM sync_outbox WHERE seq <= ?").bind(todo).execute(&mut *conn).await.unwrap();
        drop(conn);
        assert!(releasable_blobs(&db, 10).await.unwrap().is_empty(), "the file's newest manifest is not pushed yet");
        pushed(&db).await;
        assert_eq!(releasable_blobs(&db, 10).await.unwrap().len(), 2, "v1 and v2 tails");
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
        let mut st = eager();
        assert_eq!(stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 2);
        let mut ops = Vec::new();
        for (pk, _) in outbox_files(&db_a).await {
            let pk: Value = serde_json::from_str(&pk).unwrap();
            let mut conn = db_a.acquire().await.unwrap();
            let (_, row) = pending_row(&mut conn, &pk, false).await.unwrap().unwrap();
            ops.push(upsert_op(pk, row.unwrap(), 1_000, "dev-a"));
        }
        for op in &ops {
            assert_eq!(apply_op(&db_b, &pb, &blobs, op, true).await.unwrap(), Outcome::Applied);
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
        let mut st_b = eager();
        assert_eq!(stage(&db_b, &pb, &blobs, &mut st_b, ScanMode::Full, false).await.unwrap(), 1);
        fs::write(dir_a.join("SKILL.md"), "skill v2").unwrap();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let t = Tracked { kind: FileKind::Library, local: dir_a.join("SKILL.md"), portable: "@data/skills/review/SKILL.md".into(), live: false };
        let op = op_for(&db_a, &t, wall_ms() + 60_000, "dev-a").await;
        sqlx::query("DELETE FROM sync_outbox").execute(&db_b).await.unwrap();
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
        assert_eq!(apply_op(&db_b, &pb, &blobs, &del, true).await.unwrap(), Outcome::Applied);
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
        let blobs = MemBlobs::default();
        let log = add_run(&db, h.path(), "r1", "done").await;
        let portable = "~/w/p/.devdy/runs/r1.log";
        let mut body = mb(2 * CHUNK_SIZE, b'a');
        body.extend(b"tail");
        fs::write(&log, &body).unwrap();
        let mut st = eager();
        stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        let meta = fs::metadata(&log).unwrap();
        assert_eq!(rec.synced_mid(inode(&meta)).unwrap().off, 2 * CHUNK_SIZE as u64);

        // Appended: resumes after the full chunks, same result as a full read.
        body.extend(mb(CHUNK_SIZE, b'b'));
        fs::write(&log, &body).unwrap();
        let meta = fs::metadata(&log).unwrap();
        let resume = resume_point(FileKind::RunLog, Some(&rec), &log, &meta).expect("resumable");
        assert_eq!((resume.mid.off, resume.chunks.len()), (2 * CHUNK_SIZE as u64, 2));
        stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!((rec.size, rec.sha256.clone()), (body.len() as u64, sha_hex(&body)));
        assert_eq!(rec.chunks.len(), 4);

        // Rewritten in place (not an append): detected, re-read from the start.
        body[CHUNK_SIZE * 3 - 1] = b'X';
        body.extend(b"more");
        fs::write(&log, &body).unwrap();
        let meta = fs::metadata(&log).unwrap();
        assert!(resume_point(FileKind::RunLog, Some(&rec), &log, &meta).is_none());
        stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!(rec.sha256, sha_hex(&body));
        // A library file never resumes.
        assert!(resume_point(FileKind::Library, Some(&rec), &log, &meta).is_none());
    }

    #[tokio::test]
    async fn live_partial_tail_waits_until_idle() {
        let h = tempdir();
        let db = mem_db().await;
        let p = ctx(h.path());
        let blobs = MemBlobs::default();
        let log = add_run(&db, h.path(), "r1", "running").await;
        let portable = "~/w/p/.devdy/runs/r1.log";
        let mut st = ScanState::default();
        assert_eq!(st.tail_idle, TAIL_IDLE);
        assert_eq!(Timing::default().tail_idle(), Duration::from_secs(10));

        // Only a partial chunk, just written: nothing goes up yet.
        fs::write(&log, b"turn 1\n").unwrap();
        assert_eq!(stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 0);
        assert_eq!(*blobs.puts.lock().unwrap(), 0);
        // A full chunk goes at once, the partial tail after it waits.
        let mut body = mb(CHUNK_SIZE, b'a');
        body.extend(b"partial");
        fs::write(&log, &body).unwrap();
        assert_eq!(stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 1);
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!((rec.size, rec.chunks.len(), *blobs.puts.lock().unwrap()), (CHUNK_SIZE as u64, 1, 1));
        assert_eq!(rec.sha256, sha_hex(&body[..CHUNK_SIZE]));
        // Rechecked on quick scans without being re-read or re-queued.
        assert_eq!(stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 0);
        assert_eq!(*blobs.puts.lock().unwrap(), 1);

        // Idle long enough: the tail goes up.
        let old = std::time::SystemTime::now() - Duration::from_secs(30);
        fs::File::options().append(true).open(&log).unwrap().set_modified(old).unwrap();
        assert_eq!(stage(&db, &p, &blobs, &mut st, ScanMode::Quick, false).await.unwrap(), 1);
        let rec = record(&db, FileKind::RunLog, portable).await;
        assert_eq!((rec.size, rec.sha256.clone()), (body.len() as u64, sha_hex(&body)));

        // A finished run's file goes up whole at once.
        sqlx::query("UPDATE runs SET status = 'done'").execute(&db).await.unwrap();
        body.extend(b" and more");
        fs::write(&log, &body).unwrap();
        assert_eq!(stage(&db, &p, &blobs, &mut st, ScanMode::Full, false).await.unwrap(), 1);
        assert_eq!(record(&db, FileKind::RunLog, portable).await.size, body.len() as u64);
    }

    #[tokio::test]
    async fn unpushed_local_appends_survive_a_diverging_remote_version() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();
        let la = add_run(&db_a, ha.path(), "r1", "done").await;
        let lb = add_run(&db_b, hb.path(), "r1", "done").await;
        let t = Tracked { kind: FileKind::RunLog, local: la.clone(), portable: "~/w/p/.devdy/runs/r1.log".into(), live: false };
        fs::write(&la, "l1\n").unwrap();
        let (mut sa, mut sb) = (eager(), eager());
        stage(&db_a, &pa, &blobs, &mut sa, ScanMode::Full, false).await.unwrap();
        apply_op(&db_b, &pb, &blobs, &op_for(&db_a, &t, 1_000, "dev-a").await, true).await.unwrap();

        // B continues the log and stages it (record = B's own version), A
        // continues differently; A's op is newer than B's change.
        fs::write(&lb, "l1\nl2-b\n").unwrap();
        stage(&db_b, &pb, &blobs, &mut sb, ScanMode::Full, false).await.unwrap();
        sqlx::query("UPDATE sync_outbox SET changed_at = 1").execute(&db_b).await.unwrap();
        fs::write(&la, "l1\nl2-a\n").unwrap();
        stage(&db_a, &pa, &blobs, &mut sa, ScanMode::Full, false).await.unwrap();
        let op = op_for(&db_a, &t, 2_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Conflict);
        assert_eq!(fs::read_to_string(&lb).unwrap(), "l1\nl2-b\n", "B's turn is kept");
        assert_eq!(fs::read_to_string(lb.with_file_name("r1.log.conflict-dev-a")).unwrap(), "l1\nl2-a\n");
        assert_eq!(outbox_files(&db_b).await.len(), 1, "B's unpushed append is still pushed");

        // A version that extends B's: replaced (appended in place).
        let ino = inode(&fs::metadata(&lb).unwrap());
        fs::write(&la, "l1\nl2-b\nl3-a\n").unwrap();
        stage(&db_a, &pa, &blobs, &mut sa, ScanMode::Full, false).await.unwrap();
        let op = op_for(&db_a, &t, 3_000, "dev-a").await;
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
        assert_eq!(fs::read_to_string(&lb).unwrap(), "l1\nl2-b\nl3-a\n");
        assert_eq!(inode(&fs::metadata(&lb).unwrap()), ino, "appended in place");
        assert!(outbox_files(&db_b).await.is_empty(), "contained in the applied version");
    }

    #[tokio::test]
    async fn peers_append_in_place() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();
        let la = add_run(&db_a, ha.path(), "r1", "done").await;
        let t = Tracked { kind: FileKind::RunLog, local: la.clone(), portable: "~/w/p/.devdy/runs/r1.log".into(), live: false };
        let mut body = mb(CHUNK_SIZE, b'x');
        body.extend(b"tail\n");
        fs::write(&la, &body).unwrap();
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        apply_op(&db_b, &pb, &blobs, &op_for(&db_a, &t, 1_000, "dev-a").await, true).await.unwrap();
        let lb = hb.path().join("w/p/.devdy/runs/r1.log");
        let ino = inode(&fs::metadata(&lb).unwrap());

        for (n, millis) in [(1usize, 2_000u64), (2, 3_000)] {
            body.extend(mb(CHUNK_SIZE / 2 * n, b'0' + n as u8));
            fs::write(&la, &body).unwrap();
            stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
            let gets = *blobs.gets.lock().unwrap();
            let op = op_for(&db_a, &t, millis, "dev-a").await;
            assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
            assert_eq!(fs::read(&lb).unwrap(), body);
            assert_eq!(inode(&fs::metadata(&lb).unwrap()), ino, "appended, not replaced");
            let needed = op.row.as_ref().unwrap()["chunks"].as_array().unwrap().len() - 1;
            assert_eq!(*blobs.gets.lock().unwrap() - gets, needed, "only the chunks from the old tail on");
        }
        let rec = record(&db_b, FileKind::RunLog, "~/w/p/.devdy/runs/r1.log").await;
        assert_eq!((rec.size, rec.sha256.clone(), rec.origin.as_str()), (body.len() as u64, sha_hex(&body), "remote"));
        assert!(matches_record(&lb, Some(&rec)));
        // Nothing to push back.
        let mut sb = eager();
        assert_eq!(stage(&db_b, &pb, &blobs, &mut sb, ScanMode::Full, false).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn modes_are_masked_and_keys_need_local_secrets_sync() {
        let (ha, hb) = (tempdir(), tempdir());
        let (db_a, db_b) = (mem_db().await, mem_db().await);
        let (pa, pb) = (ctx(ha.path()), ctx(hb.path()));
        let blobs = MemBlobs::default();
        let skill = pa.app_data.as_ref().unwrap().join("skills/s");
        fs::create_dir_all(&skill).unwrap();
        fs::write(skill.join("run.sh"), "#!/bin/sh").unwrap();
        fs::write(skill.join("SKILL.md"), "md").unwrap();
        sqlx::query("INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES ('s', 's', '', 'claude', ?, 'x')")
            .bind(skill.to_string_lossy().as_ref())
            .execute(&db_a)
            .await
            .unwrap();
        let mut st = eager();
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, false).await.unwrap();
        for (name, sent, want) in [("run.sh", 0o777, 0o755), ("SKILL.md", 0o666, 0o644)] {
            let t = Tracked { kind: FileKind::Library, local: skill.join(name), portable: format!("@data/skills/s/{name}"), live: false };
            let mut op = op_for(&db_a, &t, 1_000, "dev-a").await;
            op.row.as_mut().unwrap().insert("mode".into(), json!(sent));
            assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
        stage(&db_a, &pa, &blobs, &mut st, ScanMode::Full, true).await.unwrap();
        let t = Tracked { kind: FileKind::SshKey, local: key.clone(), portable: "@data/server-keys/k1".into(), live: false };
        let op = op_for(&db_a, &t, 2_000, "dev-a").await;
        let key_b = pb.app_data.as_ref().unwrap().join("server-keys/k1");
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, false).await.unwrap(), Outcome::Skipped);
        assert!(!key_b.exists());
        assert_eq!(apply_op(&db_b, &pb, &blobs, &op, true).await.unwrap(), Outcome::Applied);
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
