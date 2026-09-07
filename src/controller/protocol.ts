/**
 * Devdy Remote Control — Controller wire protocol (SRS §5).
 *
 * Byte-for-byte mirror of the relay's `relay/src/protocol.rs` and the Host's
 * `src-tauri/src/remote/protocol.rs`. The Controller is a relay *client*: it
 * sends `join` + a `control` handshake answer + `cmd` data frames, and receives
 * `room_ready`, `peer_left`, `error`, `control` (handshake offer) and `stream`
 * data frames.
 *
 * This module is PURE and Tauri-free (it must run in a plain browser). It only
 * knows the wire shapes; the crypto lives in `@/remote/e2e` and the transport
 * in `./connection`.
 */

// ---- Frame types (snake_case wire strings, identical to the Rust side) ----

/**
 * The envelope `t` discriminator (SRS §5.1/§5.3). Only `join`, `handshake_done`,
 * `control`, `cmd`, `room_ready`, `peer_left`, `error` and `stream` are ever
 * seen by the Controller, but the full set is declared to round-trip cleanly.
 */
export type FrameType =
  | 'register_host'
  | 'join'
  | 'room_ready'
  | 'peer_left'
  | 'revoke'
  | 'error'
  | 'create_pair'
  | 'handshake_done'
  | 'resume'
  | 'keepalive'
  | 'stream'
  | 'permission_request'
  | 'cmd'
  | 'control'

/** The outermost relay envelope (SRS §5.1). `cipher` is opaque to the relay. */
export interface Envelope {
  t: FrameType
  /** Routing key. Empty for the pre-room `join` frame. */
  room_id?: string
  /** Monotonic sequence for loss/duplication detection (advisory). */
  seq?: number
  /** Opaque E2E payload (base64). Sealed for data frames; base64(JSON) for the
   * `control` handshake frames (public keys are not secret). */
  cipher?: string
  // ---- control-frame-only fields ----
  /** `join`: the one-time pairing code. */
  pair_code?: string
  /** `room_ready` (relay → us): rotating resume token. `resume` (us → relay):
   * the token being redeemed to re-attach after a drop. Not an E2E secret. */
  resume_token?: string
  /** `error`: machine-readable code. */
  code?: string
  /** `error`: human-readable message (never carries secrets). */
  message?: string
}

/** Build a `join { pair_code }` control frame (FR-003). */
export function joinFrame(pairCode: string): Envelope {
  return { t: 'join', pair_code: pairCode }
}

/**
 * Build a `resume { resume_token }` control frame. Used to re-attach to the same
 * room after a transport drop within the relay's reconnect window (e.g. the
 * phone screen turned off then on), instead of the one-time `join` pair code.
 */
export function resumeFrame(resumeToken: string): Envelope {
  return { t: 'resume', resume_token: resumeToken }
}

/**
 * Build a `control` data frame carrying a base64(JSON) handshake payload.
 * Public keys are not secret, so the handshake rides plaintext-base64 (spec §3
 * / Host `agent.rs`). The relay still cannot forge the session key without the
 * out-of-band `psk`.
 */
export function controlFrame(roomId: string, base64Json: string, seq: number): Envelope {
  return { t: 'control', room_id: roomId, cipher: base64Json, seq }
}

/** Build a sealed `cmd` data frame routed by `room_id` (Controller → Host). */
export function cmdFrame(roomId: string, sealedCipher: string, seq: number): Envelope {
  return { t: 'cmd', room_id: roomId, cipher: sealedCipher, seq }
}

/**
 * Build a `keepalive { room_id }` heartbeat. A browser cannot originate WS ping
 * frames, so once a run stops streaming the socket would be reaped by idle
 * proxies; this app-level beat keeps it warm and refreshes the relay room's idle
 * clock. Carries no cipher — the relay just touches the room and echoes it back.
 */
export function keepaliveFrame(roomId: string): Envelope {
  return { t: 'keepalive', room_id: roomId }
}

