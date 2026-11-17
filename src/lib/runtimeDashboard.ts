// Runtime Dashboard (the app's start screen): everything running right now,
// grouped by conductor. The pure part — session state, grouping and folding,
// ordering, keyboard navigation, sparklines, the 5-hour quota forecast, tool
// summaries and the detail log lines. The data plumbing lives in
// `composables/useRuntimeDashboard.ts`.
//
// Pure logic (no Vue / Tauri / `@/` imports) for node:test.

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
  if (i.conductor && isWaitTool(i.currentTool)) return 'idle'
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

// ── Groups ──────────────────────────────────────────────────────────────────

export type GroupKind = 'conductor' | 'solo' | 'external'

export interface GroupDef {
  /** Conductor run id, `solo` or `external`. */
  key: string
  kind: GroupKind
  conductor: RtSession | null
  members: RtSession[]
}

export const SOLO_KEY = 'solo'
export const EXTERNAL_KEY = 'external'

/**
 * Fixed groups: one per running conductor (oldest first) → standalone sessions
 * (workers whose conductor is not running included) → outside Devdy. Empty
 * standalone / external groups are left out.
 */
export function buildGroups(list: readonly RtSession[]): GroupDef[] {
  const conductors = list
    .filter((s) => s.role === 'conductor')
    .sort((a, b) => (a.startedAt ?? Infinity) - (b.startedAt ?? Infinity) || a.id.localeCompare(b.id))
  const byConductor = new Map<string, GroupDef>()
  for (const c of conductors) byConductor.set(c.id, { key: c.id, kind: 'conductor', conductor: c, members: [] })
  const solo: RtSession[] = []
  const external: RtSession[] = []
  for (const s of list) {
    if (s.role === 'conductor') continue
    if (s.role === 'external') {
      external.push(s)
      continue
    }
    const g = s.conductorId ? byConductor.get(s.conductorId) : undefined
    if (g) g.members.push(s)
    else solo.push(s)
  }
  const out = [...byConductor.values()]
  if (solo.length) out.push({ key: SOLO_KEY, kind: 'solo', conductor: null, members: solo })
  if (external.length) out.push({ key: EXTERNAL_KEY, kind: 'external', conductor: null, members: external })
  return out
}

/**
 * The sessions a group lists: its members, plus the conductor itself when it
 * needs you (otherwise it is reached through "View conductor").
 */
export function groupItems(g: GroupDef): RtSession[] {
  return g.conductor && needsAttention(g.conductor) ? [g.conductor, ...g.members] : g.members
}

export interface LayoutOptions {
  query: string
  filter: QuickFilter | null
  /** Groups the user opened / closed by hand (this app session). */
  open: Readonly<Record<string, boolean>>
  /** Groups whose "running fine" sessions the user unfolded / folded. */
  showOk: Readonly<Record<string, boolean>>
}

export interface GroupView extends GroupDef {
  open: boolean
  /** Sessions listed in the open group, ordered. */
  shown: RtSession[]
  /** States of every member, ordered (header dots). */
  dots: SessionState[]
  counts: { waiting: number; quiet: number; ctx: number }
  /** Folded "running fine" sessions. */
  foldN: number
  foldStates: SessionState[]
  hasFold: boolean
  canRefold: boolean
}

/** Without a hand choice, an open group shows its "running fine" sessions only up to this many. */
export const FOLD_SHOW_UP_TO = 4

/**
 * Apply search / filter and the fold rules. A group opens by default when
 * something in it needs you (the external group never does); while searching or
 * filtering only matching sessions show, every group with a match opens and the
 * others disappear.
 */
