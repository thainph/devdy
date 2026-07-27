/**
 * Controller transport + session engine (FR-003/004/005/007/008/012).
 *
 * Owns the single WSS connection to the relay for ONE bound run, drives the
 * handshake, seals / opens business payloads with the E2E {@link Session}, and
 * exposes a small reactive surface the Vue UI renders.
 *
 * Auth model (Phase 4): the link carries a high-entropy `link_secret`; the E2E
 * key is `KDF(link_secret, secret2)` where `secret2` is the OTP (pairing) or the
 * stored `session_secret` (reconnect). After the handshake the Controller sends
 * a first sealed `cmd` (with `auth_mode`) to trigger the Host's OTP gate. Auth
 * SUCCEEDS when the Host replies with a `session_token`; it FAILS on
 * `auth_result{ok:false}`, or on a peer_left / room-close / decode-error shortly
 * after an OTP attempt (the Host closes the room after 5 bad OTPs).
 *
 * Tauri-free: this runs in a plain browser (phone / other PC). No secrets, no
 * plaintext, no `link_secret`/OTP are ever logged.
 */
import { ref, type Ref } from 'vue'
import { ready as e2eReady, generateKeypair, type Keypair, type Session } from '@/remote/e2e'
import {
  base64ToUtf8,
  cmdFrame,
  controlFrame,
  handshakeDoneFrame,
  hsAnswerJson,
  joinFrame,
  keepaliveFrame,
  requestHistoryCmd,
  listSlashCommandsCmd,
  listEngineModelsCmd,
  listProjectFilesCmd,
  listPlanUsageCmd,
  resumeFrame,
  parseStreamPayload,
  toHex,
  utf8ToBase64,
  type AuthMode,
  type CmdPayload,
  type Envelope,
  type SessionLinkPayload,
  type StreamPayload,
} from './protocol'
import { verifyOffer, deriveSessionKey, sessionSecretBytes, HandshakeError } from './handshake'
import * as tokenStore from './sessionTokenStore'

/** RECONNECT_WINDOW — auto-reconnect budget after a drop for a fresh (OTP) link. */
export const RECONNECT_WINDOW_MS = 60_000
/** Exponential backoff base / cap between reconnect attempts (BR-010). */
const BACKOFF_BASE_MS = 500
const BACKOFF_MAX_MS = 5_000
/** After an OTP attempt, a room-close/peer-left within this window is read as a
 * rejected OTP (the Host closes the room instead of replying) rather than a
 * transport blip. */
const OTP_VERDICT_WINDOW_MS = 8_000
/** Gate-frame resend budget + spacing while the relay room races to ACTIVE
 * (the Host's handshake_done may reach the relay just after our first cmd). */
const GATE_MAX_RETRIES = 8
const GATE_RETRY_MS = 250
/** Heartbeat cadence. A browser can't send WS ping frames, so once a run stops
 * streaming this app-level beat keeps the socket warm (well under the ~60 s idle
 * cutoff of typical proxies) and refreshes the relay room's idle clock. */
const KEEPALIVE_MS = 15_000
/** Zombie-socket watchdog. The relay echoes every keepalive, so a live socket
 * receives an inbound frame at least every KEEPALIVE_MS. If nothing arrives for
 * this long while the socket still reports OPEN, it has gone half-open (mobile
 * network drop with no FIN/onclose) — force a fresh socket. Set to ~2 missed
 * beats + slack so a single dropped echo doesn't cause needless churn. This is
 * the only recovery path while the page is FOREGROUNDED (wake events don't fire),
 * which is exactly when a run is streaming and the socket silently dies. */
const LIVENESS_TIMEOUT_MS = KEEPALIVE_MS * 2 + 5_000
/** How long to wait for ANY host-originated frame after we send a command that
 * must elicit one (the auth gate `request_history`, or a `start_run` turn). The
 * relay's keepalive echoes come from the RELAY, not the host, so a silent host
 * (desynced room / dropped command after a reconnect storm) leaves the socket
 * "connected" yet dead. If nothing host-originated arrives in this window, force
 * a fresh reconnect+re-gate instead of hanging forever on "thinking". Generous
 * enough that a normal turn — which streams reasoning/text within a second or
 * two — clears it long before it fires. */
const HOST_RESPONSE_TIMEOUT_MS = 22_000
/** Returning to the foreground after being hidden at least this long (screen
 * lock, app switch, device sleep) forces a fresh socket: the old one has almost
 * certainly been reaped or gone half-open ("zombie") — yet may still report
 * OPEN — so a light "is it still open?" check would wrongly trust it. */
const WAKE_FORCE_RECONNECT_MS = 15_000
/** Wake events (visibilitychange, online, focus) often fire together within a
 * few ms of each other on unlock. A forceReconnect that just (re)opened a socket
 * must not be torn down by the next event in that burst — so a socket opened
 * within this window is trusted and left alone. */