/**
 * Build the Controller's `handshake_done` frame. The relay marks the room
 * ACTIVE only once BOTH peers have signalled handshake_done (see relay
 * `mark_handshake_done`); the Host sends its own, so the Controller must send
 * this after answering or the room is swept at the pair TTL.
 */
export function handshakeDoneFrame(roomId: string): Envelope {
  return { t: 'handshake_done', room_id: roomId }
}

// ---- Handshake payloads (spec §3, base64(JSON) in the `control` cipher) ----

/** Host → Controller: the Host's ephemeral public key + its fingerprint. */
export interface HsOffer {
  kind: 'hs_offer'
  /** Host ephemeral X25519 public key, lowercase hex (32B → 64 chars). */
  host_pub: string
  /** BLAKE2b fingerprint of `host_pub`, lowercase hex. Checked against the QR. */
  host_fingerprint: string
}

/** Controller → Host: the Controller's ephemeral public key. */
export interface HsAnswer {
  kind: 'hs_answer'
  /** Controller ephemeral X25519 public key, lowercase hex. */
  controller_pub: string
}

export type HandshakePayload = HsOffer | HsAnswer

/**
 * Parse a decoded handshake JSON into a typed {@link HsOffer}, or `null` if it
 * is not a well-formed offer. We only ever *receive* offers; we *send* answers.
 */
export function parseHsOffer(json: unknown): HsOffer | null {
  if (!json || typeof json !== 'object') return null
  const o = json as Record<string, unknown>
  if (o.kind !== 'hs_offer') return null
  if (typeof o.host_pub !== 'string' || typeof o.host_fingerprint !== 'string') return null
  if (!isHex(o.host_pub) || o.host_pub.length !== 64) return null
  if (!isHex(o.host_fingerprint) || o.host_fingerprint.length !== 64) return null
  return { kind: 'hs_offer', host_pub: o.host_pub, host_fingerprint: o.host_fingerprint }
}

/** Build the `hs_answer` JSON string to base64-encode into a control frame. */
export function hsAnswerJson(controllerPubHex: string): string {
  const answer: HsAnswer = { kind: 'hs_answer', controller_pub: controllerPubHex }
  return JSON.stringify(answer)
}

// ---- Decrypted business payloads (SRS §5.2, `#[serde(tag = "kind")]`) ----

/** A raw stream-json SDK event (`run:event`). */
export interface StreamKind {
  kind: 'stream'
  run_id: string
  event: unknown
}
/** A console/output line (`run:output`). */
export interface OutputKind {
  kind: 'output'
  run_id: string
  line: string
  is_stderr?: boolean
}
/** A tool permission request forwarded to the Controller (FR-006/BR-007). */
export interface PermissionRequestKind {
  kind: 'permission_request'
  run_id: string
  request_id: string
  tool: string
  input: unknown
  cwd?: string | null
}
/** The run finished (`run:done`). */
export interface DoneKind {
  kind: 'done'
  run_id: string
  status: string
}
/** A batch of replayed history lines (FR-005); `done` marks the last batch. */
export interface HistoryKind {
  kind: 'history'
  run_id: string
  lines: string[]
  done: boolean
}
/** An advisory the Host sends (e.g. "already handled"). */
export interface NoticeKind {
  kind: 'notice'
  run_id?: string | null
  text: string
}
/** Host acknowledgement for a controller command that must not be echoed
 * optimistically before the Host has actually accepted it. */
export interface CommandAckKind {
  kind: 'command_ack'
  command_id: string
  action: string
  accepted: boolean
  reason?: string | null
}
/** Correlated permission resolution. A stale acknowledgement must never clear a
 * newer permission request. */
export interface PermissionResolvedKind {
  kind: 'permission_resolved'
  run_id: string
  request_id: string
  accepted: boolean
  reason?: string | null
}
/**
 * The Host granted a session token after a successful OTP (or reconnect). The
 * Controller persists it (see `sessionTokenStore`) to re-attach without an OTP
 * within the sliding idle window. Receiving this is the sole signal that auth
 * SUCCEEDED.
 */
