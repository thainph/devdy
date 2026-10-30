//! Two-way sync store (design `two-way-sync` §3 / §9.2).
//!
//! Devices of a sync group push sealed ops and pull them back by cursor; the
//! relay keeps them in SQLite (`RELAY_SYNC_DB`) so devices never have to be
//! online at the same time. Everything the relay stores is ciphertext: an op
//! is an opaque base64 `blob` plus a keyed hash `key`, a file is an opaque blob
//! addressed by the sha256 of its sealed bytes. Op blobs and blob data are
//! NEVER logged.
//!
//! Supersede: a push carries the pusher's pull cursor `base_seq`. An older op
//! with the same key is deleted only when the pusher had already pulled it
//! (`seq <= base_seq`) or pushed it itself; otherwise both are kept, so an
//! offline device's stale op can never erase a newer one it has not seen (the
//! devices resolve the pair by HLC).
//!
//! A sync connection opens with `sync_hello` (shared `RELAY_HOST_TOKEN` +
//! per-device token) and from then on carries only sync / blob frames and
//! keepalives. The device that creates a group is registered on that hello;
//! any other device joins with a one-time ticket a member registered first
//! (`sync_invite`), after which its token hash is pinned. After a push, the
//! other connected devices of the group get `sync_notify` so they pull right
//! away.
//!
//! Replies to `sync_pull` / `blob_get` can be MiBs, so a connection whose
//! queued replies exceed [`MAX_QUEUED_REPLY_BYTES`] is not served another one
//! until its socket drains (and is closed if it never does).
//!
//! On-demand sync (design `on-demand-sync` §1): `sync_peer_send` forwards an
//! opaque sealed message to another live device of the same group as
//! `sync_peer_msg`; `sync_peers` lists those devices. Peer messages are never
//! stored nor logged. A target whose queue does not drain is closed as too
//! slow, and the sender gets `peer_offline`.
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
/// Longest lifetime of a join ticket.
pub const MAX_TICKET_TTL_SECS: u64 = 600;
/// Most live (unexpired, unused) join tickets per group.
const MAX_LIVE_TICKETS: i64 = 16;
/// Queued reply bytes above which a connection gets no further pull / blob
/// reply until its socket drains.
pub const MAX_QUEUED_REPLY_BYTES: usize = 16 * 1024 * 1024;
/// How long a connection may sit above [`MAX_QUEUED_REPLY_BYTES`] before it is
/// closed as too slow.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

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
            | FrameType::SyncInvite
            | FrameType::SyncGroupInfo
            | FrameType::BlobPut
            | FrameType::BlobAck
            | FrameType::BlobGet
            | FrameType::BlobData
            | FrameType::BlobRelease
            | FrameType::BlobReleased
            | FrameType::SyncPeers
            | FrameType::SyncPeerSend
            | FrameType::SyncPeerAck
            | FrameType::SyncPeerMsg
    )
}

/// Sync frames that carry data and are therefore exempt from the control-frame
/// rate limit (an initial upload legitimately sends thousands of them; a
/// folder sync answers a request with many `sync_peer_send` replies). Their
/// volume is bounded by the slow-consumer protection instead.
pub fn is_sync_data_frame(t: FrameType) -> bool {
    matches!(
        t,
        FrameType::SyncPush
            | FrameType::SyncPull
            | FrameType::BlobPut
            | FrameType::BlobGet
            | FrameType::BlobRelease
            | FrameType::SyncPeerSend
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
    ticket: Option<String>,
    ticket_hash: Option<String>,
    ttl_secs: Option<u64>,
    items: Option<Vec<PushItem>>,
    base_seq: Option<u64>,
    after_seq: Option<u64>,
    limit: Option<u64>,
    sha: Option<String>,
    data: Option<String>,
    shas: Option<Vec<String>>,
    /// `sync_revoke` of the creator by itself: the device it hands the
    /// creator role to (ignored unless an active member).
    successor: Option<String>,
    /// `sync_hello`: the store epoch the device last synced in (see
    /// [`new_store_epoch`]); the welcome reports that epoch's head.
    known_epoch: Option<String>,
    /// `sync_peer_send`: the device to forward `data` to.
    to_device: Option<String>,
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
    pub key: String,
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
    /// The connection did not drain its queued replies in time: close it.
    TooSlow,
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
    bytes       INTEGER NOT NULL DEFAULT 0,
    -- The device whose hello created the group: the only one that may revoke
    -- other devices. Moves to another active device when it revokes itself.
    creator_device TEXT
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
    PRIMARY KEY (group_id, seq)
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
-- The store's current epoch: a random id replaced every time the store is
-- opened (relay start). A store restored from a backup therefore never
-- reuses the epoch it is running in.
CREATE TABLE IF NOT EXISTS store_epoch (
    id          INTEGER PRIMARY KEY CHECK (id = 1),
    epoch       TEXT NOT NULL
);
-- Highest seq a group reached in each epoch, updated in the push transaction.
-- A device remembers (epoch, highest seq it has seen); if this store reports
-- a lower head for that epoch, or does not know it, the store lost ops.
CREATE TABLE IF NOT EXISTS epochs (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    epoch       TEXT NOT NULL,
    head        INTEGER NOT NULL,
    PRIMARY KEY (group_id, epoch)
);
CREATE TABLE IF NOT EXISTS invites (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    ticket_hash TEXT NOT NULL,
    expires_at  INTEGER NOT NULL,
    used        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, ticket_hash)
);
";

/// Columns added after the first release of the store (existing databases).
const BLOB_COLUMNS: &[(&str, &str)] = &[("device_id", "TEXT"), ("released_at", "INTEGER")];

/// Tables holding per-group rows (`group_id`), children of `groups`.
const GROUP_TABLES: &[&str] = &["ops", "blobs", "epochs", "invites", "devices"];

/// Open (creating if needed) the sync database, then purge the groups every
/// device has left.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // Only takes effect on a new file (an existing one switches on VACUUM).
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    upgrade(&conn)?;
    new_store_epoch(&conn)?;
    let purged = purge_abandoned(&mut conn)?;
    tracing::info!(event = "sync_groups_purged", groups = purged);
    Ok(conn)
}