const FORCE_RECONNECT_COOLDOWN_MS = 4_000

/** High-level connection status surfaced to the UI (SRS §6.3). */
export type ConnStatus =
  | 'idle'
  | 'joining'
  | 'handshaking'
  | 'connected'
  | 'reconnecting'
  | 'disconnected'
  | 'error'

/** The auth phase, distinct from transport status. */
export type AuthState =
  | 'pending' // handshake done, first frame sent, waiting for the Host verdict
  | 'authenticated' // session_token received
  | 'failed' // OTP rejected / attempts exhausted — re-prompt OTP

/** The secret2 source for the E2E KDF: a user OTP (pairing) or a stored token. */
export type SecretSource =
  | { mode: 'otp'; otp: string }
  | { mode: 'session'; token: string }

/** A callback invoked for each decrypted business payload from the Host. */
export type StreamHandler = (payload: StreamPayload) => void
export type CommandAckHandler = (accepted: boolean, reason?: string | null) => void

/** The reactive state the UI binds to. */
export interface ControllerState {
  status: Ref<ConnStatus>
  /** The room id once the relay assigns it (FR-003). */
  roomId: Ref<string | null>
  /** Verified Host fingerprint (hex) after handshake — shown as a trust badge. */
  hostFingerprint: Ref<string | null>
  /** The last user-facing error message (never a secret). */
  lastError: Ref<string | null>
  /** True once we have received at least one decrypted payload from the Host. */
  live: Ref<boolean>
  /** Count of frames that failed to open/parse (wrong key / tamper / bug). */
  decodeErrors: Ref<number>
  /** The auth phase (see {@link AuthState}); null until the first attempt. */
  auth: Ref<AuthState | null>
}

/**
 * A single-run controller connection. Construct with the parsed session link + a
 * secret source (OTP or stored token) + a stream handler; call {@link connect}
 * to start. Safe to {@link close} at any time.
 */
export class ControllerConnection {
  readonly state: ControllerState

  private readonly link: SessionLinkPayload
  /** The secret2 source; null for a fresh OTP link until the user submits it. */
  private secret: SecretSource | null
  private readonly onStream: StreamHandler
  private authMode: AuthMode

  private ws: WebSocket | null = null
  private session: Session | null = null
  private seq = 0
  /** Rotating resume token issued by the relay in `room_ready`. */
  private resumeToken: string | null = null
  /** Set once the first drop starts the reconnect window; null while stable. */
  private reconnectDeadline: number | null = null
  private backoff = BACKOFF_BASE_MS
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null
  private closedByUser = false
  /** ms timestamp of the last OTP attempt (first sealed frame after handshake),
   * or null once authenticated / for a session-mode connection. Used to read a
   * silent room-close as a rejected OTP. */
  private otpAttemptAt: number | null = null
  /** The Host's ephemeral public key (verified against the pinned fingerprint)
   * once the offer arrives — stored so the session can be derived later, when the
   * OTP is known. Null before the offer. */
  private hostPub: Uint8Array | null = null
  /** Our ephemeral keypair, generated when we answer the handshake offer and
   * reused for the deferred session-key derivation. */
  private myKeypair: Keypair | null = null
  /** Resend attempts for the first (gate) frame while the relay room races to
   * ACTIVE right after the handshake (host_handshake_done may lag our cmd). */
  private gateRetries = 0
  /** ms timestamp of the last socket open (onOpen). Lets forceReconnect coalesce
   * a burst of wake events: a freshly-opened socket is trusted, not torn down. */
  private lastOpenAt = 0
  /** Fires if no `session_token` arrives within the verdict window after an OTP
   * attempt ⇒ treat as a wrong/expired code and let the user retry. */
  private verdictTimer: ReturnType<typeof setTimeout> | null = null
  /** App-level heartbeat interval; keeps an idle (post-run) socket from being
   * reaped by proxies. Runs for the whole connection lifetime; guards on OPEN. */
  private keepaliveTimer: ReturnType<typeof setInterval> | null = null
  /** True while the page is backgrounded (tab hidden / screen locked), with the
   * timestamp it went hidden — used to decide whether the socket likely died. */
  private hidden = false
  private hiddenAt = 0
  /** ms timestamp of the last inbound frame (any type — including the relay's
   * keepalive echo). Drives the zombie-socket watchdog in the keepalive loop. */
  private lastInboundAt = 0
  /** Set when we send a command that MUST elicit a host-originated frame (auth
   * gate or a `start_run` turn); cleared the moment any openable host frame
   * arrives. If it stays set past HOST_RESPONSE_TIMEOUT_MS the host is silent /
   * desynced → force a reconnect. Null when not awaiting anything. */
  private awaitingHostSince: number | null = null
  /** Commands whose UI state must only commit after a correlated Host ack. */
  private pendingCommandAcks = new Map<
    string,
    { handler: CommandAckHandler; timer: ReturnType<typeof setTimeout> }
  >()

