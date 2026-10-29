// Files dropped into a remote-desktop window come from THIS machine, but every
// path the UI hands to a command must be a path on the controlled Devdy. So in
// remote mode a drop is first copied over (src-tauri/src/remote/upload.rs) and
// the UI carries on with the copies' remote paths. Outside remote mode the
// paths are returned untouched.
//
// Reading the dropped files uses the REAL local invoke: in a remote window the
// app's `invoke` goes to the controlled Devdy.
import { invoke as localInvoke } from '@tauri-real/core'
import { invoke } from '@/lib/tauri'
import { i18n } from '@/i18n'
import { useToast } from '@/composables/useToast'
import { isRemoteMode } from '@/lib/ipc/remote'
import { progressPercent, uploadFiles, type LocalFileStat, type UploadProgress } from '@/lib/remoteUploadCore'

function t(key: string, named: Record<string, unknown>): string {
  return i18n.global.t(key, named)
}

/**
 * The paths the UI should use for files dropped from this machine: the same
 * paths normally, copies on the controlled Devdy in a remote-desktop window
 * (progress and failures are toasted; failed files and folders are left out).
 */
export async function remotePathsForDrop(localPaths: string[]): Promise<string[]> {
  if (!isRemoteMode() || localPaths.length === 0) return localPaths

  const { toast, dismiss, update } = useToast()
  let toastId: number | null = null
  const progressText = (p: UploadProgress) =>
    p.count > 1
      ? t('remote.desktop.upload.progressMany', { count: p.count, percent: progressPercent(p) })
      : t('remote.desktop.upload.progress', { name: p.name, percent: progressPercent(p) })

  const out = await uploadFiles(localPaths, {
    stat: (paths) => localInvoke<LocalFileStat[]>('stat_local_files', { paths }),
    read: (path, offset, len) => localInvoke<string>('read_local_file_chunk', { path, offset, len }),
    begin: (name, size) => invoke<{ upload_id: string; path: string }>('remote_upload_begin', { name, size }),
    chunk: (uploadId, offset, dataBase64) =>
      invoke<number>('remote_upload_chunk', { uploadId, offset, dataBase64 }),
    finish: (uploadId) => invoke<string>('remote_upload_finish', { uploadId }),
    abort: (uploadId) => invoke<void>('remote_upload_abort', { uploadId }),
    onProgress: (p) => {
      if (toastId === null) toastId = toast.info(progressText(p), 0)
      else update(toastId, progressText(p))
    },
  }).finally(() => {
    if (toastId !== null) dismiss(toastId)
  })

  for (const name of out.folders) toast.error(t('remote.desktop.upload.folderSkipped', { name }))
  for (const f of out.failed) toast.error(t('remote.desktop.upload.failed', { name: f.name, error: f.error }))
  return out.paths
}
