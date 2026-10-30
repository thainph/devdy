//! Sync groups: authentication and device-to-device forwarding (design
//! `on-demand-sync` §P1 — the relay is a pipe).
//!
//! The relay stores no sync data. Its SQLite file (`RELAY_SYNC_DB`) only holds
//! what authentication needs: groups (with their creator), devices (pinned
//! token hashes, revocations) and one-time join tickets (hashes). Everything
//! else travels device to device: `sync_peer_send` forwards an opaque sealed
//! message to the other live device of the same group as `sync_peer_msg`, and
//! `sync_peers` lists those devices. Peer messages are never stored nor logged.
//!
//! A sync connection opens with `sync_hello` (shared `RELAY_HOST_TOKEN` +
//! per-device token) and from then on carries only sync frames and
//! keepalives. The device that creates a group is registered on that hello;
//! another device joins with a one-time ticket a member registered first
//! (`sync_invite`), after which its token hash is pinned. A group has at most
//! [`MAX_ACTIVE_DEVICES`] non-revoked devices.
//!
//! A target whose queue does not drain is closed as too slow, and the sender
//! gets `peer_offline`, so one device cannot pin relay memory by not reading.
//!
//! Frames of the former op / blob store are answered with `unsupported`; at
//! startup its tables are dropped and the file vacuumed once.
//!
//! SQLite is synchronous: every store call runs on the blocking pool, behind a
//! single connection mutex.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::protocol::{ErrorCode, FrameType};
use crate::room::{ConnId, Outbound};

/// Longest accepted `group_id` / `device_id`.
const MAX_ID_LEN: usize = 128;
/// Longest lifetime of a join ticket.
pub const MAX_TICKET_TTL_SECS: u64 = 600;
/// Most live (unexpired, unused) join tickets per group.
const MAX_LIVE_TICKETS: i64 = 16;
/// Most non-revoked devices per group: one asks, the other answers.
pub const MAX_ACTIVE_DEVICES: i64 = 2;
/// Queued bytes above which a target gets no further peer message until its
/// socket drains.
pub const MAX_QUEUED_REPLY_BYTES: usize = 16 * 1024 * 1024;
/// How long a connection may sit above [`MAX_QUEUED_REPLY_BYTES`] before it is
/// closed as too slow.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Frames handled by the sync module (including the removed ones, which it
/// answers with `unsupported`).
pub fn is_sync_frame(t: FrameType) -> bool {
    is_removed_frame(t)
        || matches!(
            t,
            FrameType::SyncHello
                | FrameType::SyncWelcome
                | FrameType::SyncRevoke
                | FrameType::SyncInvite
                | FrameType::SyncGroupInfo
                | FrameType::SyncPeers
                | FrameType::SyncPeerSend
                | FrameType::SyncPeerAck
                | FrameType::SyncPeerMsg
        )
}

/// Frames of the former op / blob store.
fn is_removed_frame(t: FrameType) -> bool {
    matches!(
        t,
        FrameType::SyncPush
            | FrameType::SyncAck
            | FrameType::SyncPull
            | FrameType::SyncOps
            | FrameType::SyncNotify
            | FrameType::BlobPut
            | FrameType::BlobAck
            | FrameType::BlobGet
            | FrameType::BlobData
            | FrameType::BlobRelease
            | FrameType::BlobReleased
    )
}

/// Sync frames that carry data and are therefore exempt from the control-frame
/// rate limit (a folder sync answers a request with many `sync_peer_send`
/// replies). Their volume is bounded by the slow-consumer protection instead.
pub fn is_sync_data_frame(t: FrameType) -> bool {
    t == FrameType::SyncPeerSend
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
    data: Option<String>,
    /// `sync_revoke` of the creator by itself: the device it hands the
    /// creator role to (ignored unless an active member).
    successor: Option<String>,
    /// `sync_peer_send`: the device to forward `data` to.
    to_device: Option<String>,
}