  private readonly onVisibility = (): void => {
    if (typeof document === 'undefined') return
    if (document.visibilityState === 'hidden') {
      this.hidden = true
      this.hiddenAt = Date.now()
      return
    }
    // Back in the foreground. A long hide (screen lock / sleep) almost always
    // leaves a dead-but-OPEN socket; replace it outright. A brief hide probably
    // kept the socket alive, so take the light path.
    const hiddenMs = this.hidden ? Date.now() - this.hiddenAt : 0
    this.hidden = false
    if (hiddenMs >= WAKE_FORCE_RECONNECT_MS) {
      this.forceReconnect()
    } else {
      this.wakeReconnect()
    }
  }
  // A network transition invalidates the existing socket outright; re-attach on a
  // fresh one. Plain window focus just nudges a stalled reconnect.
  private readonly onOnline = (): void => this.forceReconnect()
  private readonly onFocus = (): void => this.wakeReconnect()

  constructor(link: SessionLinkPayload, secret: SecretSource | null, onStream: StreamHandler) {
    this.link = link
    this.secret = secret
    this.onStream = onStream
    this.authMode = secret?.mode === 'session' ? 'session' : 'otp'
    this.state = {
      status: ref<ConnStatus>('idle'),
      roomId: ref<string | null>(null),
      hostFingerprint: ref<string | null>(null),
      lastError: ref<string | null>(null),
      live: ref<boolean>(false),
      decodeErrors: ref<number>(0),
      auth: ref<AuthState | null>(null),
    }
  }

  /** True once the room is ACTIVE and commands may be sent. */
  get isActive(): boolean {
    return (
      this.state.status.value === 'connected' &&
      this.state.auth.value === 'authenticated' &&
      this.session !== null
    )
  }

  /** The run id this connection is bound to. */
  get runId(): string {
    return this.link.run_id
  }

  /** Open the connection and start the join → handshake flow. */
  async connect(): Promise<void> {
    await e2eReady()
    this.closedByUser = false
    this.addWakeListeners()
    this.startKeepalive()
    this.openSocket()
  }

  /** Start the heartbeat loop (idempotent). Each beat is a no-op unless the
   * socket is OPEN and a room is assigned, so it safely spans reconnect gaps. */
  private startKeepalive(): void {
    if (this.keepaliveTimer !== null) return
    this.keepaliveTimer = setInterval(() => {
      if (this.ws?.readyState !== WebSocket.OPEN || !this.state.roomId.value) return
      // Zombie-socket watchdog: the relay echoes every keepalive, so a live
      // socket sees inbound traffic at least this often. Silence past the
      // liveness timeout means the socket went half-open (no onclose fired) —
      // replace it. This is what recovers a foregrounded, mid-run session whose
      // socket silently died (the "stuck, no stream until reload" bug).
      if (this.lastInboundAt > 0 && Date.now() - this.lastInboundAt > LIVENESS_TIMEOUT_MS) {
        this.forceReconnect()
        return
      }
      // Silent-host watchdog: we sent a command that must produce a host frame
      // (auth gate / start_run) but none arrived. The socket may still be
      // "connected" via relay keepalive echoes while the host has torn down /
      // desynced this room (the reconnect-storm split-brain). Re-attach so the
      // gate re-runs and the command isn't lost to a dead session.
      if (
        this.awaitingHostSince !== null &&
        Date.now() - this.awaitingHostSince > HOST_RESPONSE_TIMEOUT_MS
      ) {
        this.awaitingHostSince = null
        this.forceReconnect()
        return
      }
      this.sendEnvelope(keepaliveFrame(this.state.roomId.value))
    }, KEEPALIVE_MS)
  }

  private stopKeepalive(): void {
    if (this.keepaliveTimer !== null) {
      clearInterval(this.keepaliveTimer)
      this.keepaliveTimer = null
    }
  }

  /** Close permanently (no reconnect). Idempotent. */
  close(): void {
    this.closedByUser = true
    this.removeWakeListeners()
    this.stopKeepalive()
    this.clearReconnectTimer()
    this.reconnectDeadline = null
    this.session = null
    this.failPendingCommandAcks('disconnected')
    if (this.ws) {
      const ws = this.ws
      this.ws = null
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null
      try {
        ws.close()
      } catch {
        // already closing
      }
    }
    this.state.status.value = 'disconnected'
  }

  // ---- outbound commands (FR-007/008/009) ----

