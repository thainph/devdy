// Pure helpers of the secret notes vault (wrappers in lib/secretVault.ts, backend in
// src-tauri/src/secret_vault.rs). Kept free of Tauri imports so node:test can load them.

export interface VaultStatus {
  exists: boolean
  unlocked: boolean
  note_count: number
  /** Notes of an older vault (sync / import), mergeable with that vault's password. */
  legacy_count: number
  /** Notes whose vault has not reached this device yet. */
  pending_count: number
  has_recovery: boolean
  auto_lock_secs: number
}

/** A secret note as listed: never its content (`secretNoteReveal` / `secretNoteCopy`). */
export interface SecretNoteMeta {
  id: string
  title: string
  project_id: string | null
  position: number
  created_at: string
  updated_at: string
}

/** Same minimum as the backend (`MIN_PASSWORD_LEN`). */
export const MIN_VAULT_PASSWORD = 8
/** Same as the backend (`MAX_CONTENT_LEN`, bytes). */
export const MAX_SECRET_CONTENT = 64 * 1024
/** A revealed content hides itself after this long. */
export const REVEAL_SECS = 60

/** Error codes the backend returns bare (or `vault_throttled:<secs>`). */
export const VAULT_ERROR_CODES = [
  'vault_locked', 'vault_missing', 'vault_exists', 'wrong_password', 'wrong_recovery_key',
  'weak_password', 'vault_throttled', 'note_not_found', 'too_large', 'vault_corrupt',
  'remote_denied', 'clipboard_failed', 'clipboard_unsupported',
] as const
export type VaultErrorCode = (typeof VAULT_ERROR_CODES)[number]

export interface VaultError {
  code: VaultErrorCode | null
  /** Seconds to wait, for `vault_throttled`. */
  wait: number | null
}

export function vaultError(e: unknown): VaultError {
  const text = String(e instanceof Error ? e.message : e).trim()
  const [head, tail] = text.split(':', 2)
  const code = (VAULT_ERROR_CODES as readonly string[]).includes(head) ? (head as VaultErrorCode) : null
  const wait = code === 'vault_throttled' && tail !== undefined && /^\d+$/.test(tail.trim()) ? Number(tail.trim()) : null
  return { code, wait }
}

/** Why a new vault password is refused, or null. */
export function passwordProblem(password: string, confirm: string): 'short' | 'mismatch' | null {
  if ([...password].length < MIN_VAULT_PASSWORD) return 'short'
  if (password !== confirm) return 'mismatch'
  return null
}

/** UTF-8 size, to refuse an over-long content before sending it. */
export function contentBytes(text: string): number {
  return new TextEncoder().encode(text).length
}

/** Top first (backend order), filtered by project ('' = all, NO_PROJECT = none) and title. */
export function filterSecretNotes(
  notes: SecretNoteMeta[],
  query: string,
  project: string,
  noProject: string,
): SecretNoteMeta[] {
  const q = query.trim().toLowerCase()
  return notes.filter((n) => {
    if (project === noProject ? !!n.project_id : project && n.project_id !== project) return false
    return !q || n.title.toLowerCase().includes(q)
  })
}
