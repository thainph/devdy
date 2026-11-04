//! Wire protocol for the Host agent (SRS §5), the Host-side mirror of the
//! relay's `relay/src/protocol.rs`.
//!
//! The Host is a relay *client*: it sends control frames (`register_host`,
//! `create_pair`, `revoke`) and data frames (`stream`,
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
    /// Device sync: ask the relay which other Hosts are online (host token).
    ListHosts,
    /// Device sync: the relay's answer to `list_hosts`.
    Hosts,

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

    /// `create_pair` / `revoke` / `room_ready`: the relay listen slot. Absent =
    /// the session slot; `"login"` = password login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,

    /// `register_host`: the alias + app version this Host is listed under in
    /// `list_hosts` (both absent = not listed). Older relays ignore them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,

    /// `hosts`: the online Hosts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hosts: Option<Vec<HostPresence>>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// One online Host in a relay `hosts` answer (wire names match the relay).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostPresence {
    pub device_id: String,
    #[serde(default)]
    pub alias: String,
    #[serde(default)]
    pub app_version: String,
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
            slot: None,
            alias: None,
            app_version: None,
            hosts: None,
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

    /// `register_host` that also lists this Host for device sync under
    /// `alias` (`None` = not listed).
    pub fn register_host_listed(
        device_id: impl Into<String>,
        auth_token: impl Into<String>,
        listing: Option<(String, String)>,
    ) -> Self {
        let mut e = Self::register_host(device_id, auth_token);
        if let Some((alias, app_version)) = listing {
            e.alias = Some(alias);
            e.app_version = Some(app_version);
        }
        e
    }

    /// `list_hosts { auth_token, device_id }` — the other online Hosts.
    pub fn list_hosts(auth_token: impl Into<String>, device_id: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::ListHosts);
        e.auth_token = Some(auth_token.into());
        e.device_id = Some(device_id.into());
        e
    }

    /// `join { pair_code }` — this Devdy acting as a Controller.
    pub fn join(pair_code: impl Into<String>) -> Self {
        let mut e = Self::bare(FrameType::Join);
        e.pair_code = Some(pair_code.into());
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

    /// `create_pair` in a named listen slot (password login).
    pub fn create_pair_slot(pair_code: impl Into<String>, slot: impl Into<String>) -> Self {
        let mut e = Self::create_pair_persistent(pair_code, true);
        e.slot = Some(slot.into());
        e
    }

    /// Stop listening in a named slot (password login disabled).
    pub fn revoke_slot(slot: impl Into<String>) -> Self {
        let mut e = Self::revoke(String::new());
        e.slot = Some(slot.into());
        e
    }

    /// `revoke { room_id }` (FR-010) — Host asks the relay to close the room; an
    /// empty `room_id` ends the session (stop listening + close the room).
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
    /// Several consecutive output lines of one run, coalesced into a single
    /// frame to cut per-line WebSocket + AEAD overhead on a mobile link.
    ///
    /// Only sent to a Controller that advertised [`FEATURE_OUTPUT_BATCH`] in its
    /// first frame's `client_features`; older bundles keep receiving one
    /// [`StreamPayload::Output`] per line. Lines are in emission order and must
    /// be applied in order — a batch is exactly the `Output` frames it replaces.
    OutputBatch {
        run_id: String,
        lines: Vec<OutputLine>,
    },
    /// A tool permission request forwarded to the Controller (FR-006/BR-007).
    PermissionRequest {
        run_id: String,
        request_id: String,
        tool: String,
        input: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        /// A broker prompt (gh / glab / ssh / aws / az / terraform): answer it
        /// once only — Always is refused for it.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        once_only: bool,
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
        /// `start_run` that created a new session: the new run's id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_id: Option<String>,
        /// `todo_add` / `note_add`: the new item's id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        item_id: Option<String>,
    },
    /// Correlated permission outcome so an old response cannot clear a newer
    /// pending request in the Controller UI.
    PermissionResolved {
        run_id: String,
        request_id: String,
        accepted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
        /// Answers with an Always scope only: whether the standing choice was
        /// handed to the desktop to keep (false = it applied once only, e.g.
        /// the tool has no per-tool Always or the run has no project).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remembered: Option<bool>,
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
        /// What this Host supports beyond the base protocol ([`HOST_FEATURES`]).
        /// An older Host omits it, and a newer Controller then hides the
        /// screens and buttons it would only get refused for.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        host_features: Vec<String>,
    },
    /// Remote desktop: the outcome of one `ipc_invoke`. `value` is the command's
    /// return value, `error` its rejection, both exactly as the local UI sees.
    /// `value` is always present: a command that resolved to `null` must not
    /// arrive as a missing field (`undefined`) on the Controller.
    IpcResult {
        id: String,
        ok: bool,
        #[serde(default)]
        value: serde_json::Value,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<serde_json::Value>,
    },
    /// Remote desktop: one app event the Controller subscribed to.
    IpcEvent {
        event: String,
        payload: serde_json::Value,
    },
    /// Remote desktop: events were shed on a congested link, so the
    /// Controller's view may be stale — it should refetch its state.
    IpcResync,
    /// Password login succeeded (Host → Controller, on the login room): a full
    /// session link + token so the Controller continues on the session path.
    LoginGranted {
        host_fingerprint: String,
        rendezvous_code: String,
        run_id: String,
        link_secret: String,
        token: String,
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
    EngineModelOptions {
        /// LEGACY: the curated alias table, duplicated host-side. No shipped
        /// controller renders it — both surfaces read the curated table from
        /// their own bundle — so it is kept only so an older client parsing this
        /// frame still finds the field it expects.
        engines: Vec<EngineOption>,
        /// Models DISCOVERED from the account (Agent SDK `supportedModels()`),
        /// which only the Host can know. The controller merges these with its
        /// curated table exactly as the desktop composer does, so the two
        /// selectors offer the same list.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        claude_models: Vec<DiscoveredModel>,
        /// Models discovered from `codex debug models`. Authoritative when
        /// non-empty: it REPLACES the curated Codex list.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        codex_models: Vec<DiscoveredModel>,
        /// Managed Claude accounts the Controller may switch a run to, mirroring
        /// the desktop run-settings menu. Empty when none are configured.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        claude_accounts: Vec<ClaudeAccountChoice>,
    },
    /// A bounded listing of files under the bound run's project path (for
    /// @mention completion in the controller composer).
    ProjectFileList { run_id: String, files: Vec<String> },
    /// Subscription plan-usage badges (Claude + Codex), mirrored from the desktop
    /// `BudgetBadge`. Each field is a serialized `stats::BudgetStatus` (or null
    /// when unavailable). Pushed on pairing and after each turn finishes.
    PlanUsage {
        /// Usage for the ONE account the focused run uses. Kept for controllers
        /// built before `claude_accounts` existed; newer ones prefer that list.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        codex: Option<serde_json::Value>,
        /// Label of the Claude account the focused run is using (so the
        /// controller shows WHICH account the utilization belongs to). None for
        /// a global/legacy run with no managed account.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude_account: Option<String>,
        /// Every managed Claude account with its own usage — desktop
        /// `BudgetBadge` parity, which renders one row per account. Empty when
        /// no accounts are configured (the legacy single-snapshot setup), in
        /// which case `claude` above is the whole story.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        claude_accounts: Vec<ClaudeAccountUsage>,
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
        /// Which managed Claude account the run executes with. `Some("")` means
        /// the global `~/.claude` (a legacy run), `None` means "not reported" —
        /// the two differ, so the Controller must not collapse them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        claude_account_id: Option<String>,
    },
    // ---- Data features ([`FEATURE_DATA`]) ----
    /// Every todo (the Controller filters by project / done locally).
    TodoList { todos: Vec<TodoInfo> },
    /// Notes matching a `note_list` / `note_search`, echoing the filter so the
    /// Controller can drop a stale reply. Title + a short preview only; the
    /// editor fetches the full note with `note_get`.
    NoteList {
        notes: Vec<NoteSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        project_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
    },
    /// What is switched on for one project. Names + on/off only: never an MCP
    /// command, URL, env / header key or value, a server's host / user, an
    /// account's login / email, or a token.
    ProjectToggles {
        project_id: String,
        mcp_servers: Vec<ToggleItem>,
        skills: Vec<ToggleItem>,
        rules: Vec<ToggleItem>,
        builtin_devdy: BuiltinToggle,
        /// VPS servers (by label).
        #[serde(default)]
        servers: Vec<ToggleItem>,
        #[serde(default)]
        github_accounts: Vec<ToggleItem>,
        #[serde(default)]
        gitlab_accounts: Vec<ToggleItem>,
        #[serde(default)]
        aws_accounts: Vec<ToggleItem>,
        #[serde(default)]
        azure_accounts: Vec<ToggleItem>,
        /// Managed Claude accounts; at most one is `enabled` (the project's
        /// link). None enabled means the project follows the default account.
        #[serde(default)]
        claude_accounts: Vec<ClaudeAccountToggle>,
    },
    /// The desktop's look (Settings → Appearance) so the Controller renders
    /// with the same palette. Pushed on pairing.
    Appearance {
        /// `light` / `dark` / `system`.
        theme: String,
        /// The named palette (`default`, …).
        color_theme: String,
    },
    /// One full note (`note_get`).
    Note { note: NoteInfo },
    // ---- Web Push ([`FEATURE_PUSH`]) ----
    /// What the Controller needs to subscribe: the VAPID public key (base64url,
    /// the browser's `applicationServerKey`), whether the endpoint it asked
    /// about is subscribed, and which kinds the desktop has switched on.
    PushConfig {
        vapid_public_key: String,
        subscribed: bool,
        events: crate::remote::push::coalesce::PushEvents,
    },
}

/// One todo as the Controller sees it — no Focus fields (priority, estimate,
/// AI plan…), which the phone does not edit.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TodoInfo {
    pub id: String,
    /// The full text: the first line is the title, the rest a note.
    pub text: String,
    pub done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    /// Local `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_time: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

/// One note (Markdown) as the Controller sees it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteInfo {
    pub id: String,
    pub title: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub updated_at: String,
}