  /**
   * Seal and send a command to the Host. No-op (returns false) when the room
   * isn't ACTIVE, so the UI can disable controls until connected.
   */
  sendCommand(cmd: CmdPayload, onAck?: CommandAckHandler): boolean {
    if (!this.isActive) return false
    let outbound = cmd
    let commandId: string | null = null
    if (onAck) {
      commandId =
        typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function'
          ? crypto.randomUUID()
          : `cmd-${Date.now()}-${this.seq + 1}`
      outbound = { ...cmd, command_id: commandId }
      const timer = setTimeout(() => {
        const pending = this.pendingCommandAcks.get(commandId!)
        if (!pending) return
        this.pendingCommandAcks.delete(commandId!)
        pending.handler(false, 'host_ack_timeout')
      }, HOST_RESPONSE_TIMEOUT_MS)
      this.pendingCommandAcks.set(commandId, { handler: onAck, timer })
    }
    const sent = this.sendSealedCommand(outbound)
    if (!sent && commandId) {
      const pending = this.pendingCommandAcks.get(commandId)
      if (pending) clearTimeout(pending.timer)
      this.pendingCommandAcks.delete(commandId)
    }
    return sent
  }

  private failPendingCommandAcks(reason: string): void {
    const pending = [...this.pendingCommandAcks.values()]
    this.pendingCommandAcks.clear()
    for (const ack of pending) {
      clearTimeout(ack.timer)
      ack.handler(false, reason)
    }
  }

  /** Internal transport primitive also used by the pre-auth gate frame. */
  private sendSealedCommand(cmd: CmdPayload): boolean {
    if (!this.session || !this.ws || this.ws.readyState !== WebSocket.OPEN || !this.state.roomId.value) return false
    const plaintext = new TextEncoder().encode(JSON.stringify(cmd))
    const cipher = this.session.seal(plaintext)
    try {
      this.ws.send(JSON.stringify(cmdFrame(this.state.roomId.value, cipher, this.nextSeq())))
    } catch {
      return false
    }
    // Commands that MUST produce a host-originated frame arm the silent-host
    // watchdog (see the keepalive loop). Cleared by the first host frame.
    if (
      cmd.action === 'start_run' ||
      cmd.action === 'send_chat_message' ||
      cmd.action === 'request_history'
    ) {
      this.awaitingHostSince = Date.now()
    }
    return true
  }

  // ---- wake / reconnect-on-foreground ----

  private addWakeListeners(): void {
    if (typeof document !== 'undefined') {
      document.addEventListener('visibilitychange', this.onVisibility)
    }
    if (typeof window !== 'undefined') {
      window.addEventListener('online', this.onOnline)
      window.addEventListener('focus', this.onFocus)
    }
  }

  private removeWakeListeners(): void {
    if (typeof document !== 'undefined') {
      document.removeEventListener('visibilitychange', this.onVisibility)
    }
    if (typeof window !== 'undefined') {
      window.removeEventListener('online', this.onOnline)
      window.removeEventListener('focus', this.onFocus)
    }
  }

  private wakeReconnect(): void {
    if (this.closedByUser) return
    // A socket that is OPEN or still CONNECTING is already doing the work; don't
    // stack a second one (forceReconnect is the path that deliberately replaces
    // a possibly-dead socket).
    if (this.ws && (this.ws.readyState === WebSocket.OPEN || this.ws.readyState === WebSocket.CONNECTING)) {
      return
    }
    if (this.state.status.value === 'connected') return
    // A session-mode (token) connection reconnects indefinitely (matching
    // beginReconnect's `durable` path); only a fresh OTP link is bound by the
    // reconnect window. Without this a screen-lock past the window would strand
    // an authenticated session on failTerminal with no way back.
    const durable = this.secret?.mode === 'session'
    if (this.reconnectDeadline === null) {
      if (!durable) return
      // durable + no active window + socket not open ⇒ dead socket after a
      // resume; fall through and re-attach via the resume token / rendezvous.
    } else if (!durable && Date.now() >= this.reconnectDeadline) {
      this.failTerminal('Reconnect window elapsed — please reconnect.')
      return
    }
    this.backoff = BACKOFF_BASE_MS
    this.clearReconnectTimer()
    this.openSocket()
  }

