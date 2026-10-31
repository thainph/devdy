// State of the sidebar Remote Control dot (components/RemoteIndicator.vue), from
// the `remote_status` snapshot. Pure so it can be unit-tested.

/** The `RemoteStatus` fields the dot reads (see stores/remoteControl.ts). */
export interface RemoteDotStatus {
  enabled: boolean
  running: boolean
  connected: boolean
  reconnecting: boolean
  session_authenticated: boolean
  desktop_controlled: boolean
}

/**
 * - `off`: feature disabled (the dot is hidden)
 * - `error`: enabled but the agent is not running (failed to start / stopped)
 * - `offline`: agent running, relay socket not up (connecting or reconnecting)
 * - `online`: connected to the relay, nobody attached
 * - `phone`: a phone / web controller is signed in
 * - `desktop`: another Devdy is driving this one (wins over `phone`)
 */
export type RemoteTone = 'off' | 'error' | 'offline' | 'online' | 'phone' | 'desktop'

export function remoteTone(s: RemoteDotStatus | null | undefined): RemoteTone {
  if (!s?.enabled) return 'off'
  if (!s.running) return 'error'
  if (s.desktop_controlled) return 'desktop'
  if (s.session_authenticated) return 'phone'
  return s.connected ? 'online' : 'offline'
}

/** Dot colour per tone; `desktop` / `phone` also pulse. */
export const REMOTE_DOT_CLASS: Record<RemoteTone, string> = {
  off: 'bg-muted-foreground/40',
  error: 'bg-red-500',
  offline: 'bg-muted-foreground/40',
  online: 'bg-emerald-500',
  phone: 'bg-emerald-500',
  desktop: 'bg-amber-500',
}

export function isControlled(tone: RemoteTone): boolean {
  return tone === 'phone' || tone === 'desktop'
}