export function layoutGroups(defs: readonly GroupDef[], o: LayoutOptions): GroupView[] {
  const filtering = !!o.query.trim() || o.filter !== null
  const out: GroupView[] = []
  for (const g of defs) {
    const items = groupItems(g)
    const condHit = !!g.conductor && matchesQuery(g.conductor, o.query)
    const visible = items.filter((s) => matchesFilter(s, o.filter) && (condHit || matchesQuery(s, o.query)))
    if (filtering && !visible.length) continue
    const attn = visible.filter(needsAttention)
    const ok = visible.filter((s) => !needsAttention(s))
    const defaultOpen = g.kind !== 'external' && items.some(needsAttention)
    const open = filtering ? true : (o.open[g.key] ?? defaultOpen)
    const showOk = filtering || (o.showOk[g.key] ?? (attn.length === 0 && ok.length <= FOLD_SHOW_UP_TO))
    const counts = { waiting: 0, quiet: 0, ctx: 0 }
    for (const s of items) {
      if (s.state === 'waiting') counts.waiting++
      if (s.state === 'quiet') counts.quiet++
      if (ctxFull(s)) counts.ctx++
    }
    const okSorted = sortSessions(ok)
    out.push({
      ...g,
      open,
      shown: sortSessions(showOk ? visible : attn),
      dots: sortSessions(g.members).map((s) => s.state),
      counts,
      foldN: ok.length,
      foldStates: okSorted.map((s) => s.state),
      hasFold: !showOk && ok.length > 0,
      canRefold: showOk && !filtering && ok.length > 0 && (attn.length > 0 || ok.length > FOLD_SHOW_UP_TO),
    })
  }
  return out
}

/** Session ids in on-screen order (open groups only) — what j / k walk. */
export function displayOrder(views: readonly GroupView[]): string[] {
  const ids: string[] = []
  for (const g of views) if (g.open) for (const s of g.shown) ids.push(s.id)
  return ids
}

/** Every session in group order (the Session card's map). */
export function mapOrder(defs: readonly GroupDef[]): RtSession[] {
  const out: RtSession[] = []
  for (const g of defs) {
    if (g.conductor) out.push(g.conductor)
    out.push(...sortSessions(g.members))
  }
  return out
}

/** The group a session lives in. */
export function groupKeyOf(defs: readonly GroupDef[], id: string): string | null {
  for (const g of defs) {
    if (g.conductor?.id === id || g.members.some((s) => s.id === id)) return g.key
  }
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

/**
 * After answering `answered`, the waiting session to move to: the next one in
 * the queue (wrapping), null when nobody else waits.
 */
export function nextAfterAnswer(queue: readonly string[], answered: string): string | null {
  const rest = queue.filter((id) => id !== answered)
  if (!rest.length) return null
  const i = queue.indexOf(answered)
  if (i < 0) return rest[0]
  return queue.slice(i + 1).find((id) => id !== answered) ?? rest[0]
}

/** On open: the longest waiting session, else the first one on screen. */
export function initialPick(queue: readonly string[], order: readonly string[]): string | null {
  return queue[0] ?? order[0] ?? null
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
  if (typeof input === 'string') return clip(input)
  const o = obj(input)
  const n = name.toLowerCase()
  if (n === 'askuserquestion') {
    const q = Array.isArray(o.questions) ? obj(o.questions[0]) : {}
    return clip(str(q.question))
  }
  if (n === 'bash') {
    const cmd = o.command ?? o.cmd
    return clip(typeof cmd === 'string' ? cmd : Array.isArray(cmd) ? cmd.join(' ') : str(o.description))
  }
  if (n === 'task' || n === 'agent') return clip(str(o.description) || str(o.prompt))
  if (n === 'todowrite') return ''
  const file = fileOf(o)
  if (file) return clip(shortPath(file))
  for (const k of ['pattern', 'query', 'url', 'command', 'description', 'prompt']) {
    if (str(o[k]).trim()) return clip(str(o[k]))
  }
  return ''
}

// ── Detail log ──────────────────────────────────────────────────────────────

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

/** Log lines of the last `max` entries. */
export function logLines(entries: readonly LogEntryLite[], max = 150): LogLine[] {
  const out: LogLine[] = []
  for (const e of entries.slice(-max)) {
    const l = logLine(e)
    if (l) out.push(l)
  }
  return out
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
