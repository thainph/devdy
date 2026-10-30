//! Wire protocol seen by the relay (SRS §5).
//!
//! The relay only ever reads the frame type `t` and the routing key `room_id`.
//! The `cipher` field is opaque: it is NEVER parsed and NEVER logged (SEC-006 /
//! NFR-005). Business payloads are E2E-encrypted between Host and Controller.

use serde::{Deserialize, Serialize};

/// Frame type discriminator carried in the envelope's `t` field (SRS §5.1/§5.3).
///
/// Control frames drive the room lifecycle; the `Stream`, `PermissionRequest`
/// and `Cmd` variants carry an opaque `cipher` that is forwarded verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameType {
    // Control frames (relay endpoints).
    RegisterHost,
    Join,
    RoomReady,
    PeerLeft,
    Revoke,
    Error,
    /// Host announces a fresh `pair_code` awaiting a Controller.
    /// Not in the SRS §5.3 table but required so the relay can hold the
    /// pending-pair state on the Host's behalf (see dev artifact for rationale).
    CreatePair,
    /// Legacy: peers used to signal handshake completion to drive a room state
    /// machine. Rooms now route as soon as they exist; accepted and ignored.
    HandshakeDone,
    /// Legacy: older Controllers try to re-attach with a resume token. The relay
    /// always answers `resume_invalid`, which makes them `join` again.
    Resume,
    /// Application-level heartbeat (either peer → relay). Carries no payload; it
    /// proves the sender is alive (liveness) and the relay echoes it back so
    /// BOTH directions stay warm through idle-killing proxies. Needed
    /// because a browser cannot originate WS ping frames and a finished run stops
    /// all other traffic, so the socket would otherwise be reaped when idle.
    Keepalive,

    // Data frames (opaque cipher, forwarded by room_id).
    Stream,
    PermissionRequest,
    Cmd,
    Control,

    // Two-way sync (design two-way-sync §9.2). A sync connection opens with
    // `sync_hello` and from then on carries only these frames + keepalive;
    // their bodies are parsed by `sync.rs`, never logged.
    SyncHello,
    SyncWelcome,
    SyncPush,
    SyncAck,
    SyncPull,
    SyncOps,
    SyncNotify,
    SyncRevoke,
    BlobPut,
    BlobAck,
    BlobGet,
    BlobData,
}

