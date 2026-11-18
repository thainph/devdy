// Runtime Dashboard (the app's start screen): everything running right now,
// grouped by conductor. The pure part — session state, grouping and folding,
// ordering, keyboard navigation, sparklines, the 5-hour quota forecast, tool
// summaries and the detail log lines. The data plumbing lives in
// `composables/useRuntimeDashboard.ts`.
//
// Pure logic (no Vue / Tauri / `@/` imports) for node:test.
import { sanitizeSecrets } from './sanitizeSecrets.ts'

// ── Live runs (existing `list_active_runs`) ─────────────────────────────────

/** Mirrors the Rust `ActiveRun` (`list_active_runs`). */
export interface ActiveRun {
  run_id: string
  project_id: string
  project_name: string
  title: string | null
  run_type: string
  ref_number: number | null
  engine: string | null
  model: string | null
  status: string
  started_at: string | null
  created_at: string
  role: string | null
  conductor_run_id: string | null
  pending_permissions: number
  waiting_since: string | null
  /**
   * The sidecar is alive. False for a conductor listed only because its
   * workers still run while it sits between turns (older backends omit it).
   */
  live?: boolean
}

/** A run's name: its title, else "Issue #n" / "PR #n", else null. */
export function runTitle(r: Pick<ActiveRun, 'title' | 'run_type' | 'ref_number'>): string | null {
  if (r.title?.trim()) return r.title.trim()
  if (r.run_type !== 'session' && r.ref_number != null) {
    return `${r.run_type === 'analyze_issue' ? 'Issue' : 'PR'} #${r.ref_number}`
  }
  return null
}

// ── Session summary ─────────────────────────────────────────────────────────

/**
 * waiting  = a permission prompt or an AskUserQuestion is open (needs you)
 * quiet    = running but nothing new for a while (or the watchdog flagged it)
 * running  = streaming normally
 * idle     = a conductor blocked in `session_wait` on its workers
 * external = a CLI / IDE session outside Devdy (read-only)
 */
export type SessionState = 'waiting' | 'quiet' | 'running' | 'idle' | 'external'

export type SessionRole = 'conductor' | 'worker' | 'session' | 'external'

export type Engine = 'claude' | 'codex'

/** What the session is doing now (the card's "now" line). */
export interface NowLine {
  /** `last` = no step known to be running: the session's latest event instead. */
  kind: 'tool' | 'permission' | 'question' | 'wait' | 'last' | 'none'
  /** Tool name (permission / tool / a last event that was a tool), null otherwise. */
  tool: string | null
  /** Command / file / question text, possibly empty. */
  detail: string
  /** Epoch ms the step started, when known. */
  since: number | null
}

/** One session as the dashboard shows it (rebuilt on a throttled tick). */
export interface RtSession {
  /** Run id, or `ext:<id>` for an external session. */
  id: string
  /** Null for an external session. */
  runId: string | null
  projectId: string | null
  projectName: string
  title: string
  engine: Engine
  role: SessionRole
  /** Conductor run id of a worker. */
  conductorId: string | null
  /**
   * Its sidecar is alive. False for an external session and for a conductor
   * between turns (no Stop; a message resumes it first).
   */
  live: boolean
  state: SessionState
  /** Epoch ms the oldest open prompt arrived (waiting only). */
  waitingSince: number | null
  /** Epoch ms of the current turn's start (or creation). */
  startedAt: number | null
  now: NowLine
  /** Context window use 0-100, null when unknown. */
  ctxPercent: number | null
  /** Events per minute, oldest first (empty when unknown). */
  spark: number[]
  cpuPercent: number | null
  rssKb: number | null
}

/** Context use at / above this percent counts as "context full". */
export const CTX_WARN_PERCENT = 80
/** No new event for this long while running → "quiet". */
export const QUIET_AFTER_MS = 5 * 60_000
/** Above this many sessions the Auto mode switches to rows. */
export const AUTO_ROWS_OVER = 15

/** Watchdog notices (`run:attention` kinds) that mean "quiet for long". */
const QUIET_NOTICES = new Set(['quiet', 'long_tool', 'unresponsive', 'not_started', 'unmonitored'])

/** A conductor's blocking wait on its workers (`mcp__<server>__session_wait`). */
export function isWaitTool(name: string | null | undefined): boolean {
  return !!name && /(^|__)session_wait$/.test(name)
}

