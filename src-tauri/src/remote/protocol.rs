//! Wire protocol for the Host agent (SRS §5), the Host-side mirror of the
//! relay's `relay/src/protocol.rs`.
//!
//! The Host is a relay *client*: it sends control frames (`register_host`,
//! `create_pair`, `handshake_done`, `revoke`) and data frames (`stream`,
//! `permission_request`, `control`), and receives `room_ready`, `peer_left`,
//! `error` plus data frames from the Controller.
//!
//! Field names / `t` discriminant MUST match the relay so `serde_json` round
//! trips over the wire. The `cipher` payload is opaque to the relay — the Host
//! seals/opens it with the E2E session key (`remote_e2e`).

use serde::{Deserialize, Serialize};

/// Frame type carried in the envelope's `t` field — identical wire strings to
/// the relay's `FrameType` (snake_case).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameType {
    // Control frames.
    RegisterHost,
    Join,
    RoomReady,
    PeerLeft,
    Revoke,
    Error,
    CreatePair,
    HandshakeDone,
    /// Application-level heartbeat that keeps the host↔relay socket warm through
    /// idle-killing proxies once a run stops streaming (see the relay for details).
    Keepalive,

    // Data frames (opaque cipher, forwarded by room_id).
    Stream,
    PermissionRequest,
    Cmd,
    Control,
}

/// The relay envelope (SRS §5.1). Only `t` + `room_id` are meaningful to the
/// relay; `cipher` is E2E-encrypted end to end.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Envelope {
    pub t: FrameType,

    #[serde(default)]
    pub room_id: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cipher: Option<String>,

    // ---- control-frame-only fields ----
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pair_code: Option<String>,

    /// `create_pair`: request a persistent (durable-reconnect) rendezvous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persistent: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl Envelope {
    fn bare(t: FrameType) -> Self {
        Envelope {
            t,
            room_id: String::new(),
            seq: None,
            cipher: None,
            device_id: None,
            auth_token: None,
            pair_code: None,
            persistent: None,
            code: None,
            message: None,
        }
    }

    /// `register_host { device_id, auth_token }` (FR-001). The auth_token is
    /// read from the keyring by the caller — never hardcoded (SEC-001).
    pub fn register_host(device_id: impl Into<String>, auth_token: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::RegisterHost);
        e.device_id = Some(device_id.into());
        e.auth_token = Some(auth_token.into());
        e
    }

    /// `create_pair { pair_code }` (FR-002). Registers a pending pair with the
    /// relay; the relay replies `room_ready` carrying the assigned room_id.
    pub fn create_pair(pair_code: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::CreatePair);
        e.pair_code = Some(pair_code.into());
        e
    }

    /// `create_pair { pair_code, persistent }` — a standing rendezvous that does
    /// not expire by PAIR_TTL, for durable device reconnect.
    pub fn create_pair_persistent(pair_code: impl Into<String>, persistent: bool) -> Self {
        let mut e = Self::create_pair(pair_code);
        e.persistent = Some(persistent);
        e
    }

    /// `handshake_done { room_id }` (FR-003) — half of the HANDSHAKING→ACTIVE
    /// signal (SRS §6.2).
    pub fn handshake_done(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::HandshakeDone);
        e.room_id = room_id.into();
        e
    }

    /// `revoke { room_id }` (FR-010) — Host asks the relay to close the room.
    pub fn revoke(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Revoke);
        e.room_id = room_id.into();
        e
    }

    /// `keepalive { room_id }` — periodic heartbeat that keeps the host↔relay
    /// socket warm (and refreshes the room idle clock) when no run is streaming.
    pub fn keepalive(room_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Keepalive);
        e.room_id = room_id.into();
        e
    }

    /// A data frame carrying a sealed (or, for the handshake, plaintext-base64)
    /// `cipher` routed by `room_id`.
    pub fn data(t: FrameType, room_id: impl Into<String>, cipher: String, seq: u64) -> Self {
        let mut e = Self::bare(t);
        e.room_id = room_id.into();
        e.cipher = Some(cipher);
        e.seq = Some(seq);
        e
    }
}

