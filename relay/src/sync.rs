//! Two-way sync store (design `two-way-sync` §3 / §9.2).
//!
//! Devices of a sync group push sealed ops and pull them back by cursor; the
//! relay keeps them in SQLite (`RELAY_SYNC_DB`) so devices never have to be
//! online at the same time. Everything the relay stores is ciphertext: an op
//! is an opaque base64 `blob` plus a keyed hash `key` (later ops with the same
//! key supersede earlier ones), a file is an opaque blob addressed by the
//! sha256 of its sealed bytes. Op blobs and blob data are NEVER logged.
//!
//! A sync connection opens with `sync_hello` (shared `RELAY_HOST_TOKEN` +
//! per-device token, TOFU on first hello) and from then on carries only sync /
//! blob frames and keepalives. After a push, the other connected devices of
//! the group get `sync_notify` so they pull right away.
//!
//! Blob GC: ops are sealed, so the relay can't tell which blobs are still
//! referenced. The device that uploaded a blob tells it instead
//! (`blob_release`, once the op that stopped using it is stored); a released
//! blob is deleted [`BLOB_RELEASE_GRACE_SECS`] later, lazily on the group's
//! next `blob_release` or hello. Until then it can still be fetched (a device
//! that pulled the older op just before it was superseded).
//!
//! SQLite is synchronous: every store call runs on the blocking pool, behind a
//! single connection mutex (which also serialises seq assignment per group).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::protocol::{ErrorCode, FrameType};
use crate::room::{ConnId, Outbound};

/// Most ops returned by one `sync_pull`, whatever `limit` asks for.
pub const PULL_MAX_OPS: usize = 500;
/// Soft byte budget of one `sync_ops` page (sum of op blobs). A page always
/// carries at least one op, so a single op larger than this still gets through.
pub const PULL_MAX_BYTES: usize = 8 * 1024 * 1024;
/// Longest accepted `group_id` / `device_id` / op `key`.
const MAX_ID_LEN: usize = 128;
/// How long a released blob stays fetchable before it is deleted.
pub const BLOB_RELEASE_GRACE_SECS: i64 = 24 * 3600;
/// Most shas accepted by one `blob_release`.
const MAX_RELEASE: usize = 1000;

/// Frames handled by the sync module.
pub fn is_sync_frame(t: FrameType) -> bool {
    matches!(
        t,
        FrameType::SyncHello
            | FrameType::SyncWelcome
            | FrameType::SyncPush
            | FrameType::SyncAck
            | FrameType::SyncPull
            | FrameType::SyncOps
            | FrameType::SyncNotify
            | FrameType::SyncRevoke
            | FrameType::BlobPut
            | FrameType::BlobAck
            | FrameType::BlobGet
            | FrameType::BlobData
            | FrameType::BlobRelease
            | FrameType::BlobReleased
    )
}

/// Sync frames that carry data and are therefore exempt from the control-frame
/// rate limit (an initial upload legitimately sends thousands of them).
pub fn is_sync_data_frame(t: FrameType) -> bool {
    matches!(
        t,
        FrameType::SyncPush
            | FrameType::SyncPull
            | FrameType::BlobPut
            | FrameType::BlobGet
            | FrameType::BlobRelease
    )
}

/// One inbound sync frame. Unknown fields are ignored; `id`, when present, is
/// echoed on the response (or error) so a client can correlate them.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Request {
    t: Option<FrameType>,
    id: Option<Value>,
    auth_token: Option<String>,
    group_id: Option<String>,
    device_id: Option<String>,
    device_token: Option<String>,
    create: Option<bool>,
    items: Option<Vec<PushItem>>,
    after_seq: Option<u64>,
    limit: Option<u64>,
    sha: Option<String>,
    data: Option<String>,
    shas: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct PushItem {
    pub key: String,
    pub blob: String,
}

/// One op as returned by a pull.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PulledOp {
    pub seq: u64,
    pub device_id: String,
    pub blob: String,
}

/// A page of ops after a cursor.
#[derive(Debug)]
pub struct PullPage {
    pub items: Vec<PulledOp>,
    /// Cursor to pull after next: the last returned seq while `more`, else the
    /// group head.
    pub last_seq: u64,
    pub more: bool,
}

