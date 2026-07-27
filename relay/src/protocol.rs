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
    /// Either peer signals its E2E handshake half completed (drives
    /// HANDSHAKING → ACTIVE in the room state machine, SRS §6.2).
    HandshakeDone,
    /// Controller re-attaches to a room it was dropped from, within the
    /// `reconnect_window`, using a `resume_token` (phone screen off/on). Re-runs
    /// the E2E handshake so forward secrecy is preserved.
    Resume,
    /// Application-level heartbeat (either peer → relay). Carries no payload; the
    /// relay treats it as room activity (resets the idle-sweep clock) and echoes
    /// it back so BOTH directions stay warm through idle-killing proxies. Needed
    /// because a browser cannot originate WS ping frames and a finished run stops
    /// all other traffic, so the socket would otherwise be reaped when idle.
    Keepalive,

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

    /// `register_host`: shared secret to authenticate the Host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,

    /// `create_pair` / `join`: one-time pairing code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_code: Option<String>,

    /// `create_pair`: when true the pending room does not expire by PAIR_TTL — a
    /// standing rendezvous a trusted device re-joins anytime (durable reconnect).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,

    /// `room_ready` (relay → Controller): the rotating token the Controller
    /// presents in a later `resume` frame. `resume` (Controller → relay): the
    /// token being redeemed. Never sent to the Host; not an E2E secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_token: Option<String>,

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
            code: None,
            message: None,
        }
    }

    /// Build a `room_ready` for the Controller that also carries the rotating
    /// `resume_token` (used to re-attach after a drop within the window).
    pub fn room_ready_with_token(
        room_id: impl Into<String>,
        resume_token: impl Into<String>,
    ) -> Self {
        let mut e = Self::room_ready(room_id);
        e.resume_token = Some(resume_token.into());
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
    /// `resume_token` unknown, already rotated, or the reconnect window elapsed.
    ResumeInvalid,
    /// A control frame is not permitted for this connection role.
    Forbidden,
    /// Too many frames in the rate-limit window.
    RateLimited,
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
            ErrorCode::Forbidden => "forbidden",
            ErrorCode::RateLimited => "rate_limited",
        }
    }
}