/// The decrypted business payload carried in a data frame's `cipher` (SRS §5.2).
///
/// Host→Controller uses [`StreamPayload`]; Controller→Host uses [`CmdPayload`].
/// Serialized as `{ "kind": "...", ... }`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StreamPayload {
    /// Raw stream-json SDK message (`run:event`).
    Stream {
        run_id: String,
        event: serde_json::Value,
    },
    /// A console/output line (`run:output`).
    Output {
        run_id: String,
        line: String,
        #[serde(default)]
        is_stderr: bool,
    },
    /// A tool permission request forwarded to the Controller (FR-006/BR-007).
    PermissionRequest {
        run_id: String,
        request_id: String,
        tool: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
    },
    /// The run finished (`run:done`).
    Done { run_id: String, status: String },
    /// A batch of replayed history lines (FR-005). `done` marks the last batch.
    History {
        run_id: String,
        lines: Vec<String>,
        done: bool,
    },
    /// An advisory the Host sends the Controller (e.g. "already handled").
    Notice {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        text: String,
    },
    /// Correlated acknowledgement for a controller command. The Controller only
    /// clears its draft after an accepted ack from the Host.
    CommandAck {
        command_id: String,
        action: String,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// Correlated permission outcome so an old response cannot clear a newer
    /// pending request in the Controller UI.
    PermissionResolved {
        run_id: String,
        request_id: String,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// An opaque session token (Host → Controller) minted once the OTP gate is
    /// passed. Lets the Controller reconnect within the sliding idle window
    /// without re-entering the OTP. Sealed like any other stream payload.
    SessionToken {
        /// Opaque bearer token the Controller persists (localStorage).
        token: String,
        /// The single bound run this session controls.
        run_id: String,
        /// The relay rendezvous code the Controller rejoins on.
        rendezvous_code: String,
        /// Absolute idle expiry (Unix seconds); slides forward on activity.
        idle_expires_at: u64,
    },
    /// Result of the first-frame authentication attempt (OTP or session).
    AuthResult {
        ok: bool,
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    /// The single bound run's metadata (scoped run list — one run in v1).
    RunList { runs: Vec<RunInfo> },
    /// The projects the Controller may create a new run in (kept for parity).
    ProjectList { projects: Vec<ProjectInfo> },
    /// Slash commands the Controller may offer in its composer palette.
    SlashCommandList { commands: Vec<SlashCommandInfo> },
    /// Engine + model options the Controller may offer in its selectors.
    EngineModelOptions { engines: Vec<EngineOption> },
    /// A bounded listing of files under the bound run's project path (for
    /// @mention completion in the controller composer).
    ProjectFileList { run_id: String, files: Vec<String> },
    /// Subscription plan-usage badges (Claude + Codex), mirrored from the desktop
    /// `BudgetBadge`. Each field is a serialized `stats::BudgetStatus` (or null
    /// when unavailable). Pushed on pairing and after each turn finishes.
    PlanUsage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex: Option<serde_json::Value>,
    },
    /// The bound run's live engine/model/permission-mode selection. Pushed on
    /// pairing (initial snapshot) and whenever either surface changes one, so the
    /// controller's composer mirrors the desktop exactly (realtime, both ways).
    RunMeta {
        run_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        engine: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        permission_mode: Option<String>,
    },
}

/// One slash command exposed to the Controller's composer palette.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlashCommandInfo {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One selectable model within an engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    pub label: String,
}

/// One engine + its selectable models exposed to the Controller.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineOption {
    pub id: String,
    pub label: String,
    pub models: Vec<ModelOption>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
}

/// An image attachment carried on a Controller turn (base64, no `data:` prefix).
#[derive(Debug, Clone, Deserialize)]
pub struct ImageAttachmentWire {
    /// Original filename (accepted for display/logging; not required downstream).
    #[allow(dead_code)]
    pub name: String,
    pub mime: String,
    pub data_base64: String,
}

/// The link payload the desktop encodes into the QR / copy-link. Carries the
/// high-entropy `link_secret` out-of-band — it is NEVER sent to the relay.
#[derive(Debug, Clone, Serialize)]
pub struct LinkPayload {
    pub relay_url: String,
    pub host_fingerprint: String,
    pub rendezvous_code: String,
    pub run_id: String,
    pub link_secret: String,
}

/// One project the Controller can start a run in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// One row in a [`StreamPayload::RunList`] — non-secret run metadata mirroring
/// the desktop run list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunInfo {
    pub id: String,
    pub project_id: String,
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Run kind (`session` / `issue` / `pr` …).
    #[serde(rename = "type")]
    pub run_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_number: Option<i64>,
    pub status: String,
    pub engine: String,
    pub created_at: String,
    /// Most recent activity timestamp (finished/started/created) for sorting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
}