export interface StateInput {
  external?: boolean
  /** Open permission / question prompts. */
  pending: number
  conductor: boolean
  /** Not running a turn now (a conductor between turns while its workers run). */
  ended?: boolean
  /** Live workers of this conductor. */
  liveWorkers?: number
  /** Name of the tool running now, if known. */
  currentTool: string | null
  /** Epoch ms of the last stream event, null when unknown. */
  lastEventAt: number | null
  /** Active `run:attention` kinds. */
  attention: readonly string[]
  now: number
}

export function classifyState(i: StateInput): SessionState {
  if (i.external) return 'external'
  if (i.pending > 0) return 'waiting'
  if (i.conductor && i.ended) return 'idle'
  if (i.conductor && isWaitTool(i.currentTool)) return 'idle'
  // A conductor with live workers and no other tool running is waiting on them
  // (Codex never reports the running tool, so `session_wait` is not seen there).
  if (i.conductor && (i.liveWorkers ?? 0) > 0 && i.currentTool === null) return 'idle'
  if (i.attention.some((k) => QUIET_NOTICES.has(k))) return 'quiet'
  if (i.lastEventAt !== null && i.now - i.lastEventAt > QUIET_AFTER_MS) return 'quiet'
  return 'running'
}

export function ctxFull(s: Pick<RtSession, 'ctxPercent'>): boolean {
  return s.ctxPercent !== null && s.ctxPercent >= CTX_WARN_PERCENT
}

/** Waiting, quiet, or its context nearly full. */
export function needsAttention(s: Pick<RtSession, 'state' | 'ctxPercent'>): boolean {
  return s.state === 'waiting' || s.state === 'quiet' || ctxFull(s)
}

/** 0-100 (capped), null when either side is unknown (0 tokens = just compacted). */
export function contextPercent(tokens: number | null | undefined, limit: number | null | undefined): number | null {
  if (tokens === null || tokens === undefined || !Number.isFinite(tokens) || !limit || limit <= 0) return null
  return Math.min(100, Math.round((tokens / limit) * 100))
}

function rank(s: RtSession): number {
  switch (s.state) {
    case 'waiting': return 0
    case 'quiet': return 1
    case 'running': return ctxFull(s) ? 2 : 3
    case 'idle': return 4
    default: return 5
  }
}

/** Waiting (longest first) → quiet → context full → running → idle → external. */
export function compareSessions(a: RtSession, b: RtSession): number {
  return (
    rank(a) - rank(b) ||
    (a.state === 'waiting' ? (a.waitingSince ?? Infinity) - (b.waitingSince ?? Infinity) : 0) ||
    (a.startedAt ?? Infinity) - (b.startedAt ?? Infinity) ||
    a.title.localeCompare(b.title) ||
    a.id.localeCompare(b.id)
  )
}

export function sortSessions(list: readonly RtSession[]): RtSession[] {
  return [...list].sort(compareSessions)
}

// ── Search / filter ─────────────────────────────────────────────────────────

export type QuickFilter = 'wait' | 'quiet' | 'ctx'

export function matchesQuery(s: Pick<RtSession, 'title' | 'projectName'>, query: string): boolean {
  const q = query.trim().toLowerCase()
  return !q || `${s.title} ${s.projectName}`.toLowerCase().includes(q)
}

export function matchesFilter(s: RtSession, filter: QuickFilter | null): boolean {
  if (filter === 'wait') return s.state === 'waiting'
  if (filter === 'quiet') return s.state === 'quiet'
  if (filter === 'ctx') return ctxFull(s)
  return true
}

/** Counts behind the Session card's chips. */
export function filterCounts(list: readonly RtSession[]): Record<QuickFilter, number> {
  const out = { wait: 0, quiet: 0, ctx: 0 }
  for (const s of list) {
    if (s.state === 'waiting') out.wait++
    if (s.state === 'quiet') out.quiet++
    if (ctxFull(s)) out.ctx++
  }
  return out
}

// ── Session tree ────────────────────────────────────────────────────────────

/** A top-level session and the workers it spawned (still running). */
export interface TreeNode {
  session: RtSession
  children: RtSession[]
}

function treeRank(n: TreeNode): RtSession {
  // A parent sorts by the most urgent of itself and its workers.
  return sortSessions([n.session, ...n.children])[0] ?? n.session
}

/**
 * One list for every session: workers sit under the session that spawned them
 * (a worker whose parent is gone becomes top-level). Top level: the most
 * urgent first (counting the workers), sessions outside Devdy last.
 */