export interface SessionTokenKind {
  kind: 'session_token'
  /** Opaque reconnect secret (the `session_secret`, hex). Never logged. */
  token: string
  run_id: string
  rendezvous_code: string
  /** Absolute idle expiry as Unix seconds (advisory; the store slides its own). */
  idle_expires_at: number
}
/**
 * The outcome of an auth attempt. `ok:false` means the OTP was wrong / expired /
 * attempts exhausted; the UI re-prompts. `ok:true` is redundant with
 * {@link SessionTokenKind} but kept for completeness.
 */
export interface AuthResultKind {
  kind: 'auth_result'
  ok: boolean
  run_id: string
  reason?: string | null
}
/** One slash command the Host advertises for the bound run's engine. */
export interface SlashCommand {
  name: string
  description?: string | null
}
/** The slash commands available for the bound run (composer palette). */
export interface SlashCommandListKind {
  kind: 'slash_command_list'
  commands: SlashCommand[]
}
/** One engine + its selectable models (composer footer selectors). */
export interface EngineModelOption {
  id: string
  label: string
  models: { id: string; label: string }[]
  default_model?: string | null
}
/** The engine/model options the Host offers (composer footer selectors). */
export interface EngineModelOptionsKind {
  kind: 'engine_model_options'
  engines: EngineModelOption[]
}
/** The project file list for the bound run (@mention autocomplete). */
export interface ProjectFileListKind {
  kind: 'project_file_list'
  run_id: string
  files: string[]
}
/** The bound run's live engine/model/permission-mode selection (mirror of the
 * desktop composer). Pushed on pairing and on every change, either direction. */
export interface RunMetaKind {
  kind: 'run_meta'
  run_id: string
  engine?: string | null
  model?: string | null
  permission_mode?: string | null
}

/** One provider's subscription plan-usage verdict (mirror of the desktop
 * `stats::BudgetStatus`). `source === 'plan'` means a real /usage window was
 * captured; `'disabled'` means no plan data (render a quiet, empty row). */
export interface PlanBudget {
  source: string
  period: string
  percent: number
  is_warning: boolean
  is_over: boolean
  reset?: string | null
  captured_at?: string | null
  is_stale: boolean
  status?: string | null
  rolled_over?: boolean
}
/** Subscription plan-usage badges (Claude + Codex), mirror of the desktop
 * `BudgetBadge`. Pushed on pairing and after each turn finishes. */
export interface PlanUsageKind {
  kind: 'plan_usage'
  claude?: PlanBudget | null
  codex?: PlanBudget | null
  /** Label of the Claude account the bound run is using (which account the
   * utilization belongs to). Absent for a global/legacy run. */
  claude_account?: string | null
}

/** Non-secret run metadata (mirror of Host `protocol.rs::RunInfo`). */
export interface RunInfo {
  id: string
  project_id: string
  project_name: string
  title?: string | null
  /** Run kind (`session` / `issue` / `pr`). */
  type: string
  ref_number?: number | null
  status: string
  engine: string
  created_at: string
  updated_at?: string | null
}

/** A snapshot of every run the Controller may browse/control (FR-004+). */
export interface RunListKind {
  kind: 'run_list'
  runs: RunInfo[]
}

/** One project the Controller can start a run in (mirror of Host `ProjectInfo`). */
export interface ProjectInfo {
  id: string
  name: string
  path: string
}

/** The projects the Controller can create a new run in (Phase 2). */
export interface ProjectListKind {
  kind: 'project_list'
  projects: ProjectInfo[]
}

