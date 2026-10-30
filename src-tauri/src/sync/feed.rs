//! The change feed between the two devices of a group (on-demand-sync.md
//! §P2): the relay only forwards, so the asking device pulls everything from
//! the other one.
//!
//! - Every device keeps the latest state of each synced row, file and
//!   Keychain item with a local change seq (`sync_feed`, fed by `sync_rows`).
//!   [`CHANGES`] reads it from a cursor: `changes{since, max_ops, max_bytes,
//!   skip_categories, skip_projects, files}` → one `changes_page{ops, next,
//!   more}` (last). Ops are full snapshots ([`Op`]), ordered by seq.
//! - The asker ([`pull`]) applies each page through its store (LWW +
//!   selective filters), files `file` / `secret` ops as "to fetch", saves
//!   `peer_cursor:<source>` = `next`, and asks again until `!more`; then it
//!   reads the content of the files it took with [`FILE_READ`]
//!   (`file_read{kind, path, sha256, size, offset, len}` → `file_chunk{data,
//!   total}`), [`peer::CHUNK_WINDOW`] requests in flight ([`PeerFiles`]).
//! - The source serves both through its store ([`serve`]); the engine wires
//!   that to incoming requests, [`FeedHandler`] does the same for in-memory
//!   links in tests.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::files::{FileSource, TBL as FILE_TBL};
use super::op::Op;
use super::peer::{decode_chunk, encode_chunk, PeerLink, CHUNK_SIZE};
#[cfg(test)]
use super::peer::{PeerHandler, Reply};
use super::store::{is_extra_tbl, SyncStore};

/// Request: one page of the source's change feed.
pub const CHANGES: &str = "changes";
/// Reply to [`CHANGES`]: `{ops, next, more}`.
pub const CHANGES_PAGE: &str = "changes_page";
/// Request: a range of a file's content.
pub const FILE_READ: &str = "file_read";
/// Reply to [`FILE_READ`]: `{data, total}`.
pub const FILE_CHUNK: &str = "file_chunk";
/// Error of [`FILE_READ`]: the file is another version now.
pub const FILE_CHANGED: &str = "file_changed";
/// Error of [`FILE_READ`]: the file is not there (any more).
pub const FILE_GONE: &str = "file_gone";
/// Non-last reply while a request is still being worked on (no body).
pub const STILL_WORKING: &str = "still_working";
/// Most ops per page.
pub const MAX_OPS: usize = 500;
/// Most op bytes (JSON) per page; a single bigger op still goes alone.
pub const MAX_BYTES: usize = 1 << 20;

/// Which local files the source rescans before answering (file ops already
/// in its feed are always listed: they are metadata, and skipping them
/// would move the asker's cursor past them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileScope {
    /// Every tracked file (library, keys, run logs, CLI transcripts).
    #[default]
    All,
    /// Everything but run logs and CLI transcripts (the bulk of the data).
    NoSessions,
    /// No file scan (and no Keychain scan) at all.
    Nothing,
}

/// Body of [`CHANGES`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChangesReq {
    pub since: u64,
    #[serde(default = "max_ops")]
    pub max_ops: usize,
    #[serde(default = "max_bytes")]
    pub max_bytes: usize,
    /// Data categories the asker does not take (`prefs::CATEGORIES`).
    #[serde(default)]
    pub skip_categories: Vec<String>,
    /// Projects the asker does not take.
    #[serde(default)]
    pub skip_projects: Vec<String>,
    #[serde(default)]
    pub files: FileScope,
}

fn max_ops() -> usize {
    MAX_OPS
}

fn max_bytes() -> usize {
    MAX_BYTES
}

impl ChangesReq {
    pub fn new(since: u64) -> Self {
        ChangesReq {
            since,
            max_ops: MAX_OPS,
            max_bytes: MAX_BYTES,
            skip_categories: Vec::new(),
            skip_projects: Vec::new(),
            files: FileScope::All,
        }
    }

    /// Within the contract's limits (at least one op per page).
    pub fn clamped(mut self) -> Self {
        self.max_ops = self.max_ops.clamp(1, MAX_OPS);
        self.max_bytes = self.max_bytes.clamp(1, MAX_BYTES);
        self
    }
}

/// Body of [`CHANGES_PAGE`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangesPage {
    pub ops: Vec<Op>,
    /// The cursor to ask from next (the seq of the last entry looked at).
    pub next: u64,
    pub more: bool,
}

/// Body of [`FILE_READ`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileReadReq {
    pub kind: String,
    pub path: String,
    pub sha256: String,
    pub size: u64,
    pub offset: u64,
    pub len: u64,
}

/// Phases of a manual sync, as emitted on `sync://run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Asking the source (first page).
    Requesting,
    /// Pages arriving / file contents being read.
    Pulling,
    Applying,
    Done,
    Error,
}