export function buildTree(list: readonly RtSession[]): TreeNode[] {
  const byId = new Map(list.map((s) => [s.id, s]))
  const nodes = new Map<string, TreeNode>()
  const roots: TreeNode[] = []
  for (const s of list) {
    if (s.role === 'worker' && s.conductorId && byId.has(s.conductorId)) continue
    const n: TreeNode = { session: s, children: [] }
    nodes.set(s.id, n)
    roots.push(n)
  }
  for (const s of list) {
    if (s.role === 'worker' && s.conductorId) nodes.get(s.conductorId)?.children.push(s)
  }
  for (const n of roots) n.children = sortSessions(n.children)
  return roots.sort((a, b) => compareSessions(treeRank(a), treeRank(b)) || compareSessions(a.session, b.session))
}

export interface TreeOptions {
  query: string
  filter: QuickFilter | null
  /** Parents the user expanded / collapsed by hand (this app session). */
  open: Readonly<Record<string, boolean>>
}

/** One visible line of the list. */
export interface TreeRow {
  session: RtSession
  depth: 0 | 1
  /** Top-level only: id of the session, how many workers it has, whether they show. */
  childCount: number
  open: boolean
  /** Its workers that need you (waiting / quiet / context full), for the collapsed badge. */
  childAttention: number
  /** A worker that is the last one shown under its parent (tree connector). */
  last: boolean
}

/** Without a hand choice, a parent shows its workers when there are at most this many, or one needs you. */
export const AUTO_OPEN_UP_TO = 4

/**
 * The visible rows. A parent with workers starts open when it has few of them
 * or one needs you; the user's choice wins. While searching / filtering a
 * session shows when it matches or one of its workers does (shown open, with
 * only the matching workers — all of them when the parent matches the search).
 */
export function layoutTree(nodes: readonly TreeNode[], o: TreeOptions): TreeRow[] {
  const filtering = !!o.query.trim() || o.filter !== null
  const rows: TreeRow[] = []
  for (const n of nodes) {
    const p = n.session
    const parentHit = matchesQuery(p, o.query)
    const kids = filtering
      ? n.children.filter((c) => matchesFilter(c, o.filter) && (parentHit || matchesQuery(c, o.query)))
      : n.children
    const selfShown = !filtering || (parentHit && matchesFilter(p, o.filter))
    if (!selfShown && !kids.length) continue
    const childAttention = n.children.filter(needsAttention).length
    const defaultOpen = n.children.length <= AUTO_OPEN_UP_TO || childAttention > 0
    const open = n.children.length > 0 && (filtering ? true : (o.open[p.id] ?? defaultOpen))
    rows.push({ session: p, depth: 0, childCount: n.children.length, open, childAttention, last: false })
    if (open) kids.forEach((c, i) => rows.push({ session: c, depth: 1, childCount: 0, open: false, childAttention: 0, last: i === kids.length - 1 }))
  }
  return rows
}

/** Session ids in on-screen order — what j / k walk. */
export function displayOrder(rows: readonly TreeRow[]): string[] {
  return rows.map((r) => r.session.id)
}

/** Every session in list order, workers after their parent (the Session card's map). */
export function mapOrder(nodes: readonly TreeNode[]): RtSession[] {
  return nodes.flatMap((n) => [n.session, ...n.children])
}

/** The parent of a worker shown under it, else null. */
export function parentOf(nodes: readonly TreeNode[], id: string): string | null {
  for (const n of nodes) if (n.children.some((c) => c.id === id)) return n.session.id
  return null
}

/** Neighbour of `current` in `order` (`dir` ±1), clamped; the first one when `current` is not listed. */
export function stepOrder(order: readonly string[], current: string | null, dir: 1 | -1): string | null {
  if (!order.length) return null
  const i = current ? order.indexOf(current) : -1
  if (i < 0) return dir > 0 ? order[0] : order[order.length - 1]
  return order[Math.min(order.length - 1, Math.max(0, i + dir))]
}

// ── "Waiting for you" queue ─────────────────────────────────────────────────

/** Sessions waiting for you, the longest waiting first. */
export function waitQueue(list: readonly RtSession[]): string[] {
  return list
    .filter((s) => s.state === 'waiting')
    .sort((a, b) => (a.waitingSince ?? Infinity) - (b.waitingSince ?? Infinity) || a.id.localeCompare(b.id))
    .map((s) => s.id)
}

/** Next / previous waiting session, wrapping around. */
export function stepQueue(queue: readonly string[], current: string | null, dir: 1 | -1): string | null {
  if (!queue.length) return null
  const i = current ? queue.indexOf(current) : -1
  if (i < 0) return dir > 0 ? queue[0] : queue[queue.length - 1]
  return queue[(i + dir + queue.length) % queue.length]
}

