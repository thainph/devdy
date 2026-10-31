// Tauri wiring of the File Transfer window (views/FileTransferWindow.vue, logic
// in ./fileTransferCore.ts): the two file systems it shows and the copy I/O of
// both directions.
//
// THIS machine (A) is reached with the REAL local invoke (`local_fs_*`, refused
// to the bridge); the controlled Devdy (B) with the app's `invoke`, which a
// remote window routes over the relay (`remote_fs_*`, `remote_upload_*`, held
// to B's remote-desktop file policy). See src-tauri/src/remote/transfer.rs.
import { invoke as localInvoke } from '@tauri-real/core'
import { invoke } from '@/lib/tauri'
import type { FsEntry, FsListing } from '@/lib/ipc/nativeCore'
import type { LocalFileStat } from '@/lib/remoteUploadCore'
import { joinRel, safeRel, type CopyIo, type Direction, type Walk } from '@/lib/fileTransferCore'

export type Side = 'local' | 'remote'

/** One machine's file system as a pane uses it. */
export interface PaneFs {
  side: Side
  home: () => Promise<string>
  list: (path: string) => Promise<FsListing>
  /** Create the folder `rel` (`/`-separated) inside `root`. */
  mkdir: (root: string, rel: string) => Promise<string>
  rename: (path: string, newName: string) => Promise<string>
  remove: (path: string) => Promise<void>
  walk: (path: string) => Promise<Walk>
}

export const localPaneFs: PaneFs = {
  side: 'local',
  home: () => localInvoke<string>('local_fs_home'),
  list: (path) => localInvoke<FsListing>('local_fs_list', { path }),
  // Never outside `root`: checked again by the backend (names may come from B).
  mkdir: (root, rel) => localInvoke<string>('local_fs_mkdir', { root, rel }),
  rename: (path, newName) => localInvoke<string>('local_fs_rename', { path, newName }),
  remove: (path) => localInvoke<void>('local_fs_delete', { path }),
  walk: (path) => localInvoke<Walk>('local_fs_walk', { path }),
}

export const remotePaneFs: PaneFs = {
  side: 'remote',
  home: () => invoke<string>('remote_fs_home'),
  list: (path) => invoke<FsListing>('remote_fs_list', { path }),
  mkdir: (root, rel) => invoke<string>('remote_fs_mkdir', { path: joinRel(root, safeRel(rel)) }),
  rename: (path, newName) => invoke<string>('remote_fs_rename', { path, newName }),
  remove: (path) => invoke<void>('remote_fs_delete', { path }),
  walk: (path) => invoke<Walk>('remote_fs_walk', { path }),
}

export function paneFs(side: Side): PaneFs {
  return side === 'local' ? localPaneFs : remotePaneFs
}

/** The side a direction writes to. */
export function destSide(direction: Direction): Side {
  return direction === 'up' ? 'remote' : 'local'
}

/** Copy I/O of both directions. */
export const transferIo: Record<Direction, CopyIo> = {
  // This machine → controlled machine.
  up: {
    begin: async ({ destDir, name, size, conflict, mode }) =>
      (await invoke<{ upload_id: string }>('remote_upload_begin', { name, size, destDir, conflict, mode })).upload_id,
    read: (src, offset, len) => localInvoke<string>('read_local_file_chunk', { path: src, offset, len }),
    write: (id, offset, dataBase64) => invoke<number>('remote_upload_chunk', { uploadId: id, offset, dataBase64 }),
    hash: (src) => localInvoke<string>('local_fs_hash', { path: src }),
    finish: (id, sha256) => invoke<string>('remote_upload_finish', { uploadId: id, sha256 }),
    abort: (id) => invoke<void>('remote_upload_abort', { uploadId: id }),
  },
  // Controlled machine → this machine.
  down: {
    // The folder the user picked + a relative path: the backend refuses a
    // destination outside it, whatever names B sent.
    begin: async ({ destRoot, destRel, name, size, conflict, mode }) =>
      (
        await localInvoke<{ upload_id: string }>('local_fs_write_begin', {
          root: destRoot,
          rel: destRel,
          name,
          size,
          conflict,
          mode,
        })
      ).upload_id,
    read: async (src, offset, len) =>
      (await invoke<{ data_base64: string }>('remote_fs_read', { path: src, offset, len })).data_base64,
    write: (id, offset, dataBase64) => localInvoke<number>('local_fs_write_chunk', { uploadId: id, offset, dataBase64 }),
    hash: (src) => invoke<string>('remote_fs_hash', { path: src }),
    finish: (id, sha256) => localInvoke<string>('local_fs_write_finish', { uploadId: id, sha256 }),
    abort: (id) => localInvoke<void>('local_fs_write_abort', { uploadId: id }),
  },
}

/** Files dropped from the OS onto the window (paths on this machine) as entries. */
export async function droppedEntries(paths: string[]): Promise<{ entries: FsEntry[]; failed: LocalFileStat[] }> {
  const stats = await localInvoke<LocalFileStat[]>('stat_local_files', { paths })
  return {
    entries: stats
      .filter((s) => !s.error)
      .map((s) => ({ name: s.name, path: s.path, is_dir: s.is_dir, size: s.size, modified: null, mode: s.mode })),
    failed: stats.filter((s) => !!s.error),
  }
}
