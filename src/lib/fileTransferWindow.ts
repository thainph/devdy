// Opens THE File Transfer window (views/FileTransferWindow.vue,
// `?fileTransferWindow=1`): copy files and folders between this machine and
// the Devdy a remote-desktop window controls. Only exists in the remote set —
// its label is `remote-file-transfer` and it talks to the controlled Devdy
// through the main remote-desktop window (lib/ipc/muxTransport.ts) — so it is
// offered only from a remote-desktop window. Opening it again focuses it.
import { WebviewWindow } from '@tauri-apps/api/webviewWindow'
import { trackWindowForTray } from '@/lib/tray'
import { centerOnCurrentMonitor } from '@/lib/windowCascade'
import { isRemoteMode } from '@/lib/ipc/remote'
import { popoutLabel, popoutUrl } from '@/lib/popout'

/** Local label (the window itself is `remote-file-transfer`, see lib/popout.ts). */
const FILE_TRANSFER_WINDOW_LABEL = 'file-transfer'

/** File transfer is between two machines: only offered while controlling another Devdy. */
export function canOpenFileTransfer(): boolean {
  return isRemoteMode()
}

// In-flight creation guard (like lib/remoteDesktopWindow.ts).
let creating: Promise<WebviewWindow> | null = null

async function focusWindow(win: WebviewWindow) {
  try {
    await win.unminimize()
    await win.show()
    await win.setFocus()
  } catch {
    /* window may be mid-teardown */
  }
}

export async function openFileTransferWindow(): Promise<WebviewWindow | null> {
  if (!canOpenFileTransfer()) return null
  if (creating) {
    const win = await creating
    await focusWindow(win)
    return win
  }
  const label = popoutLabel(FILE_TRANSFER_WINDOW_LABEL)
  const existing = await WebviewWindow.getByLabel(label)
  if (existing) {
    await focusWindow(existing)
    return existing
  }
  if (creating) return creating
  creating = createFileTransferWindow(label)
  try {
    return await creating
  } finally {
    creating = null
  }
}

async function createFileTransferWindow(label: string): Promise<WebviewWindow> {
  const width = 1100
  const height = 760
  const win = new WebviewWindow(label, {
    url: popoutUrl('index.html?fileTransferWindow=1'),
    title: 'File Transfer — Devdy',
    width,
    height,
    minWidth: 820,
    minHeight: 520,
    ...(await centerOnCurrentMonitor(width, height)),
  })
  win.once('tauri://error', (e) => {
    console.error('[fileTransferWindow] failed to open window', e)
  })
  trackWindowForTray(win)
  return win
}
