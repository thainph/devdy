// Project Pulse (docs/project-pulse-spec.md): types mirroring the Rust
// `pulse_overview` payload and the pure helpers of the Pulse screen and the
// "Watch" buttons — labels, filters, row limits, source refs. Health itself is
// computed by the backend (BR-08/09/10/12); nothing here re-derives it.

export type PulseHealth = 'red' | 'yellow' | 'green' | 'done' | 'pending'
export type ProjectHealthTone = 'red' | 'yellow' | 'green' | 'none'
export type PulseSourceType = 'slack' | 'github_issue'

/** Mirrors the Rust `Watch` (camelCase). */
export interface PulseWatch {
  id: string
  projectId: string
  projectName: string
  sourceType: PulseSourceType
  sourceRef: string
  sourceUrl: string
  slackThreadId: string | null
  title: string | null
  active: boolean
  addedVia: 'mcp' | 'button' | 'paste'
  addedRunId: string | null
  resolveStatus: 'pending' | 'ready' | 'error'
  lastError: string | null
  checkedAt: string | null
  createdAt: string
  updatedAt: string
}

/** Digest list items keep the AI's snake_case (stored JSON). */
export interface PulseDeadline {
  title: string
  due_at: string
  all_day: boolean
  evidence: string
}
export interface PulseDecision {
  text: string
  decided_at: string | null
  evidence: string
}
export interface PulseQuestion {
  text: string
  asked_by: 'me' | 'other'
  evidence: string | null
}

export interface PulseDigest {
  id: string
  watchId: string
  contentHash: string
  status: 'on_track' | 'at_risk' | 'blocked' | 'waiting' | 'done'
  ball: 'me' | 'customer' | 'team' | 'none'
  ballSince: string | null
  summary: string
  nextAction: string | null
  deadlines: PulseDeadline[]
  decisions: PulseDecision[]
  openQuestions: PulseQuestion[]
  truncated: boolean
  sourceClosed: boolean
  model: string | null
  createdAt: string
}

export interface PulseNextDue {
  title: string
  dueAt: string
  allDay: boolean
}

/** Mirrors the Rust `WatchItem` (Watch + digest + WatchHealth, flattened). */
export interface PulseWatchItem extends PulseWatch {
  digest: PulseDigest | null
  health: PulseHealth
  finished: boolean
  overdue: boolean
  needsMe: boolean
  waitingCustomer: boolean
  nextDue: PulseNextDue | null
}

export interface PulseCounts {
  needsMe: number
  waitingCustomer: number
  overdue: number
  watching: number
}

/** Mirrors the Rust `ProjectPulse` (cards come in BR-09 order). */
export interface ProjectPulse {
  projectId: string
  projectName: string
  health: ProjectHealthTone
  nextMilestone: PulseNextDue | null
  counts: PulseCounts
  watches: PulseWatchItem[]
}

export interface PulseProgress {
  done: number
  total: number
}

/** Rows shown per card before "+N more" (§6.1). */
export const ROWS_PER_CARD = 5

export const HEALTH_DOT: Record<PulseHealth | ProjectHealthTone, string> = {
  red: 'bg-red-500',
  yellow: 'bg-amber-500',
  green: 'bg-emerald-500',
  done: 'bg-emerald-500/40',
  pending: 'bg-muted-foreground/40',
  none: 'bg-muted-foreground/30',
}

const DAY = 86_400_000
const HOUR = 3_600_000

/** Local calendar day number (days since epoch in local time). */
function localDay(t: number): number {
  const d = new Date(t)
  return Math.floor(Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()) / DAY)
}

/** "Mốc kế" wording: due today, N days left, or N days late — by local date. */
export type DueRelative = { kind: 'today' } | { kind: 'left'; days: number } | { kind: 'late'; days: number }

export function dueRelative(dueAt: string, now = Date.now()): DueRelative | null {
  const t = Date.parse(dueAt)
  if (!Number.isFinite(t)) return null
  const diff = localDay(t) - localDay(now)
  if (diff === 0) return t < now ? { kind: 'late', days: 0 } : { kind: 'today' }
  return diff > 0 ? { kind: 'left', days: diff } : { kind: 'late', days: -diff }
}