/** Everything the Host can send the Controller inside a sealed `stream` frame. */
export type StreamPayload =
  | StreamKind
  | OutputKind
  | PermissionRequestKind
  | DoneKind
  | HistoryKind
  | NoticeKind
  | CommandAckKind
  | PermissionResolvedKind
  | SessionTokenKind
  | AuthResultKind
  | SlashCommandListKind
  | EngineModelOptionsKind
  | ProjectFileListKind
  | RunMetaKind
  | PlanUsageKind
  | RunListKind
  | ProjectListKind

/**
 * Narrow a decoded stream payload. Returns `null` for anything with an
 * unrecognized `kind`, so callers never act on malformed frames.
 */
export function parseStreamPayload(json: unknown): StreamPayload | null {
  if (!json || typeof json !== 'object') return null
  const o = json as Record<string, unknown>
  switch (o.kind) {
    case 'stream':
      return typeof o.run_id === 'string'
        ? { kind: 'stream', run_id: o.run_id, event: o.event }
        : null
    case 'output':
      return typeof o.run_id === 'string' && typeof o.line === 'string'
        ? { kind: 'output', run_id: o.run_id, line: o.line, is_stderr: o.is_stderr === true }
        : null
    case 'permission_request':
      return typeof o.run_id === 'string' &&
        typeof o.request_id === 'string' &&
        typeof o.tool === 'string'
        ? {
            kind: 'permission_request',
            run_id: o.run_id,
            request_id: o.request_id,
            tool: o.tool,
            input: o.input,
            cwd: typeof o.cwd === 'string' ? o.cwd : null,
          }
        : null
    case 'done':
      return typeof o.run_id === 'string' && typeof o.status === 'string'
        ? { kind: 'done', run_id: o.run_id, status: o.status }
        : null
    case 'history':
      return typeof o.run_id === 'string' && Array.isArray(o.lines)
        ? {
            kind: 'history',
            run_id: o.run_id,
            lines: (o.lines as unknown[]).map(String),
            done: o.done === true,
          }
        : null
    case 'notice':
      return typeof o.text === 'string'
        ? { kind: 'notice', run_id: typeof o.run_id === 'string' ? o.run_id : null, text: o.text }
        : null
    case 'command_ack':
      return typeof o.command_id === 'string' &&
        typeof o.action === 'string' &&
        typeof o.accepted === 'boolean'
        ? {
            kind: 'command_ack',
            command_id: o.command_id,
            action: o.action,
            accepted: o.accepted,
            reason: typeof o.reason === 'string' ? o.reason : null,
          }
        : null
    case 'permission_resolved':
      return typeof o.run_id === 'string' &&
        typeof o.request_id === 'string' &&
        typeof o.accepted === 'boolean'
        ? {
            kind: 'permission_resolved',
            run_id: o.run_id,
            request_id: o.request_id,
            accepted: o.accepted,
            reason: typeof o.reason === 'string' ? o.reason : null,
          }
        : null
    case 'session_token':
      return typeof o.token === 'string' &&
        typeof o.run_id === 'string' &&
        typeof o.rendezvous_code === 'string'
        ? {
            kind: 'session_token',
            token: o.token,
            run_id: o.run_id,
            rendezvous_code: o.rendezvous_code,
            idle_expires_at: typeof o.idle_expires_at === 'number' ? o.idle_expires_at : 0,
          }
        : null
    case 'auth_result':
      return typeof o.ok === 'boolean' && typeof o.run_id === 'string'
        ? {
            kind: 'auth_result',
            ok: o.ok,
            run_id: o.run_id,
            reason: typeof o.reason === 'string' ? o.reason : null,
          }
        : null
    case 'slash_command_list':
      return Array.isArray(o.commands)
        ? { kind: 'slash_command_list', commands: (o.commands as unknown[]).filter(isSlashCommand) }
        : null
    case 'engine_model_options':
      return Array.isArray(o.engines)
        ? {
            kind: 'engine_model_options',
            engines: (o.engines as unknown[]).filter(isEngineModelOption),
          }
        : null
    case 'project_file_list':
      return typeof o.run_id === 'string' && Array.isArray(o.files)
        ? {
            kind: 'project_file_list',
            run_id: o.run_id,
            files: (o.files as unknown[]).map(String),
          }
        : null
    case 'run_meta':
      return typeof o.run_id === 'string'
        ? {
            kind: 'run_meta',
            run_id: o.run_id,
            engine: typeof o.engine === 'string' ? o.engine : null,
            model: typeof o.model === 'string' ? o.model : null,
            permission_mode: typeof o.permission_mode === 'string' ? o.permission_mode : null,
          }
        : null
    case 'run_list':
      return Array.isArray(o.runs)
        ? { kind: 'run_list', runs: (o.runs as unknown[]).filter(isRunInfo) }
        : null
    case 'project_list':
      return Array.isArray(o.projects)
        ? { kind: 'project_list', projects: (o.projects as unknown[]).filter(isProjectInfo) }
        : null
    case 'plan_usage': {
      // Subscription usage badges (Claude + Codex). Each field is a serialized
      // `stats::BudgetStatus` or null; pass through only well-formed objects so
      // a malformed provider degrades to "no data" instead of dropping the frame.
      const asBudget = (v: unknown): PlanBudget | null =>
        v && typeof v === 'object' && typeof (v as Record<string, unknown>).source === 'string'
          ? (v as PlanBudget)
          : null
      return { kind: 'plan_usage', claude: asBudget(o.claude), codex: asBudget(o.codex) }
    }
    default:
      return null
  }
}

