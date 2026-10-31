// Typed wrappers for the secret notes vault commands (src-tauri/src/commands/secret_vault.rs).
// Local windows only: the commands refuse the Remote Control bridge, so hide the UI in remote mode.
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { invoke } from '@/lib/tauri'
import type { SecretNoteMeta, VaultStatus } from '@/lib/secretVaultCore'

export * from '@/lib/secretVaultCore'

/** Vault locked / unlocked / notes changed (no payload, no note data). */
export const SECRET_VAULT_EVENT = 'secret-vault://changed'

export const vaultStatus = () => invoke<VaultStatus>('secret_vault_status')
/** Creates the vault (unlocked); resolves to the recovery key, to show once. */
export const vaultSetup = (password: string) => invoke<string>('secret_vault_setup', { password })
export const vaultUnlock = (password: string) => invoke<void>('secret_vault_unlock', { password })
export const vaultUnlockRecovery = (recoveryKey: string) =>
  invoke<void>('secret_vault_unlock_recovery', { recoveryKey })
export const vaultLock = () => invoke<void>('secret_vault_lock')
/** Typing in the editor: push the idle auto-lock back (`vault_locked` once it locked). */
export const vaultTouch = () => invoke<void>('secret_vault_touch')
/**
 * New password: rotates to a new vault (old password / recovery key open nothing current) and
 * resolves to the new recovery key, to show once. `currentPassword` is ignored right after a
 * recovery-key unlock.
 */
export const vaultChangePassword = (currentPassword: string | null, password: string) =>
  invoke<string>('secret_vault_change_password', { currentPassword, password })
/** Merges the notes of older vaults this password (or recovery key) opens; resolves to how many moved. */
export const vaultMergeOld = (password: string) => invoke<number>('secret_vault_merge_old', { password })

export const secretNotesList = () => invoke<SecretNoteMeta[]>('secret_notes_list')
export const secretNoteReveal = (id: string) => invoke<string>('secret_note_reveal', { id })
/** Copies in the backend (content never reaches here); resolves to the seconds until it is cleared. */
export const secretNoteCopy = (id: string) => invoke<number>('secret_note_copy', { id })
export const secretNoteCreate = (title: string, content: string, projectId: string | null) =>
  invoke<SecretNoteMeta>('secret_note_create', { title, content, projectId })
export const secretNoteUpdate = (id: string, title: string, content: string) =>
  invoke<SecretNoteMeta>('secret_note_update', { id, title, content })
export const secretNoteSetProject = (id: string, projectId: string | null) =>
  invoke<void>('secret_note_set_project', { id, projectId })
export const secretNoteDelete = (id: string) => invoke<void>('secret_note_delete', { id })
export const secretNotesReorder = (ids: string[]) => invoke<void>('secret_notes_reorder', { ids })
/** Moves a regular note into the vault (the note row and its session links are deleted). */
export const secretNoteFromNote = (noteId: string) => invoke<SecretNoteMeta>('secret_note_from_note', { noteId })

export function onVaultChanged(cb: () => void): Promise<UnlistenFn> {
  return listen(SECRET_VAULT_EVENT, () => cb())
}