/// Progress of a manual sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RunProgress {
    /// Pages of changes received so far.
    pub pages: u64,
    /// Ops received so far.
    pub ops: u64,
    /// File content bytes read so far / to read.
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub files_done: u64,
    pub files_total: u64,
}

/// Outcome of a manual sync.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SyncRunReport {
    /// Ops received from the source.
    pub pulled: usize,
    pub applied: usize,
    /// The source's feed position this device is at now.
    pub last_seq: u64,
    pub pages: u64,
    /// File content bytes read.
    pub bytes: u64,
    /// File / Keychain items still waiting to be fetched (retried next Sync).
    pub pending_files: usize,
}

/// What a pull fetches once the pages are in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PullOptions {
    /// Scan scope asked of the source.
    pub files: FileScope,
    /// Which pending file contents to read afterwards (`Nothing`: leave every
    /// file op for a later Sync; Keychain items still apply).
    pub fetch: FileScope,
}

impl PullOptions {
    pub fn all() -> Self {
        PullOptions { files: FileScope::All, fetch: FileScope::All }
    }

    /// Rows only (leader hand-off): no file scan, no file transfer.
    pub fn rows_only() -> Self {
        PullOptions { files: FileScope::Nothing, fetch: FileScope::Nothing }
    }
}

pub type ProgressFn<'a> = &'a (dyn Fn(RunPhase, &RunProgress) + Send + Sync);

/// Wait for the one reply of kind `want` to a request (keepalives skipped).
async fn reply_of(mut replies: super::peer::Replies, want: &str) -> Result<Value, String> {
    loop {
        match replies.recv().await {
            Some(Ok((kind, body))) if kind == want => return Ok(body),
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(e),
            None => return Err("peer connection lost".into()),
        }
    }
}

/// [`FileSource`] reading from `source` over `link`; counts what it read.
pub struct PeerFiles {
    link: Arc<dyn PeerLink>,
    source: String,
    pub bytes: AtomicU64,
}

impl PeerFiles {
    pub fn new(link: Arc<dyn PeerLink>, source: &str) -> Self {
        PeerFiles { link, source: source.to_string(), bytes: AtomicU64::new(0) }
    }
}

#[async_trait]
impl FileSource for PeerFiles {
    async fn read(&self, req: FileReadReq) -> Result<Vec<u8>, String> {
        let body = serde_json::to_value(&req).map_err(|e| e.to_string())?;
        let replies = self.link.request(&self.source, FILE_READ, body).await?;
        let chunk = reply_of(replies, FILE_CHUNK).await?;
        let data = decode_chunk(chunk.get("data").and_then(Value::as_str).ok_or("file_chunk without data")?)?;
        self.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
        Ok(data)
    }
}

/// The asking side of a manual sync from `source`: pull its change feed page
/// by page from this device's cursor, applying each page, then read the
/// content of the files taken (per `opts`).
pub async fn pull(
    link: Arc<dyn PeerLink>,
    store: &dyn SyncStore,
    source: &str,
    opts: PullOptions,
    progress: ProgressFn<'_>,
) -> Result<SyncRunReport, String> {
    let (skip_categories, skip_projects) = store.ask_filter().await?;
    let mut cursor = store.cursor_start(source).await?;
    let mut report = SyncRunReport::default();
    let mut shown = RunProgress::default();
    loop {
        progress(if report.pages == 0 { RunPhase::Requesting } else { RunPhase::Pulling }, &shown);
        let req = ChangesReq {
            skip_categories: skip_categories.clone(),
            skip_projects: skip_projects.clone(),
            files: opts.files,
            ..ChangesReq::new(cursor)
        };
        let body = serde_json::to_value(&req).map_err(|e| e.to_string())?;
        let page = reply_of(link.request(source, CHANGES, body).await?, CHANGES_PAGE).await?;
        let page: ChangesPage = serde_json::from_value(page).map_err(|e| format!("bad changes page: {e}"))?;
        report.pages += 1;
        report.pulled += page.ops.len();
        shown.pages = report.pages;
        shown.ops = report.pulled as u64;
        if !page.ops.is_empty() {
            progress(RunPhase::Applying, &shown);
        }
        let (extra, rows): (Vec<Op>, Vec<Op>) = page.ops.into_iter().partition(|o| is_extra_tbl(&o.tbl));
        if !rows.is_empty() {
            let r = store.apply_remote(rows).await?;
            report.applied += r.applied;
            tracing::debug!(event = "sync_applied", applied = r.applied, skipped = r.skipped_older, deferred = r.deferred);
        }
        if !extra.is_empty() {
            // Filed first, fetched after the pages: the cursor may move on.
            store.park_extra(extra).await?;
        }
        if page.next < cursor || (page.more && page.next == cursor) {
            return Err(format!("the source's change feed did not move on (at {cursor})"));
        }
        if page.next != cursor {
            store.set_cursor(source, page.next).await?;
            cursor = page.next;
        }
        if !page.more {
            break;
        }
    }
    report.last_seq = cursor;
    let files = Arc::new(PeerFiles::new(link.clone(), source));
    let (total, count) = store.extra_to_fetch(opts.fetch).await?;
    shown.total_bytes = total;
    shown.files_total = count as u64;
    if count > 0 {
        progress(RunPhase::Pulling, &shown);
    }
    let done_files = AtomicUsize::new(0);
    let on_item = |bytes: u64| {
        let mut p = shown.clone();
        p.files_done = done_files.fetch_add(1, Ordering::Relaxed) as u64 + 1;
        p.done_bytes = bytes;
        progress(RunPhase::Pulling, &p);
    };
    let fetched = store.fetch_extra(files.as_ref(), opts.fetch, &|| on_item(files.bytes.load(Ordering::Relaxed))).await?;
    report.applied += fetched.applied;
    report.bytes = files.bytes.load(Ordering::Relaxed);
    report.pending_files = store.extra_to_fetch(FileScope::All).await?.1;
    Ok(report)
}