/// The outermost envelope the relay can observe (SRS §5.1).
///
/// Deserialized with `#[serde(default)]` on optional fields so a malformed or
/// partial frame degrades to a routable/typed error rather than a panic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    /// Frame type — the only routing/dispatch discriminator besides `room_id`.
    pub t: FrameType,

    /// Routing key. Empty for pre-room control frames (register_host/join/create_pair).
    #[serde(default)]
    pub room_id: String,

    /// Monotonic sequence for loss/duplication detection (SRS §5.1). Optional.
    #[serde(default)]
    pub seq: Option<u64>,

    /// Opaque E2E-encrypted business payload. Relay never decodes or logs this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cipher: Option<String>,

    // ---- control-frame-only fields (present only on control frames) ----
    /// `register_host`: device identifier (metadata only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,

    /// `register_host`: shared secret to authenticate the Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,

    /// `create_pair` / `join`: one-time pairing code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_code: Option<String>,

    /// `create_pair`: legacy flag. Every listen now lives as long as the Host's
    /// connection, so it is accepted and ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,

    /// `resume` (legacy Controller → relay): ignored; always `resume_invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,

    /// `create_pair` / `revoke` (Host → relay) and `room_ready` (relay → Host):
    /// which listen slot the code belongs to. Absent = the session slot;
    /// `"login"` = password login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,

    /// `error`: machine-readable error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    /// `error`: human-readable message (no secrets, no cipher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Envelope {
    /// Build an `error` control frame (relay → sender).
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        Envelope {
            t: FrameType::Error,
            room_id: String::new(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            code: Some(code.as_str().to_string()),
            message: Some(message.into()),
        }
    }

    /// Build a `room_ready` control frame carrying the assigned `room_id`.
    pub fn room_ready(room_id: impl Into<String>) -> Self {
        Envelope {
            t: FrameType::RoomReady,
            room_id: room_id.into(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            code: None,
            message: None,
        }
    }

    /// Ack for `create_pair` (relay → Host): the Host now listens on its code.
    /// Carries the room id the next join will get and `code: "armed"` so the
    /// Host can tell it apart from a real join's `room_ready`.
    pub fn room_armed(room_id: impl Into<String>) -> Self {
        let mut e = Self::room_ready(room_id);
        e.code = Some("armed".to_string());
        e
    }

    /// Build a `peer_left` control frame for the surviving peer.
    pub fn peer_left(room_id: impl Into<String>) -> Self {
        Envelope {
            t: FrameType::PeerLeft,
            room_id: room_id.into(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            code: None,
            message: None,
        }
    }

    /// Build a `keepalive` heartbeat frame for `room_id` (empty for a pre-room
    /// socket-warming beat). The relay echoes this back to the sender.
    pub fn keepalive(room_id: impl Into<String>) -> Self {
        Envelope {
            t: FrameType::Keepalive,
            room_id: room_id.into(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            code: None,
            message: None,
        }
    }

    /// Ack for a control frame that has no dedicated response type.
    pub fn control_ack(room_id: impl Into<String>) -> Self {
        Envelope {
            t: FrameType::Control,
            room_id: room_id.into(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            code: Some("ok".to_string()),
            message: None,
        }
    }
}

/// Stable, machine-readable error codes returned in `error` frames.
#[derive(Debug, Clone, Copy)]
pub enum ErrorCode {
    /// Authentication of the Host failed (bad `auth_token`).
    AuthFailed,
    /// The frame was malformed or of an unexpected type for the current state.
    BadRequest,
    /// `pair_code` unknown, already used, or expired (SRS §6.2 EXPIRED).
    PairInvalid,
    /// The room already has its single allowed Controller (MAX_CONTROLLER=1).
    RoomFull,
    /// No such room / room already CLOSED.
    RoomNotFound,
    /// `resume` is no longer supported; the Controller should `join` instead.
    /// Kept so older Controllers fall back to a fresh join.
    ResumeInvalid,
    /// A newer Controller joined the same code and replaced this one.
    Superseded,
    /// A control frame is not permitted for this connection role.
    Forbidden,
    /// Too many frames in the rate-limit window.
    RateLimited,
    /// The relay runs without `RELAY_SYNC_DB`.
    SyncDisabled,
    /// `sync_hello` for an unknown group without `create`.
    GroupNotFound,
    /// The device was revoked (`sync_revoke`).
    DeviceRevoked,
    /// The group would exceed `RELAY_SYNC_MAX_GROUP_BYTES`.
    QuotaExceeded,
    /// `blob_get` for a sha the group does not hold.
    BlobNotFound,
    /// `blob_put` data does not hash to the given sha.
    ShaMismatch,
    /// A blob or op exceeds `RELAY_SYNC_MAX_BLOB_BYTES`.
    TooLarge,
    /// The sync store failed (details only in the relay log).
    Internal,
}

impl ErrorCode {
    /// Wire string for the error code.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::AuthFailed => "auth_failed",
            ErrorCode::BadRequest => "bad_request",
            ErrorCode::PairInvalid => "pair_invalid",
            ErrorCode::RoomFull => "room_full",
            ErrorCode::RoomNotFound => "room_not_found",
            ErrorCode::ResumeInvalid => "resume_invalid",
            ErrorCode::Superseded => "superseded",
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::RateLimited => "rate_limited",
            ErrorCode::SyncDisabled => "sync_disabled",
            ErrorCode::GroupNotFound => "group_not_found",
            ErrorCode::DeviceRevoked => "device_revoked",
            ErrorCode::QuotaExceeded => "quota_exceeded",
            ErrorCode::BlobNotFound => "blob_not_found",
            ErrorCode::ShaMismatch => "sha_mismatch",
            ErrorCode::TooLarge => "too_large",
            ErrorCode::Internal => "internal",
        }
    }
}
