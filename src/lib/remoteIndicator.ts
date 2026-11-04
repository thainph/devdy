// State of the sidebar Remote Control dot (components/RemoteIndicator.vue), from
// the `remote_status` snapshot. Pure so it can be unit-tested.

import type { DeviceConflict } from './remoteDevice.ts'

/** The `RemoteStatus` fields the dot reads (see stores/remoteControl.ts). */
export interface RemoteDotStatus {
  enabled: boolean
  running: boolean
  connected: boolean
  reconnecting: boolean
  session_authenticated: boolean
  relay_url?: string
  deviceConflict?: DeviceConflict | null
}

/**
 * - `hidden`: no relay configured (the dot is not shown)
 * - `off`: relay configured, Remote Control off
 * - `error`: enabled but the agent is not running (failed to start / stopped)
 * - `conflict`: the relay reported a device id / name clash
 * - `offline`: agent running, relay socket not up (connecting or reconnecting)
 * - `online`: connected to the relay, nobody attached
 * - `phone`: a phone / web controller is signed in
 */
export type RemoteTone = 'hidden' | 'off' | 'error' | 'conflict' | 'offline' | 'online' | 'phone'

export function remoteTone(s: RemoteDotStatus | null | undefined): RemoteTone {
  if (!s) return 'hidden'
  if (!s.enabled) return s.relay_url?.trim() ? 'off' : 'hidden'
  if (!s.running) return 'error'
  if (s.session_authenticated) return 'phone'
  if (s.deviceConflict) return 'conflict'
  return s.connected ? 'online' : 'offline'
}

/** Dot colour per tone; `phone` also pulses. */
export const REMOTE_DOT_CLASS: Record<RemoteTone, string> = {
  hidden: 'bg-muted-foreground/40',
  off: 'bg-muted-foreground/40',
  error: 'bg-destructive',
  conflict: 'bg-warning ring-2 ring-warning/30',
  offline: 'bg-warning',
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