/// A command from the Controller (SRS §5.2). `action` is validated against the
/// frozen allow-list before anything else runs (FR-009/AC-12).
#[derive(Debug, Clone, Deserialize)]
pub struct CmdPayload {
    /// Always `"cmd"` in the SRS example; not required for dispatch.
    #[serde(default)]
    #[allow(dead_code)]
    pub kind: Option<String>,
    /// The requested action (allow-list gated).
    pub action: String,
    /// Optional Controller-generated correlation id for delivery ack.
    #[serde(default)]
    pub command_id: Option<String>,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(default)]
    pub request_id: Option<String>,
    /// `respond_permission`: `allow_once` | `deny_once` only (BR-016/AC-17).
    #[serde(default)]
    pub decision: Option<String>,
    /// `respond_permission` for `AskUserQuestion`: the selected answers
    /// (`{ question: answer }`). Forwarded verbatim to the sidecar; only ever
    /// paired with an `allow_once` decision (never relaxes the decision gate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<serde_json::Value>,
    /// `start_run` / `send_chat_message`: free-form message text.
    #[serde(default)]
    pub text: Option<String>,
    /// `start_run`: project to create a session run in (when no run_id given).
    #[serde(default)]
    pub project_id: Option<String>,
    /// Optional engine hint for `start_run` (`claude` | `codex`).
    #[serde(default)]
    pub engine: Option<String>,
    /// Optional model hint for `start_run` / `send_chat_message`.
    #[serde(default)]
    pub model: Option<String>,
    /// `start_run` / `set_run_meta`: the permission mode to apply. Honored for
    /// execution (desktop parity); a blank/omitted value falls back to the
    /// settings default. Also mirrored to the desktop composer via the meta store.
    #[serde(default)]
    pub permission_mode: Option<String>,
    /// A slash command name the Controller invoked (accepted+ignored in v1).
    #[serde(default)]
    #[allow(dead_code)]
    pub slash_command: Option<String>,
    /// @mentioned file paths, inlined into the prompt text.
    #[serde(default)]
    pub mentions: Option<Vec<String>>,
    /// Image attachments (base64), converted to the internal ImageAttachment.
    #[serde(default)]
    pub attachments: Option<Vec<ImageAttachmentWire>>,
    /// Which credential the Controller believes it authenticated with
    /// (`otp` | `session`) — advisory only.
    #[serde(default)]
    #[allow(dead_code)]
    pub auth_mode: Option<String>,
}

/// The E2E handshake exchange (spec §3). Ephemeral public keys are NOT secret,
/// so this rides in the `cipher` field as base64(JSON) over a `control` data
/// frame — the relay still cannot forge a session key without the out-of-band
/// `psk`. `host_fingerprint` lets the Controller detect a relay MITM (SEC-006).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandshakePayload {
    /// Host → Controller: offer the Host's ephemeral public key + fingerprint.
    HsOffer {
        host_pub: String,        // hex
        host_fingerprint: String, // hex
    },
    /// Controller → Host: answer with the Controller's ephemeral public key.
    HsAnswer {
        controller_pub: String, // hex
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_wire_matches_relay_field_names() {
        let e = Envelope::register_host("dev-1", "tok");
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["t"], "register_host");
        assert_eq!(j["device_id"], "dev-1");
        assert_eq!(j["auth_token"], "tok");
        // Empty room_id + None options are omitted or empty, never null noise.
        assert_eq!(j["room_id"], "");
        assert!(j.get("cipher").is_none());
    }

    #[test]
    fn data_frame_roundtrips() {
        let e = Envelope::data(FrameType::Stream, "rm_1", "Y2lwaGVy".to_string(), 7);
        let text = serde_json::to_string(&e).unwrap();
        let back: Envelope = serde_json::from_str(&text).unwrap();
        assert_eq!(back.t, FrameType::Stream);
        assert_eq!(back.room_id, "rm_1");
        assert_eq!(back.cipher.as_deref(), Some("Y2lwaGVy"));
        assert_eq!(back.seq, Some(7));
    }

    #[test]
    fn cmd_payload_parses_respond_permission() {
        let v = serde_json::json!({
            "kind": "cmd", "action": "respond_permission",
            "run_id": "r_1", "request_id": "p_9", "decision": "allow_once"
        });
        let cmd: CmdPayload = serde_json::from_value(v).unwrap();
        assert_eq!(cmd.action, "respond_permission");
        assert_eq!(cmd.request_id.as_deref(), Some("p_9"));
        assert_eq!(cmd.decision.as_deref(), Some("allow_once"));
        // No AskUserQuestion answers on a plain permission response.
        assert!(cmd.answers.is_none());
    }

    #[test]
    fn cmd_payload_parses_ask_user_question_answers() {
        let v = serde_json::json!({
            "kind": "cmd", "action": "respond_permission",
            "run_id": "r_1", "request_id": "p_9", "decision": "allow_once",
            "answers": { "Which framework?": "Vue, React" }
        });
        let cmd: CmdPayload = serde_json::from_value(v).unwrap();
        assert_eq!(cmd.decision.as_deref(), Some("allow_once"));
        let answers = cmd.answers.expect("answers present");
        assert_eq!(answers["Which framework?"], "Vue, React");
    }
}