/** `dd/MM` of a deadline (local). */
export function shortDate(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return `${String(d.getDate()).padStart(2, '0')}/${String(d.getMonth() + 1).padStart(2, '0')}`
}

/** How long the ball has been held: hours under two days, then days. */
export type BallAge = { unit: 'h'; value: number } | { unit: 'd'; value: number }

export function ballAge(since: string | null, now = Date.now()): BallAge | null {
  const t = since ? Date.parse(since) : NaN
  if (!Number.isFinite(t)) return null
  const hours = Math.max(0, Math.floor((now - t) / HOUR))
  return hours < 48 ? { unit: 'h', value: hours } : { unit: 'd', value: Math.floor(hours / 24) }
}

/** Card body split: open rows (first `limit` shown, the rest counted) and the
 * collapsed "done" group (BR-12). */
export function splitRows(watches: PulseWatchItem[], limit = ROWS_PER_CARD) {
  const open = watches.filter((w) => !w.finished)
  return {
    shown: open.slice(0, limit),
    more: Math.max(0, open.length - limit),
    finished: watches.filter((w) => w.finished),
  }
}

const fold = (s: string) => s.normalize('NFD').replace(/\p{Diacritic}/gu, '').replace(/đ/g, 'd').replace(/Đ/g, 'D').toLowerCase()

/** Header filters: project-name search (accent-insensitive) and "needs attention" (🔴 + 🟡). */
export function filterProjects(projects: ProjectPulse[], query: string, attentionOnly: boolean): ProjectPulse[] {
  const q = fold(query.trim())
  return projects.filter(
    (p) =>
      (!q || fold(p.projectName).includes(q)) && (!attentionOnly || p.health === 'red' || p.health === 'yellow'),
  )
}

export function redProjectCount(projects: ProjectPulse[]): number {
  return projects.filter((p) => p.health === 'red').length
}

/** Slack `source_ref` (BR-02). */
export function slackSourceRef(channelId: string | null, threadTs: string | null): string | null {
  return channelId && threadTs ? `${channelId}:${threadTs}` : null
}

/** GitHub issue `source_ref` (BR-02) from an issue URL; null for anything else. */
export function githubIssueRef(url: string | null | undefined): string | null {
  const m = /^https?:\/\/(?:www\.)?github\.com\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/issues\/0*([1-9][0-9]{0,9})(?:[/?#]|$)/i.exec(
    (url ?? '').trim(),
  )
  return m ? `${m[1].toLowerCase()}/${m[2].toLowerCase()}#${m[3]}` : null
}

/** `"type|ref"` key of the watched-source index. */
export const sourceKey = (type: PulseSourceType, ref: string) => `${type}|${ref}`

/** Active watches by source key, for the Watch buttons. */
export function watchIndex(projects: ProjectPulse[]): Map<string, PulseWatchItem> {
  const m = new Map<string, PulseWatchItem>()
  for (const p of projects) for (const w of p.watches) m.set(sourceKey(w.sourceType, w.sourceRef), w)
  return m
}

/** Command errors are `"<code>: <detail>"`. */
export function pulseErrorCode(e: unknown): { code: string | null; detail: string } {
  const msg = String(e instanceof Error ? e.message : e)
  const m = /^([a-z_]+):\s*(.*)$/s.exec(msg)
  return m ? { code: m[1], detail: m[2] } : { code: null, detail: msg }
}

/** Error codes with a `pulse.errors.*` message (§7 + watch `last_error`s). */
export const KNOWN_ERRORS = [
  'invalid_source',
  'project_required',
  'project_not_found',
  'project_ambiguous',
  'repo_ambiguous',
  'watched_in_other_project',
  'github_not_found',
  'github_auth',
  'github_fetch_failed',
  'slack_fetch_failed',
  'summarize_failed',
  'duplicate_source',
  'board_unavailable',
  'not_found',
] as const