/// Store failure: a wire error code, or a database error (logged, answered
/// with `internal`).
#[derive(Debug)]
pub enum SyncError {
    Code(ErrorCode, &'static str),
    Db(rusqlite::Error),
    /// The connection did not drain its queued frames in time: close it.
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

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS groups (
    id          TEXT PRIMARY KEY,
    created_at  INTEGER NOT NULL,
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
CREATE TABLE IF NOT EXISTS invites (
    group_id    TEXT NOT NULL REFERENCES groups(id),
    ticket_hash TEXT NOT NULL,
    expires_at  INTEGER NOT NULL,
    used        INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, ticket_hash)
);
";

/// Tables holding per-group rows (`group_id`), children of `groups`.
const GROUP_TABLES: &[&str] = &["invites", "devices"];

/// Tables of the former op / blob store, dropped at startup.
const OLD_DATA_TABLES: &[&str] = &["ops", "blobs", "epochs", "store_epoch"];
/// Columns of `groups` that belonged to the former store (head seq, quota).
const OLD_GROUP_COLUMNS: &[&str] = &["last_seq", "bytes"];

/// Open (creating if needed) the sync database, drop the former data store if
/// present (then `VACUUM` once), and purge the groups every device has left.
pub fn open_db(path: &Path) -> rusqlite::Result<Connection> {
    let mut conn = Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.execute_batch(SCHEMA)?;
    upgrade(&conn)?;
    if let Some(before) = drop_old_data(&conn)? {
        match conn.execute_batch("VACUUM").and_then(|()| file_bytes(&conn)) {
            Ok(after) => tracing::info!(event = "sync_store_migrated", before_bytes = before, after_bytes = after),
            Err(e) => tracing::warn!(event = "sync_vacuum_failed", before_bytes = before, error = %e),
        }
    }
    let purged = purge_abandoned(&mut conn)?;
    tracing::info!(event = "sync_groups_purged", groups = purged);
    Ok(conn)
}

/// Size of the database (bytes).
fn file_bytes(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row(
        "SELECT page_count * page_size FROM pragma_page_count(), pragma_page_size()",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n.max(0) as u64)
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        params![table],
        |r| r.get(0),
    )
}

/// Drop the former store's tables and `groups` columns in one transaction.
/// Returns the database size before, or `None` when there was nothing to drop.
pub fn drop_old_data(conn: &Connection) -> rusqlite::Result<Option<u64>> {
    let mut tables = Vec::new();
    for t in OLD_DATA_TABLES {
        if table_exists(conn, t)? {
            tables.push(*t);
        }
    }
    let have = columns(conn, "groups")?;
    let cols: Vec<&str> = OLD_GROUP_COLUMNS.iter().copied().filter(|c| have.iter().any(|h| h == c)).collect();
    if tables.is_empty() && cols.is_empty() {
        return Ok(None);
    }
    let before = file_bytes(conn)?;
    let mut sql = String::from("BEGIN;");
    for t in &tables {
        sql.push_str(&format!("DROP TABLE {t};"));
    }
    for c in &cols {
        sql.push_str(&format!("ALTER TABLE groups DROP COLUMN {c};"));
    }
    sql.push_str("COMMIT;");
    conn.execute_batch(&sql)?;
    Ok(Some(before))
}

/// The group still has a device that has not been revoked.
fn has_active_device(conn: &Connection, group: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM devices WHERE group_id = ?1 AND revoked = 0)",
        params![group],
        |r| r.get(0),
    )
}

/// Delete everything stored for `group` (its devices, invites and row). Only
/// for a group without an active device, inside the caller's transaction.
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
    Ok(purged)
}

fn columns(conn: &Connection, table: &str) -> rusqlite::Result<Vec<String>> {
    conn.prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
        .query_map([], |r| r.get(0))?
        .collect()
}

fn upgrade(conn: &Connection) -> rusqlite::Result<()> {
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
}

/// `sync_hello`: create the group (`create`, the device becomes its creator),
/// register an unknown device that presents a valid join ticket while the
/// group has a free slot, or check a known one's token. Returns the creator.
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
            // Checked after the ticket so only an invited device learns it;
            // returning rolls the ticket's use back.
            let active: i64 = tx.query_row(
                "SELECT COUNT(*) FROM devices WHERE group_id = ?1 AND revoked = 0",
                params![group],
                |r| r.get(0),
            )?;
            if active >= MAX_ACTIVE_DEVICES {
                return Err(SyncError::Code(ErrorCode::GroupFull, "the sync group already has two devices"));
            }
            register(&tx, group, device, auth.token_hash, now)?;
        }
    }
    let creator = group_creator(&tx, group)?;
    tx.commit()?;
    Ok(Welcome { creator })
}

