/**
 * The remote-desktop window's sign-in record: the session link + token the
 * controlled Devdy granted on password login, kept in this machine's webview
 * localStorage so the window reconnects without asking again.
 */
import type { SessionLinkPayload } from '@/controller/protocol'

export const REMOTE_DESKTOP_STORAGE_KEY = 'devdy.remoteDesktop.v1'

export interface RemoteDesktopSession {
  link: SessionLinkPayload
  token: string
  /** Alias of the controlled device ('' = unnamed). */
  device: string
}

const LINK_FIELDS = ['relay_url', 'host_fingerprint', 'rendezvous_code', 'run_id', 'link_secret'] as const
/** Fields that must be non-empty (`run_id` is informational here). */
const REQUIRED = ['relay_url', 'host_fingerprint', 'rendezvous_code', 'link_secret'] as const

/** Parse a stored record; `null` when missing or malformed. */
export function parseRemoteDesktopSession(raw: string | null): RemoteDesktopSession | null {
  if (!raw) return null
  let v: unknown
  try {
    v = JSON.parse(raw)
  } catch {
    return null
  }
  if (!v || typeof v !== 'object') return null
  const rec = v as Record<string, unknown>
  const link = rec.link as Record<string, unknown> | null
  if (!link || typeof link !== 'object') return null
  if (!LINK_FIELDS.every((k) => typeof link[k] === 'string')) return null
  if (!REQUIRED.every((k) => link[k])) return null
  if (typeof rec.token !== 'string' || !rec.token) return null
  return {
    link: Object.fromEntries(LINK_FIELDS.map((k) => [k, link[k]])) as unknown as SessionLinkPayload,
    token: rec.token,
    device: typeof rec.device === 'string' ? rec.device : '',
  }
}

export function loadRemoteDesktopSession(): RemoteDesktopSession | null {
  try {
    return parseRemoteDesktopSession(localStorage.getItem(REMOTE_DESKTOP_STORAGE_KEY))
  } catch {
    return null
  }
}

export function saveRemoteDesktopSession(s: RemoteDesktopSession): void {
  localStorage.setItem(REMOTE_DESKTOP_STORAGE_KEY, JSON.stringify(s))
}

export function clearRemoteDesktopSession(): void {
  try {
    localStorage.removeItem(REMOTE_DESKTOP_STORAGE_KEY)
  } catch {
    // storage unavailable — nothing to clear
  }
}