/// The group still has a device that has not been revoked.
fn has_active_device(conn: &Connection, group: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM devices WHERE group_id = ?1 AND revoked = 0)",
        params![group],
        |r| r.get(0),
    )
}

/// Delete everything stored for `group`. Only for a group without an active
/// device, inside the caller's transaction.
fn purge_group(conn: &Connection, group: &str) -> rusqlite::Result<()> {
    for table in GROUP_TABLES {
        conn.execute(&format!("DELETE FROM {table} WHERE group_id = ?1"), params![group])?;
    }
    conn.execute("DELETE FROM groups WHERE id = ?1", params![group])?;
    Ok(())
}

/// Purge every group without an active device (left before purging existed),
/// one transaction per group. Returns how many were purged.
pub fn purge_abandoned(conn: &mut Connection) -> rusqlite::Result<usize> {
    let groups: Vec<String> = conn
        .prepare(
            "SELECT id FROM groups WHERE NOT EXISTS \
             (SELECT 1 FROM devices d WHERE d.group_id = groups.id AND d.revoked = 0)",
        )?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut purged = 0;
    for group in &groups {
        let tx = conn.transaction()?;
        if !has_active_device(&tx, group)? {
            purge_group(&tx, group)?;
            purged += 1;
        }
        tx.commit()?;
    }
    if purged > 0 {
        reclaim_space(conn);
    }
    Ok(purged)
}

/// After a purge: hand the freed pages back to the file system when the store
/// uses incremental auto-vacuum. Otherwise they stay in the file and are
/// reused by later writes (no blocking full VACUUM here).
fn reclaim_space(conn: &Connection) {
    let res = conn.query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0)).and_then(|mode| {
        if mode == 2 {
            // Frees pages step by step: run it to completion.
            let mut stmt = conn.prepare("PRAGMA incremental_vacuum")?;
            let mut rows = stmt.query([])?;
            while rows.next()?.is_some() {}
        }
        Ok(())
    });
    if let Err(e) = res {
        tracing::warn!(event = "sync_vacuum_failed", error = %e);
    }
}

/// `RELAY_SYNC_VACUUM_ON_START`: a one-time `VACUUM` that shrinks the file to
/// its live data and switches it to incremental auto-vacuum, so later purges
/// give their pages back. Returns the file size (bytes) before and after.
pub fn vacuum(conn: &Connection) -> rusqlite::Result<(u64, u64)> {
    let size = || {
        conn.query_row(
            "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|n| n.max(0) as u64)
    };
    let before = size()?;
    conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")?;
    Ok((before, size()?))
}

/// Epochs of a group kept for devices that last synced in them; one is added
/// per relay start the group sees a hello in.
const MAX_EPOCHS_PER_GROUP: i64 = 1000;

/// Start a new store epoch (every open). Restore detection (design §9.2,
/// "store epochs"): seqs are only comparable within one store history, and a
/// restored backup rewinds `last_seq`, so the seqs it hands out next collide
/// with ones devices already saw. Each device keeps (epoch, highest seq seen)
/// and presents the epoch on hello; the welcome returns the head this store
/// recorded for it. A store without that epoch, or with a lower head for it,
/// is missing ops the device saw: the device stops and merges again. This
/// holds even after other devices pushed the head back past the device's
/// cursor, which a plain `head < cursor` check misses.
pub fn new_store_epoch(conn: &Connection) -> rusqlite::Result<String> {
    let epoch = uuid::Uuid::new_v4().simple().to_string();
    conn.execute(
        "INSERT INTO store_epoch (id, epoch) VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET epoch = excluded.epoch",
        params![epoch],
    )?;
    Ok(epoch)
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect()
}

fn upgrade(conn: &Connection) -> rusqlite::Result<()> {
    let have = columns(conn, "blobs")?;
    for (col, ty) in BLOB_COLUMNS {
        if !have.iter().any(|c| c == col) {
            conn.execute_batch(&format!("ALTER TABLE blobs ADD COLUMN {col} {ty};"))?;
        }
    }
    if !columns(conn, "groups")?.iter().any(|c| c == "creator_device") {
        conn.execute_batch("ALTER TABLE groups ADD COLUMN creator_device TEXT;")?;
    }
    // Groups created before the column existed: the earliest active registered
    // device (pre-revoked placeholders have no token) is taken as the creator.
    // A group whose last member left stays without one.
    conn.execute_batch(
        "UPDATE groups SET creator_device = (
             SELECT d.device_id FROM devices d
             WHERE d.group_id = groups.id AND d.token_hash != '' AND d.revoked = 0
             ORDER BY d.created_at, d.rowid LIMIT 1)
         WHERE creator_device IS NULL;",
    )?;
    // Ops used to be UNIQUE per key (unconditional supersede); same-key ops may
    // now coexist. SQLite cannot drop a constraint, so rebuild the table.
    let ops_sql: String = conn.query_row(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'ops'",
        [],
        |r| r.get(0),
    )?;
    if ops_sql.contains("UNIQUE") {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE ops_new (
                 group_id    TEXT NOT NULL REFERENCES groups(id),
                 seq         INTEGER NOT NULL,
                 device_id   TEXT NOT NULL,
                 key         TEXT NOT NULL,
                 blob        TEXT NOT NULL,
                 PRIMARY KEY (group_id, seq)
             );
             INSERT INTO ops_new (group_id, seq, device_id, key, blob)
                 SELECT group_id, seq, device_id, key, blob FROM ops;
             DROP TABLE ops;
             ALTER TABLE ops_new RENAME TO ops;
             COMMIT;",
        )?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_blobs_released ON blobs(group_id, released_at) WHERE released_at IS NOT NULL;
         CREATE INDEX IF NOT EXISTS idx_ops_key ON ops(group_id, key);",
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

/// What a `sync_hello` presents besides the group and device ids.
#[derive(Debug, Clone, Copy)]
pub struct HelloAuth<'a> {
    /// sha256 hex of the device token.
    pub token_hash: &'a str,
    pub create: bool,
    /// sha256 hex of the join ticket, if one was sent.
    pub ticket_hash: Option<&'a str>,
    /// The store epoch the device last synced in.
    pub known_epoch: Option<&'a str>,
}