  /** Shed the current socket — which after a resume may be a "zombie" that still
   * reports OPEN but carries no traffic — and immediately re-attach on a fresh
   * one (via the resume token, else the rendezvous code). Used on foreground /
   * network-online, where trusting readyState would silently strand us.
   * openSocket() tears down the previous socket, keeping the single-socket
   * invariant. */
  private forceReconnect(): void {
    if (this.closedByUser) return
    // Coalesce a burst of wake events (unlock fires visibilitychange + online +
    // focus within milliseconds). If a reconnect is already CONNECTING, or a
    // socket opened moments ago, trust it — tearing it down here is exactly what
    // caused the resume→immediate-drop storm (resume_ok then peer_left 20ms
    // later). A genuinely stale/zombie socket has an old lastOpenAt (from before
    // the device slept), so it still gets replaced.
    if (this.ws && this.ws.readyState === WebSocket.CONNECTING) return
    if (
      this.ws &&
      this.ws.readyState === WebSocket.OPEN &&
      Date.now() - this.lastOpenAt < FORCE_RECONNECT_COOLDOWN_MS
    ) {
      return
    }
    this.failPendingCommandAcks('connection_lost')
    this.session = null
    this.backoff = BACKOFF_BASE_MS
    this.openSocket()
  }

  // ---- socket lifecycle ----

  private openSocket(): void {
    // Single-socket invariant. Every reconnect path (backoff timer, wake events,
    // resume_invalid retry) funnels through here, and browsers fire
    // visibilitychange + focus + online together on resume — so without this we
    // can spin up several sockets that all race for the same room, each tripping
    // the other's peer_left (a reconnect storm). Tear down any prior socket and
    // cancel a pending reconnect before dialing a fresh one.
    this.clearReconnectTimer()
    this.failPendingCommandAcks('connection_lost')
    if (this.ws) {
      const prev = this.ws
      this.ws = null
      prev.onopen = prev.onmessage = prev.onerror = prev.onclose = null
      try {
        prev.close()
      } catch {
        // already closing
      }
    }
    this.state.lastError.value = null
    this.state.live.value = false
    this.state.status.value = this.reconnectDeadline !== null ? 'reconnecting' : 'joining'
    let ws: WebSocket
    try {
      ws = new WebSocket(this.link.relay_url)
    } catch {
      this.fail('Invalid relay URL')
      return
    }
    ws.binaryType = 'arraybuffer'
    this.ws = ws
    ws.onopen = () => this.onOpen()
    ws.onmessage = (ev) => this.onMessage(ev)
    ws.onerror = () => {
      // The `close` event carries the actionable outcome; error is advisory.
    }
    ws.onclose = () => this.onClose()
  }

  private onOpen(): void {
    this.lastOpenAt = Date.now()
    // Prime the liveness clock so the watchdog measures from the (re)open, not
    // from a stale timestamp of the previous socket (which would fire instantly).
    this.lastInboundAt = Date.now()
    // A fresh socket re-runs the gate (which re-arms this if needed); clear any
    // pending await from the dead socket so it can't fire on the new one.
    this.awaitingHostSince = null
    // Fresh session per connection (forward secrecy) — cleared until handshake.
    this.session = null
    if (this.resumeToken) {
      // Fast re-attach to the same room with the rotating token (the relay puts
      // the room back into HANDSHAKING and the Host re-offers the handshake).
      this.state.status.value = this.reconnectDeadline !== null ? 'reconnecting' : 'joining'
      this.sendEnvelope(resumeFrame(this.resumeToken))
    } else {
      // First join with the per-session rendezvous code (used where the old flow
      // used the pair code).
      this.state.status.value = 'joining'
      this.sendEnvelope(joinFrame(this.link.rendezvous_code))
    }
  }

  private onMessage(ev: MessageEvent<unknown>): void {
    // Any inbound frame — including the relay's keepalive echo — proves the
    // socket is alive; refresh the liveness clock before anything else.
    this.lastInboundAt = Date.now()
    const env = decodeEnvelope(ev.data)
    if (!env) return
    switch (env.t) {
      case 'room_ready':
        if (env.room_id) {
          this.state.roomId.value = env.room_id
          this.state.status.value = 'handshaking'
        }
        if (env.resume_token) this.resumeToken = env.resume_token
        break
      case 'control':
        this.onHandshakeOffer(env)
        break
      case 'stream':
        this.onStreamFrame(env)
        break
      case 'peer_left':
        // The Host dropped. If we just sent an OTP attempt and haven't been
        // authenticated, read this as a rejected OTP (the Host closes the room
        // after a bad/exhausted OTP) rather than a transient drop.
        if (this.otpVerdictPending()) {
          this.failAuth('Wrong or expired code — please try again.')
        } else {
          this.beginReconnect('Host disconnected')
        }
        break
      case 'error':
        this.onRelayError(env)
        break
      default:
        break
    }
  }

  /**
   * Provide the OTP the user typed. Sets the secret and, if the handshake offer
   * already arrived, derives the session and fires the gate frame. Safe to call
   * repeatedly to retry a wrong code (re-derives + resends on the same room).
   */
  submitOtp(otp: string): void {
    this.secret = { mode: 'otp', otp }
    this.authMode = 'otp'
    this.state.lastError.value = null
    if (this.hostPub && this.myKeypair && this.state.roomId.value) {
      this.deriveAndAuthenticate()
    }
    // else: the offer hasn't arrived yet; deriveAndAuthenticate() runs on offer.
  }

