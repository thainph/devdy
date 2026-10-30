// Background-process manager: the backend contract (types, command / event names)
// plus the pure helpers the Processes screen, the sidebar row, the tray and the
// startup / quit prompts share. Kept free of Vue / Tauri / `@/` imports so the
// helpers can be unit-tested with `node --test`.

/** What spawned an entry. `run` is user work; every other kind is app plumbing. */
export type ProcKind =
  | 'run'
  | 'translate'
  | 'slack_translate'
  | 'focus_ai'
  | 'mascot'
  | 'probe'
  | 'mcp_test'
  | 'ssh_agent'

/**
 * active   = owned by something live in this app session
 * leftover = its owner (e.g. a run) ended, but processes it started are still alive
 * orphan   = left over from a PREVIOUS app session
 */
export type ProcState = 'active' | 'leftover' | 'orphan'

export interface BgProcess {
  pid: number
  ppid: number
  pgid: number
  command: string
  /** Percent of one core. */
  cpu: number
  rss_kb: number
  /** RFC3339. */
  started_at: string | null
  is_root: boolean
}

export interface BgEntry {
  id: string
  kind: ProcKind
  state: ProcState
  label: string
  run_id: string | null
  project_id: string | null
  project_name: string | null
  created_at: string
  ended_at: string | null
  /** App plumbing (everything except `run`). */
  infra: boolean
  processes: BgProcess[]
  cpu: number
  rss_kb: number
}

/** `active` excludes infra entries. */
export interface BgCounts {
  active: number
  leftover: number
  orphan: number
  total_processes: number
}

export interface BgSnapshot {
  entries: BgEntry[]
  counts: BgCounts
}

export type QuitAction = 'stop_all' | 'keep_leftovers' | 'cancel'
export type QuitPolicy = 'ask' | 'stop_all' | 'keep_leftovers'

/** Payload of `app:quit-requested`; `entries` excludes infra. */
export interface QuitRequest {
  counts: BgCounts
  entries: BgEntry[]
}

// Tauri command / event names, in one place so they're easy to diff against the backend.
export const CMD_LIST = 'list_background_processes'
export const CMD_STOP = 'stop_background_processes'
export const CMD_KILL = 'kill_background_process'
export const CMD_RESOLVE_QUIT = 'resolve_quit'
export const EVT_CHANGED = 'procs:changed'
export const EVT_QUIT_REQUESTED = 'app:quit-requested'
export const EVT_TRAY_OPEN = 'tray:open-processes'
/** Tray menu row id; Rust emits `tray:open-processes` when it is clicked. */
export const TRAY_PROCS_ID = 'tray:procs'

export const EMPTY_COUNTS: BgCounts = { active: 0, leftover: 0, orphan: 0, total_processes: 0 }

/** Processes nobody owns any more: leftover from a run, or from a previous session. */
export function strayCount(c: BgCounts): number {
  return c.leftover + c.orphan
}

/** Everything the tray row counts: user sessions plus stray processes. */
export function trayCount(c: BgCounts): number {
  return c.active + c.leftover + c.orphan
}

export interface EntryGroups {
  orphan: BgEntry[]
  leftover: BgEntry[]
  /** Active user work (runs). */
  sessions: BgEntry[]
  /** Active app plumbing. */
  services: BgEntry[]
}

/**
 * Split a snapshot into the screen's sections. Orphan / leftover keep infra
 * entries too — a stray process is stray whoever started it. Each section is
 * sorted heaviest-first (RAM) so the thing worth stopping sits on top.
 */
export function groupEntries(entries: BgEntry[]): EntryGroups {
  const g: EntryGroups = { orphan: [], leftover: [], sessions: [], services: [] }
  for (const e of entries) {
    if (e.state === 'orphan') g.orphan.push(e)
    else if (e.state === 'leftover') g.leftover.push(e)
    else if (e.infra) g.services.push(e)
    else g.sessions.push(e)
  }
  const byRss = (a: BgEntry, b: BgEntry) => b.rss_kb - a.rss_kb || a.label.localeCompare(b.label)
  g.orphan.sort(byRss)
  g.leftover.sort(byRss)
  g.sessions.sort(byRss)
  g.services.sort(byRss)
  return g
}

/** Ids of every leftover + orphan entry ("Stop all leftovers"). */
export function strayIds(entries: BgEntry[]): string[] {
  return entries.filter((e) => e.state !== 'active').map((e) => e.id)
}

/** Sum of the processes and RAM of a list of entries (for prompt summaries). */
export function totals(entries: BgEntry[]): { processes: number; rss_kb: number } {
  return entries.reduce(
    (acc, e) => ({ processes: acc.processes + e.processes.length, rss_kb: acc.rss_kb + e.rss_kb }),
    { processes: 0, rss_kb: 0 },
  )
}

/** Human-readable RAM from a KiB count: "512 KB", "34.5 MB", "1.25 GB". */
export function formatRss(kb: number): string {
  if (!Number.isFinite(kb) || kb <= 0) return '0 KB'
  if (kb < 1024) return `${Math.round(kb)} KB`
  const mb = kb / 1024
  if (mb < 1024) return `${mb < 10 ? mb.toFixed(1) : Math.round(mb)} MB`
  const gb = mb / 1024
  return `${gb.toFixed(gb < 10 ? 2 : 1)} GB`
}

/** CPU percent with one decimal below 10%, else whole numbers: "0.4%", "37%". */
export function formatCpu(cpu: number): string {
  if (!Number.isFinite(cpu) || cpu <= 0) return '0%'
  return cpu < 10 ? `${cpu.toFixed(1)}%` : `${Math.round(cpu)}%`
}

/**
 * Compact, language-neutral age since an RFC3339 timestamp: "45s", "12m",
 * "3h 5m", "2d 4h". Empty for a missing / unparseable time.
 */
export function formatAge(since: string | null, now: number = Date.now()): string {
  if (!since) return ''
  const t = Date.parse(since)
  if (Number.isNaN(t)) return ''
  const s = Math.max(0, Math.floor((now - t) / 1000))
  if (s < 60) return `${s}s`
  const m = Math.floor(s / 60)
  if (m < 60) return `${m}m`
  const h = Math.floor(m / 60)
  if (h < 24) return m % 60 ? `${h}h ${m % 60}m` : `${h}h`
  const d = Math.floor(h / 24)
  return h % 24 ? `${d}d ${h % 24}h` : `${d}d`
}

/** Shorten a long command line for a table cell; the full text goes in `title`. */
export function truncateCommand(cmd: string, max = 80): string {
  const c = cmd.trim()
  return c.length > max ? `${c.slice(0, max - 1)}…` : c
}