// ── View mode ───────────────────────────────────────────────────────────────

export type ViewMode = 'auto' | 'cards' | 'rows'

export function effectiveMode(mode: ViewMode, total: number): 'cards' | 'rows' {
  if (mode !== 'auto') return mode
  return total > AUTO_ROWS_OVER ? 'rows' : 'cards'
}

export function parseViewMode(v: unknown): ViewMode {
  return v === 'cards' || v === 'rows' ? v : 'auto'
}

// ── Sparklines ──────────────────────────────────────────────────────────────

/** The trailing buckets that must all be empty for a line to read "stalled". */
export const STALL_BUCKETS = 5

export interface Sparkline {
  /** SVG polyline points in a `width` × `height` box. */
  points: string
  /** Activity stopped (the last buckets are all zero after earlier activity). */
  stalled: boolean
  empty: boolean
}

export function sparkline(values: readonly number[], width = 100, height = 20): Sparkline {
  if (!values.length) return { points: '', stalled: false, empty: true }
  const max = Math.max(1, ...values)
  const step = values.length > 1 ? width / (values.length - 1) : 0
  const pad = 2
  const points = values
    .map((v, i) => `${round1(i * step)},${round1(height - pad - (Math.max(0, v) / max) * (height - pad * 2))}`)
    .join(' ')
  const tail = values.slice(-STALL_BUCKETS)
  const stalled = values.length >= STALL_BUCKETS && tail.every((v) => v <= 0) && values.some((v) => v > 0)
  return { points, stalled, empty: values.every((v) => v <= 0) }
}

function round1(n: number): number {
  return Math.round(n * 10) / 10
}

/** Append a sample, keeping the last `max`. */
export function pushSample(list: readonly number[], v: number, max: number): number[] {
  const out = [...list, v]
  return out.length > max ? out.slice(out.length - max) : out
}

// ── Activity fallback (no backend snapshot) ─────────────────────────────────

export interface ActivityTrack {
  count: number
  lastEventAt: number | null
  /** Minute index (epoch ms / 60 000) → events. */
  buckets: Map<number, number>
}

/**
 * Fold a run's entry count into its track: growth since the last look counts
 * as that many events in the current minute. Used only while the backend does
 * not provide per-run activity.
 */
export function noteActivity(tracks: Map<string, ActivityTrack>, runId: string, count: number, now: number): ActivityTrack {
  let t = tracks.get(runId)
  if (!t) {
    t = { count, lastEventAt: null, buckets: new Map() }
    tracks.set(runId, t)
    return t
  }
  const delta = count >= t.count ? count - t.count : count
  t.count = count
  if (delta > 0) {
    t.lastEventAt = now
    const m = Math.floor(now / 60_000)
    t.buckets.set(m, (t.buckets.get(m) ?? 0) + delta)
    for (const k of t.buckets.keys()) if (k < m - 60) t.buckets.delete(k)
  }
  return t
}

/** `n` per-minute buckets ending at the current minute, oldest first. */
export function activityBuckets(t: ActivityTrack | undefined, now: number, n = 20): number[] {
  const m = Math.floor(now / 60_000)
  return Array.from({ length: n }, (_, i) => t?.buckets.get(m - (n - 1 - i)) ?? 0)
}

// ── Durations ───────────────────────────────────────────────────────────────

/** Clock-style elapsed time: "0:42", "12:05", "1:12:40". */
export function formatElapsed(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return ''
  const s = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(s / 3600)
  const m = Math.floor((s % 3600) / 60)
  const sec = String(s % 60).padStart(2, '0')
  return h ? `${h}:${String(m).padStart(2, '0')}:${sec}` : `${m}:${sec}`
}

export function parseTime(iso: string | null | undefined): number | null {
  if (!iso) return null
  const t = Date.parse(iso)
  return Number.isNaN(t) ? null : t
}

// ── 5-hour quota forecast ───────────────────────────────────────────────────

export const FIVE_HOURS_MS = 5 * 3_600_000

export interface QuotaSample {
  at: number
  pct: number
}

export type QuotaForecast =
  | { kind: 'unknown' }
  | { kind: 'full' }
  /** At the current pace the window resets before it runs out. */
  | { kind: 'safe' }
  | { kind: 'exhaust'; at: number }

