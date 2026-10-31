// File Transfer window of a remote desktop (views/FileTransferWindow.vue):
// copy files and folders between THIS machine (A, "local") and the controlled
// Devdy (B, "remote"), both ways. Planning a copy from a folder walk, conflict
// answers, path joining across separators, the chunked copy of one file and
// the queue that runs them. Free of Vue / Tauri so it can be unit-tested in
// plain Node; lib/fileTransfer.ts wires it to the commands
// (src-tauri/src/remote/transfer.rs).
import { baseName, joinPath, separatorOf, type FsEntry } from './ipc/nativeCore.ts'

/** Raw bytes per chunk (base64 in JSON stays well under the 4 MiB per call limit). */
export const TRANSFER_CHUNK = 2 * 1024 * 1024
/** Files copied at once: enough to hide a round trip, not enough to flood the relay. */
export const MAX_PARALLEL = 2
/**
 * Hard cap on chunk bytes in flight across ALL transfers (as base64 on the
 * wire). The relay buffers at most 4096 frames per connection in RAM and drops
 * the connection beyond that (frames ≤ `RELAY_MAX_MESSAGE_BYTES`, 32 MiB by
 * default), so the copy never pipelines unboundedly: a chunk reserves its bytes
 * before it is read and frees them only once the receiver has acked its write
 * (each chunk call is awaited). 16 MiB is a few chunks — per file at most one
 * crosses the relay while the next is read on the other end.
 */
export const MAX_IN_FLIGHT_BYTES = 16 * 1024 * 1024

/** `up` = this machine → controlled machine, `down` = the reverse. */
export type Direction = 'up' | 'down'
export type ConflictChoice = 'overwrite' | 'rename' | 'skip'
/** What the writing side does when the name is taken (see `upload::Conflict`). */
export type WriteConflict = 'overwrite' | 'rename' | 'fail'

/** `local_fs_walk` / `remote_fs_walk`. */
export interface WalkEntry {
  /** `/`-separated, relative to the walked folder. */
  rel: string
  is_dir: boolean
  size: number
}
export interface Walk {
  entries: WalkEntry[]
  total_bytes: number
  truncated: boolean
  skipped: number
}

// ── Paths ───────────────────────────────────────────────────────────────────

/** `dir` joined with a `/`-separated relative path, in `dir`'s own separator. */
export function joinRel(dir: string, rel: string): string {
  const parts = rel.split('/').filter(Boolean)
  if (parts.length === 0) return dir
  return joinPath(dir, parts.join(separatorOf(dir)))
}

/** The folder part of a `/`-separated relative path (`''` at the top). */
export function relParent(rel: string): string {
  const i = rel.lastIndexOf('/')
  return i < 0 ? '' : rel.slice(0, i)
}

/** `name`, then `stem (1).ext`, `stem (2).ext`… (mirrors `file_proxy::numbered_name`). */
export function numberedName(name: string, n: number): string {
  if (n === 0) return name
  const i = name.lastIndexOf('.')
  return i > 0 ? `${name.slice(0, i)} (${n})${name.slice(i)}` : `${name} (${n})`
}

/** The first of `name`, `name (1)`… not in `taken`. */
export function freeName(name: string, taken: Iterable<string>): string {
  const set = new Set(taken)
  for (let n = 0; ; n++) {
    const candidate = numberedName(name, n)
    if (!set.has(candidate)) return candidate
  }
}

/** Whether `path` is `dir` or inside it (same side, either separator). */
export function isInside(path: string, dir: string): boolean {
  const norm = (p: string) => p.replace(/[\\/]+$/, '')
  const p = norm(path)
  const d = norm(dir)
  return p === d || p.startsWith(d + '/') || p.startsWith(d + '\\')
}

// ── Conflicts ───────────────────────────────────────────────────────────────

export interface ConflictAnswer {
  choice: ConflictChoice
  /** Use the same choice for the remaining conflicts of this copy. */
  applyToAll: boolean
}

/** Asks about each name clash of one copy, remembering "apply to all". */
export class ConflictResolver {
  private remembered: ConflictChoice | null = null
  private readonly ask: (name: string, isDir: boolean) => Promise<ConflictAnswer | null>

  /** `ask` resolves `null` when the user cancels the whole copy. */
  constructor(ask: (name: string, isDir: boolean) => Promise<ConflictAnswer | null>) {
    this.ask = ask
  }

  /** The choice for a clash on `name`, or `null` to cancel the copy. */
  async resolve(name: string, isDir: boolean): Promise<ConflictChoice | null> {
    if (this.remembered) return this.remembered
    const answer = await this.ask(name, isDir)
    if (!answer) return null
    if (answer.applyToAll) this.remembered = answer.choice
    return answer.choice
  }
}

