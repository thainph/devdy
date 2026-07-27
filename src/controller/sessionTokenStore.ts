/**
 * Session-token persistence (Controller side) — replaces `reconnectStore.ts`.
 *
 * After a successful OTP the Host mints a `session_token` (an opaque
 * `session_secret`). The Controller persists it here so a reload / drop within
 * the sliding idle window re-attaches WITHOUT re-typing the OTP: it re-joins the
 * `rendezvous_code` and derives the E2E key from `KDF(link_secret, session_secret)`.
 *
 * SECURITY TRADE-OFF (read carefully):
 * Unlike the plan's "opaque token only" idea, token reconnect on this side
 * REQUIRES the `link_secret` too — the Host derives the reconnect key with
 * `link_secret + session_secret`, so the Controller must hold BOTH to reconstruct
 * the same key. Therefore `localStorage` here holds the reconnect secrets
 * (`link_secret` + `token`). Exposure is bounded by:
 *   - a 3h SLIDING idle window enforced here (a stale entry is cleared on load);
 *   - host-side deletion (idle > 3h at the Host → its record is gone → the
 *     reconnect frame can't be opened → we fall back to OTP);
 *   - every command remaining `allow_once`-gated at the Host regardless.
 * The OTP itself is NEVER stored.
 */

const STORAGE_KEY = 'devdy.controller.session.v1'

/** Sliding idle window: 3 hours (mirrors the Host `IDLE_TTL_SECS`). */
export const IDLE_TTL_MS = 3 * 60 * 60 * 1000

/** Everything needed to re-attach a bound session without an OTP. */
export interface SessionToken {
  /** WSS relay URL to dial. */
  relay_url: string
  /** Pinned Host fingerprint (hex) for MITM detection on reconnect. */
  host_fingerprint: string
  /** Per-session relay join key. */
  rendezvous_code: string
  /** The single bound run id. */
  run_id: string
  /** High-entropy link secret (hex) — base of the reconnect KDF (see header). */
  link_secret: string
  /** Opaque session secret (the reconnect `secret2`). */
  token: string
  /** Last successful attach, ms since epoch — the sliding idle anchor. */
  last_used_at: number
}

function isToken(v: unknown): v is SessionToken {
  if (!v || typeof v !== 'object') return false
  const o = v as Record<string, unknown>
  return (
    typeof o.relay_url === 'string' &&
    typeof o.host_fingerprint === 'string' &&
    typeof o.rendezvous_code === 'string' &&
    typeof o.run_id === 'string' &&
    typeof o.link_secret === 'string' &&
    typeof o.token === 'string' &&
    typeof o.last_used_at === 'number'
  )
}

/** Persist (or replace) the session token. Stamps `last_used_at = now`. */
export function save(token: Omit<SessionToken, 'last_used_at'>): void {
  const record: SessionToken = { ...token, last_used_at: Date.now() }
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(record))
  } catch {
    // Storage disabled/full — token reconnect simply won't be available.
  }
}

/**
 * Load a still-fresh token, or `null` if absent/malformed/idle-expired. An
 * idle-expired token (`now - last_used_at > 3h`) is cleared as a side effect.
 */
export function load(): SessionToken | null {
  let raw: string | null
  try {
    raw = localStorage.getItem(STORAGE_KEY)
  } catch {
    return null
  }
  if (!raw) return null
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    clear()
    return null
  }
  if (!isToken(parsed)) {
    clear()
    return null
  }
  if (Date.now() - parsed.last_used_at > IDLE_TTL_MS) {
    clear()
    return null
  }
  return parsed
}

/** Slide the idle window: stamp `last_used_at = now` on a successful attach. */
export function touch(): void {
  let raw: string | null
  try {
    raw = localStorage.getItem(STORAGE_KEY)
  } catch {
    return
  }
  if (!raw) return
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return
  }
  if (!isToken(parsed)) return
  parsed.last_used_at = Date.now()
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(parsed))
  } catch {
    // ignore
  }
}

/** Remove any stored token (auth failed / host rejected / user disconnected). */
export function clear(): void {
  try {
    localStorage.removeItem(STORAGE_KEY)
  } catch {
    // ignore
  }
}
