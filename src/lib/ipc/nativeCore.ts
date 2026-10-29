/**
 * Pure helpers of the remote-desktop native shim (./native.ts) and its file
 * browser, free of Vue, Tauri and the transport so they can be unit-tested in
 * plain Node. Paths here are the controlled machine's, which may use either
 * separator.
 */

/** Most bytes one `remote_fs_read` returns (mirrors `fs_bridge::MAX_READ`). */
export const READ_CHUNK = 4 * 1024 * 1024
/** Largest file served whole (no Range) to a preview: it is held in memory. */
export const MAX_WHOLE_FILE = 256 * 1024 * 1024

/** A file-system entry of the controlled machine (`remote_fs_list` / `remote_fs_stat`). */
export interface FsEntry {
  name: string
  path: string
  is_dir: boolean
  size: number
  /** Unix milliseconds. */
  modified: number | null
}

/** A directory listing (`remote_fs_list`); `truncated` when the directory had
 * more entries than the controlled machine returns in one listing. */
export interface FsListing {
  entries: FsEntry[]
  truncated: boolean
}

/** A `plugin:dialog` filter. */
export interface DialogFilter {
  name: string
  extensions: string[]
}

/** A `Range` request as parsed by the `rdfile` protocol (see `file_proxy::ByteRange`). */
export interface ByteRange {
  /** First byte; `null` for a suffix range, then `end` is the suffix length. */
  start: number | null
  end: number | null
}

export function separatorOf(path: string): '/' | '\\' {
  return path.includes('\\') && !path.includes('/') ? '\\' : '/'
}

/** The last path component (`''` for a root). */
export function baseName(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean)
  return parts.length ? parts[parts.length - 1] : ''
}

/** The containing directory, or `null` for a root (`/`, `C:\`). */
export function parentDir(path: string): string | null {
  const sep = separatorOf(path)
  const trimmed = path.length > 1 ? path.replace(/[\\/]+$/, '') : path
  if (trimmed === '' || trimmed === '/' || /^[A-Za-z]:$/.test(trimmed)) return null
  const i = Math.max(trimmed.lastIndexOf('/'), trimmed.lastIndexOf('\\'))
  if (i < 0) return null
  const head = trimmed.slice(0, i)
  if (head === '') return sep === '/' ? '/' : null
  return /^[A-Za-z]:$/.test(head) ? head + '\\' : head
}

export function joinPath(dir: string, name: string): string {
  const sep = separatorOf(dir)
  return dir.endsWith('/') || dir.endsWith('\\') ? dir + name : dir + sep + name
}

/** Whether a file name passes a filter (`*` or an empty list allow everything). */
export function matchesFilter(name: string, filter: DialogFilter | null): boolean {
  if (!filter || filter.extensions.length === 0) return true
  const lower = name.toLowerCase()
  return filter.extensions.some((ext) => {
    const e = ext.replace(/^\*?\.?/, '').toLowerCase()
    return e === '' || e === '*' || lower.endsWith('.' + e)
  })
}

/**
 * The first byte and length to read for a Range request, or `null` when the
 * range cannot be satisfied. Capped at one read: media players ask again for
 * the rest, which a shorter 206 allows.
 */
export function resolveRange(range: ByteRange, total: number): { start: number; len: number } | null {
  let start: number
  let last: number
  if (range.start === null) {
    const suffix = Math.min(range.end ?? 0, total)
    if (suffix <= 0) return null
    start = total - suffix
    last = total - 1
  } else {
    start = range.start
    last = range.end === null ? total - 1 : Math.min(range.end, total - 1)
  }
  if (start >= total || last < start) return null
  return { start, len: Math.min(last - start + 1, READ_CHUNK) }
}

/** The HTTP status for a failed read, from the controlled side's error text. */
export function errorStatus(err: unknown): number {
  const msg = err instanceof Error ? err.message : String(err)
  if (msg.startsWith('not found')) return 404
  if (msg.startsWith('permission denied')) return 403
  if (msg.startsWith('is a directory')) return 400
  return 502
}

/** Offsets of the reads that fetch `[from, total)` in {@link READ_CHUNK}s. */
export function chunkOffsets(from: number, total: number): number[] {
  const out: number[] = []
  for (let o = from; o < total; o += READ_CHUNK) out.push(o)
  return out
}

export function decodeBase64(b64: string): Uint8Array {
  const native = (Uint8Array as unknown as { fromBase64?: (s: string) => Uint8Array }).fromBase64
  if (native) return native(b64)
  const bin = atob(b64)
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/**
 * Where a dialog starts from its `defaultPath`: a directory opens as is; a
 * file (or a path that does not exist yet) opens its parent with the name
 * pre-filled. `stat` is `null` when the path does not exist.
 */
export function dialogStart(
  defaultPath: string | undefined,
  stat: FsEntry | null,
): { dir: string | null; fileName: string } {
  if (!defaultPath) return { dir: null, fileName: '' }
  if (stat?.is_dir) return { dir: defaultPath, fileName: '' }
  const looksRelative = !/^([\\/]|[A-Za-z]:)/.test(defaultPath)
  if (looksRelative) return { dir: null, fileName: baseName(defaultPath) }
  return { dir: parentDir(defaultPath), fileName: baseName(defaultPath) }
}

/** A save dialog's file name with the active filter's first extension appended when missing. */
export function withFilterExtension(name: string, filter: DialogFilter | null): string {
  if (!filter || matchesFilter(name, filter)) return name
  const ext = filter.extensions.find((e) => e && e !== '*')
  return ext ? `${name}.${ext.replace(/^\*?\./, '')}` : name
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const units = ['KB', 'MB', 'GB', 'TB']
  let v = bytes / 1024
  let i = 0
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024
    i++
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${units[i]}`
}