  private onHandshakeOffer(env: Envelope): void {
    if (!env.cipher || !this.state.roomId.value) return
    let offerJson: unknown
    try {
      offerJson = JSON.parse(base64ToUtf8(env.cipher))
    } catch {
      this.fail('Malformed handshake frame')
      return
    }
    try {
      // Verify the Host (SEC-006) WITHOUT deriving the key — the OTP may not be
      // known yet. Answering + handshake_done drives the relay room to ACTIVE so
      // the first sealed cmd (the OTP gate) can actually route.
      this.hostPub = verifyOffer(offerJson, this.link.host_fingerprint)
      this.state.hostFingerprint.value = this.link.host_fingerprint
      if (!this.myKeypair) this.myKeypair = generateKeypair()
      this.sendEnvelope(
        controlFrame(
          this.state.roomId.value,
          utf8ToBase64(hsAnswerJson(toHex(this.myKeypair.publicKey))),
          this.nextSeq(),
        ),
      )
      this.sendEnvelope(handshakeDoneFrame(this.state.roomId.value))
      // The relay handshake is complete, but business commands are not safe
      // until the Host accepts the OTP/session gate and returns a fresh token.
      this.state.status.value = 'handshaking'
      // If the secret is already known (token reconnect, or the OTP was typed
      // before the offer arrived), derive + gate now; otherwise wait for the OTP.
      if (this.secret) this.deriveAndAuthenticate()
    } catch (e) {
      const msg = e instanceof HandshakeError ? e.message : 'Handshake failed'
      // A fingerprint mismatch is terminal — do NOT reconnect into a MITM.
      this.failTerminal(msg)
    }
  }

  /** Derive the E2E session from the verified host key + the current secret, then
   * send the first sealed frame that the Host opens at its OTP/token gate. */
  private deriveAndAuthenticate(): void {
    if (!this.hostPub || !this.myKeypair || !this.secret || !this.state.roomId.value) return
    try {
      const psk = sessionSecretBytes(this.link.link_secret, this.currentSecret2())
      const { session } = deriveSessionKey(this.hostPub, psk, this.myKeypair)
      this.session = session
    } catch (e) {
      this.failTerminal(e instanceof HandshakeError ? e.message : 'Handshake failed')
      return
    }
    this.gateRetries = 0
    this.sendGateFrame()
  }

  /** Send the first sealed cmd — the Host opens it with its OTP/session key to
   * authenticate. Starts the verdict timer so a silent non-answer (wrong OTP)
   * re-prompts the user. */
  private sendGateFrame(): void {
    if (!this.session) return
    this.reconnectDeadline = null
    this.backoff = BACKOFF_BASE_MS
    if (this.state.auth.value !== 'authenticated') this.state.auth.value = 'pending'
    this.otpAttemptAt = Date.now()
    this.sendSealedCommand({ ...requestHistoryCmd(this.link.run_id), auth_mode: this.authMode })
    this.startVerdictTimer()
  }

  private startVerdictTimer(): void {
    this.clearVerdictTimer()
    // Only an OTP attempt can be silently wrong; a token reconnect that fails
    // surfaces via peer_left / relay error instead.
    if (this.authMode !== 'otp') return
    this.verdictTimer = setTimeout(() => {
      if (this.state.auth.value !== 'authenticated') {
        this.softAuthFail('Wrong or expired code — please try again.')
      }
    }, OTP_VERDICT_WINDOW_MS)
  }

  private clearVerdictTimer(): void {
    if (this.verdictTimer !== null) {
      clearTimeout(this.verdictTimer)
      this.verdictTimer = null
    }
  }

  /** OTP rejected but the room is still open (the Host does not close on a bad
   * OTP): re-prompt WITHOUT tearing down, so submitOtp() can retry on the same
   * connection. */
  private softAuthFail(message: string): void {
    this.clearVerdictTimer()
    this.otpAttemptAt = null
    this.awaitingHostSince = null
    this.state.auth.value = 'failed'
    this.state.lastError.value = message
  }

