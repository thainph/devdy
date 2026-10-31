// Opens THE Secret notes window (views/SecretNotesWindow.vue, `?secretNotesWindow=1
// [&projectId=][&id=]`): the vault of secret notes, laid out like the Notes / Todos
// windows. There is only ever ONE such window (fixed label); opening it again
// focuses it and re-points it (project scope and / or the note to select).
//
// Local only: the vault commands (src-tauri/src/commands/secret_vault.rs) refuse
// every other webview, so a remote-desktop window never opens it.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { emit } from '@tauri-apps/api/event'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'
import { isRemoteMode } from '@/lib/ipc/remote'

/** Window label; the backend allows the vault commands from this window only. */
export const SECRET_NOTES_WINDOW_LABEL = 'secret-notes'

/** Sent to the already-open window: re-point it. */
export const SECRET_NOTES_WINDOW_SELECT = 'secret-notes-window:select'

export interface SecretNotesWindowSelect {
  /** undefined = keep the current project scope. */
  projectId?: string | null
  /** Secret note to select. */
  id?: string | null
}

export interface OpenSecretNotesWindowOptions {
  /** Project to pre-filter the sidebar with (and default "+ New" to). */
  projectId?: string | null
  /** Secret note to select. */
  id?: string | null
}

/** Secret notes exist on this machine only: no window when driving another Devdy. */
export function canOpenSecretNotes(): boolean {
  return !isRemoteMode()
}

export async function openSecretNotesWindow(
  opts: OpenSecretNotesWindowOptions = {},
): Promise<WebviewWindow | null> {
  if (!canOpenSecretNotes()) return null
  const projectId = opts.projectId ?? null

  const existing = await WebviewWindow.getByLabel(SECRET_NOTES_WINDOW_LABEL)
  if (existing) {
    try {
      await existing.unminimize()
      await existing.show()
      await existing.setFocus()
      await emit(SECRET_NOTES_WINDOW_SELECT, {
        projectId: opts.id ? undefined : projectId,
        id: opts.id ?? null,
      } satisfies SecretNotesWindowSelect)
    } catch {
      /* window may be mid-teardown */
    }
    return existing
  }

  const params = new URLSearchParams({ secretNotesWindow: '1' })
  if (projectId) params.set('projectId', projectId)
  if (opts.id) params.set('id', opts.id)

  const width = 1000
  const height = 720
  const win = new WebviewWindow(SECRET_NOTES_WINDOW_LABEL, {
    url: `index.html?${params.toString()}`,
    title: 'Secret notes — Devdy',
    width,
    height,
    minWidth: 760,
    minHeight: 480,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[secretNotesWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