/// Serve one request of the feed on the source: `Ok((reply kind, body))`.
pub async fn serve(store: &dyn SyncStore, from: &str, kind: &str, body: Value) -> Result<(&'static str, Value), String> {
    match kind {
        CHANGES => {
            let req: ChangesReq = serde_json::from_value(body).map_err(|e| format!("bad_request: {e}"))?;
            let page = store.changes(from, &req.clamped()).await?;
            Ok((CHANGES_PAGE, serde_json::to_value(page).map_err(|e| e.to_string())?))
        }
        FILE_READ => {
            let req: FileReadReq = serde_json::from_value(body).map_err(|e| format!("bad_request: {e}"))?;
            if req.len > CHUNK_SIZE as u64 {
                return Err("bad_request: range".into());
            }
            let total = req.size;
            let data = store.read_file(&req).await?;
            Ok((FILE_CHUNK, json!({ "data": encode_chunk(&data), "total": total })))
        }
        other => Err(format!("unknown request kind {other}")),
    }
}

/// [`PeerHandler`] serving the feed from a store (in-memory links in tests;
/// the engine serves the same kinds itself, with keepalives).
#[cfg(test)]
pub struct FeedHandler(pub Arc<dyn SyncStore>);

#[cfg(test)]
#[async_trait]
impl PeerHandler for FeedHandler {
    fn kinds(&self) -> &'static [&'static str] {
        &[CHANGES, FILE_READ]
    }

    async fn handle(&self, _link: Arc<dyn PeerLink>, from: String, kind: String, body: Value, reply: Arc<dyn Reply>) {
        let sent = match serve(self.0.as_ref(), &from, &kind, body).await {
            Ok((k, b)) => reply.send(k, b, true).await,
            Err(e) => reply.error(e).await,
        };
        if let Err(e) = sent {
            tracing::debug!(event = "sync_feed_reply_failed", error = %e);
        }
    }
}