/** The writing side's mode for a file, given the user's choice (if it clashed). */
export function writeConflict(choice: ConflictChoice | null): WriteConflict {
  return choice === 'overwrite' ? 'overwrite' : choice === 'rename' ? 'rename' : 'fail'
}

// ── Planning ────────────────────────────────────────────────────────────────

/** One file to copy. */
export interface FileJob {
  direction: Direction
  /** Path on the source side. */
  src: string
  /** Existing folder on the destination side (created by planning). */
  destDir: string
  name: string
  size: number
  conflict: WriteConflict
  /** Shown in the queue: the path relative to what the user picked. */
  label: string
}

export interface PlanDeps {
  /** Every entry under a source folder. */
  walk: (path: string) => Promise<Walk>
  /** Names in a destination folder. */
  list: (dir: string) => Promise<FsEntry[]>
  /** Create a destination folder (fine when it exists). */
  mkdir: (path: string) => Promise<unknown>
  resolver: ConflictResolver
}

export interface Plan {
  jobs: FileJob[]
  /** Picked names left out by a "Skip". */
  skipped: string[]
  failed: { name: string; error: string }[]
  /** The user cancelled at a conflict prompt: nothing is queued. */
  canceled: boolean
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** The folders and files to copy for a picked folder, under `destRoot`. */
export function planFolder(
  direction: Direction,
  srcRoot: string,
  destRoot: string,
  walk: Walk,
  conflict: WriteConflict,
): { dirs: string[]; jobs: FileJob[] } {
  const top = baseName(srcRoot)
  const dirs = [destRoot]
  const jobs: FileJob[] = []
  for (const e of walk.entries) {
    if (e.is_dir) {
      dirs.push(joinRel(destRoot, e.rel))
      continue
    }
    jobs.push({
      direction,
      src: joinRel(srcRoot, e.rel),
      destDir: joinRel(destRoot, relParent(e.rel)),
      name: baseName(e.rel),
      size: e.size,
      conflict,
      label: `${top}/${e.rel}`,
    })
  }
  return { dirs, jobs }
}

/**
 * The files to copy for the entries picked on one side into `destDir` on the
 * other, asking about name clashes. Folders are walked and their folder tree
 * created on the destination right away; a clashing folder is merged
 * (overwrite), copied next to it under a free name (rename) or skipped.
 */
export async function planTransfer(
  direction: Direction,
  picked: FsEntry[],
  destDir: string,
  deps: PlanDeps,
): Promise<Plan> {
  const plan: Plan = { jobs: [], skipped: [], failed: [], canceled: false }
  const existing = new Set((await deps.list(destDir)).map((e) => e.name))
  for (const entry of picked) {
    let choice: ConflictChoice | null = null
    if (existing.has(entry.name)) {
      choice = await deps.resolver.resolve(entry.name, entry.is_dir)
      if (choice === null) return { ...plan, jobs: [], canceled: true }
      if (choice === 'skip') {
        plan.skipped.push(entry.name)
        continue
      }
    }
    if (!entry.is_dir) {
      plan.jobs.push({
        direction,
        src: entry.path,
        destDir,
        name: entry.name,
        size: entry.size,
        conflict: writeConflict(choice),
        label: entry.name,
      })
      continue
    }
    try {
      const walk = await deps.walk(entry.path)
      if (walk.truncated) throw new Error('too_many_entries')
      const name = choice === 'rename' ? freeName(entry.name, existing) : entry.name
      existing.add(name)
      // A merged folder replaces what it clashes with; a fresh one cannot clash.
      const { dirs, jobs } = planFolder(direction, entry.path, joinPath(destDir, name), walk, choice === 'overwrite' ? 'overwrite' : 'fail')
      for (const d of dirs) await deps.mkdir(d)
      plan.jobs.push(...jobs)
    } catch (e) {
      plan.failed.push({ name: entry.name, error: message(e) })
    }
  }
  return plan
}

// ── Copying one file ────────────────────────────────────────────────────────

/** Both ends of one direction (the source read, the destination write). */
export interface CopyIo {
  /** Start writing; returns the write's id. */
  begin: (destDir: string, name: string, size: number, conflict: WriteConflict) => Promise<string>
  /** Base64 of up to `len` bytes of the source at `offset`. */
  read: (src: string, offset: number, len: number) => Promise<string>
  /** Append a chunk; returns the bytes written so far. */
  write: (id: string, offset: number, dataBase64: string) => Promise<number>
  /** SHA-256 (hex) of the source file. */
  hash: (src: string) => Promise<string>
  /** Complete the write if it hashes to `sha256`; returns the final path. */
  finish: (id: string, sha256: string) => Promise<string>
  /** Drop the write and its temp file. */
  abort: (id: string) => Promise<void>
}

export class CanceledError extends Error {
  constructor() {
    super('canceled')
  }
}

/** Size on the wire (base64) of `n` raw bytes. */
export function wireBytes(n: number): number {
  return Math.ceil(n / 3) * 4
}

/** A shared allowance of bytes in flight; waiters are served in order. */
export class ByteBudget {
  readonly max: number
  private used = 0
  private readonly waiters: { n: number; resolve: () => void }[] = []

