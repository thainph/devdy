import { defineStore } from 'pinia'
import { ref } from 'vue'
import { invoke } from '@/lib/tauri'

/**
 * Host-side Remote Control state. Wraps the `remote_*` Tauri commands from the
 * Host agent and mirrors the current status so the Settings UI can reflect
 * whether the feature is on, whether the relay socket is up, and whether a run
 * is currently bound to a remote session.
 *
 * Secrets never live here: `createSessionLink` returns the one-time link (which
 * carries `link_secret`) once and hands it straight to the caller to render as
 * a QR / copy link — it is deliberately NOT cached in the store and NEVER
 * logged.
 */

/** Mirror of the backend `RemoteStatus` (commands.rs). */
export interface RemoteStatus {
  enabled: boolean
  running: boolean
  connected: boolean
  relay_url: string
  has_auth_token: boolean
  /** Whether an owner master password is set (controller may use it vs the OTP). */
  has_master_password: boolean
  device_id: string | null
  /** Run currently bound to the active remote session, if any. */
  bound_run_id: string | null
  /** Whether a controller has completed the OTP handshake for the bound run. */
  session_authenticated: boolean
  /** Unix seconds at which the idle session token expires (sliding 3h). */
  session_idle_expires_at: number | null
}

/**
 * Mirror of the backend `LinkPayload` (agent.rs), returned once by
 * `remote_create_session_link`. Carries the out-of-band `link_secret`.
 */
export interface SessionLink {
  relay_url: string
  host_fingerprint: string
  rendezvous_code: string
  run_id: string
  link_secret: string
}

/** Mirror of the backend `OtpReveal` (on-demand pairing code). */
export interface OtpReveal {
  otp: string
  otp_expires_at: number
  attempts_left: number
}

/** Mirror of the backend `AuditEntry`. */
export interface AuditEntry {
  id: string
  ts: string
  device_id: string | null
  room_id: string | null
  run_id: string | null
  action: string
  result: string
  reason: string | null
}

export const useRemoteControlStore = defineStore('remoteControl', () => {
  const status = ref<RemoteStatus | null>(null)
  const audit = ref<AuditEntry[]>([])
  const loaded = ref(false)

  /** Fetch the current status snapshot (feature on?, socket up?, bound run). */
  async function refreshStatus(): Promise<RemoteStatus> {
    const s = await invoke<RemoteStatus>('remote_status')
    status.value = s
    loaded.value = true
    return s
  }

  /** Read the most recent audit entries. */
  async function refreshAudit(limit = 200): Promise<AuditEntry[]> {
    const list = await invoke<AuditEntry[]>('remote_get_audit', { limit })
    audit.value = list
    return list
  }

  /** Load everything the Settings screen needs in one shot. */
  async function refreshAll(): Promise<void> {
    await refreshStatus()
    await refreshAudit()
  }

  /**
   * Purge the entire audit log (owner-initiated, destructive). Returns the
   * number of entries removed and clears the local mirror so the UI updates
   * without waiting for a refresh.
   */
  async function clearAudit(): Promise<number> {
    const removed = await invoke<number>('remote_clear_audit')
    audit.value = []
    return removed
  }

  /**
   * Persist the relay URL and (optionally) the auth token. An empty/omitted
   * token leaves the stored token unchanged (never in SQLite).
   */
  async function setConfig(relayUrl: string, authToken?: string): Promise<void> {
    await invoke('remote_set_config', {
      relayUrl,
      authToken: authToken && authToken.trim() ? authToken.trim() : null,
    })
    await refreshStatus()
  }

  /** Enable the feature and start the agent. */
  async function enable(): Promise<void> {
    await invoke('remote_enable')
    await refreshStatus()
  }

  /** Disable the feature and stop the agent. */
  async function disable(): Promise<void> {
    await invoke('remote_disable')
    await refreshStatus()
  }

  /**
   * Create a per-run remote session link and return the payload ONCE. The
   * caller renders it as a QR / copy link; the `link_secret` is not kept in the
   * store. Supersedes any existing bound session on the host side.
   */
  async function createSessionLink(runId: string): Promise<SessionLink> {
    const link = await invoke<SessionLink>('remote_create_session_link', { runId })
    await refreshStatus()
    return link
  }

  /** End the active remote session (best-effort) and refresh status/audit. */
  async function endSession(): Promise<void> {
    await invoke('remote_end_session')
    await Promise.all([refreshStatus(), refreshAudit()])
  }

  /**
   * Reveal the current pairing OTP for a run on demand (minting one if none is
   * live), so the desktop can re-show the code without waiting for a controller
   * to (re)join. Returns null when no session is bound to this run.
   */
  async function revealOtp(runId: string): Promise<OtpReveal | null> {
    return await invoke<OtpReveal | null>('remote_reveal_otp', { runId })
  }

  /** Set (or clear, with an empty string) the owner master password, then refresh
   * status so `has_master_password` reflects the change. */
  async function setMasterPassword(password: string): Promise<void> {
    await invoke('remote_set_master_password', { password })
    await refreshStatus()
  }

  return {
    status,
    audit,
    loaded,
    refreshStatus,
    refreshAudit,
    refreshAll,
    clearAudit,
    setConfig,
    enable,
    disable,
    createSessionLink,
    endSession,
    revealOtp,
    setMasterPassword,
  }
})