/// `file` ops are counted as bytes to read by the asker's progress.
pub fn file_op_size(op: &Op) -> u64 {
    if op.tbl != FILE_TBL {
        return 0;
    }
    op.row.as_ref().and_then(|r| r.get("size")).and_then(Value::as_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::codec::PathCtx;
    use crate::sync::files::tests::{mem_db, tempdir, TempDir};
    use crate::sync::peer::testing::{pair, MemLink};
    use crate::sync::store::SqliteSyncStore;

    struct Side {
        db: crate::db::Db,
        store: Arc<SqliteSyncStore>,
        link: Arc<MemLink>,
        _home: TempDir,
    }

    impl Side {
        async fn pull(&self, from: &str) -> SyncRunReport {
            let link: Arc<dyn PeerLink> = self.link.clone();
            pull(link, self.store.as_ref(), from, PullOptions::all(), &|_, _| {}).await.unwrap()
        }
        async fn exec(&self, sql: &str) {
            sqlx::query(sql).execute(&self.db).await.unwrap();
        }
        async fn one(&self, sql: &str) -> String {
            sqlx::query_scalar(sql).fetch_one(&self.db).await.unwrap()
        }
    }

    /// Two devices wired through in-memory links, each serving its feed.
    async fn sides() -> (Side, Side) {
        let (la, lb) = pair("dev-a", "dev-b");
        let mut out = Vec::new();
        for (id, link) in [("dev-a", la), ("dev-b", lb)] {
            let home = tempdir();
            let db = mem_db().await;
            let paths = PathCtx::with_home(home.path()).with_app_data(home.path().join("data"));
            let store = Arc::new(SqliteSyncStore::new(db.clone(), id.into()).with_paths(paths));
            link.add_handler(Arc::new(FeedHandler(store.clone())));
            out.push(Side { db, store, link, _home: home });
        }
        let b = out.pop().unwrap();
        (out.pop().unwrap(), b)
    }

    /// Finding #1 with two devices: B edits a row while offline (older), A
    /// edits it later; whoever pulls first, both end with A's newer edit.
    #[tokio::test]
    async fn offline_edits_converge_on_the_newer_one_either_way() {
        for b_first in [true, false] {
            let (a, b) = sides().await;
            a.exec("INSERT INTO todos (id, text, position, created_at) VALUES ('t1', 'v0', 0, 'x')").await;
            b.pull("dev-a").await;
            b.exec("UPDATE todos SET text = 'from B (older)' WHERE id = 't1'").await;
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            a.exec("UPDATE todos SET text = 'from A (newer)' WHERE id = 't1'").await;
            if b_first {
                b.pull("dev-a").await;
                a.pull("dev-b").await;
            } else {
                a.pull("dev-b").await;
                b.pull("dev-a").await;
            }
            for (name, side) in [("a", &a), ("b", &b)] {
                assert_eq!(side.one("SELECT text FROM todos WHERE id = 't1'").await, "from A (newer)", "{name} (b_first {b_first})");
            }
        }
    }

    /// Finding N4 with two devices: A owns a run and is cut off; B takes it
    /// over (epoch 1); A — not knowing — resumes and renews its lease (newer
    /// HLCs, epoch 0). Whichever pulls first, both end with B as the owner,
    /// and A stops renewing.
    #[tokio::test]
    async fn a_take_over_during_a_split_wins_over_the_old_owners_renewals() {
        use crate::sync::sessions;
        for a_first in [false, true] {
            let (a, b) = sides().await;
            let id_b = crate::sync::ensure_device(&b.db).await.unwrap();
            let project = std::env::temp_dir().join(format!("devdy-split-{}", uuid::Uuid::new_v4().simple()));
            let path = project.to_string_lossy().to_string();
            a.exec(&format!("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', '{path}', 'x')")).await;
            a.exec("INSERT INTO runs (id, project_id, type, status, engine, created_at) VALUES ('r1', 'p1', 'session', 'done', 'claude', 'x')").await;
            sessions::claim(&a.db, "r1", &path).await.unwrap();
            sessions::release(&a.db, "r1").await;
            b.pull("dev-a").await;

            let taken = sessions::take_over(&b.db, "r1", false, true, true).await.unwrap();
            assert!(taken.taken);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            sessions::claim(&a.db, "r1", &path).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            sessions::renew(&a.db, &["r1".to_string()]).await.unwrap();

            if a_first {
                a.pull("dev-b").await;
                b.pull("dev-a").await;
            } else {
                b.pull("dev-a").await;
                a.pull("dev-b").await;
            }
            let owner = "SELECT owner_device || '/' || owner_epoch || '/' || COALESCE(lease_until, '-') FROM runs WHERE id = 'r1'";
            for (name, side) in [("a", &a), ("b", &b)] {
                assert_eq!(side.one(owner).await, format!("{id_b}/1/-"), "{name} (a_first {a_first})");
            }
            sessions::renew(&a.db, &["r1".to_string()]).await.unwrap();
            assert_eq!(a.one(owner).await, format!("{id_b}/1/-"), "A no longer renews a run it lost");
        }
    }

    /// A malformed or out-of-range request is refused, not served.
    #[tokio::test]
    async fn bad_requests_are_refused() {
        let (a, _b) = sides().await;
        let store: &dyn SyncStore = a.store.as_ref();
        assert!(serve(store, "dev-b", CHANGES, json!({ "since": "x" })).await.unwrap_err().starts_with("bad_request"));
        let big = json!({ "kind": "run_log", "path": "~/x", "sha256": "0", "size": 9, "offset": 0, "len": CHUNK_SIZE + 1 });
        assert!(serve(store, "dev-b", FILE_READ, big).await.unwrap_err().starts_with("bad_request"));
        assert!(serve(store, "dev-b", "nope", json!({})).await.is_err());
        // Limits are clamped: at most MAX_OPS per page, at least one.
        let req = ChangesReq { max_ops: 0, max_bytes: usize::MAX, ..ChangesReq::new(0) }.clamped();
        assert_eq!((req.max_ops, req.max_bytes), (1, MAX_BYTES));
        let (kind, page) = serve(store, "dev-b", CHANGES, json!({ "since": 0, "max_ops": 100000 })).await.unwrap();
        assert_eq!(kind, CHANGES_PAGE);
        assert!(serde_json::from_value::<ChangesPage>(page).is_ok());
    }
}