  constructor(max = MAX_IN_FLIGHT_BYTES) {
    this.max = max
  }

  get inFlight(): number {
    return this.used
  }

  /** Wait until `n` more bytes fit under {@link max}, then take them. */
  acquire(n: number): Promise<void> {
    if (n > this.max) return Promise.reject(new Error(`chunk of ${n} bytes exceeds the in-flight cap`))
    if (this.waiters.length === 0 && this.used + n <= this.max) {
      this.used += n
      return Promise.resolve()
    }
    return new Promise((resolve) => this.waiters.push({ n, resolve }))
  }

  release(n: number): void {
    this.used = Math.max(0, this.used - n)
    while (this.waiters.length > 0 && this.used + this.waiters[0].n <= this.max) {
      const w = this.waiters.shift()!
      this.used += w.n
      w.resolve()
    }
  }
}

/** Bytes of a base64 string. */
export function base64Bytes(b64: string): number {
  const pad = b64.endsWith('==') ? 2 : b64.endsWith('=') ? 1 : 0
  return (b64.length / 4) * 3 - pad
}

export interface CopyOptions {
  canceled: () => boolean
  onProgress?: (done: number) => void
  chunk?: number
  /** Shared in-flight allowance (see {@link MAX_IN_FLIGHT_BYTES}). */
  budget?: ByteBudget
}

/**
 * Copy one file: chunks strictly in order, each write awaited (acked) before
 * the next is sent; only the next READ overlaps the current write. Every chunk
 * holds its bytes in `budget` from its read until its write is acked. Then the
 * source's SHA-256 is checked by the writing side before the file is moved
 * into place. A failure or cancel drops the partial write.
 */
export async function copyFile(
  job: Pick<FileJob, 'src' | 'destDir' | 'name' | 'size' | 'conflict'>,
  io: CopyIo,
  opts: CopyOptions,
): Promise<string> {
  const chunk = opts.chunk ?? TRANSFER_CHUNK
  const budget = opts.budget
  const check = () => {
    if (opts.canceled()) throw new CanceledError()
  }
  check()
  const id = await io.begin(job.destDir, job.name, job.size, job.conflict)
  let held = 0
  const readAt = (offset: number) => {
    const len = Math.min(chunk, job.size - offset)
    const cost = wireBytes(len)
    const p = (budget ? budget.acquire(cost) : Promise.resolve())
      .then(() => {
        held += cost
        return io.read(job.src, offset, len)
      })
      .then((data) => ({ data, cost }))
    // Awaited below; a failed write first must not leave it unhandled.
    p.catch(() => {})
    return p
  }
  let next: ReturnType<typeof readAt> | null = null
  try {
    let offset = 0
    next = job.size > 0 ? readAt(0) : null
    while (next) {
      const { data, cost } = await next
      next = null
      check()
      const len = base64Bytes(data)
      if (len === 0) throw new Error('file_changed')
      const after = offset + len
      next = after < job.size ? readAt(after) : null
      offset = await io.write(id, offset, data)
      held -= cost
      budget?.release(cost)
      if (offset !== after) throw new Error('file_changed')
      opts.onProgress?.(offset)
    }
    check()
    const sha = await io.hash(job.src)
    check()
    return await io.finish(id, sha)
  } catch (e) {
    await io.abort(id).catch(() => {})
    throw e
  } finally {
    // A read still under way holds bytes too: let it land before freeing them.
    if (next) await next.catch(() => {})
    if (held > 0) budget?.release(held)
  }
}

// ── The queue ───────────────────────────────────────────────────────────────

export type ItemStatus = 'queued' | 'running' | 'done' | 'failed' | 'canceled'

export interface TransferItem extends FileJob {
  id: number
  status: ItemStatus
  /** Bytes copied. */
  done: number
  error: string | null
  /** Where it landed (it may have been renamed). */
  dest: string | null
}

export interface QueueProgress {
  done: number
  total: number
  percent: number
  active: number
  queued: number
  finished: number
  failed: number
}

/** Overall progress; skipped, cancelled and failed files do not count toward it. */
export function queueProgress(items: TransferItem[]): QueueProgress {
  const p: QueueProgress = { done: 0, total: 0, percent: 100, active: 0, queued: 0, finished: 0, failed: 0 }
  for (const it of items) {
    if (it.status === 'failed') p.failed++
    if (it.status === 'failed' || it.status === 'canceled') continue
    p.total += it.size
    p.done += it.status === 'done' ? it.size : Math.min(it.done, it.size)
    if (it.status === 'running') p.active++
    else if (it.status === 'queued') p.queued++
    else p.finished++
  }
  p.percent = p.total > 0 ? Math.min(100, Math.floor((p.done * 100) / p.total)) : 100
  return p
}

/** Percentage (0–100) of one item. */
export function itemPercent(it: Pick<TransferItem, 'done' | 'size' | 'status'>): number {
  if (it.status === 'done' || it.size === 0) return it.status === 'done' ? 100 : 0
  return Math.min(100, Math.floor((it.done * 100) / it.size))
}

export interface QueueOptions {
  io: Record<Direction, CopyIo>
  /** Wraps the items array and each new item (Vue's `reactive`); identity by default. */
  wrap?: <T extends object>(o: T) => T
  parallel?: number
  chunk?: number
  /** In-flight allowance; a fresh {@link MAX_IN_FLIGHT_BYTES} one by default. */
  budget?: ByteBudget
  /** An item finished (done / failed / canceled). */
  onSettled?: (item: TransferItem) => void
}

/** Runs queued copies, {@link MAX_PARALLEL} at a time, in the order added. */
export class TransferQueue {
  readonly items: TransferItem[]
  private readonly opts: QueueOptions
  private readonly wrap: <T extends object>(o: T) => T
  private readonly cancelled = new Set<number>()
  private readonly running = new Map<number, Promise<void>>()
  /** Shared by every copy of this queue: see {@link MAX_IN_FLIGHT_BYTES}. */
  private readonly budget: ByteBudget
  private nextId = 1