/** Narrow one slash-command row. */
function isSlashCommand(v: unknown): v is SlashCommand {
  if (!v || typeof v !== 'object') return false
  const o = v as Record<string, unknown>
  return typeof o.name === 'string'
}

/** Narrow one engine/model-options row (drops rows without a valid models list). */
function isEngineModelOption(v: unknown): v is EngineModelOption {
  if (!v || typeof v !== 'object') return false
  const o = v as Record<string, unknown>
  if (typeof o.id !== 'string' || typeof o.label !== 'string') return false
  if (!Array.isArray(o.models)) return false
  return true
}

/** Narrow one project row; drops anything missing required fields. */
function isProjectInfo(v: unknown): v is ProjectInfo {
  if (!v || typeof v !== 'object') return false
  const o = v as Record<string, unknown>
  return typeof o.id === 'string' && typeof o.name === 'string' && typeof o.path === 'string'
}

/** Narrow one run-list row; drops anything missing the required fields. */
function isRunInfo(v: unknown): v is RunInfo {
  if (!v || typeof v !== 'object') return false
  const o = v as Record<string, unknown>
  return (
    typeof o.id === 'string' &&
    typeof o.project_id === 'string' &&
    typeof o.project_name === 'string' &&
    typeof o.type === 'string' &&
    typeof o.status === 'string' &&
    typeof o.engine === 'string' &&
    typeof o.created_at === 'string'
  )
}

// ---- Commands the Controller may send (SRS §5.2 / FR-009 allow-list) ----

/**
 * The only permission decisions a Controller may ever send (BR-016/SEC-011/
 * AC-17). "allow_always"/"deny_always" are NOT representable here by design —
 * they can only be set by the Owner at the Host.
 */
export type RemoteDecision = 'allow_once' | 'deny_once'

/**
 * AskUserQuestion answers: `{ question text: selected answer }`. Multiple
 * selections for one question are joined with ", " (mirrors the desktop
 * `PermissionPrompt.vue`). Only ever sent with an `allow_once` decision.
 */
export type QuestionAnswers = Record<string, string>

/** How the first sealed frame after a (re)join was keyed — lets the Host pick
 * the right secret2 for the OTP gate (`otp` on pairing, `session` on reconnect). */
export type AuthMode = 'otp' | 'session'

/** A file/image attachment embedded (base64) in a turn (no Tauri fs). */
export interface CmdAttachment {
  name: string
  mime: string
  data_base64: string
}

