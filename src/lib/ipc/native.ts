/**
 * Native features of a remote-desktop window, done the controlled machine's way.
 *
 * Plugin commands and `convertFileSrc` stay on this machine (./core.ts), but in
 * a remote-desktop window every path the UI holds belongs to the controlled
 * Devdy. Rule: behave as the user sitting there would see it, rendered here.
 *
 * - `plugin:dialog|open` / `save` → an in-app browser of the controlled
 *   machine (RemoteFileDialog.vue) that resolves exactly like the plugin:
 *   a path, an array of paths (`multiple`), or `null` on cancel.
 * - `convertFileSrc(path)` → an `rdfile:` URL (same shape as `asset:`). This
 *   machine's backend serves that scheme by asking this page for the bytes
 *   (`rdfile://request`), which we fetch over the relay with `remote_fs_read`
 *   (see `src-tauri/src/remote/file_proxy.rs`). A real protocol rather than
 *   blob URLs because `convertFileSrc` is synchronous and its URLs are used by
 *   `<img>`, `<video>` (Range), `<iframe>` (relative sub-resources) and pdf.js
 *   `fetch` alike — none of the call sites had to change.
 * - `plugin:opener|open_path` on a file → downloaded into this machine's
 *   Downloads folder, then opened here; on a folder → the in-app browser.
 *   `reveal_item_in_dir` → the in-app browser at the item. URLs stay local.
 *
 * Everything that must run on THIS machine goes through the real `invoke` /
 * `listen` (`@tauri-real/*`); the controlled machine is reached through
 * {@link remoteIpc}.
 */
import { convertFileSrc as realConvertFileSrc, invoke as realInvoke } from '@tauri-real/core'
import { listen as realListen } from '@tauri-real/event'
import { remoteIpc } from './remote'
import {
  baseName,
  chunkOffsets,
  decodeBase64,
  dialogStart,
  errorStatus,
  MAX_WHOLE_FILE,
  READ_CHUNK,
  resolveRange,
  type ByteRange,
  type DialogFilter,
  type FsEntry,
} from './nativeCore'

// ── The controlled machine's file system ────────────────────────────────────

function remote() {
  const r = remoteIpc()
  if (!r) throw new Error('remote_closed')
  return r
}

export const remoteFs = {
  list: (path: string) => remote().invoke<FsEntry[]>('remote_fs_list', { path }),
  stat: (path: string) => remote().invoke<FsEntry>('remote_fs_stat', { path }),
  home: () => remote().invoke<string>('remote_fs_home'),
  async read(path: string, offset: number, len: number): Promise<{ bytes: Uint8Array; total: number }> {
    const r = await remote().invoke<{ data_base64: string; total_size: number }>('remote_fs_read', { path, offset, len })
    return { bytes: decodeBase64(r.data_base64), total: r.total_size }
  },
  /** `stat`, or `null` when the path does not exist / cannot be read. */
  async tryStat(path: string): Promise<FsEntry | null> {
    try {
      return await this.stat(path)
    } catch {
      return null
    }
  },
}

/** Read `[from, total)` of a file into `out` (a few chunks in flight at once). */
async function readInto(path: string, out: Uint8Array, from: number): Promise<void> {
  const offsets = chunkOffsets(from, out.length)
  let next = 0
  const worker = async (): Promise<void> => {
    while (next < offsets.length) {
      const offset = offsets[next++]
      const { bytes } = await remoteFs.read(path, offset, READ_CHUNK)
      out.set(bytes.subarray(0, out.length - offset), offset)
    }
  }
  await Promise.all([worker(), worker(), worker()])
}

// ── convertFileSrc: the `rdfile` protocol ───────────────────────────────────

interface FileRequest {
  id: string
  path: string
  range: ByteRange | null
}

const RDFILE_SCHEME = 'rdfile'
let fileServer: Promise<void> | null = null
const serving = new Set<string>()

