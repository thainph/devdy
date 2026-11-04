//! Wire protocol seen by the relay (SRS §5).
//!
//! The relay only ever reads the frame type `t` and the routing key `room_id`.
//! The `cipher` field is opaque: it is NEVER parsed and NEVER logged (SEC-006 /
//! NFR-005). Business payloads are E2E-encrypted between Host and Controller.

use serde::{Deserialize, Serialize};

use crate::presence::HostPresence;

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
    /// Client → relay: list the online Hosts. Carries the host `auth_token`
    /// (only the owner's own Devdy desktops hold it) and optionally the
    /// caller's `device_id`, which is left out of the answer.
    ListHosts,
    /// Relay → client: the answer to `list_hosts`, in `hosts`.
    Hosts,

    // Data frames (opaque cipher, forwarded by room_id).
    Stream,
    PermissionRequest,
    Cmd,
    Control,
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

    /// `register_host` (optional, newer Hosts): random id minted each time the
    /// app starts. Tells a reconnect of the same running app (same id: takes
    /// over its stale socket) from another machine using the same `device_id`
    /// (different id while the first is connected: refused with
    /// `device_id_conflict`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,

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

    /// `register_host` (optional, newer Hosts): the device alias to list this
    /// Host under. Its presence (with `app_version`) opts the Host into
    /// `list_hosts`; a re-register without it withdraws the Host from the list.
    /// Also on `error{alias_conflict}`: the alias another listed Host has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    /// `register_host` (optional, newer Hosts): the Devdy app version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,

    /// `hosts` (relay → client): the online Hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<HostPresence>>,

    /// `error`: machine-readable error code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    /// `error`: human-readable message (no secrets, no cipher).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Envelope {
    /// A frame of type `t` with every optional field empty.
    pub fn bare(t: FrameType) -> Self {
        Envelope {
            t,
            room_id: String::new(),
            seq: None,
            cipher: None,
            device_id: None,
            instance_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            resume_token: None,
            slot: None,
            alias: None,
            app_version: None,
            hosts: None,
            code: None,
            message: None,
        }
    }

    /// Build an `error` control frame (relay → sender).
    pub fn error(code: ErrorCode, message: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Error);
        e.code = Some(code.as_str().to_string());
        e.message = Some(message.into());
        e
    }

    /// Build a `room_ready` control frame carrying the assigned `room_id`.
    pub fn room_ready(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::RoomReady);
        e.room_id = room_id.into();
        e
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
        let mut e = Self::bare(FrameType::PeerLeft);
        e.room_id = room_id.into();
        e
    }

    /// Build a `keepalive` heartbeat frame for `room_id` (empty for a pre-room
    /// socket-warming beat). The relay echoes this back to the sender.
    pub fn keepalive(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Keepalive);
        e.room_id = room_id.into();
        e
    }

    /// Ack for a control frame that has no dedicated response type.
    pub fn control_ack(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Control);
        e.room_id = room_id.into();
        e.code = Some("ok".to_string());
        e
    }

    /// Answer to `list_hosts`.
    pub fn hosts(hosts: Vec<HostPresence>) -> Self {
        let mut e = Self::bare(FrameType::Hosts);
        e.hosts = Some(hosts);
        e
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
    /// `register_host`: another live connection of a different app instance
    /// already uses this `device_id`. The newcomer is refused and closed.
    DeviceIdConflict,
    /// `register_host`: another listed Host (another device) uses the same
    /// alias. Only a warning: the registration still went through.
    AliasConflict,
    /// Relay → Host: another connection now listens on a code this Host
    /// listened on (in `slot`); this Host no longer gets its joins.
    ListenSuperseded,
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
            ErrorCode::DeviceIdConflict => "device_id_conflict",
            ErrorCode::AliasConflict => "alias_conflict",
            ErrorCode::ListenSuperseded => "listen_superseded",
        }
    }
}