/// What `sync_welcome` tells a device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    /// The device allowed to revoke others (`None`: every member left).
    pub creator: Option<String>,
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

/// `sync_revoke`: mark `target` revoked. A device may revoke itself; only the
/// group's creator may revoke another one. A device not seen yet is recorded
/// as revoked, so it can never register later.
///
/// When the creator revokes itself the role moves on, so the group never
/// loses the one device that may remove others: to `successor` if that is an
/// active member, else to the oldest active member (`None` if none is left).
///
/// When no active device is left the group is purged in the same transaction
/// (devices, invites, the group row); a later hello for it gets
/// `group_not_found`.
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
    Ok(Revoked { creator_moved: moved, purged })
}

/// What a `sync_revoke` changed besides the target.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Revoked {
    /// The new creator, when the role moved.
    pub creator_moved: Option<String>,
    /// No active device was left: the group was deleted.
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

/// Sync state shared by all connections: the auth store (absent = disabled)
/// and the live sync connections per group, for forwarding and revocation.
pub struct SyncHub {
    db: Option<Arc<Mutex<Connection>>>,
    /// Longest accepted `sync_peer_send` `data` (base64 chars).
    max_peer_chars: usize,
    live: Mutex<HashMap<String, Vec<LiveConn>>>,
}

impl SyncHub {
    /// Open the store at `path`, or build a disabled hub when `None`.
    /// `max_peer_bytes`: largest peer message (sealed bytes); its `data` may be
    /// at most the base64 length of this.
    pub fn open(path: Option<&Path>, max_peer_bytes: usize) -> rusqlite::Result<Self> {
        let db = match path {
            Some(p) => Some(Arc::new(Mutex::new(open_db(p)?))),
            None => None,
        };
        Ok(SyncHub {
            db,
            max_peer_chars: max_peer_bytes.div_ceil(3) * 4,
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
            Some(t) if is_removed_frame(t) => {
                send_error(out, id, ErrorCode::Unsupported, "the relay no longer stores sync data");
                false
            }
            Some(FrameType::SyncHello) => {
                if session.is_some() {
                    send_error(out, id, ErrorCode::Forbidden, "already welcomed");
                    return false;
                }
                match self.hello(conn_id, out, req, host_token).await {
                    Ok((s, w)) => {
                        tracing::info!(event = "sync_welcome", conn = conn_id, group = %s.group_id, device = %s.device_id);
                        send_json(out, json!({ "t": "sync_welcome", "creator_device": w.creator }), id);
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
        let (g, d) = (group.clone(), device.clone());
        let welcome = self
            .call(move |c| {
                let auth = HelloAuth {
                    token_hash: &token_hash,
                    create,
                    ticket_hash: ticket_hash.as_deref(),
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
        match t {
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
                if data.len() > self.max_peer_chars {
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
    matches!(
        t,
        FrameType::SyncRevoke | FrameType::SyncInvite | FrameType::SyncPeers | FrameType::SyncPeerSend
    )
}

fn error_code(e: &SyncError) -> ErrorCode {
    match e {
        SyncError::Code(c, _) => *c,
        SyncError::Db(_) | SyncError::TooSlow => ErrorCode::Internal,
    }
}

/// Wait until the connection's queued frames are under
/// [`MAX_QUEUED_REPLY_BYTES`] before giving it another one. This bounds what
/// one client can pin in relay memory by not reading its socket.
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

    fn auth(token_hash: &str, create: bool) -> HelloAuth<'_> {
        HelloAuth { token_hash, create, ticket_hash: None }
    }

    fn with(t: &str) -> HelloAuth<'_> {
        HelloAuth { token_hash: "h", create: false, ticket_hash: Some(t) }
    }

    fn tick(s: &str) -> String {
        sha256_hex(s.as_bytes())
    }

    fn code(r: SyncResult<Welcome>) -> &'static str {
        match r {
            Err(SyncError::Code(c, _)) => c.as_str(),
            Ok(_) => "ok",
            Err(_) => "db",
        }
    }

    /// `device` joins group `g` with a ticket `inviter` registers at `now`.
    fn join(c: &mut Connection, inviter: &str, device: &str, now: i64) -> SyncResult<Welcome> {
        let ticket = tick(&format!("ticket-{device}-{now}"));
        invite(c, "g", inviter, &ticket, 60, now).unwrap();
        hello(c, "g", device, with(&ticket), now)
    }

    /// Group `g` with creator `a` and member `b`.
    fn pair(c: &mut Connection) {
        hello(c, "g", "a", auth("h", true), 0).unwrap();
        join(c, "a", "b", 1).unwrap();
    }

    #[test]
    fn joining_needs_a_valid_unused_ticket() {
        let mut c = mem();
        hello(&mut c, "g", "a", auth("ha", true), 100).unwrap();
        fn hb(t: &str) -> HelloAuth<'_> {
            HelloAuth { token_hash: "hb", create: false, ticket_hash: Some(t) }
        }
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", false), 100)), "ticket_required");
        let (t1, t2) = (&tick("t1"), &tick("t2"));
        assert_eq!(code(hello(&mut c, "g", "b", hb(t1), 100)), "ticket_invalid");
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", true), 100)), "group_exists");
        // Creator retrying its create hello is fine.
        assert_eq!(code(hello(&mut c, "g", "a", auth("ha", true), 100)), "ok");

        invite(&mut c, "g", "a", t1, 60, 100).unwrap();
        assert!(invite(&mut c, "g", "a", t1, 60, 100).is_err(), "duplicate");
        assert!(invite(&mut c, "g", "a", t2, 601, 100).is_err(), "ttl capped");
        assert!(invite(&mut c, "g", "nobody", t2, 60, 100).is_err(), "members only");
        invite(&mut c, "g", "a", t2, 10, 100).unwrap();
        // Expired.
        assert_eq!(code(hello(&mut c, "g", "c", hb(t2), 110)), "ticket_invalid");
        assert_eq!(code(hello(&mut c, "g", "b", hb(t1), 101)), "ok");
        // Used: a second device cannot reuse it.
        assert_eq!(code(hello(&mut c, "g", "c", hb(t1), 102)), "ticket_invalid");
        // The joined device now authenticates with its token alone.
        assert_eq!(code(hello(&mut c, "g", "b", auth("hb", false), 103)), "ok");
        assert_eq!(code(hello(&mut c, "g", "b", auth("other", false), 103)), "auth_failed");
    }

    #[test]
    fn a_third_active_device_is_refused() {
        let mut c = mem();
        pair(&mut c);
        let t = tick("for-c");
        invite(&mut c, "g", "a", &t, 60, 2).unwrap();
        assert_eq!(code(hello(&mut c, "g", "c", with(&t), 2)), "group_full");
        // Without a valid ticket the group's size is not revealed.
        assert_eq!(code(hello(&mut c, "g", "c", with(&tick("nope")), 2)), "ticket_invalid");
        // Known devices still get in.
        assert_eq!(code(hello(&mut c, "g", "b", auth("h", false), 2)), "ok");
        // A revoked device frees its slot; the refused ticket was not used up.
        revoke(&mut c, "g", "b", "b", None).unwrap();
        assert_eq!(code(hello(&mut c, "g", "c", with(&t), 3)), "ok");
    }

    #[test]
    fn only_the_creator_revokes_others() {
        let mut c = mem();
        pair(&mut c);
        assert!(matches!(revoke(&mut c, "g", "b", "a", None), Err(SyncError::Code(ErrorCode::Forbidden, _))));
        assert!(matches!(revoke(&mut c, "g", "b", "x", None), Err(SyncError::Code(ErrorCode::Forbidden, _))));
        assert_eq!(revoke(&mut c, "g", "a", "b", None).unwrap(), Revoked::default());
        assert!(matches!(ensure_active(&c, "g", "b"), Err(SyncError::Code(ErrorCode::DeviceRevoked, _))));
    }

    #[test]
    fn creator_leaving_hands_the_role_on() {
        let creator = |c: &Connection| group_creator(c, "g").unwrap();
        let mut c = mem();
        pair(&mut c);
        // A revoked placeholder (never registered) is not a candidate.
        revoke(&mut c, "g", "a", "x", None).unwrap();
        // Requested successor wins when it is an active member.
        assert_eq!(revoke(&mut c, "g", "a", "a", Some("b")).unwrap().creator_moved.as_deref(), Some("b"));
        assert_eq!(creator(&c).as_deref(), Some("b"));

        // Unknown / revoked successor: the oldest active member.
        join(&mut c, "b", "c", 5).unwrap();
        assert_eq!(revoke(&mut c, "g", "b", "b", Some("x")).unwrap().creator_moved.as_deref(), Some("c"));
        let w = hello(&mut c, "g", "c", auth("h", false), 9).unwrap();
        assert_eq!(w.creator.as_deref(), Some("c"), "the welcome carries the creator");

        // The last member leaving leaves no creator: the group is purged.
        assert_eq!(revoke(&mut c, "g", "c", "c", None).unwrap(), Revoked { creator_moved: None, purged: true });
        assert_eq!(creator(&c), None);
        assert_eq!(code(hello(&mut c, "g", "c", auth("h", false), 10)), "group_not_found");
    }

    #[test]
    fn revoke_before_first_hello_blocks_tofu() {
        let mut c = mem();
        hello(&mut c, "g", "a", auth("h", true), 0).unwrap();
        revoke(&mut c, "g", "a", "b", None).unwrap();
        assert!(matches!(
            hello(&mut c, "g", "b", auth("h2", false), 0),
            Err(SyncError::Code(ErrorCode::DeviceRevoked, _))
        ));
    }

    #[test]
    fn old_store_is_migrated() {
        let mut c = Connection::open_in_memory().unwrap();
        c.execute_batch(
            "CREATE TABLE groups (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_seq INTEGER NOT NULL DEFAULT 0, bytes INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE devices (group_id TEXT NOT NULL, device_id TEXT NOT NULL, token_hash TEXT NOT NULL, revoked INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL, PRIMARY KEY (group_id, device_id));
             CREATE TABLE ops (group_id TEXT NOT NULL, seq INTEGER NOT NULL, device_id TEXT NOT NULL, key TEXT NOT NULL, blob TEXT NOT NULL, PRIMARY KEY (group_id, seq));
             CREATE INDEX idx_ops_key ON ops(group_id, key);
             CREATE TABLE blobs (group_id TEXT NOT NULL, sha TEXT NOT NULL, data BLOB NOT NULL, PRIMARY KEY (group_id, sha));
             CREATE TABLE store_epoch (id INTEGER PRIMARY KEY, epoch TEXT NOT NULL);
             CREATE TABLE epochs (group_id TEXT NOT NULL, epoch TEXT NOT NULL, head INTEGER NOT NULL);
             INSERT INTO groups (id, created_at, last_seq, bytes) VALUES ('g', 0, 1, 1);
             INSERT INTO devices VALUES ('g', 'x', '', 1, 0), ('g', 'b', 'hb', 0, 5), ('g', 'a', 'ha', 0, 3);
             INSERT INTO ops VALUES ('g', 1, 'a', 'k', 'z');
             INSERT INTO blobs VALUES ('g', 's', x'00');",
        )
        .unwrap();
        c.execute_batch(SCHEMA).unwrap();
        upgrade(&c).unwrap();
        assert!(drop_old_data(&c).unwrap().is_some());
        assert!(drop_old_data(&c).unwrap().is_none(), "only once");
        for t in OLD_DATA_TABLES {
            assert!(!table_exists(&c, t).unwrap(), "{t} dropped");
        }
        assert_eq!(columns(&c, "groups").unwrap(), vec!["id", "created_at", "creator_device"]);
        let creator: String = c.query_row("SELECT creator_device FROM groups", [], |r| r.get(0)).unwrap();
        assert_eq!(creator, "a", "earliest real device");
        assert_eq!(code(hello(&mut c, "g", "b", HelloAuth { token_hash: "hb", create: false, ticket_hash: None }, 9)), "ok");
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

    #[tokio::test(start_paused = true)]
    async fn big_queues_wait_for_the_socket_to_drain() {
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