function respond(id: string, status: number, start = 0, total = 0, body: Uint8Array = new Uint8Array()): Promise<void> {
  return realInvoke<void>('rdfile_respond', body, {
    headers: { 'x-rd-id': id, 'x-rd-status': String(status), 'x-rd-start': String(start), 'x-rd-total': String(total) },
  })
}

async function serve(req: FileRequest): Promise<void> {
  if (serving.has(req.id)) return
  serving.add(req.id)
  try {
    if (req.range) {
      // A suffix range needs the size first; otherwise the read reports it.
      const size = req.range.start === null ? (await remoteFs.stat(req.path)).size : null
      const first = resolveRange(req.range, size ?? Number.MAX_SAFE_INTEGER)
      if (!first) return await respond(req.id, 416, 0, size ?? 0)
      const { bytes, total } = await remoteFs.read(req.path, first.start, first.len)
      const win = resolveRange(req.range, total)
      if (!win) return await respond(req.id, 416, 0, total)
      return await respond(req.id, 206, win.start, total, bytes.subarray(0, win.len))
    }
    const head = await remoteFs.read(req.path, 0, READ_CHUNK)
    if (head.total > MAX_WHOLE_FILE) return await respond(req.id, 413, 0, head.total)
    const out = new Uint8Array(head.total)
    out.set(head.bytes.subarray(0, head.total))
    if (head.bytes.length < head.total) await readInto(req.path, out, head.bytes.length)
    await respond(req.id, 200, 0, head.total, out)
  } catch (e) {
    await respond(req.id, errorStatus(e)).catch(() => {})
  } finally {
    serving.delete(req.id)
  }
}

/** Start answering this page's `rdfile:` requests (once). */
function ensureFileServer(): void {
  fileServer ??= (async () => {
    await realListen<FileRequest>('rdfile://request', (e) => void serve(e.payload))
    // Requests made before we listened (or left by a previous page load).
    const waiting = await realInvoke<FileRequest[]>('rdfile_register')
    for (const req of waiting) void serve(req)
  })().catch((e) => {
    fileServer = null
    console.error('[remote-native] file server failed', e)
  })
}

/** `convertFileSrc` for a path on the controlled machine. */
export function remoteFileSrc(path: string): string {
  ensureFileServer()
  return realConvertFileSrc(path, RDFILE_SCHEME)
}

// ── The in-app file browser ─────────────────────────────────────────────────

export interface FileDialogRequest {
  mode: 'open' | 'save' | 'browse'
  title?: string
  directory?: boolean
  multiple?: boolean
  filters?: DialogFilter[]
  /** Where to start; the home folder when unset. */
  startDir?: string | null
  /** Pre-filled save name / entry to select. */
  fileName?: string
}

/** Show the browser; resolves with the chosen paths, `null` on cancel. */
export async function showRemoteFileDialog(req: FileDialogRequest): Promise<string[] | null> {
  const [{ createApp, h }, { default: RemoteFileDialog }, { i18n }] = await Promise.all([
    import('vue'),
    import('@/components/remote/RemoteFileDialog.vue'),
    import('@/i18n'),
  ])
  return new Promise((resolve) => {
    const host = document.createElement('div')
    document.body.appendChild(host)
    const app = createApp({
      render: () =>
        h(RemoteFileDialog, {
          ...req,
          onDone: (paths: string[] | null) => {
            app.unmount()
            host.remove()
            resolve(paths)
          },
        }),
    })
    app.use(i18n)
    app.mount(host)
  })
}

interface OpenDialogOptions {
  title?: string
  filters?: DialogFilter[]
  defaultPath?: string
  multiple?: boolean
  directory?: boolean
}

async function dialogOpen(options: OpenDialogOptions = {}): Promise<string | string[] | null> {
  const start = dialogStart(options.defaultPath, options.defaultPath ? await remoteFs.tryStat(options.defaultPath) : null)
  const paths = await showRemoteFileDialog({
    mode: 'open',
    title: options.title,
    directory: !!options.directory,
    multiple: !!options.multiple,
    filters: options.filters,
    startDir: start.dir,
    fileName: start.fileName,
  })
  if (!paths || paths.length === 0) return null
  return options.multiple ? paths : paths[0]
}

