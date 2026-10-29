/**
 * Password-login constants + password stretching for the Remote Control
 * controller (pure, WebCrypto only). Mirrors `src-tauri/src/remote/login.rs`:
 * the E2E PSK is `deriveEffectivePsk(LOGIN_LINK_SECRET, await stretchPassword(pw))`.
 */

/** Rendezvous code of the Host's password-login slot (public by design). */
export const LOGIN_CODE = 'devdy-login-v1'
/** Public stand-in for the link secret in the login KDF. */
export const LOGIN_LINK_SECRET =
  '64657664792072656d6f7465206c6f67696e207631207075626c6963206b6579'
const PBKDF2_SALT = 'devdy-remote-login-v1'
export const PBKDF2_ROUNDS = 210_000

/** PBKDF2-HMAC-SHA256(password) → lowercase hex (the login KDF's `secret2`). */
export async function stretchPassword(password: string): Promise<string> {
  const enc = new TextEncoder()
  const key = await crypto.subtle.importKey('raw', enc.encode(password), 'PBKDF2', false, ['deriveBits'])
  const bits = await crypto.subtle.deriveBits(
    { name: 'PBKDF2', hash: 'SHA-256', salt: enc.encode(PBKDF2_SALT), iterations: PBKDF2_ROUNDS },
    key,
    256,
  )
  return Array.from(new Uint8Array(bits), (b) => b.toString(16).padStart(2, '0')).join('')
}

/** The relay WebSocket URL served next to this controller page. */
export function sameOriginRelayUrl(loc: { protocol: string; host: string }): string {
  return `${loc.protocol === 'https:' ? 'wss' : 'ws'}://${loc.host}/ws`
}