  private onStreamFrame(env: Envelope): void {
    if (!env.cipher || !this.session) return
    let plaintext: Uint8Array
    try {
      plaintext = this.session.open(env.cipher)
    } catch {
      // A frame we can't open. If we're mid-OTP-verdict, a decode error means the
      // key (from the OTP) is wrong → surface as a rejected OTP. Otherwise count
      // it (never log the cipher) so a broken link is visible.
      this.state.decodeErrors.value += 1
      if (this.otpVerdictPending()) this.softAuthFail('Wrong or expired code — please try again.')
      return
    }
    let json: unknown
    try {
      json = JSON.parse(new TextDecoder().decode(plaintext))
    } catch {
      this.state.decodeErrors.value += 1
      return
    }
    const payload = parseStreamPayload(json)
    if (!payload) {
      this.state.decodeErrors.value += 1
      return
    }
    this.state.live.value = true
    // A host-originated frame proves the host is alive and this room is in sync
    // → clear the silent-host watchdog.
    this.awaitingHostSince = null

    // Transport-level auth signals: handle here, don't forward to the run store.
    if (payload.kind === 'session_token') {
      this.onSessionToken(payload.token)
      return
    }
    if (payload.kind === 'auth_result') {
      if (!payload.ok) this.failAuth(payload.reason || 'Authentication failed — please try again.')
      // ok:true is redundant with session_token; wait for the token.
      return
    }
    if (payload.kind === 'command_ack') {
      const ack = this.pendingCommandAcks.get(payload.command_id)
      if (ack) {
        this.pendingCommandAcks.delete(payload.command_id)
        clearTimeout(ack.timer)
        ack.handler(payload.accepted, payload.reason)
      }
      return
    }
    this.onStream(payload)
  }

  /** Auth SUCCEEDED: persist the token (link_secret + session_secret), flip to
   * session mode so future reconnects skip the OTP, and pull the parity data the
   * composer needs (now that commands are accepted). */
  private onSessionToken(token: string): void {
    this.clearVerdictTimer()
    this.otpAttemptAt = null
    this.gateRetries = 0
    this.state.auth.value = 'authenticated'
    this.state.status.value = 'connected'
    this.secret = { mode: 'session', token }
    this.authMode = 'session'
    tokenStore.save({
      relay_url: this.link.relay_url,
      host_fingerprint: this.link.host_fingerprint,
      rendezvous_code: this.link.rendezvous_code,
      run_id: this.link.run_id,
      link_secret: this.link.link_secret,
      token,
    })
    // Parity data for the composer (host also pushes some of this on activation).
    this.sendCommand(listSlashCommandsCmd())
    this.sendCommand(listEngineModelsCmd())
    this.sendCommand(listProjectFilesCmd(this.link.run_id))
    this.sendCommand(listPlanUsageCmd())
  }

  /** OTP rejected / expired / exhausted (or an equivalent silent close). The UI
   * re-prompts for the OTP; we clear any stale token and stop retrying. */
  private failAuth(message: string): void {
    tokenStore.clear()
    this.state.auth.value = 'failed'
    this.otpAttemptAt = null
    this.awaitingHostSince = null
    this.closedByUser = true
    this.removeWakeListeners()
    this.stopKeepalive()
    this.clearReconnectTimer()
    this.reconnectDeadline = null
    this.session = null
    this.failPendingCommandAcks('authentication_failed')
    if (this.ws) {
      const ws = this.ws
      this.ws = null
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null
      try {
        ws.close()
      } catch {
        // ignore
      }
    }
    this.state.lastError.value = message
    this.state.status.value = 'error'
  }

  /** True while an OTP attempt is awaiting the Host's verdict (recent + unauth). */
  private otpVerdictPending(): boolean {
    if (this.authMode !== 'otp') return false
    if (this.state.auth.value === 'authenticated') return false
    return this.otpAttemptAt !== null && Date.now() - this.otpAttemptAt < OTP_VERDICT_WINDOW_MS
  }

  private onRelayError(env: Envelope): void {
    const code = env.code ?? 'error'
    // resume_invalid: the fast-path token is spent / the away room was swept.
    if (code === 'resume_invalid') {
      this.resumeToken = null
      this.state.lastError.value = null
      this.dropSocketForRetry()
      return
    }
    // "room not active" right after the handshake means our first (gate) frame
    // raced ahead of the room reaching ACTIVE (the Host's handshake_done is still
    // in flight). Resend the gate frame shortly, a few times — this does NOT burn
    // an OTP attempt (the frame never reached the Host's gate).
    if (
      code === 'room_not_found' &&
      this.session !== null &&
      this.gateRetries < GATE_MAX_RETRIES
    ) {
      // The gate frame outran the room reaching ACTIVE; resend shortly. This must
      // NOT require auth==='pending': on a reconnect of an already-authenticated
      // session, auth stays 'authenticated', and gating that on 'pending' made
      // every reconnect skip the retry and tear the socket down instantly — an
      // endless resume→drop storm. The session must be re-gated on each fresh
      // socket regardless of the prior auth phase.
      this.gateRetries += 1
      this.clearVerdictTimer()
      setTimeout(() => this.sendGateFrame(), GATE_RETRY_MS)
      return
    }
    // pair_invalid on a session-mode (token) reconnect is ambiguous — the Host
    // may be restarting / re-arming its rendezvous. Keep retrying briefly; the
    // reconnect window still bounds it. For a fresh OTP link it is terminal.
    if (code === 'pair_invalid' || code === 'room_not_found') {
      if (this.secret?.mode === 'session') {
        this.state.lastError.value = 'Waiting for the host…'
        this.dropSocketForRetry()
      } else {
        this.failTerminal(relayErrorMessage(code))
      }
      return
    }
    if (code === 'room_full' || code === 'auth_failed') {
      this.failTerminal(relayErrorMessage(code))
    } else {
      this.state.lastError.value = relayErrorMessage(code)
    }
  }