async function dialogSave(options: OpenDialogOptions = {}): Promise<string | null> {
  const start = dialogStart(options.defaultPath, options.defaultPath ? await remoteFs.tryStat(options.defaultPath) : null)
  const paths = await showRemoteFileDialog({
    mode: 'save',
    title: options.title,
    filters: options.filters,
    startDir: start.dir,
    fileName: start.fileName,
  })
  return paths?.[0] ?? null
}

// ── Opener: download, then open locally ─────────────────────────────────────

/** Remote file (path + size + mtime) → its copy in this machine's Downloads. */
const downloaded = new Map<string, string>()

async function toast(variant: 'info' | 'success' | 'error', key: string, params: Record<string, unknown>, duration?: number) {
  const [{ useToast }, { i18n }] = await Promise.all([import('@/composables/useToast'), import('@/i18n')])
  useToast().toast[variant](i18n.global.t(key, params), duration)
}

/** Copy a file of the controlled machine into this machine's Downloads folder. */
export async function downloadRemoteFile(entry: FsEntry): Promise<string> {
  const key = `${entry.path}\n${entry.size}\n${entry.modified ?? ''}`
  const cached = downloaded.get(key)
  if (cached) return cached
  const name = entry.name || baseName(entry.path)
  void toast('info', 'remoteFiles.downloading', { name })
  const local = await realInvoke<string>('rd_download_create', { name })
  const headers = { 'x-rd-path': encodeURIComponent(local) }
  // Sequential: appends must land in order.
  for (const offset of chunkOffsets(0, Math.max(entry.size, 1))) {
    const { bytes } = await remoteFs.read(entry.path, offset, READ_CHUNK)
    if (bytes.length === 0) break
    await realInvoke<void>('rd_download_append', bytes, { headers })
  }
  downloaded.set(key, local)
  void toast('success', 'remoteFiles.downloaded', { path: local })
  return local
}

async function openPath(path: string, openWith?: string): Promise<void> {
  const entry = await remoteFs.stat(path)
  if (entry.is_dir) {
    await openRemoteBrowser(path)
    return
  }
  let local: string
  try {
    local = await downloadRemoteFile(entry)
  } catch (e) {
    void toast('error', 'remoteFiles.downloadFailed', { error: e instanceof Error ? e.message : String(e) }, 6000)
    throw e
  }
  await realInvoke('plugin:opener|open_path', { path: local, with: openWith })
}

/** The in-app browser at `dir` (optionally with `select` selected). */
export async function openRemoteBrowser(dir: string | null, select?: string): Promise<void> {
  await showRemoteFileDialog({ mode: 'browse', startDir: dir, fileName: select })
}

async function revealItemInDir(paths: string[]): Promise<void> {
  const first = paths[0]
  if (!first) return
  const start = dialogStart(first, null)
  await openRemoteBrowser(start.dir, start.fileName)
}

// ── The hook in ./core.ts ───────────────────────────────────────────────────

/**
 * A remote-desktop window's handling of a plugin command, or `undefined` to
 * run it locally as usual. Only called when {@link remoteIpc} is non-null.
 */
export function interceptNative(cmd: string, args: unknown): Promise<unknown> | undefined {
  const a = (args ?? {}) as Record<string, unknown>
  switch (cmd) {
    case 'plugin:dialog|open':
      return dialogOpen(a.options as OpenDialogOptions | undefined)
    case 'plugin:dialog|save':
      return dialogSave(a.options as OpenDialogOptions | undefined)
    case 'plugin:opener|open_path':
      return openPath(String(a.path), a.with as string | undefined)
    case 'plugin:opener|reveal_item_in_dir':
      return revealItemInDir((a.paths as string[] | undefined) ?? [])
    default:
      return undefined
  }
}
