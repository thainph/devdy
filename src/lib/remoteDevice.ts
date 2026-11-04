// Settings → Devices: what "this machine" looks like to Remote Control and device sync,
// derived from the `remote_status` snapshot. Pure so it can be unit-tested.

import { DEVICE_ALIAS_RE } from './remoteLogin.ts'

/** A clash the relay reported (`remote_status.deviceConflict`, event `remote://device-conflict`). */
export interface DeviceConflict {
  kind: 'device_id' | 'alias'
  alias?: string
}

/** The `RemoteStatus` fields read here (see stores/remoteControl.ts). */
export interface DeviceStatus {
  enabled: boolean
  running: boolean
  connected: boolean
  reconnecting: boolean
  relay_url: string
  has_auth_token: boolean
  has_master_password: boolean
  device_alias: string | null
  session_authenticated: boolean
  deviceConflict?: DeviceConflict | null
  last_error?: string | null
}

/** Setup checklist, in order. */
export const SETUP_STEPS = ['relay', 'name', 'password', 'enable'] as const
export type SetupStep = (typeof SETUP_STEPS)[number]

export function setupDone(s: DeviceStatus | null | undefined): Record<SetupStep, boolean> {
  return {
    relay: !!s?.relay_url?.trim() && !!s?.has_auth_token,
    name: !!s?.device_alias,
    password: !!s?.has_master_password,
    enable: !!s?.enabled,
  }
}

/** The first step still to do, or `null` once everything is set up. */
export function nextSetupStep(s: DeviceStatus | null | undefined): SetupStep | null {
  const done = setupDone(s)
  return SETUP_STEPS.find(k => !done[k]) ?? null
}

/**
 * State of the "This machine" card (badge + primary action):
 * - `setup`: off and not fully set up
 * - `off`: set up, Remote Control off
 * - `idConflict`: another running Devdy has this device id (the relay refuses this one)
 * - `phone`: a phone / web controller is signed in
 * - `aliasConflict`: another online device uses the same name
 * - `error`: enabled but not connected for a known reason, or the agent stopped
 * - `connecting`: enabled, dialing the relay
 * - `ready`: connected, nobody attached
 */
export type DeviceState =
  | 'setup' | 'off' | 'idConflict' | 'phone' | 'aliasConflict' | 'error' | 'connecting' | 'ready'

export function deviceState(s: DeviceStatus | null | undefined): DeviceState {
  if (!s) return 'setup'
  if (!s.enabled) return nextSetupStep(s) === 'enable' ? 'off' : 'setup'
  if (s.deviceConflict?.kind === 'device_id' || s.last_error === 'device_id_conflict') return 'idConflict'
  if (s.session_authenticated) return 'phone'
  if (s.deviceConflict?.kind === 'alias') return 'aliasConflict'
  if (!s.running) return 'error'
  if (s.connected) return 'ready'
  return s.last_error ? 'error' : 'connecting'
}

/** `remote_status.last_error` codes with a message (i18n `remote.device.errors.<code>`). */
export const LAST_ERROR_CODES = [
  'relay_unreachable', 'relay_bad_url', 'relay_auth_failed', 'relay_unsupported',
  'relay_rate_limited', 'device_id_conflict', 'relay_not_configured', 'token_missing',
] as const
export type LastErrorCode = (typeof LAST_ERROR_CODES)[number] | 'agent_stopped' | 'unknown'

/** Why the card is in `error`: the backend code, `agent_stopped`, or `unknown`. */
export function errorReason(s: DeviceStatus | null | undefined): LastErrorCode {
  const code = s?.last_error ?? ''
  if ((LAST_ERROR_CODES as readonly string[]).includes(code)) return code as LastErrorCode
  if (s?.enabled && !s.running) return 'agent_stopped'
  return 'unknown'
}

/** Longest device name the backend accepts (see DEVICE_ALIAS_RE). */
const ALIAS_MAX = 32

/**
 * A free variant of `alias` to offer when it clashes: `macbook` → `macbook-2`,
 * `macbook-2` → `macbook-3`. Always a valid device name; `taken` is skipped too.
 */
export function suggestAlias(alias: string, taken: string[] = []): string {
  const clean = alias.trim().toLowerCase().replace(/[^a-z0-9-]+/g, '-').replace(/^-+|-+$/g, '') || 'devdy'
  const m = clean.match(/^(.*?)-(\d+)$/)
  const base = m ? m[1] : clean
  let n = m ? Number(m[2]) + 1 : 2
  const used = new Set([clean, ...taken.map(a => a.toLowerCase())])
  for (;;) {
    const suffix = `-${n}`
    const candidate = `${base.slice(0, ALIAS_MAX - suffix.length).replace(/-+$/, '')}${suffix}`
    if (!used.has(candidate) && DEVICE_ALIAS_RE.test(candidate)) return candidate
    n++
  }
}

/** Audit actions with a readable label (i18n `remote.activity.actions.<action>`). */
export const AUDIT_ACTION_LABELS = [
  'login', 'authenticate', 'kick_controller', 'set_master_password', 'clear_master_password',
  'device_sync_pull', 'start_run', 'cancel_run', 'send_chat_message', 'respond_permission',
  'open_run', 'close_run', 'list_runs', 'request_history',
] as const

/** Rejection reasons with a readable label (i18n `remote.activity.reasons.<reason>`). */
export const AUDIT_REASON_LABELS = [
  'bad_password', 'bad_otp', 'throttled', 'rate_limited', 'malformed', 'unauthorized',
  'action_not_allowed', 'wrong_run', 'not_found', 'bad_request',
] as const

/** `send_chat_message` → `Send chat message`: the fallback for an action with no label. */
export function humanizeAction(action: string): string {
  const words = action.replace(/[_.]+/g, ' ').trim()
  return words ? words[0].toUpperCase() + words.slice(1) : action
}