/// One row of the note list: no full body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteSummary {
    pub id: String,
    pub title: String,
    /// The start of the Markdown body (at most [`NOTE_PREVIEW_CHARS`] chars).
    pub preview: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub updated_at: String,
}

/// How much of a note body the list carries.
pub const NOTE_PREVIEW_CHARS: usize = 300;

/// A skill / rule / MCP server switch for one project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToggleItem {
    pub id: String,
    pub name: String,
    pub enabled: bool,
}

/// One managed Claude account as a project switch: `enabled` when the project
/// is linked to it, `is_default` for the app-wide default it otherwise follows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaudeAccountToggle {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub is_default: bool,
}

/// The built-in `devdy` MCP server switch. `global` is true while the setting
/// is app-wide (it is today): flipping it affects every project.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuiltinToggle {
    pub enabled: bool,
    pub global: bool,
}

/// One managed Claude account offered in the Controller's run-settings menu.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAccountChoice {
    pub id: String,
    pub label: String,
    pub is_default: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

/// A model discovered from the account rather than from the curated alias table.
/// Mirrors `commands::models::ModelOption` and the controller's `FetchedModel`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveredModel {
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// One managed Claude account's plan utilization, mirroring a desktop
/// `BudgetBadge` row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeAccountUsage {
    pub id: String,
    /// Human label the Owner gave the account.
    pub label: String,
    /// Whether this is the account the focused run actually executes with, so
    /// the controller can mark the row that matters right now.
    pub active: bool,
    /// Serialized `stats::BudgetStatus`, or null when no snapshot exists yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<serde_json::Value>,
}