/// Store failure: a wire error code, or a database error (logged, answered
/// with `internal`).
#[derive(Debug)]
pub enum SyncError {
    Code(ErrorCode, &'static str),
    Db(rusqlite::Error),
}

impl From<rusqlite::Error> for SyncError {
    fn from(e: rusqlite::Error) -> Self {
        SyncError::Db(e)
    }
}

type SyncResult<T> = Result<T, SyncError>;

fn bad(msg: &'static str) -> SyncError {
    SyncError::Code(ErrorCode::BadRequest, msg)
}

/// Limits enforced by the store.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_group_bytes: u64,
    pub max_blob_bytes: usize,
}

impl Limits {
    /// An op blob is base64 text; it may be as large as a maximum blob encoded.
    fn max_op_chars(&self) -> usize {
        self.max_blob_bytes.div_ceil(3) * 4
    }
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS groups (
    id          TEXT PRIMARY KEY,
    created_at  INTEGER NOT NULL,
    last_seq    INTEGER NOT NULL DEFAULT 0,
    bytes       INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS devices (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    device_id   TEXT NOT NULL,
    token_hash  TEXT NOT NULL,
    revoked     INTEGER NOT NULL DEFAULT 0,
    created_at  INTEGER NOT NULL,
    PRIMARY KEY (group_id, device_id)
);
CREATE TABLE IF NOT EXISTS ops (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    seq         INTEGER NOT NULL,
    device_id   TEXT NOT NULL,
    key         TEXT NOT NULL,
    blob        TEXT NOT NULL,
    PRIMARY KEY (group_id, seq),
    UNIQUE (group_id, key)
);
CREATE TABLE IF NOT EXISTS blobs (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    sha         TEXT NOT NULL,
    data        BLOB NOT NULL,
    -- Uploader (NULL: stored before blob GC existed, any device may release).
    device_id   TEXT,
    -- Unix secs of the uploader's blob_release; deleted after the grace period.
    released_at INTEGER,
    PRIMARY KEY (group_id, sha)
);
";

/// Columns added after the first release of the store (existing databases).
const BLOB_COLUMNS: &[(&str, &str)] = &[("device_id", "TEXT"), ("released_at", "INTEGER")];

/// Open (creating if needed) the sync database.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    upgrade(&conn)?;
    Ok(conn)
}

fn upgrade(conn: &Connection) -> rusqlite::Result<()> {
    let have: Vec<String> = conn
        .prepare("SELECT name FROM pragma_table_info('blobs')")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for (col, ty) in BLOB_COLUMNS {
        if !have.iter().any(|c| c == col) {
            conn.execute_batch(&format!("ALTER TABLE blobs ADD COLUMN {col} {ty};"))?;
        }
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_blobs_released ON blobs(group_id, released_at) WHERE released_at IS NOT NULL;",
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= MAX_ID_LEN
}

fn valid_sha(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn group_head(conn: &Connection, group: &str) -> SyncResult<(u64, u64)> {
    let row: Option<(i64, i64)> = conn
        .query_row(
            "SELECT last_seq, bytes FROM groups WHERE id = ?1",
            params![group],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (seq, bytes) = row.ok_or(SyncError::Code(ErrorCode::GroupNotFound, "unknown sync group"))?;
    Ok((seq as u64, bytes as u64))
}

/// The device exists in the group and is not revoked.
fn ensure_active(conn: &Connection, group: &str, device: &str) -> SyncResult<()> {
    let revoked: Option<bool> = conn
        .query_row(
            "SELECT revoked FROM devices WHERE group_id = ?1 AND device_id = ?2",
            params![group, device],
            |r| r.get(0),
        )
        .optional()?;
    match revoked {
        Some(false) => Ok(()),
        Some(true) => Err(SyncError::Code(ErrorCode::DeviceRevoked, "device revoked")),
        None => Err(SyncError::Code(ErrorCode::AuthFailed, "unknown device")),
    }
}

/// `sync_hello`: create the group (`create`), register an unknown device
/// (TOFU on the token hash) or check a known one. Returns the group head.
pub fn hello(
    conn: &mut Connection,
    group: &str,
    device: &str,
    token_hash: &str,
    create: bool,
) -> SyncResult<u64> {
    let tx = conn.transaction()?;
    let exists: bool = tx
        .query_row("SELECT 1 FROM groups WHERE id = ?1", params![group], |_| Ok(()))
        .optional()?
        .is_some();
    if !exists {
        if !create {
            return Err(SyncError::Code(ErrorCode::GroupNotFound, "unknown sync group"));
        }
        tx.execute(
            "INSERT INTO groups (id, created_at) VALUES (?1, ?2)",
            params![group, now_secs()],
        )?;
    }
    let known: Option<(String, bool)> = tx
        .query_row(
            "SELECT token_hash, revoked FROM devices WHERE group_id = ?1 AND device_id = ?2",
            params![group, device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match known {
        None => {
            tx.execute(
                "INSERT INTO devices (group_id, device_id, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![group, device, token_hash, now_secs()],
            )?;
        }
        Some((_, true)) => return Err(SyncError::Code(ErrorCode::DeviceRevoked, "device revoked")),
        Some((stored, false)) => {
            if !crate::server::constant_time_eq(&stored, token_hash) {
                return Err(SyncError::Code(ErrorCode::AuthFailed, "invalid device token"));
            }
        }
    }
    gc_blobs(&tx, group, now_secs())?;
    let (head, _) = group_head(&tx, group)?;
    tx.commit()?;
    Ok(head)
}

/// `sync_push`: assign seqs, drop older ops with the same key, enforce the
/// group quota. All or nothing. Returns the new head.
pub fn push(
    conn: &mut Connection,
    limits: Limits,
    group: &str,
    device: &str,
    items: &[PushItem],
) -> SyncResult<u64> {
    for it in items {
        if !valid_id(&it.key) || it.blob.is_empty() {
            return Err(bad("each item needs a key and a blob"));
        }
        if it.blob.len() > limits.max_op_chars() {
            return Err(SyncError::Code(ErrorCode::TooLarge, "op too large"));
        }
    }
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    let (mut seq, mut bytes) = group_head(&tx, group)?;
    for it in items {
        let old: Option<i64> = tx
            .query_row(
                "SELECT length(CAST(blob AS BLOB)) FROM ops WHERE group_id = ?1 AND key = ?2",
                params![group, it.key],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(len) = old {
            tx.execute(
                "DELETE FROM ops WHERE group_id = ?1 AND key = ?2",
                params![group, it.key],
            )?;
            bytes = bytes.saturating_sub(len as u64);
        }
        bytes += it.blob.len() as u64;
        if bytes > limits.max_group_bytes {
            return Err(SyncError::Code(ErrorCode::QuotaExceeded, "group storage quota exceeded"));
        }
        seq += 1;
        tx.execute(
            "INSERT INTO ops (group_id, seq, device_id, key, blob) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![group, seq as i64, device, it.key, it.blob],
        )?;
    }
    tx.execute(
        "UPDATE groups SET last_seq = ?2, bytes = ?3 WHERE id = ?1",
        params![group, seq as i64, bytes as i64],
    )?;
    tx.commit()?;
    Ok(seq)
}

/// `sync_pull`: ops with `seq > after_seq`, at most `limit` (capped at
/// [`PULL_MAX_OPS`]) and about [`PULL_MAX_BYTES`].
pub fn pull(conn: &mut Connection, group: &str, device: &str, after_seq: u64, limit: usize) -> SyncResult<PullPage> {
    let limit = if limit == 0 { PULL_MAX_OPS } else { limit.min(PULL_MAX_OPS) };
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    let (head, _) = group_head(&tx, group)?;
    let mut items = Vec::new();
    let mut more = false;
    {
        let mut stmt = tx.prepare(
            "SELECT seq, device_id, blob FROM ops WHERE group_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let mut rows = stmt.query(params![group, after_seq.min(i64::MAX as u64) as i64])?;
        let mut page_bytes = 0usize;
        while let Some(row) = rows.next()? {
            let blob: String = row.get(2)?;
            if items.len() >= limit || (!items.is_empty() && page_bytes + blob.len() > PULL_MAX_BYTES) {
                more = true;
                break;
            }
            page_bytes += blob.len();
            items.push(PulledOp {
                seq: row.get::<_, i64>(0)? as u64,
                device_id: row.get(1)?,
                blob,
            });
        }
    }
    tx.commit()?;
    let last_seq = match (more, items.last()) {
        (true, Some(last)) => last.seq,
        _ => head,
    };
    Ok(PullPage { items, last_seq, more })
}

/// `blob_put`: store a sealed blob under its sha256 (idempotent).
pub fn blob_put(
    conn: &mut Connection,
    limits: Limits,
    group: &str,
    device: &str,
    sha: &str,
    data_b64: &str,
) -> SyncResult<()> {
    if !valid_sha(sha) {
        return Err(bad("sha must be 64 lowercase hex chars"));
    }
    if data_b64.len() > limits.max_op_chars() {
        return Err(SyncError::Code(ErrorCode::TooLarge, "blob too large"));
    }
    let data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|_| bad("data is not base64"))?;
    if data.len() > limits.max_blob_bytes {
        return Err(SyncError::Code(ErrorCode::TooLarge, "blob too large"));
    }
    if sha256_hex(&data) != sha {
        return Err(SyncError::Code(ErrorCode::ShaMismatch, "data does not match sha"));
    }
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    let exists = tx
        .query_row(
            "SELECT 1 FROM blobs WHERE group_id = ?1 AND sha = ?2",
            params![group, sha],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if exists {
        // Uploaded again: in use after all.
        tx.execute(
            "UPDATE blobs SET released_at = NULL WHERE group_id = ?1 AND sha = ?2",
            params![group, sha],
        )?;
    } else {
        let (_, bytes) = group_head(&tx, group)?;
        let bytes = bytes + data.len() as u64;
        if bytes > limits.max_group_bytes {
            return Err(SyncError::Code(ErrorCode::QuotaExceeded, "group storage quota exceeded"));
        }
        tx.execute(
            "INSERT INTO blobs (group_id, sha, data, device_id) VALUES (?1, ?2, ?3, ?4)",
            params![group, sha, data, device],
        )?;
        tx.execute("UPDATE groups SET bytes = ?2 WHERE id = ?1", params![group, bytes as i64])?;
    }
    tx.commit()?;
    Ok(())
}

/// `blob_get`: the stored bytes, base64.
pub fn blob_get(conn: &mut Connection, group: &str, device: &str, sha: &str) -> SyncResult<String> {
    ensure_active(conn, group, device)?;
    let data: Option<Vec<u8>> = conn
        .query_row(
            "SELECT data FROM blobs WHERE group_id = ?1 AND sha = ?2",
            params![group, sha],
            |r| r.get(0),
        )
        .optional()?;
    let data = data.ok_or(SyncError::Code(ErrorCode::BlobNotFound, "blob not found"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(data))
}

/// `blob_release{shas}`: the uploader no longer references these blobs. Marks
/// the ones it uploaded (unknown / other devices' shas are ignored), then
/// deletes the group's blobs released more than the grace period before `now`.
/// Returns how many were marked.
pub fn blob_release(conn: &mut Connection, group: &str, device: &str, shas: &[String], now: i64) -> SyncResult<usize> {
    if shas.len() > MAX_RELEASE {
        return Err(SyncError::Code(ErrorCode::TooLarge, "too many shas"));
    }
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    let mut marked = 0;
    for sha in shas.iter().filter(|s| valid_sha(s)) {
        marked += tx.execute(
            "UPDATE blobs SET released_at = ?4 WHERE group_id = ?1 AND sha = ?2 AND released_at IS NULL \
             AND (device_id IS NULL OR device_id = ?3)",
            params![group, sha, device, now],
        )?;
    }
    gc_blobs(&tx, group, now)?;
    tx.commit()?;
    Ok(marked)
}

/// Delete the group's blobs released before `now - grace`, freeing their bytes.
fn gc_blobs(conn: &Connection, group: &str, now: i64) -> SyncResult<usize> {
    let cutoff = now - BLOB_RELEASE_GRACE_SECS;
    let freed: i64 = conn.query_row(
        "SELECT COALESCE(SUM(length(data)), 0) FROM blobs WHERE group_id = ?1 AND released_at IS NOT NULL AND released_at <= ?2",
        params![group, cutoff],
        |r| r.get(0),
    )?;
    let n = conn.execute(
        "DELETE FROM blobs WHERE group_id = ?1 AND released_at IS NOT NULL AND released_at <= ?2",
        params![group, cutoff],
    )?;
    if n > 0 {
        conn.execute(
            "UPDATE groups SET bytes = MAX(0, bytes - ?2) WHERE id = ?1",
            params![group, freed],
        )?;
    }
    Ok(n)
}

/// `sync_revoke`: mark `target` revoked. A device not seen yet is recorded as
/// revoked, so it can never register later.
pub fn revoke(conn: &mut Connection, group: &str, device: &str, target: &str) -> SyncResult<()> {
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    tx.execute(
        "INSERT INTO devices (group_id, device_id, token_hash, revoked, created_at) VALUES (?1, ?2, '', 1, ?3)
         ON CONFLICT (group_id, device_id) DO UPDATE SET revoked = 1",
        params![group, target, now_secs()],
    )?;
    tx.commit()?;
    Ok(())
}

/// The identity a connection proved with `sync_hello`.
#[derive(Debug, Clone)]
pub struct SyncSession {
    pub group_id: String,
    pub device_id: String,
}

struct LiveConn {
    conn: ConnId,
    device_id: String,
    out: Outbound,
}

/// Sync state shared by all connections: the store (absent = disabled) and
/// the live sync connections per group, for `sync_notify` and revocation.
pub struct SyncHub {
    db: Option<Arc<Mutex<Connection>>>,
    limits: Limits,
    live: Mutex<HashMap<String, Vec<LiveConn>>>,
}

impl SyncHub {
    /// Open the store at `path`, or build a disabled hub when `None`.
    pub fn open(path: Option<&Path>, limits: Limits) -> rusqlite::Result<Self> {
        let db = match path {
            Some(p) => Some(Arc::new(Mutex::new(open_db(p)?))),
            None => None,
        };
        Ok(SyncHub {
            db,
            limits,
            live: Mutex::new(HashMap::new()),
        })
    }

    pub fn enabled(&self) -> bool {
        self.db.is_some()
    }

    /// Run `f` against the database on the blocking pool.
    async fn call<T, F>(&self, f: F) -> SyncResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> SyncResult<T> + Send + 'static,
    {
        let db = self
            .db
            .clone()
            .ok_or(SyncError::Code(ErrorCode::SyncDisabled, "sync is disabled on this relay"))?;
        tokio::task::spawn_blocking(move || {
            let mut conn = db.lock().unwrap_or_else(|p| p.into_inner());
            f(&mut conn)
        })
        .await
        .unwrap_or(Err(SyncError::Code(ErrorCode::Internal, "sync store task failed")))
    }

    /// Handle one sync frame (`text` is the raw frame, already known to be a
    /// sync frame). `session` is this connection's identity once welcomed.
    /// Returns `true` when the connection must close.
    pub async fn handle(
        &self,
        conn_id: ConnId,
        session: &mut Option<SyncSession>,
        out: &Outbound,
        text: &str,
        host_token: &str,
    ) -> bool {
        let req: Request = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(_) => {
                send_error(out, None, ErrorCode::BadRequest, "malformed sync frame");
                return false;
            }
        };
        let id = req.id.clone();
        let t = req.t;
        match t {
            Some(FrameType::SyncHello) => {
                if session.is_some() {
                    send_error(out, id, ErrorCode::Forbidden, "already welcomed");
                    return false;
                }
                match self.hello(conn_id, out, req, host_token).await {
                    Ok((s, head)) => {
                        tracing::info!(event = "sync_welcome", conn = conn_id, group = %s.group_id, device = %s.device_id);
                        send_json(out, json!({ "t": "sync_welcome", "last_seq": head }), id);
                        *session = Some(s);
                        false
                    }
                    Err(e) => {
                        tracing::warn!(event = "sync_hello_denied", conn = conn_id, code = error_code(&e).as_str());
                        send_sync_error(out, id, e);
                        true
                    }
                }
            }
            Some(t) if is_client_frame(t) => {
                let Some(s) = session.clone() else {
                    let code = if self.enabled() { ErrorCode::Forbidden } else { ErrorCode::SyncDisabled };
                    send_error(out, id, code, "send sync_hello first");
                    return false;
                };
                match self.request(conn_id, out, &s, t, req).await {
                    Ok(()) => false,
                    Err(e) => {
                        let revoked = matches!(e, SyncError::Code(ErrorCode::DeviceRevoked, _));
                        send_sync_error(out, id, e);
                        revoked
                    }
                }
            }
            // Relay → device frame types sent by a client: ignored.
            _ => false,
        }
    }

    async fn hello(
        &self,
        conn_id: ConnId,
        out: &Outbound,
        req: Request,
        host_token: &str,
    ) -> SyncResult<(SyncSession, u64)> {
        let token_ok = req
            .auth_token
            .as_deref()
            .map(|t| crate::server::constant_time_eq(t, host_token))
            .unwrap_or(false);
        if !token_ok {
            return Err(SyncError::Code(ErrorCode::AuthFailed, "invalid auth token"));
        }
        if !self.enabled() {
            return Err(SyncError::Code(ErrorCode::SyncDisabled, "sync is disabled on this relay"));
        }
        let group = req.group_id.unwrap_or_default();
        let device = req.device_id.unwrap_or_default();
        let token = req.device_token.unwrap_or_default();
        if !valid_id(&group) || !valid_id(&device) || token.is_empty() {
            return Err(bad("group_id, device_id and device_token are required"));
        }
        let token_hash = sha256_hex(token.as_bytes());
        let create = req.create.unwrap_or(false);
        let (g, d) = (group.clone(), device.clone());
        let head = self.call(move |c| hello(c, &g, &d, &token_hash, create)).await?;
        self.live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(group.clone())
            .or_default()
            .push(LiveConn {
                conn: conn_id,
                device_id: device.clone(),
                out: out.clone(),
            });
        Ok((
            SyncSession {
                group_id: group,
                device_id: device,
            },
            head,
        ))
    }

    async fn request(
        &self,
        conn_id: ConnId,
        out: &Outbound,
        s: &SyncSession,
        t: FrameType,
        req: Request,
    ) -> SyncResult<()> {
        let id = req.id.clone();
        let (g, d) = (s.group_id.clone(), s.device_id.clone());
        let limits = self.limits;
        match t {
            FrameType::SyncPush => {
                let items = req.items.ok_or_else(|| bad("missing items"))?;
                let n = items.len();
                let head = if n == 0 {
                    self.call(move |c| Ok(group_head(c, &g)?.0)).await?
                } else {
                    self.call(move |c| push(c, limits, &g, &d, &items)).await?
                };
                tracing::debug!(event = "sync_push", conn = conn_id, ops = n, last_seq = head);
                send_json(out, json!({ "t": "sync_ack", "last_seq": head }), id);
                if n > 0 {
                    self.notify(&s.group_id, &s.device_id, head);
                }
            }
            FrameType::SyncPull => {
                let after = req.after_seq.unwrap_or(0);
                let limit = req.limit.unwrap_or(0).min(PULL_MAX_OPS as u64) as usize;
                let page = self.call(move |c| pull(c, &g, &d, after, limit)).await?;
                tracing::debug!(event = "sync_pull", conn = conn_id, ops = page.items.len(), more = page.more);
                let items: Vec<Value> = page
                    .items
                    .into_iter()
                    .map(|op| json!({ "seq": op.seq, "device_id": op.device_id, "blob": op.blob }))
                    .collect();
                send_json(
                    out,
                    json!({ "t": "sync_ops", "items": items, "last_seq": page.last_seq, "more": page.more }),
                    id,
                );
            }
            FrameType::BlobPut => {
                let sha = req.sha.ok_or_else(|| bad("missing sha"))?;
                let data = req.data.ok_or_else(|| bad("missing data"))?;
                let sha2 = sha.clone();
                self.call(move |c| blob_put(c, limits, &g, &d, &sha2, &data)).await?;
                tracing::debug!(event = "blob_put", conn = conn_id);
                send_json(out, json!({ "t": "blob_ack", "sha": sha }), id);
            }
            FrameType::BlobGet => {
                let sha = req.sha.ok_or_else(|| bad("missing sha"))?;
                let sha2 = sha.clone();
                let data = self.call(move |c| blob_get(c, &g, &d, &sha2)).await?;
                send_json(out, json!({ "t": "blob_data", "sha": sha, "data": data }), id);
            }
            FrameType::BlobRelease => {
                let shas = req.shas.ok_or_else(|| bad("missing shas"))?;
                let n = self.call(move |c| blob_release(c, &g, &d, &shas, now_secs())).await?;
                tracing::debug!(event = "blob_release", conn = conn_id, released = n);
                send_json(out, json!({ "t": "blob_released", "released": n }), id);
            }
            FrameType::SyncRevoke => {
                let target = req.device_id.unwrap_or_default();
                if !valid_id(&target) {
                    return Err(bad("missing device_id"));
                }
                let t2 = target.clone();
                self.call(move |c| revoke(c, &g, &d, &t2)).await?;
                tracing::info!(event = "sync_revoke", conn = conn_id, group = %s.group_id, device = %target);
                send_json(out, json!({ "t": "control", "code": "ok" }), id);
                self.kick(&s.group_id, &target);
            }
            _ => {}
        }
        Ok(())
    }

    /// Tell every other device of the group that ops up to `last_seq` exist.
    fn notify(&self, group: &str, from_device: &str, last_seq: u64) {
        let text = json!({ "t": "sync_notify", "last_seq": last_seq }).to_string();
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        for c in live.get(group).into_iter().flatten() {
            if c.device_id != from_device {
                let _ = c.out.send_text(text.clone());
            }
        }
    }

    /// Close the live connections of a revoked device.
    fn kick(&self, group: &str, device: &str) {
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        for c in live.get(group).into_iter().flatten() {
            if c.device_id == device {
                send_error(&c.out, None, ErrorCode::DeviceRevoked, "device revoked");
                c.out.close();
            }
        }
    }

    /// Forget a closed connection.
    pub fn on_disconnect(&self, conn_id: ConnId, session: Option<&SyncSession>) {
        let Some(s) = session else { return };
        let mut live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(conns) = live.get_mut(&s.group_id) {
            conns.retain(|c| c.conn != conn_id);
            if conns.is_empty() {
                live.remove(&s.group_id);
            }
        }
    }
}

/// Frame types a device may send after `sync_hello`.
fn is_client_frame(t: FrameType) -> bool {
    is_sync_data_frame(t) || t == FrameType::SyncRevoke
}

fn error_code(e: &SyncError) -> ErrorCode {
    match e {
        SyncError::Code(c, _) => *c,
        SyncError::Db(_) => ErrorCode::Internal,
    }
}

fn send_json(out: &Outbound, mut v: Value, id: Option<Value>) {
    if let (Some(id), Some(obj)) = (id, v.as_object_mut()) {
        obj.insert("id".into(), id);
    }
    let _ = out.send_text(v.to_string());
}

fn send_error(out: &Outbound, id: Option<Value>, code: ErrorCode, message: &str) {
    send_json(out, json!({ "t": "error", "code": code.as_str(), "message": message }), id);
}

fn send_sync_error(out: &Outbound, id: Option<Value>, e: SyncError) {
    match e {
        SyncError::Code(code, msg) => send_error(out, id, code, msg),
        SyncError::Db(err) => {
            tracing::error!(event = "sync_db_error", error = %err);
            send_error(out, id, ErrorCode::Internal, "sync store error");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(SCHEMA).unwrap();
        upgrade(&c).unwrap();
        c
    }

    const LIMITS: Limits = Limits {
        max_group_bytes: 1000,
        max_blob_bytes: 100,
    };

    fn item(key: &str, blob: &str) -> PushItem {
        PushItem {
            key: key.into(),
            blob: blob.into(),
        }
    }

    #[test]
    fn supersede_keeps_byte_accounting() {
        let mut c = mem();
        hello(&mut c, "g", "a", "h", true).unwrap();
        assert_eq!(push(&mut c, LIMITS, "g", "a", &[item("k1", "aaaa"), item("k2", "bb")]).unwrap(), 2);
        assert_eq!(push(&mut c, LIMITS, "g", "a", &[item("k1", "c")]).unwrap(), 3);
        assert_eq!(group_head(&c, "g").unwrap(), (3, 3));
        let page = pull(&mut c, "g", "a", 0, 0).unwrap();
        let seqs: Vec<u64> = page.items.iter().map(|o| o.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
        assert!(!page.more);
        assert_eq!(page.last_seq, 3);
    }

    #[test]
    fn failed_push_is_all_or_nothing() {
        let mut c = mem();
        hello(&mut c, "g", "a", "h", true).unwrap();
        let big = "x".repeat(100);
        let items: Vec<PushItem> = (0..11).map(|i| item(&format!("k{i}"), &big)).collect();
        assert!(matches!(
            push(&mut c, LIMITS, "g", "a", &items),
            Err(SyncError::Code(ErrorCode::QuotaExceeded, _))
        ));
        assert_eq!(group_head(&c, "g").unwrap(), (0, 0));
    }

    fn put(c: &mut Connection, device: &str, data: &[u8]) -> String {
        let sha = sha256_hex(data);
        let b64 = base64::engine::general_purpose::STANDARD.encode(data);
        blob_put(c, LIMITS, "g", device, &sha, &b64).unwrap();
        sha
    }

    #[test]
    fn released_blobs_are_deleted_after_the_grace_period() {
        let mut c = mem();
        hello(&mut c, "g", "a", "h", true).unwrap();
        hello(&mut c, "g", "b", "h", false).unwrap();
        let old = put(&mut c, "a", b"tail v1");
        let kept = put(&mut c, "a", b"chunk 0");
        let theirs = put(&mut c, "b", b"other");
        assert_eq!(group_head(&c, "g").unwrap().1, 19);

        // Only the uploader releases; bad / unknown shas are ignored.
        let t0 = 1_000_000;
        let shas = vec![old.clone(), theirs.clone(), "nope".into(), "0".repeat(64)];
        assert_eq!(blob_release(&mut c, "g", "a", &shas, t0).unwrap(), 1);
        assert_eq!(blob_release(&mut c, "g", "a", &shas, t0).unwrap(), 0, "already released");
        // Still fetchable during the grace period.
        assert!(blob_get(&mut c, "g", "b", &old).is_ok());
        blob_release(&mut c, "g", "b", &[], t0 + BLOB_RELEASE_GRACE_SECS - 1).unwrap();
        assert!(blob_get(&mut c, "g", "b", &old).is_ok());

        blob_release(&mut c, "g", "b", &[], t0 + BLOB_RELEASE_GRACE_SECS).unwrap();
        assert!(matches!(
            blob_get(&mut c, "g", "b", &old),
            Err(SyncError::Code(ErrorCode::BlobNotFound, _))
        ));
        assert!(blob_get(&mut c, "g", "a", &kept).is_ok());
        assert!(blob_get(&mut c, "g", "a", &theirs).is_ok());
        assert_eq!(group_head(&c, "g").unwrap().1, 12, "freed bytes leave the quota");

        // Uploading a released blob again revives it.
        let again = put(&mut c, "a", b"again");
        blob_release(&mut c, "g", "a", std::slice::from_ref(&again), t0).unwrap();
        put(&mut c, "a", b"again");
        blob_release(&mut c, "g", "a", &[], t0 + 2 * BLOB_RELEASE_GRACE_SECS).unwrap();
        assert!(blob_get(&mut c, "g", "a", &again).is_ok());
    }

    #[test]
    fn upgrade_adds_the_gc_columns_to_an_old_store() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE groups (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_seq INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE blobs (group_id TEXT NOT NULL, sha TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (group_id, sha));
             INSERT INTO groups (id, created_at) VALUES ('g', 0);
             INSERT INTO blobs (group_id, sha, data) VALUES ('g', 's', x'00');",
        )
        .unwrap();
        c.execute_batch(SCHEMA).unwrap();
        upgrade(&c).unwrap();
        upgrade(&c).unwrap();
        let (dev, rel): (Option<String>, Option<i64>) = c
            .query_row("SELECT device_id, released_at FROM blobs", [], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        assert_eq!((dev, rel), (None, None));
    }

    #[test]
    fn revoke_before_first_hello_blocks_tofu() {
        let mut c = mem();
        hello(&mut c, "g", "a", "h", true).unwrap();
        revoke(&mut c, "g", "a", "b").unwrap();
        assert!(matches!(
            hello(&mut c, "g", "b", "h2", false),
            Err(SyncError::Code(ErrorCode::DeviceRevoked, _))
        ));
    }
}
