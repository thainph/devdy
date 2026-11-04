// State of the sidebar Remote Control dot (components/RemoteIndicator.vue), from
// the `remote_status` snapshot. Pure so it can be unit-tested.

/** The `RemoteStatus` fields the dot reads (see stores/remoteControl.ts). */
export interface RemoteDotStatus {
  enabled: boolean
  running: boolean
  connected: boolean
  reconnecting: boolean
  session_authenticated: boolean
}

/**
 * - `off`: feature disabled (the dot is hidden)
 * - `error`: enabled but the agent is not running (failed to start / stopped)
 * - `offline`: agent running, relay socket not up (connecting or reconnecting)
 * - `online`: connected to the relay, nobody attached
 * - `phone`: a phone / web controller is signed in
 */
export type RemoteTone = 'off' | 'error' | 'offline' | 'online' | 'phone'

export function remoteTone(s: RemoteDotStatus | null | undefined): RemoteTone {
  if (!s?.enabled) return 'off'
  if (!s.running) return 'error'
  if (s.session_authenticated) return 'phone'
  return s.connected ? 'online' : 'offline'
}

/** Dot colour per tone; `phone` also pulses. */
export const REMOTE_DOT_CLASS: Record<RemoteTone, string> = {
  off: 'bg-muted-foreground/40',
  error: 'bg-destructive',
  offline: 'bg-muted-foreground/40',
  online: 'bg-success',
  phone: 'bg-success',
}

export function isControlled(tone: RemoteTone): boolean {
  return tone === 'phone'
}

/** Whether a phone / web controller is signed in. */
export function phoneControlled(s: RemoteDotStatus | null | undefined): boolean {
  return !!s?.session_authenticated
}