/// One console line inside a [`StreamPayload::OutputBatch`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputLine {
    pub line: String,
    #[serde(default)]
    pub is_stderr: bool,
}

/// Feature flag a Controller advertises to receive coalesced output batches.
pub const FEATURE_OUTPUT_BATCH: &str = "output_batch";

/// Feature flag a Controller advertises to drive MORE than the run its link was
/// minted for: it can list every run, `open_run`/`close_run` to move between
/// them, and receives permission requests and status changes for ALL runs.
///
/// Gated because an older bundle renders exactly one run — handing it a prompt
/// for a run it cannot display would strand the user. Without this flag the Host
/// behaves exactly as it did before multi-run: one bound run, nothing else
/// forwarded. The trust boundary is unchanged either way (device approval,
/// SRS BR-005 v1.1) — this only controls what the client is able to render.
pub const FEATURE_MULTI_RUN: &str = "multi_run";

/// Feature flag a Controller advertises to manage todos, notes and per-project
/// switches (`todo_*`, `note_*`, `project_toggle*`). Without it those actions
/// are refused, so an older bundle sees exactly the pre-data Host.
pub const FEATURE_DATA: &str = "data";

/// Feature flag a Controller advertises to manage Web Push (`push_*`). Without
/// it those actions are refused.
pub const FEATURE_PUSH: &str = "push";