  private dropSocketForRetry(): void {
    if (this.ws) {
      try {
        this.ws.close()
      } catch {
        // already closing — onClose will still fire
      }
    }
  }

  private onClose(): void {
    this.ws = null
    this.failPendingCommandAcks('connection_lost')
    if (this.closedByUser) return
    // A close right after an OTP attempt (no verdict yet) is a rejected OTP.
    if (this.otpVerdictPending()) {
      this.failAuth('Wrong or expired code — please try again.')
      return
    }
    this.beginReconnect('Connection lost')
  }

  private beginReconnect(reason: string): void {
    if (this.closedByUser) return
    this.session = null
    const now = Date.now()
    if (this.reconnectDeadline === null) {
      this.reconnectDeadline = now + RECONNECT_WINDOW_MS
    }
    // A session-mode (token) connection keeps retrying its reusable rendezvous
    // code; a fresh OTP link gives up when the resume window elapses.
    const durable = this.secret?.mode === 'session'
    if (!durable && now >= this.reconnectDeadline) {
      this.failTerminal('Reconnect window elapsed — please reconnect.')
      return
    }
    this.state.status.value = 'reconnecting'
    this.state.lastError.value = reason
    const cap = durable ? BACKOFF_MAX_MS : this.reconnectDeadline - now
    const delay = Math.min(this.backoff, cap)
    this.backoff = Math.min(this.backoff * 2, BACKOFF_MAX_MS)
    this.clearReconnectTimer()
    this.reconnectTimer = setTimeout(() => this.openSocket(), delay)
  }

  private clearReconnectTimer(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer)
      this.reconnectTimer = null
    }
  }

  private fail(message: string): void {
    this.state.lastError.value = message
    this.state.status.value = 'error'
  }

  private failTerminal(message: string): void {
    this.closedByUser = true
    this.removeWakeListeners()
    this.stopKeepalive()
    this.clearReconnectTimer()
    this.reconnectDeadline = null
    this.session = null
    this.failPendingCommandAcks(message)
    if (this.ws) {
      const ws = this.ws
      this.ws = null
      ws.onopen = ws.onmessage = ws.onerror = ws.onclose = null
      try {
        ws.close()
      } catch {
        // ignore
      }
    }
    this.state.lastError.value = message
    this.state.status.value = 'error'
  }

  /** The `secret2` fed to the KDF for the current mode (empty if not set yet). */
  private currentSecret2(): string {
    if (!this.secret) return ''
    return this.secret.mode === 'otp' ? this.secret.otp : this.secret.token
  }

  private sendEnvelope(env: Envelope): void {
    if (this.ws && this.ws.readyState === WebSocket.OPEN) {
      this.ws.send(JSON.stringify(env))
    }
  }

  private nextSeq(): number {
    this.seq += 1
    return this.seq
  }
}

/** Decode an inbound frame (text JSON) into a typed {@link Envelope}, or null. */
function decodeEnvelope(data: unknown): Envelope | null {
  let text: string
  if (typeof data === 'string') {
    text = data
  } else if (data instanceof ArrayBuffer) {
    text = new TextDecoder().decode(new Uint8Array(data))
  } else {
    return null
  }
  try {
    const v: unknown = JSON.parse(text)
    if (v && typeof v === 'object' && typeof (v as Envelope).t === 'string') {
      return v as Envelope
    }
  } catch {
    // malformed — ignore
  }
  return null
}

/** Map a relay error code to a friendly message. */
function relayErrorMessage(code: string): string {
  switch (code) {
    case 'pair_invalid':
      return 'Link is invalid or expired. Please reconnect.'
    case 'room_full':
      return 'This session already has an active controller.'
    case 'auth_failed':
      return 'Authentication failed.'
    case 'resume_invalid':
      return 'Reconnect window elapsed — please reconnect.'
    case 'room_not_found':
      return 'The session is no longer available.'
    case 'rate_limited':
      return 'Too many requests — slow down.'
    case 'forbidden':
      return 'That action is not permitted.'
    default:
      return `Relay error: ${code}`
  }
}