/** A command payload (`kind: "cmd"`) sealed and sent to the Host. */
export interface CmdPayload {
  kind: 'cmd'
  action:
    | 'respond_permission'
    | 'start_run'
    | 'cancel_run'
    | 'send_chat_message'
    | 'set_run_meta'
    | 'request_history'
    | 'list_runs'
    | 'list_projects'
    | 'list_slash_commands'
    | 'list_engine_models'
    | 'list_project_files'
    | 'list_plan_usage'
  /** Per-send correlation id used for Host delivery acknowledgement. */
  command_id?: string
  run_id?: string
  request_id?: string
  decision?: RemoteDecision
  /** AskUserQuestion selections; paired with `allow_once` only (BR-016/AC-17). */
  answers?: QuestionAnswers
  text?: string
  project_id?: string
  /** Composer selectors (UI only — the Host forces `default` permission for exec). */
  engine?: string
  model?: string
  permission_mode?: string
  slash_command?: string
  /** `@`-mentioned file paths (backticked into the prompt by the Host). */
  mentions?: string[]
  /** Inline image/file attachments (base64). */
  attachments?: CmdAttachment[]
  /** Set on the first frame after a (re)join so the Host keys the OTP gate. */
  auth_mode?: AuthMode
}

/**
 * `respond_permission` — only `allow_once`/`deny_once` (BR-016). For
 * `AskUserQuestion` pass `answers` alongside `allow_once`; the answers ride the
 * same once-only decision and never unlock allow_always/deny_always.
 */
export function respondPermissionCmd(
  runId: string,
  requestId: string,
  decision: RemoteDecision,
  answers?: QuestionAnswers,
): CmdPayload {
  const cmd: CmdPayload = {
    kind: 'cmd',
    action: 'respond_permission',
    run_id: runId,
    request_id: requestId,
    decision,
  }
  if (answers) cmd.answers = answers
  return cmd
}

/** `cancel_run`. */
export function cancelRunCmd(runId: string): CmdPayload {
  return { kind: 'cmd', action: 'cancel_run', run_id: runId }
}

/** `request_history` (FR-005). */
export function requestHistoryCmd(runId: string): CmdPayload {
  return { kind: 'cmd', action: 'request_history', run_id: runId }
}

/** `list_slash_commands` — refresh the composer's slash palette. */
export function listSlashCommandsCmd(): CmdPayload {
  return { kind: 'cmd', action: 'list_slash_commands' }
}

/** `list_plan_usage` — refresh the subscription usage badges (Claude + Codex). */
export function listPlanUsageCmd(): CmdPayload {
  return { kind: 'cmd', action: 'list_plan_usage' }
}

/** `list_engine_models` — refresh the composer's engine/model selectors. */
export function listEngineModelsCmd(): CmdPayload {
  return { kind: 'cmd', action: 'list_engine_models' }
}

/** `list_project_files` — refresh the `@`-mention file list for a run. */
export function listProjectFilesCmd(runId: string): CmdPayload {
  return { kind: 'cmd', action: 'list_project_files', run_id: runId }
}

/**
 * Push the controller's engine/model/permission-mode selection to the Host so
 * the desktop composer + the shared store mirror it (realtime, both ways). Blank
 * fields are omitted so a partial change never clobbers the others.
 */
export function setRunMetaCmd(
  runId: string,
  sel: { engine?: string; model?: string; permissionMode?: string },
): CmdPayload {
  const cmd: CmdPayload = { kind: 'cmd', action: 'set_run_meta', run_id: runId }
  if (sel.engine) cmd.engine = sel.engine
  if (sel.model) cmd.model = sel.model
  if (sel.permissionMode) cmd.permission_mode = sel.permissionMode
  return cmd
}

/** One turn's worth of composer input. */
export interface TurnInput {
  text?: string
  engine?: string
  model?: string
  permissionMode?: string
  slashCommand?: string
  mentions?: string[]
  attachments?: CmdAttachment[]
}