/// What this Host advertises in `session_token.host_features`: the data
/// screens, the Inbox's `list_pending`, Always (`allow_always` /
/// `deny_always`) from a Controller, and Web Push.
pub const HOST_FEATURES: [&str; 4] = ["data", "inbox", "remember", "push"];

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
    /// When the run's last turn ended. A multi-turn session's sidecar exits
    /// after every turn (status `done`, waiting for the next message) while
    /// `updated_at` stays on the last user activity — so the Inbox's "since
    /// last visit" keys on this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<String>,
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
    /// `respond_permission`: `allow_once` | `deny_once` | `allow_always` | `deny_always` (BR-016/AC-17).
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
    /// `set_run_meta`: switch the run's managed Claude account. Empty string
    /// selects the global `~/.claude`. Absent leaves the account untouched —
    /// which is why this is an Option and not a plain String.
    #[serde(default)]
    pub claude_account_id: Option<String>,
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
    /// Optional capability flags the Controller advertises on its FIRST (auth)
    /// frame, e.g. [`FEATURE_OUTPUT_BATCH`]. Absent for older bundles, which is
    /// why every negotiated feature must default to the old behaviour.
    #[serde(default)]
    pub client_features: Option<Vec<String>>,
    /// `ipc_invoke`: correlation id echoed in the `ipc_result`.
    #[serde(default)]
    pub id: Option<String>,
    /// `ipc_invoke`: the Tauri command to run, exactly as the UI named it.
    #[serde(default)]
    pub cmd: Option<String>,
    /// `ipc_invoke`: the command's arguments, verbatim.
    #[serde(default)]
    pub args: Option<serde_json::Value>,
    /// `ipc_listen` / `ipc_unlisten`: the app event name.
    #[serde(default)]
    pub event: Option<String>,
    // ---- Data features ([`FEATURE_DATA`]) ----
    /// The todo / note / skill / rule / MCP server a data action targets.
    #[serde(default)]
    pub item_id: Option<String>,
    /// `todo_update` / `note_update`: the fields to change. A missing key is
    /// left alone, `null` clears it.
    #[serde(default)]
    pub patch: Option<serde_json::Value>,
    /// `note_search`: the text to look for in titles and contents.
    #[serde(default)]
    pub query: Option<String>,
    /// `note_add`: the note's title.
    #[serde(default)]
    pub title: Option<String>,
    /// `note_add`: the note's Markdown.
    #[serde(default)]
    pub content: Option<String>,
    /// `todo_add`: local `YYYY-MM-DD` deadline.
    #[serde(default)]
    pub due_date: Option<String>,
    /// `project_toggle_set`: `skill` | `rule` | `mcp` | `builtin_mcp`.
    #[serde(default)]
    pub toggle_kind: Option<String>,
    /// `project_toggle_set`: the new state.
    #[serde(default)]
    pub enabled: Option<bool>,
    // ---- Web Push ([`FEATURE_PUSH`]) ----
    /// `push_*`: the browser's push endpoint (identifies the subscription).
    #[serde(default)]
    pub endpoint: Option<String>,
    /// `push_subscribe`: the subscription's P-256 public key (base64url).
    #[serde(default)]
    pub p256dh: Option<String>,
    /// `push_subscribe`: the subscription's auth secret (base64url).
    #[serde(default)]
    pub auth: Option<String>,
    /// `push_subscribe`: a device name for the desktop list.
    #[serde(default)]
    pub label: Option<String>,
    /// `push_visibility`: whether the controller is on screen.
    #[serde(default)]
    pub visible: Option<bool>,
    /// `push_subscribe`: the phone re-subscribed on its own (not a tap on
    /// "turn on"), so a removal on the desktop stands.
    #[serde(default)]
    pub auto: Option<bool>,
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
    /// Host → Controller: the first sealed frame opened with none of the
    /// accepted secrets (wrong OTP, or a session token that was rotated away by
    /// the kill switch / a password change). Unsealed by necessity — the Host
    /// has no key the Controller shares — and carries nothing.
    HsReject,
    /// Host → device-sync puller: the pull was refused before any key was
    /// shared (`wrong_password`, `rate_limited`, `bad_request`). Unsealed for
    /// the same reason as [`HandshakePayload::HsReject`]; carries only the code.
    SyncReject { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_token_advertises_host_features() {
        let v = serde_json::to_value(StreamPayload::SessionToken {
            token: "t".into(),
            run_id: "r".into(),
            rendezvous_code: "c".into(),
            idle_expires_at: 1,
            host_features: HOST_FEATURES.iter().map(|f| f.to_string()).collect(),
        })
        .unwrap();
        assert_eq!(v["host_features"], serde_json::json!(["data", "inbox", "remember", "push"]));
    }

    #[test]
    fn push_config_wire_matches_the_controller() {
        let v = serde_json::to_value(StreamPayload::PushConfig {
            vapid_public_key: "BPk".into(),
            subscribed: true,
            events: Default::default(),
        })
        .unwrap();
        assert_eq!(
            v,
            serde_json::json!({
                "kind": "push_config", "vapid_public_key": "BPk", "subscribed": true,
                "events": { "pending": true, "done": true, "failed": true }
            })
        );
    }

    #[test]
    fn cmd_payload_parses_push_subscribe() {
        let cmd: CmdPayload = serde_json::from_value(serde_json::json!({
            "kind": "cmd", "action": "push_subscribe", "endpoint": "https://fcm.googleapis.com/x",
            "p256dh": "BCV", "auth": "BTB", "label": "iPhone"
        }))
        .unwrap();
        assert_eq!(cmd.endpoint.as_deref(), Some("https://fcm.googleapis.com/x"));
        assert_eq!((cmd.p256dh.as_deref(), cmd.auth.as_deref()), (Some("BCV"), Some("BTB")));
        assert_eq!(cmd.label.as_deref(), Some("iPhone"));
    }

    #[test]
    fn broker_prompts_are_marked_once_only_on_the_wire() {
        let req = |once_only| StreamPayload::PermissionRequest {
            run_id: "r".into(),
            request_id: "q".into(),
            tool: "gh".into(),
            input: serde_json::Value::Null,
            cwd: None,
            once_only,
        };
        assert_eq!(serde_json::to_value(req(true)).unwrap()["once_only"], true);
        assert!(serde_json::to_value(req(false)).unwrap().get("once_only").is_none());
    }

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
    fn hs_reject_wire_matches_the_controller() {
        // `src/controller/protocol.ts` `isHsReject` reads exactly this.
        let j = serde_json::to_value(HandshakePayload::HsReject).unwrap();
        assert_eq!(j, serde_json::json!({ "kind": "hs_reject" }));
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
