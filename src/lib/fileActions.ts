// Everything you can DO to a file or folder, in one place.
//
// The same set of actions is offered from the explorer's right-click menu and
// from the file window's ⋯ menu (see components/FileActionsMenu.vue). They used
// to be two hand-written lists that had drifted apart — the viewer could copy
// content and open in the default app, the tree could rename, delete, duplicate
// and paste, and neither could do what the other could.
//
// Two of these actions belong to the MAIN window and are therefore bridged by a
// Tauri event, because the file window is a separate webview with its own store
// copies: mentioning a file (the composer lives in the run screen) and comparing
// images (the compare host is mounted in the main window only).
//
// The cut/copy clipboard is likewise shared across webviews: copying in the file
// window and pasting in the explorer must work, so every change is broadcast and
// mirrored into each webview's copy.
import { ref } from 'vue'
import { emit, listen } from '@tauri-apps/api/event'
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener'
import { invoke } from '@/lib/tauri'
import { useToast } from '@/composables/useToast'
import { useConfirm } from '@/composables/useConfirm'
import { useFileTreeStore } from '@/stores/fileTree'
import { useRunsStore } from '@/stores/runs'
import { i18n } from '@/i18n'
import {
  FILE_CLIPBOARD_EVENT,
  FILE_COMPARE_EVENT,
  FILE_MENTION_EVENT,
} from '@/lib/fileEvents'

/** What a menu acts on. `path` is project-relative, as everywhere else. */
export interface FileTarget {
  path: string
  name: string
  isDir: boolean
}

export interface FileClip {
  path: string
  name: string
  mode: 'copy' | 'cut'
}

const clipboard = ref<FileClip | null>(null)
let clipboardBound = false

/** Mirror clipboard changes made in other webviews into this one. */
function bindClipboard() {
  if (clipboardBound) return
  clipboardBound = true
  listen<FileClip | null>(FILE_CLIPBOARD_EVENT, (e) => {
    clipboard.value = e.payload ?? null
  }).catch(() => {
    /* running outside the Tauri shell */
  })
}

function setClipboard(clip: FileClip | null) {
  clipboard.value = clip
  emit(FILE_CLIPBOARD_EVENT, clip).catch(() => { /* event bus unavailable */ })
}

export function parentDir(relPath: string): string {
  const i = relPath.lastIndexOf('/')
  return i === -1 ? '' : relPath.slice(0, i)
}

export function absPath(projectPath: string, relPath: string): string {
  const root = projectPath.replace(/\/+$/, '')
  return relPath ? `${root}/${relPath}` : root
}

export function useFileActions() {
  const { toast } = useToast()
  const { confirm } = useConfirm()
  const store = useFileTreeStore()
  const runs = useRunsStore()
  const t = i18n.global.t

  bindClipboard()

  async function copyText(text: string, okKey: string) {
    try {
      await navigator.clipboard.writeText(text)
      toast.success(t(okKey))
    } catch {
      toast.error(t('files.tree.toastFailedCopyPath'))
    }
  }

  function copyAbsPath(projectPath: string, target: FileTarget) {
    return copyText(absPath(projectPath, target.path), 'files.tree.toastPathCopied')
  }

  function copyRelPath(target: FileTarget) {
    return copyText(target.path, 'files.tree.toastRelativePathCopied')
  }

  /** Copy the file's text. Binary / unreadable files simply report the error. */
  async function copyContent(projectPath: string, target: FileTarget) {
    if (target.isDir) return
    try {
      const res = await runs.readProjectFile(projectPath, target.path)
      await navigator.clipboard.writeText(res.content ?? '')
      toast.success(t('files.tree.toastContentCopied'))
    } catch (e) {
      toast.error(String(e))
    }
  }

  function revealInFolder(projectPath: string, target: FileTarget) {
    const abs = absPath(projectPath, target.path)
    // Folders open themselves; a file reveals itself inside its folder.
    const p = target.isDir ? openPath(abs) : revealItemInDir(abs)
    return p.catch(() => { /* opener unavailable */ })
  }

  function openInDefaultApp(projectPath: string, target: FileTarget) {
    return openPath(absPath(projectPath, target.path)).catch(() => { /* opener unavailable */ })
  }

  async function openInVscode(projectPath: string, target: FileTarget) {
    const abs = absPath(projectPath, target.path)
    try {
      if (target.isDir) await invoke('open_in_vscode', { path: abs, file: null })
      else await invoke('open_in_vscode', { path: projectPath, file: abs })
    } catch (e) {
      toast.error(String(e))
    }
  }

  async function openInChrome(projectPath: string, target: FileTarget) {
    try {
      await invoke('open_in_chrome', { path: absPath(projectPath, target.path) })
    } catch (e) {
      toast.error(String(e))
    }
  }

  /** Hand the path to the run composer (main window). */
  function mentionInChat(projectPath: string, target: FileTarget) {
    emit(FILE_MENTION_EVENT, {
      projectPath,
      path: `${target.path}${target.isDir ? '/' : ''}`,
    }).catch(() => { /* event bus unavailable */ })
  }

  /**
   * Feed the image to the compare host (main window), which decides whether this
   * starts a comparison or completes the one already waiting — only it knows
   * what is pending.
   */
  function compareImage(projectPath: string, target: FileTarget) {
    emit(FILE_COMPARE_EVENT, { projectPath, path: target.path }).catch(() => {
      /* event bus unavailable */
    })
  }

  function cut(target: FileTarget) {
    setClipboard({ path: target.path, name: target.name, mode: 'cut' })
  }

  function copy(target: FileTarget) {
    setClipboard({ path: target.path, name: target.name, mode: 'copy' })
  }

  /** Paste into `target`'s folder (or into it, when it IS a folder). */
  async function paste(projectPath: string, target: FileTarget | null) {
    const clip = clipboard.value
    if (!clip) return
    const dir = !target ? '' : target.isDir ? target.path : parentDir(target.path)
    try {
      if (clip.mode === 'copy') await store.copyInto(projectPath, clip.path, dir)
      else {
        await store.moveInto(projectPath, clip.path, dir)
        setClipboard(null)
      }
      toast.success(t('files.tree.toastPasted'))
    } catch (e) {
      toast.error(String(e))
    }
  }

  async function duplicate(projectPath: string, target: FileTarget) {
    try {
      await store.duplicate(projectPath, target.path)
      toast.success(t('files.tree.toastDuplicated'))
    } catch (e) {
      toast.error(String(e))
    }
  }

  /** Delete to trash, after confirming. Returns true when it actually went. */
  async function remove(projectPath: string, target: FileTarget): Promise<boolean> {
    const ok = await confirm({
      title: target.isDir ? t('files.tree.deleteFolder') : t('files.tree.deleteFile'),
      message: t('files.tree.moveToTrash', { name: target.name }),
      confirmLabel: t('common.delete'),
      variant: 'destructive',
    })
    if (!ok) return false
    try {
      await store.remove(projectPath, target.path)
      toast.success(t('files.tree.toastMovedToTrash'))
      return true
    } catch (e) {
      toast.error(String(e))
      return false
    }
  }

  return {
    clipboard,
    copyAbsPath,
    copyRelPath,
    copyContent,
    revealInFolder,
    openInDefaultApp,
    openInVscode,
    openInChrome,
    mentionInChat,
    compareImage,
    cut,
    copy,
    paste,
    duplicate,
    remove,
  }
}