/**
 * Build the command for sending one composer turn to the bound run. A running
 * run takes a follow-up (`send_chat_message`); an idle/finished run is
 * (re)started (`start_run`) — mirrors the desktop running-vs-start decision.
 * Never carries an honored permission mode: the Host forces `default` for exec
 * (CON-05/BR-011); `permission_mode` rides along for the Host's UI/audit only.
 */
export function sendTurnCmd(runId: string, input: TurnInput, running: boolean): CmdPayload {
  const cmd: CmdPayload = {
    kind: 'cmd',
    action: running ? 'send_chat_message' : 'start_run',
    run_id: runId,
  }
  if (input.text) cmd.text = input.text
  if (input.engine) cmd.engine = input.engine
  if (input.model) cmd.model = input.model
  if (input.permissionMode) cmd.permission_mode = input.permissionMode
  if (input.slashCommand) cmd.slash_command = input.slashCommand
  if (input.mentions && input.mentions.length) cmd.mentions = input.mentions
  if (input.attachments && input.attachments.length) cmd.attachments = input.attachments
  return cmd
}

// ---- Session link payload (per-run link: QR / copy / manual) ----

/**
 * The per-session link the Host mints (Phase 3). It carries the high-entropy
 * `link_secret` (the QR/copy channel) but NOT the OTP — the OTP is shown on the
 * Host screen (a second out-of-band channel) and typed by the user. The
 * effective E2E key is `KDF(link_secret, otp)`, so a wrong OTP just yields a
 * different key with no comparison oracle.
 */
export interface SessionLinkPayload {
  /** WSS URL of the relay the Controller dials. */
  relay_url: string
  /** Host fingerprint (hex, 64 chars) for MITM detection (SEC-006). */
  host_fingerprint: string
  /** Per-session relay join key (used where the old code used `pair_code`). */
  rendezvous_code: string
  /** The single run this link controls (everything is scoped to it). */
  run_id: string
  /** High-entropy link secret (hex). Out-of-band; the base of the E2E KDF. */
  link_secret: string
}

/** A parse failure for a session link, with a user-facing reason. */
export class SessionLinkParseError extends Error {
  constructor(message: string) {
    super(message)
    this.name = 'SessionLinkParseError'
  }
}

/**
 * Parse a {@link SessionLinkPayload} from a raw string. Accepts:
 *   1. A controller URL whose HASH carries the five URL-encoded params
 *      (`…/controller.html#relay_url=…&host_fingerprint=…&rendezvous_code=…&run_id=…&link_secret=…`).
 *   2. A bare `k=v&k=v` fragment (the hash pasted without the URL).
 *   3. A bare JSON object with the same fields (manual paste convenience).
 *
 * @throws {SessionLinkParseError} when required fields are missing/invalid.
 */
export function parseSessionLink(raw: string): SessionLinkPayload {
  const text = raw.trim()
  if (!text) throw new SessionLinkParseError('Empty link input')

  // 1. Bare JSON.
  const asJson = tryParseJson(text)
  if (asJson) return fromRecord(asJson)

  // 2. URL: read the fragment (params or JSON) first, then the query string.
  const fromUrl = tryParseUrl(text)
  if (fromUrl) return fromUrl

  // 3. Bare "k=v&k=v" params (fragment pasted without the URL).
  const params = tryParseParams(text)
  if (params) return fromRecord(params)

  throw new SessionLinkParseError('Unrecognized link (expected the controller link or its params)')
}

function tryParseJson(text: string): Record<string, unknown> | null {
  if (!(text.startsWith('{') && text.endsWith('}'))) return null
  try {
    const v: unknown = JSON.parse(text)
    return v && typeof v === 'object' ? (v as Record<string, unknown>) : null
  } catch {
    return null
  }
}