/** Shortest span a recent-pace estimate is trusted over. */
const MIN_PACE_SPAN_MS = 10 * 60_000
/** Shortest elapsed window an average-pace estimate is trusted over. */
const MIN_WINDOW_SPAN_MS = 15 * 60_000

/**
 * When the 5-hour window runs out at the current pace. The pace comes from the
 * samples seen in this window (oldest one at least 10 min back), else from the
 * average since the window opened (reset − 5 h). Null-safe: unknown when there
 * is no percentage or no usable pace.
 */
export function quotaForecast(
  pct: number | null,
  resetsAt: number | null,
  samples: readonly QuotaSample[],
  now: number,
): QuotaForecast {
  if (pct === null || !Number.isFinite(pct)) return { kind: 'unknown' }
  if (pct >= 100) return { kind: 'full' }
  const windowStart = resetsAt !== null ? resetsAt - FIVE_HOURS_MS : null
  let slope: number | null = null
  const inWindow = samples
    .filter((s) => s.at <= now && (windowStart === null || s.at >= windowStart))
    .sort((a, b) => a.at - b.at)
  const first = inWindow[0]
  if (first && now - first.at >= MIN_PACE_SPAN_MS && pct >= first.pct) {
    slope = (pct - first.pct) / (now - first.at)
  } else if (windowStart !== null && now - windowStart >= MIN_WINDOW_SPAN_MS) {
    slope = pct / (now - windowStart)
  }
  if (slope === null) return { kind: 'unknown' }
  if (slope <= 0) return { kind: 'safe' }
  const at = now + (100 - pct) / slope
  if (resetsAt !== null && at >= resetsAt) return { kind: 'safe' }
  return { kind: 'exhaust', at }
}

/** Remember a quota reading (one per minute at most, the last 6 hours). */
export function pushQuotaSample(list: readonly QuotaSample[], s: QuotaSample): QuotaSample[] {
  const last = list[list.length - 1]
  if (last && s.at - last.at < 60_000 && last.pct === s.pct) return [...list]
  return [...list, s].filter((x) => s.at - x.at <= FIVE_HOURS_MS + 3_600_000).slice(-400)
}

// ── Tool summaries ──────────────────────────────────────────────────────────

const SUMMARY_MAX = 120

function obj(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}

function str(v: unknown): string {
  return typeof v === 'string' ? v : ''
}

/** Clipped, with anything credential-like masked (a command / target shown on a card or in the log). */
function safe(s: string, max = SUMMARY_MAX): string {
  return clip(sanitizeSecrets(s), max)
}

function clip(s: string, max = SUMMARY_MAX): string {
  const one = s.replace(/\s+/g, ' ').trim()
  return one.length > max ? `${one.slice(0, max - 1)}…` : one
}

/** A short tool name: `mcp__devdy__todos_list` → `devdy todos_list`. */
export function toolLabel(name: string): string {
  const m = /^mcp__(.+?)__(.+)$/.exec(name)
  return m ? `${m[1]} ${m[2]}` : name
}

const PATCH_FILE_RE = /^\*\*\* (?:Add|Update|Delete) File: (.+)$/m

/** The file a tool input targets (Claude path keys, Codex `changes` / patch). */
function fileOf(input: Record<string, unknown>): string {
  for (const k of ['file_path', 'notebook_path', 'path']) if (str(input[k]).trim()) return str(input[k]).trim()
  const changes = input.changes
  if (Array.isArray(changes) && changes.length) {
    const c = changes[0]
    return typeof c === 'string' ? c : str(obj(c).path)
  }
  if (changes && typeof changes === 'object') {
    const k = Object.keys(changes)[0]
    if (k) return k
  }
  for (const k of ['input', 'patch']) {
    const m = PATCH_FILE_RE.exec(str(input[k]))
    if (m) return m[1]
  }
  return ''
}

/** Last path segments, so a long absolute path still names the file. */
export function shortPath(p: string, keep = 2): string {
  const parts = p.split('/').filter(Boolean)
  return parts.length > keep ? parts.slice(-keep).join('/') : p
}

/**
 * The detail half of a "now" / log line: the command, file, pattern, URL or
 * question a tool call is about. Clipped to one short line.
 */
