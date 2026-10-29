// Copy files of THIS machine onto the controlled Devdy, chunk by chunk (see
// lib/remoteUpload.ts for the wiring and src-tauri/src/remote/upload.rs for
// both ends). Free of Vue / Tauri so it can be unit-tested in plain Node.

/** Raw bytes per chunk (base64 in JSON stays well under the 4 MiB per call limit). */
export const UPLOAD_CHUNK_BYTES = 2 * 1024 * 1024

/** `stat_local_files` row. */
export interface LocalFileStat {
  path: string
  name: string
  size: number
  is_dir: boolean
  error: string | null
}

export interface UploadDeps {
  /** Stat the dropped paths on this machine. */
  stat: (paths: string[]) => Promise<LocalFileStat[]>
  /** Base64 of up to `len` bytes at `offset` of a local file. */
  read: (path: string, offset: number, len: number) => Promise<string>
  begin: (name: string, size: number) => Promise<{ upload_id: string; path: string }>
  /** Returns the bytes written so far. */
  chunk: (uploadId: string, offset: number, dataBase64: string) => Promise<number>
  /** Returns the file's path on the controlled Devdy. */
  finish: (uploadId: string) => Promise<string>
  abort: (uploadId: string) => Promise<void>
  onProgress?: (p: UploadProgress) => void
  chunkBytes?: number
}

export interface UploadProgress {
  /** File being copied. */
  name: string
  /** Files to copy in this batch. */
  count: number
  /** Bytes copied / to copy, over the whole batch. */
  done: number
  total: number
}

export interface UploadOutcome {
  /** Remote paths, in the order of the dropped files that made it. */
  paths: string[]
  /** Dropped folders (not copied). */
  folders: string[]
  failed: { name: string; error: string }[]
}

/** Percentage (0–100) of a progress report. */
export function progressPercent(p: UploadProgress): number {
  return p.total > 0 ? Math.min(100, Math.floor((p.done * 100) / p.total)) : 100
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** Copy every dropped file (folders are reported, not copied), one after another. */
export async function uploadFiles(localPaths: string[], deps: UploadDeps): Promise<UploadOutcome> {
  const out: UploadOutcome = { paths: [], folders: [], failed: [] }
  const chunkBytes = deps.chunkBytes ?? UPLOAD_CHUNK_BYTES
  const stats = await deps.stat(localPaths)
  const files: LocalFileStat[] = []
  for (const s of stats) {
    if (s.error) out.failed.push({ name: s.name, error: s.error })
    else if (s.is_dir) out.folders.push(s.name)
    else files.push(s)
  }
  const total = files.reduce((n, f) => n + f.size, 0)
  let base = 0
  for (const f of files) {
    const report = (done: number) =>
      deps.onProgress?.({ name: f.name, count: files.length, done: base + done, total })
    let uploadId: string | null = null
    try {
      report(0)
      const begun = await deps.begin(f.name, f.size)
      uploadId = begun.upload_id
      let offset = 0
      while (offset < f.size) {
        const data = await deps.read(f.path, offset, Math.min(chunkBytes, f.size - offset))
        if (!data) throw new Error('file changed while copying')
        offset = await deps.chunk(uploadId, offset, data)
        report(offset)
      }
      out.paths.push(await deps.finish(uploadId))
    } catch (e) {
      if (uploadId) await deps.abort(uploadId).catch(() => {})
      out.failed.push({ name: f.name, error: message(e) })
    }
    base += f.size
  }
  return out
}