/// `sync_hello`: create the group (`create`, the device becomes its creator),
/// register an unknown device that presents a valid join ticket, or check a
/// known one's token. Returns the group head and creator.
pub fn hello(conn: &mut Connection, group: &str, device: &str, auth: HelloAuth, now: i64) -> SyncResult<Welcome> {
    let tx = conn.transaction()?;
    let exists: bool = tx
        .query_row("SELECT 1 FROM groups WHERE id = ?1", params![group], |_| Ok(()))
        .optional()?
        .is_some();
    let known: Option<(String, bool)> = if exists {
        tx.query_row(
            "SELECT token_hash, revoked FROM devices WHERE group_id = ?1 AND device_id = ?2",
            params![group, device],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
    } else {
        None
    };
    match known {
        Some((_, true)) => return Err(SyncError::Code(ErrorCode::DeviceRevoked, "device revoked")),
        Some((stored, false)) => {
            if !crate::server::constant_time_eq(&stored, auth.token_hash) {
                return Err(SyncError::Code(ErrorCode::AuthFailed, "invalid device token"));
            }
        }
        None if !exists => {
            if !auth.create {
                return Err(SyncError::Code(ErrorCode::GroupNotFound, "unknown sync group"));
            }
            tx.execute(
                "INSERT INTO groups (id, created_at, creator_device) VALUES (?1, ?2, ?3)",
                params![group, now, device],
            )?;
            register(&tx, group, device, auth.token_hash, now)?;
        }
        None => {
            if auth.create {
                return Err(SyncError::Code(ErrorCode::GroupExists, "sync group already exists"));
            }
            let ticket = auth
                .ticket_hash
                .ok_or(SyncError::Code(ErrorCode::TicketRequired, "a join ticket is required"))?;
            let used = tx.execute(
                "UPDATE invites SET used = 1 WHERE group_id = ?1 AND ticket_hash = ?2 AND used = 0 AND expires_at > ?3",
                params![group, ticket, now],
            )?;
            if used == 0 {
                return Err(SyncError::Code(ErrorCode::TicketInvalid, "join ticket invalid, expired or used"));
            }
            register(&tx, group, device, auth.token_hash, now)?;
        }
    }
    gc_blobs(&tx, group, now)?;
    let (last_seq, _) = group_head(&tx, group)?;
    let creator = group_creator(&tx, group)?;
    tx.execute(
        "INSERT INTO epochs (group_id, epoch, head) SELECT ?1, epoch, ?2 FROM store_epoch WHERE true \
         ON CONFLICT(group_id, epoch) DO NOTHING",
        params![group, last_seq as i64],
    )?;
    tx.execute(
        "DELETE FROM epochs WHERE group_id = ?1 AND rowid NOT IN \
         (SELECT rowid FROM epochs WHERE group_id = ?1 ORDER BY rowid DESC LIMIT ?2)",
        params![group, MAX_EPOCHS_PER_GROUP],
    )?;
    let epoch: Option<String> = tx
        .query_row("SELECT epoch FROM store_epoch WHERE id = 1", [], |r| r.get(0))
        .optional()?;
    let known_head: Option<i64> = match auth.known_epoch {
        Some(e) => tx
            .query_row(
                "SELECT head FROM epochs WHERE group_id = ?1 AND epoch = ?2",
                params![group, e],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    tx.commit()?;
    Ok(Welcome { last_seq, creator, epoch, known_head: known_head.map(|h| h as u64) })
}

/// What `sync_welcome` tells a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub last_seq: u64,
    /// The device allowed to revoke others (`None`: every member left).
    pub creator: Option<String>,
    /// The store's current epoch.
    pub epoch: Option<String>,
    /// The head this store recorded for the hello's `known_epoch` (`None`:
    /// not asked, or an epoch this store never saw).
    pub known_head: Option<u64>,
}

fn group_creator(conn: &Connection, group: &str) -> SyncResult<Option<String>> {
    Ok(conn
        .query_row("SELECT creator_device FROM groups WHERE id = ?1", params![group], |r| r.get(0))
        .optional()?
        .flatten())
}

fn register(conn: &Connection, group: &str, device: &str, token_hash: &str, now: i64) -> SyncResult<()> {
    conn.execute(
        "INSERT INTO devices (group_id, device_id, token_hash, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![group, device, token_hash, now],
    )?;
    Ok(())
}

/// `sync_invite`: a member registers a one-time join ticket (by hash) valid for
/// `ttl_secs`. Expired / used tickets of the group are pruned on the way.
pub fn invite(conn: &mut Connection, group: &str, device: &str, ticket_hash: &str, ttl_secs: u64, now: i64) -> SyncResult<()> {
    if !valid_sha(ticket_hash) {
        return Err(bad("ticket_hash must be 64 lowercase hex chars"));
    }
    if ttl_secs == 0 || ttl_secs > MAX_TICKET_TTL_SECS {
        return Err(bad("ttl_secs must be 1..=600"));
    }
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    tx.execute(
        "DELETE FROM invites WHERE group_id = ?1 AND (used = 1 OR expires_at <= ?2)",
        params![group, now],
    )?;
    let live: i64 = tx.query_row("SELECT COUNT(*) FROM invites WHERE group_id = ?1", params![group], |r| r.get(0))?;
    if live >= MAX_LIVE_TICKETS {
        return Err(SyncError::Code(ErrorCode::QuotaExceeded, "too many open join tickets"));
    }
    let inserted = tx.execute(
        "INSERT INTO invites (group_id, ticket_hash, expires_at) VALUES (?1, ?2, ?3) ON CONFLICT DO NOTHING",
        params![group, ticket_hash, now + ttl_secs as i64],
    )?;
    if inserted == 0 {
        return Err(bad("ticket already registered"));
    }
    tx.commit()?;
    Ok(())
}

/// `sync_push`: assign seqs, drop the older ops with the same key that the
/// pusher has seen (`seq <= base_seq`) or pushed itself, enforce the group
/// quota. All or nothing. Returns the new head.
pub fn push(
    conn: &mut Connection,
    limits: Limits,
    group: &str,
    device: &str,
    base_seq: u64,
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
    let base = base_seq.min(i64::MAX as u64) as i64;
    for it in items {
        const SEEN: &str = "group_id = ?1 AND key = ?2 AND (seq <= ?3 OR device_id = ?4)";
        let old: i64 = tx.query_row(
            &format!("SELECT COALESCE(SUM(length(CAST(blob AS BLOB))), 0) FROM ops WHERE {SEEN}"),
            params![group, it.key, base, device],
            |r| r.get(0),
        )?;
        if tx.execute(&format!("DELETE FROM ops WHERE {SEEN}"), params![group, it.key, base, device])? > 0 {
            bytes = bytes.saturating_sub(old as u64);
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
    tx.execute(
        "INSERT INTO epochs (group_id, epoch, head) SELECT ?1, epoch, ?2 FROM store_epoch WHERE true \
         ON CONFLICT(group_id, epoch) DO UPDATE SET head = excluded.head",
        params![group, seq as i64],
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
            "SELECT seq, device_id, key, blob FROM ops WHERE group_id = ?1 AND seq > ?2 ORDER BY seq",
        )?;
        let mut rows = stmt.query(params![group, after_seq.min(i64::MAX as u64) as i64])?;
        let mut page_bytes = 0usize;
        while let Some(row) = rows.next()? {
            let blob: String = row.get(3)?;
            if items.len() >= limit || (!items.is_empty() && page_bytes + blob.len() > PULL_MAX_BYTES) {
                more = true;
                break;
            }
            page_bytes += blob.len();
            items.push(PulledOp {
                seq: row.get::<_, i64>(0)? as u64,
                device_id: row.get(1)?,
                key: row.get(2)?,
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

/// `sync_revoke`: mark `target` revoked. A device may revoke itself; only the
/// group's creator may revoke another one. A device not seen yet is recorded
/// as revoked, so it can never register later.
///
/// When the creator revokes itself the role moves on, so the group never
/// loses the one device that may remove others: to `successor` if that is an
/// active member, else to the oldest active member (`None` if none is left).
///
/// When no active device is left the whole group is purged in the same
/// transaction (ops, blobs, devices, epochs, invites); a later hello for it
/// gets `group_not_found`. The store's single connection runs every request
/// in its own transaction, so a purge never interleaves with a push / hello.
pub fn revoke(
    conn: &mut Connection,
    group: &str,
    device: &str,
    target: &str,
    successor: Option<&str>,
) -> SyncResult<Revoked> {
    let tx = conn.transaction()?;
    ensure_active(&tx, group, device)?;
    let creator = group_creator(&tx, group)?;
    if target != device && creator.as_deref() != Some(device) {
        return Err(SyncError::Code(ErrorCode::Forbidden, "only the group creator can revoke another device"));
    }
    tx.execute(
        "INSERT INTO devices (group_id, device_id, token_hash, revoked, created_at) VALUES (?1, ?2, '', 1, ?3)
         ON CONFLICT (group_id, device_id) DO UPDATE SET revoked = 1",
        params![group, target, now_secs()],
    )?;
    let mut moved = None;
    if creator.as_deref() == Some(target) {
        let next = next_creator(&tx, group, successor)?;
        tx.execute(
            "UPDATE groups SET creator_device = ?2 WHERE id = ?1",
            params![group, next],
        )?;
        moved = next;
    }
    let purged = !has_active_device(&tx, group)?;
    if purged {
        purge_group(&tx, group)?;
    }
    tx.commit()?;
    if purged {
        reclaim_space(conn);
    }
    Ok(Revoked { creator_moved: moved, purged })
}

/// What a `sync_revoke` changed besides the target.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Revoked {
    /// The new creator, when the role moved.
    pub creator_moved: Option<String>,
    /// No active device was left: the group and all its data were deleted.
    pub purged: bool,
}

/// The creator's successor: `wanted` if it is an active member, else the
/// member registered first (ties by device id, so the pick is deterministic).
fn next_creator(conn: &Connection, group: &str, wanted: Option<&str>) -> SyncResult<Option<String>> {
    const ACTIVE: &str = "group_id = ?1 AND revoked = 0 AND token_hash != ''";
    if let Some(w) = wanted.filter(|w| valid_id(w)) {
        let ok = conn
            .query_row(
                &format!("SELECT 1 FROM devices WHERE {ACTIVE} AND device_id = ?2"),
                params![group, w],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if ok {
            return Ok(Some(w.to_string()));
        }
    }
    Ok(conn
        .query_row(
            &format!("SELECT device_id FROM devices WHERE {ACTIVE} ORDER BY created_at, device_id LIMIT 1"),
            params![group],
            |r| r.get(0),
        )
        .optional()?)
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
    /// `vacuum_on_start`: run [`vacuum`] once after the startup purge (a
    /// failure is logged, the relay still starts).
    pub fn open(path: Option<&Path>, limits: Limits, vacuum_on_start: bool) -> rusqlite::Result<Self> {
        let db = match path {
            Some(p) => {
                let conn = open_db(p)?;
                if vacuum_on_start {
                    match vacuum(&conn) {
                        Ok((before, after)) => {
                            tracing::info!(event = "sync_vacuum", before_bytes = before, after_bytes = after)
                        }
                        Err(e) => tracing::warn!(event = "sync_vacuum_failed", error = %e),
                    }
                }
                Some(Arc::new(Mutex::new(conn)))
            }
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
                    Ok((s, w)) => {
                        tracing::info!(event = "sync_welcome", conn = conn_id, group = %s.group_id, device = %s.device_id);
                        send_json(
                            out,
                            json!({
                                "t": "sync_welcome",
                                "last_seq": w.last_seq,
                                "creator_device": w.creator,
                                "epoch": w.epoch,
                                "known_head": w.known_head,
                            }),
                            id,
                        );
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
                    Err(SyncError::TooSlow) => {
                        tracing::warn!(event = "sync_conn_too_slow", conn = conn_id);
                        true
                    }
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
    ) -> SyncResult<(SyncSession, Welcome)> {
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
        let ticket_hash = req.ticket.filter(|t| !t.is_empty()).map(|t| sha256_hex(t.as_bytes()));
        let known_epoch = req.known_epoch.filter(|e| valid_id(e));
        let (g, d) = (group.clone(), device.clone());
        let welcome = self
            .call(move |c| {
                let auth = HelloAuth {
                    token_hash: &token_hash,
                    create,
                    ticket_hash: ticket_hash.as_deref(),
                    known_epoch: known_epoch.as_deref(),
                };
                hello(c, &g, &d, auth, now_secs())
            })
            .await?;
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
            welcome,
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
                let base = req.base_seq.unwrap_or(0);
                let head = if n == 0 {
                    self.call(move |c| Ok(group_head(c, &g)?.0)).await?
                } else {
                    self.call(move |c| push(c, limits, &g, &d, base, &items)).await?
                };
                tracing::debug!(event = "sync_push", conn = conn_id, ops = n, last_seq = head);
                send_json(out, json!({ "t": "sync_ack", "last_seq": head }), id);
                if n > 0 {
                    self.notify(&s.group_id, &s.device_id, head);
                }
            }
            FrameType::SyncPull => {
                drained(out).await?;
                let after = req.after_seq.unwrap_or(0);
                let limit = req.limit.unwrap_or(0).min(PULL_MAX_OPS as u64) as usize;
                let page = self.call(move |c| pull(c, &g, &d, after, limit)).await?;
                tracing::debug!(event = "sync_pull", conn = conn_id, ops = page.items.len(), more = page.more);
                let items: Vec<Value> = page
                    .items
                    .into_iter()
                    .map(|op| json!({ "seq": op.seq, "device_id": op.device_id, "key": op.key, "blob": op.blob }))
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
                drained(out).await?;
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
                let successor = req.successor;
                let r = self.call(move |c| revoke(c, &g, &d, &t2, successor.as_deref())).await?;
                tracing::info!(event = "sync_revoke", conn = conn_id, group = %s.group_id, device = %target);
                if r.purged {
                    tracing::info!(event = "sync_groups_purged", groups = 1);
                }
                send_json(out, json!({ "t": "control", "code": "ok" }), id);
                self.kick(&s.group_id, &target);
                if let Some(creator) = r.creator_moved {
                    tracing::info!(event = "sync_creator_moved", group = %s.group_id, device = %creator);
                    self.group_info(&s.group_id, &target, &creator);
                }
            }
            FrameType::SyncInvite => {
                let hash = req.ticket_hash.ok_or_else(|| bad("missing ticket_hash"))?;
                let ttl = req.ttl_secs.unwrap_or(MAX_TICKET_TTL_SECS);
                self.call(move |c| invite(c, &g, &d, &hash, ttl, now_secs())).await?;
                tracing::info!(event = "sync_invite", conn = conn_id, group = %s.group_id);
                send_json(out, json!({ "t": "control", "code": "ok" }), id);
            }
            FrameType::SyncPeers => {
                let devices = self.peers(&s.group_id, &s.device_id);
                send_json(out, json!({ "t": "sync_peers", "devices": devices }), id);
            }
            FrameType::SyncPeerSend => {
                let to = req.to_device.unwrap_or_default();
                let data = req.data.unwrap_or_default();
                if !valid_id(&to) || data.is_empty() {
                    return Err(bad("to_device and data are required"));
                }
                if data.len() > limits.max_op_chars() {
                    return Err(SyncError::Code(ErrorCode::TooLarge, "peer message too large"));
                }
                self.peer_send(conn_id, s, &to, data).await?;
                send_json(out, json!({ "t": "sync_peer_ack" }), id);
            }
            _ => {}
        }
        Ok(())
    }

    /// Device ids of the group's live connections other than `device`, sorted.
    fn peers(&self, group: &str, device: &str) -> Vec<String> {
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        let ids: std::collections::BTreeSet<&str> = live
            .get(group)
            .into_iter()
            .flatten()
            .map(|c| c.device_id.as_str())
            .filter(|d| *d != device)
            .collect();
        ids.into_iter().map(str::to_string).collect()
    }

    /// Forward `data` to `to`'s most recent live connection in the sender's
    /// group as `sync_peer_msg`. `peer_offline` when there is none, the target
    /// is revoked / not a member, or its queue never drains (it is then closed
    /// as too slow).
    async fn peer_send(&self, conn_id: ConnId, s: &SyncSession, to: &str, data: String) -> SyncResult<()> {
        const OFFLINE: SyncError = SyncError::Code(ErrorCode::PeerOffline, "peer is not connected");
        if to == s.device_id {
            return Err(OFFLINE);
        }
        let (g, d, t) = (s.group_id.clone(), s.device_id.clone(), to.to_string());
        let target_active = self
            .call(move |c| {
                ensure_active(c, &g, &d)?;
                match ensure_active(c, &g, &t) {
                    Ok(()) => Ok(true),
                    Err(SyncError::Code(..)) => Ok(false),
                    Err(e) => Err(e),
                }
            })
            .await?;
        if !target_active {
            return Err(OFFLINE);
        }
        let target = {
            let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
            live.get(&s.group_id)
                .and_then(|conns| conns.iter().rev().find(|c| c.device_id == to))
                .map(|c| (c.conn, c.out.clone()))
        };
        let Some((target_conn, target_out)) = target else {
            return Err(OFFLINE);
        };
        if drained(&target_out).await.is_err() {
            tracing::warn!(event = "sync_conn_too_slow", conn = target_conn);
            target_out.close();
            return Err(OFFLINE);
        }
        let bytes = data.len();
        let text = json!({ "t": "sync_peer_msg", "from_device": s.device_id, "data": data }).to_string();
        if target_out.send_text(text).is_err() {
            return Err(OFFLINE);
        }
        tracing::debug!(event = "sync_peer_send", conn = conn_id, to_conn = target_conn, bytes);
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

    /// Tell the group's other connected devices who its creator is now.
    fn group_info(&self, group: &str, except_device: &str, creator: &str) {
        let text = json!({ "t": "sync_group_info", "creator_device": creator }).to_string();
        let live = self.live.lock().unwrap_or_else(|p| p.into_inner());
        for c in live.get(group).into_iter().flatten() {
            if c.device_id != except_device {
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
    is_sync_data_frame(t) || matches!(t, FrameType::SyncRevoke | FrameType::SyncInvite | FrameType::SyncPeers)
}

fn error_code(e: &SyncError) -> ErrorCode {
    match e {
        SyncError::Code(c, _) => *c,
        SyncError::Db(_) | SyncError::TooSlow => ErrorCode::Internal,
    }
}

/// Wait until the connection's queued replies are under
/// [`MAX_QUEUED_REPLY_BYTES`] before producing another large one. Requests of
/// a connection are handled one at a time, so this bounds what one client can
/// pin in relay memory by not reading its socket.
async fn drained(out: &Outbound) -> SyncResult<()> {
    let deadline = tokio::time::Instant::now() + DRAIN_TIMEOUT;
    while out.queued_text_bytes() > MAX_QUEUED_REPLY_BYTES {
        if tokio::time::Instant::now() >= deadline {
            return Err(SyncError::TooSlow);
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Ok(())
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
        SyncError::TooSlow => send_error(out, id, ErrorCode::Internal, "connection too slow"),
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

    fn auth(token_hash: &str, create: bool) -> HelloAuth<'_> {
        HelloAuth { token_hash, create, ticket_hash: None, known_epoch: None }
    }

    /// Create group `g` with `a` as creator and join `others` with tickets.
    fn group(c: &mut Connection, others: &[&str]) {
        hello(c, "g", "a", auth("h", true), 0).unwrap();
        for (i, d) in others.iter().enumerate() {
            let ticket = sha256_hex(format!("ticket-{i}").as_bytes());
            invite(c, "g", "a", &ticket, 60, 0).unwrap();
            let a = HelloAuth { token_hash: "h", create: false, ticket_hash: Some(&ticket), known_epoch: None };
            hello(c, "g", d, a, 1).unwrap();
        }
    }

    fn item(key: &str, blob: &str) -> PushItem {
        PushItem {
            key: key.into(),
            blob: blob.into(),
        }
    }

    #[test]
    fn supersede_keeps_byte_accounting() {
        let mut c = mem();
        group(&mut c, &[]);
        assert_eq!(push(&mut c, LIMITS, "g", "a", 0, &[item("k1", "aaaa"), item("k2", "bb")]).unwrap(), 2);
        // Own earlier op: superseded whatever the cursor.
        assert_eq!(push(&mut c, LIMITS, "g", "a", 0, &[item("k1", "c")]).unwrap(), 3);
        assert_eq!(group_head(&c, "g").unwrap(), (3, 3));
        let page = pull(&mut c, "g", "a", 0, 0).unwrap();
        let seqs: Vec<u64> = page.items.iter().map(|o| o.seq).collect();
        assert_eq!(seqs, vec![2, 3]);
        assert!(!page.more);
        assert_eq!(page.last_seq, 3);
        assert_eq!(page.items[1].key, "k1");
    }

    #[test]
    fn unseen_op_of_another_device_is_kept() {
        let mut c = mem();
        group(&mut c, &["b"]);
        // B pulled up to 0, then A pushed k1 at seq 1.
        push(&mut c, LIMITS, "g", "a", 0, &[item("k1", "new")]).unwrap();
        // B's stale push (base 0) must not erase A's op it never saw.
        push(&mut c, LIMITS, "g", "b", 0, &[item("k1", "old")]).unwrap();
        let blobs: Vec<String> = pull(&mut c, "g", "a", 0, 0).unwrap().items.into_iter().map(|o| o.blob).collect();
        assert_eq!(blobs, vec!["new", "old"]);
        assert_eq!(group_head(&c, "g").unwrap(), (2, 6));
        // Once B has pulled past seq 2 its next push replaces both.
        push(&mut c, LIMITS, "g", "b", 2, &[item("k1", "x")]).unwrap();
        let page = pull(&mut c, "g", "a", 0, 0).unwrap();
        assert_eq!(page.items.iter().map(|o| o.seq).collect::<Vec<_>>(), vec![3]);
        assert_eq!(group_head(&c, "g").unwrap(), (3, 1));
    }

    #[test]
    fn joining_needs_a_valid_unused_ticket() {
        let mut c = mem();
        hello(&mut c, "g", "a", auth("ha", true), 100).unwrap();
        let tick = |s: &str| sha256_hex(s.as_bytes());
        fn with(t: &str) -> HelloAuth<'_> {
            HelloAuth { token_hash: "hb", create: false, ticket_hash: Some(t), known_epoch: None }
        }
        let code = |r: SyncResult<Welcome>| match r {
            Err(SyncError::Code(c, _)) => c.as_str(),
            Ok(_) => "ok",
            Err(_) => "db",
        };
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", false), 100)), "ticket_required");
        assert_eq!(code(hello(&mut c, "g", "b", with(&tick("t1")), 100)), "ticket_invalid");
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", true), 100)), "group_exists");
        // Creator retrying its create hello is fine.
        assert_eq!(code(hello(&mut c, "g", "a", auth("ha", true), 100)), "ok");

        invite(&mut c, "g", "a", &tick("t1"), 60, 100).unwrap();
        assert!(invite(&mut c, "g", "a", &tick("t1"), 60, 100).is_err(), "duplicate");
        assert!(invite(&mut c, "g", "a", &tick("t2"), 601, 100).is_err(), "ttl capped");
        assert!(invite(&mut c, "g", "nobody", &tick("t2"), 60, 100).is_err(), "members only");
        invite(&mut c, "g", "a", &tick("t2"), 10, 100).unwrap();
        // Expired.
        assert_eq!(code(hello(&mut c, "g", "c", with(&tick("t2")), 110)), "ticket_invalid");
        assert_eq!(code(hello(&mut c, "g", "b", with(&tick("t1")), 101)), "ok");
        // Used: a second device cannot reuse it.
        assert_eq!(code(hello(&mut c, "g", "c", with(&tick("t1")), 102)), "ticket_invalid");
        // The joined device now authenticates with its token alone.
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", false), 103)), "ok");
        assert_eq!(code(hello(&mut c, "g", "b", auth("other", false), 103)), "auth_failed");
    }

    #[test]
    fn only_the_creator_revokes_others() {
        let mut c = mem();
        group(&mut c, &["b", "c"]);
        assert!(matches!(revoke(&mut c, "g", "b", "c", None), Err(SyncError::Code(ErrorCode::Forbidden, _))));
        assert!(matches!(revoke(&mut c, "g", "b", "a", None), Err(SyncError::Code(ErrorCode::Forbidden, _))));
        assert_eq!(revoke(&mut c, "g", "b", "b", None).unwrap(), Revoked::default(), "a member leaving keeps the creator");
        assert_eq!(revoke(&mut c, "g", "a", "c", None).unwrap(), Revoked::default());
        assert!(matches!(ensure_active(&c, "g", "c"), Err(SyncError::Code(ErrorCode::DeviceRevoked, _))));
    }

    #[test]
    fn creator_leaving_hands_the_role_on() {
        let creator = |c: &Connection| group_creator(c, "g").unwrap();
        let mut c = mem();
        group(&mut c, &["b", "c", "d"]);
        c.execute("UPDATE devices SET created_at = 5 WHERE device_id = 'b'", []).unwrap();
        // A revoked placeholder (never registered) is not a candidate.
        revoke(&mut c, "g", "a", "x", None).unwrap();

        // Requested successor wins when it is an active member.
        assert_eq!(revoke(&mut c, "g", "a", "a", Some("d")).unwrap().creator_moved.as_deref(), Some("d"));
        assert_eq!(creator(&c).as_deref(), Some("d"));
        // The new creator may now remove others; a former peer may not.
        assert!(matches!(revoke(&mut c, "g", "c", "b", None), Err(SyncError::Code(ErrorCode::Forbidden, _))));

        // Unknown / revoked successor: the oldest active member (c joined at 1, b at 5).
        assert_eq!(revoke(&mut c, "g", "d", "d", Some("x")).unwrap().creator_moved.as_deref(), Some("c"));
        assert_eq!(revoke(&mut c, "g", "c", "c", None).unwrap().creator_moved.as_deref(), Some("b"));
        let w = hello(&mut c, "g", "b", auth("h", false), 9).unwrap();
        assert_eq!(w.creator.as_deref(), Some("b"), "the welcome carries the creator");

        // The last member leaving leaves no creator: the group is purged.
        assert_eq!(revoke(&mut c, "g", "b", "b", None).unwrap(), Revoked { creator_moved: None, purged: true });
        assert_eq!(creator(&c), None);
    }

    #[test]
    fn failed_push_is_all_or_nothing() {
        let mut c = mem();
        group(&mut c, &[]);
        let big = "x".repeat(100);
        let items: Vec<PushItem> = (0..11).map(|i| item(&format!("k{i}"), &big)).collect();
        assert!(matches!(
            push(&mut c, LIMITS, "g", "a", 0, &items),
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
        group(&mut c, &["b"]);
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
    fn upgrade_migrates_an_old_store() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE groups (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_seq INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE devices (group_id TEXT NOT NULL, device_id TEXT NOT NULL, token_hash TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, PRIMARY KEY (group_id, device_id));
             CREATE TABLE ops (group_id TEXT NOT NULL, seq INTEGER NOT NULL, device_id TEXT NOT NULL, key TEXT NOT NULL, blob TEXT NOT NULL, PRIMARY KEY (group_id, seq), UNIQUE (group_id, key));
             CREATE TABLE blobs (group_id TEXT NOT NULL, sha TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (group_id, sha));
             INSERT INTO groups (id, created_at, last_seq, bytes) VALUES ('g', 0, 1, 1);
             INSERT INTO devices VALUES ('g', 'x', '', 1, 0), ('g', 'b', 'hb', 0, 5), ('g', 'a', 'ha', 0, 3);
             INSERT INTO ops VALUES ('g', 1, 'a', 'k', 'z');
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
        let creator: String = c.query_row("SELECT creator_device FROM groups", [], |r| r.get(0)).unwrap();
        assert_eq!(creator, "a", "earliest real device");
        // Same-key ops may now coexist.
        push(&mut c, LIMITS, "g", "b", 0, &[item("k", "y")]).unwrap();
        assert_eq!(pull(&mut c, "g", "a", 0, 0).unwrap().items.len(), 2);
    }

    /// Review-3 M6: a store restored from a backup is caught through the
    /// device's (epoch, seen seq) even after another device pushed its head
    /// past that seq again; a plain restart is not mistaken for a restore.
    #[test]
    fn restored_store_is_detected_through_epochs() {
        let dir = std::env::temp_dir().join(format!("devdy-relay-epoch-{}", uuid::Uuid::new_v4().simple()));
        std::fs::create_dir_all(&dir).unwrap();
        let (live, backup) = (dir.join("sync.db"), dir.join("backup.db"));
        let known = |c: &mut Connection, device: &str, token: &str, epoch: &str| {
            let a = HelloAuth { token_hash: token, create: false, ticket_hash: None, known_epoch: Some(epoch) };
            hello(c, "g", device, a, 0).unwrap()
        };

        let mut c = open_db(&live).unwrap();
        let w = hello(&mut c, "g", "a", auth("ha", true), 0).unwrap();
        let e1 = w.epoch.clone().unwrap();
        invite(&mut c, "g", "a", &sha256_hex(b"t"), 60, 0).unwrap();
        let tb = sha256_hex(b"t");
        hello(&mut c, "g", "b", HelloAuth { token_hash: "hb", create: false, ticket_hash: Some(&tb), known_epoch: None }, 0).unwrap();
        push(&mut c, LIMITS, "g", "a", 0, &[item("k1", "x")]).unwrap();
        drop(c);
        std::fs::copy(&live, &backup).unwrap();

        // Restart: a new epoch; the old one is still known with its head.
        let mut c = open_db(&live).unwrap();
        let w = known(&mut c, "a", "ha", &e1);
        let e2 = w.epoch.clone().unwrap();
        assert_ne!(e1, e2);
        assert_eq!(w.known_head, Some(1));
        assert_eq!(push(&mut c, LIMITS, "g", "a", 1, &[item("k2", "y"), item("k3", "z")]).unwrap(), 3);
        // A device's own epoch reports the live head.
        assert_eq!(known(&mut c, "a", "ha", &e2).known_head, Some(3));
        drop(c);

        // Restore the backup (head 1) and let b push the head past 3.
        std::fs::copy(&backup, &live).unwrap();
        let mut c = open_db(&live).unwrap();
        let w = known(&mut c, "b", "hb", &e1);
        assert_eq!((w.last_seq, w.known_head), (1, Some(1)), "b only saw seq 1: nothing lost for it");
        push(&mut c, LIMITS, "g", "b", 1, &[item("k4", "1"), item("k5", "2"), item("k6", "3")]).unwrap();
        let w = known(&mut c, "a", "ha", &e2);
        assert!(w.last_seq >= 3, "the head alone no longer shows the loss");
        assert_eq!(w.known_head, None, "a synced in an epoch this store never saw");
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every table keyed by `group_id` is cleared by a purge.
    #[test]
    fn purge_covers_every_group_table() {
        let c = mem();
        let tables: Vec<String> = c
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name != 'groups'")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        for t in tables {
            let keyed = columns(&c, &t).unwrap().iter().any(|col| col == "group_id");
            assert_eq!(keyed, GROUP_TABLES.contains(&t.as_str()), "table {t}");
        }
    }

    #[test]
    fn revoke_before_first_hello_blocks_tofu() {
        let mut c = mem();
        group(&mut c, &[]);
        revoke(&mut c, "g", "a", "b", None).unwrap();
        assert!(matches!(
            hello(&mut c, "g", "b", auth("h2", false), 0),
            Err(SyncError::Code(ErrorCode::DeviceRevoked, _))
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn big_replies_wait_for_the_socket_to_drain() {
        let (out, mut rx) = crate::room::outbound_channel();
        assert!(drained(&out).await.is_ok());
        out.send_text("x".repeat(MAX_QUEUED_REPLY_BYTES + 1)).unwrap();
        assert_eq!(out.queued_text_bytes(), MAX_QUEUED_REPLY_BYTES + 1);
        // Never drains: the connection is given up on.
        assert!(matches!(drained(&out).await, Err(SyncError::TooSlow)));
        // Drains while waiting: served.
        out.send_text("y".into()).unwrap();
        let reader = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            rx.recv().await;
            rx
        });
        assert!(drained(&out).await.is_ok());
        assert_eq!(out.queued_text_bytes(), 1);
        drop(reader.await.unwrap());
    }
}