  constructor(opts: QueueOptions) {
    this.opts = opts
    this.budget = opts.budget ?? new ByteBudget()
    this.wrap = opts.wrap ?? ((o) => o)
    this.items = this.wrap([] as TransferItem[])
  }

  add(jobs: FileJob[]): void {
    for (const job of jobs) {
      this.items.push(this.wrap({ ...job, id: this.nextId++, status: 'queued', done: 0, error: null, dest: null }))
    }
    this.pump()
  }

  cancel(id: number): void {
    const it = this.items.find((i) => i.id === id)
    if (!it) return
    if (it.status === 'queued') this.settle(it, 'canceled', null)
    else if (it.status === 'running') this.cancelled.add(id)
  }

  cancelAll(): void {
    for (const it of this.items) this.cancel(it.id)
  }

  /** Queue a failed or cancelled item again. */
  retry(id: number): void {
    const it = this.items.find((i) => i.id === id)
    if (!it || (it.status !== 'failed' && it.status !== 'canceled')) return
    it.status = 'queued'
    it.done = 0
    it.error = null
    this.pump()
  }

  retryFailed(): void {
    for (const it of this.items) if (it.status === 'failed') this.retry(it.id)
  }

  /** Drop finished items from the list. */
  clearFinished(): void {
    const keep = this.items.filter((i) => i.status === 'queued' || i.status === 'running')
    this.items.splice(0, this.items.length, ...keep)
  }

  /** Resolves when nothing is running or queued. */
  async idle(): Promise<void> {
    while (this.running.size > 0) await Promise.all([...this.running.values()])
  }

  private settle(it: TransferItem, status: ItemStatus, error: string | null): void {
    it.status = status
    it.error = error
    this.opts.onSettled?.(it)
  }

  private pump(): void {
    const parallel = this.opts.parallel ?? MAX_PARALLEL
    while (this.running.size < parallel) {
      const it = this.items.find((i) => i.status === 'queued')
      if (!it) return
      it.status = 'running'
      this.cancelled.delete(it.id)
      const run = this.run(it).finally(() => {
        this.running.delete(it.id)
        this.pump()
      })
      this.running.set(it.id, run)
    }
  }

  private async run(it: TransferItem): Promise<void> {
    try {
      const dest = await copyFile(it, this.opts.io[it.direction], {
        canceled: () => this.cancelled.has(it.id),
        onProgress: (done) => (it.done = done),
        chunk: this.opts.chunk,
        budget: this.budget,
      })
      it.dest = dest
      it.done = it.size
      this.settle(it, 'done', null)
    } catch (e) {
      if (e instanceof CanceledError) this.settle(it, 'canceled', null)
      else this.settle(it, 'failed', message(e))
    } finally {
      this.cancelled.delete(it.id)
    }
  }
}