export function toolSummary(name: string, input: unknown): string {
  if (typeof input === 'string') return safe(input)
  const o = obj(input)
  const n = name.toLowerCase()
  if (n === 'askuserquestion') {
    const q = Array.isArray(o.questions) ? obj(o.questions[0]) : {}
    return clip(str(q.question))
  }
  if (n === 'bash') {
    const cmd = o.command ?? o.cmd
    return safe(typeof cmd === 'string' ? cmd : Array.isArray(cmd) ? cmd.join(' ') : str(o.description))
  }
  if (n === 'task' || n === 'agent') return clip(str(o.description) || str(o.prompt))
  if (n === 'todowrite') return ''
  const file = fileOf(o)
  if (file) return clip(shortPath(file))
  for (const k of ['pattern', 'query', 'url', 'command', 'description', 'prompt']) {
    if (str(o[k]).trim()) return safe(str(o[k]))
  }
  return ''
}

// ── Log lines (the "now" line's latest event) ─────────────────────────────

export type LogKind = 'tool' | 'ok' | 'ask' | 'err' | 'text' | 'user' | 'note'

export interface LogLine {
  kind: LogKind
  /** Empty when the UI supplies a localized text (a finished turn). */
  text: string
}

/** The subset of a stream entry the log reads (see `streamEvents.StreamEntry`). */
export type LogEntryLite =
  | { kind: 'tool'; name: string; input: unknown; displayName?: string; result?: { is_error: boolean } }
  | { kind: 'text' | 'user' | 'error'; text: string }
  | { kind: 'result'; text: string; is_error: boolean }
  | { kind: 'log'; level: string; text: string }
  | { kind: 'compact' }
  | { kind: 'command'; name: string; args?: string }
  | { kind: string }

function firstLine(s: string, max = 200): string {
  const line = s.split('\n').map((l) => l.trim()).find(Boolean) ?? ''
  return clip(line, max)
}

/** One compact log line per meaningful entry (thinking / system noise skipped). */
export function logLine(e: LogEntryLite): LogLine | null {
  switch (e.kind) {
    case 'tool': {
      const t = e as Extract<LogEntryLite, { kind: 'tool' }>
      const detail = toolSummary(t.name, t.input)
      if (t.name === 'AskUserQuestion') return { kind: 'ask', text: detail }
      const label = t.displayName || toolLabel(t.name)
      return { kind: t.result?.is_error ? 'err' : 'tool', text: detail ? `${label} ${detail}` : label }
    }
    case 'text': {
      const text = firstLine((e as { text: string }).text)
      return text ? { kind: 'text', text } : null
    }
    case 'user': {
      const text = firstLine((e as { text: string }).text)
      return text ? { kind: 'user', text } : null
    }
    case 'error':
      return { kind: 'err', text: firstLine((e as { text: string }).text) }
    case 'result': {
      const r = e as Extract<LogEntryLite, { kind: 'result' }>
      return r.is_error ? { kind: 'err', text: firstLine(r.text) } : { kind: 'ok', text: '' }
    }
    case 'log': {
      const l = e as Extract<LogEntryLite, { kind: 'log' }>
      if (l.level === 'debug') return null
      return { kind: l.level === 'error' ? 'err' : 'note', text: firstLine(l.text) }
    }
    case 'command': {
      const c = e as Extract<LogEntryLite, { kind: 'command' }>
      return { kind: 'note', text: `/${c.name}${c.args ? ` ${c.args}` : ''}` }
    }
    default:
      return null
  }
}

/**
 * The latest meaningful event of a session (looking back `within` entries), for
 * the "now" line when no running step is known — Codex reports tools only once
 * they finished.
 */
export function lastEvent(entries: readonly LogEntryLite[], within = 30): { tool: string | null; text: string } | null {
  for (let i = entries.length - 1; i >= Math.max(0, entries.length - within); i--) {
    const e = entries[i]
    const l = logLine(e)
    if (!l || !l.text) continue
    return { tool: e.kind === 'tool' ? (e as { name: string }).name : null, text: l.text }
  }
  return null
}

/** The tool still running at the end of a turn's entries (no result yet), if any. */
export function runningTool(entries: readonly LogEntryLite[]): { name: string; input: unknown } | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i]
    if (e.kind === 'result' || e.kind === 'user') return null
    if (e.kind === 'tool') {
      const t = e as Extract<LogEntryLite, { kind: 'tool' }>
      return t.result === undefined ? { name: t.name, input: t.input } : null
    }
  }
  return null
}

// ── Backend availability ────────────────────────────────────────────────────

/**
 * The snapshot command does not exist (an older backend) — as opposed to a
 * transient failure, which keeps the last good snapshot.
 */
export function isMissingCommandError(err: unknown): boolean {
  const msg = String(err instanceof Error ? err.message : err)
  return /command\s+\S+\s+not\s+found|unknown\s+command|not\s+found.*command|no such command/i.test(msg)
}