function tryParseUrl(text: string): SessionLinkPayload | null {
  let url: URL
  try {
    url = new URL(text)
  } catch {
    return null
  }
  // Fragment carries the params (or, tolerantly, JSON).
  const frag = url.hash.replace(/^#/, '')
  if (frag) {
    const fragJson = tryParseJson(decodeURIComponent(frag))
    if (fragJson) return fromRecord(fragJson)
    const fragParams = tryParseParams(frag)
    if (fragParams && fragParams.rendezvous_code) return fromRecord(fragParams)
  }
  // Fall back to query params.
  const q = url.searchParams
  if (q.get('rendezvous_code')) {
    return fromRecord({
      relay_url: q.get('relay_url') ?? '',
      host_fingerprint: q.get('host_fingerprint') ?? '',
      rendezvous_code: q.get('rendezvous_code') ?? '',
      run_id: q.get('run_id') ?? '',
      link_secret: q.get('link_secret') ?? '',
    })
  }
  return null
}

function tryParseParams(text: string): Record<string, unknown> | null {
  if (!text.includes('=')) return null
  const params = new URLSearchParams(text)
  const out: Record<string, unknown> = {}
  for (const [k, v] of params) out[k] = v
  return Object.keys(out).length ? out : null
}

function fromRecord(o: Record<string, unknown>): SessionLinkPayload {
  const relay_url = strField(o, 'relay_url')
  const host_fingerprint = strField(o, 'host_fingerprint')
  const rendezvous_code = strField(o, 'rendezvous_code')
  const run_id = strField(o, 'run_id')
  const link_secret = strField(o, 'link_secret')
  if (!relay_url) throw new SessionLinkParseError('Missing relay_url')
  if (!/^wss:\/\//i.test(relay_url)) {
    throw new SessionLinkParseError('relay_url must be a wss:// URL (encrypted transport required)')
  }
  if (!isHex(host_fingerprint) || host_fingerprint.length !== 64) {
    throw new SessionLinkParseError('Missing or invalid host_fingerprint')
  }
  if (!rendezvous_code) throw new SessionLinkParseError('Missing rendezvous_code')
  if (!run_id) throw new SessionLinkParseError('Missing run_id')
  if (!isHex(link_secret) || link_secret.length < 32) {
    throw new SessionLinkParseError('Missing or invalid link_secret')
  }
  return { relay_url, host_fingerprint, rendezvous_code, run_id, link_secret }
}

function strField(o: Record<string, unknown>, key: string): string {
  const v = o[key]
  return typeof v === 'string' ? v.trim() : ''
}

// ---- Hex helpers (lowercase, matching Rust `pairing_hex`) ----

/** True if `s` is a non-empty, even-length string of hex digits. */
export function isHex(s: unknown): s is string {
  return typeof s === 'string' && s.length > 0 && s.length % 2 === 0 && /^[0-9a-fA-F]+$/.test(s)
}

/** Lowercase-hex encode bytes (matches Rust `pairing_hex`). */
export function toHex(bytes: Uint8Array): string {
  let s = ''
  for (const b of bytes) s += b.toString(16).padStart(2, '0')
  return s
}

/**
 * Decode a hex string to bytes (matches Rust `pairing_unhex`).
 * @throws {SessionLinkParseError} on odd length or an invalid character.
 */
export function fromHex(s: string): Uint8Array {
  const len = s.length
  if (!isHex(s)) throw new SessionLinkParseError(`invalid hex: ${len} chars`)
  const out = new Uint8Array(s.length / 2)
  for (let i = 0; i < out.length; i += 1) {
    out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16)
  }
  return out
}

/** UTF-8/base64 helpers for the handshake control cipher (base64 of JSON). */
export function utf8ToBase64(s: string): string {
  const bytes = new TextEncoder().encode(s)
  let bin = ''
  for (const b of bytes) bin += String.fromCharCode(b)
  return btoa(bin)
}

/** Decode a base64 string to a UTF-8 string (handshake JSON). */
export function base64ToUtf8(b64: string): string {
  const bin = atob(b64)
  const bytes = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i += 1) bytes[i] = bin.charCodeAt(i)
  return new TextDecoder().decode(bytes)
}
