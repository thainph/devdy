// Pure logic behind the Remote Controller's Inbox ("needs attention") screen:
// the per-run queue of open permission prompts, the cross-run list the Inbox
// renders, and which runs finished since the user last had the app open.
// No Vue / DOM here so it is unit-testable with node:test.

/** The fields of a permission prompt this module needs (structural). */
export interface PendingLike {
  run_id: string
  request_id: string
  tool_name: string
}

/** The fields of a Host run-list row this module needs (structural). */
export interface RunLike {
  id: string
  status: string
  updated_at?: string | null
  created_at?: string | null
}

/** Add `req` to its run's queue (arrival order), replacing a same-id entry. */
export function enqueuePending<T extends PendingLike>(
  queues: Record<string, T[]>,
  req: T,
): Record<string, T[]> {
  const list = queues[req.run_id] ?? []
  const i = list.findIndex((p) => p.request_id === req.request_id)
  const next = i === -1 ? [...list, req] : list.map((p, j) => (j === i ? req : p))
  return { ...queues, [req.run_id]: next }
}

/** Drop one prompt (by run + request id). An emptied run leaves the map. */
export function dequeuePending<T extends PendingLike>(
  queues: Record<string, T[]>,
  runId: string,
  requestId: string,
): Record<string, T[]> {
  const list = queues[runId]
  if (!list) return queues
  const next = list.filter((p) => p.request_id !== requestId)
  if (next.length === list.length) return queues
  const out = { ...queues }
  if (next.length) out[runId] = next
  else delete out[runId]
  return out
}

/** Drop every prompt of one run (it finished, or the Host said handled). */
export function dropRun<T extends PendingLike>(
  queues: Record<string, T[]>,
  runId: string,
): Record<string, T[]> {
  if (!(runId in queues)) return queues
  const out = { ...queues }
  delete out[runId]
  return out
}

/** Keep only prompts whose request id is in `keep` (the Host's re-sent set). */
export function retainPending<T extends PendingLike>(
  queues: Record<string, T[]>,
  keep: ReadonlySet<string>,
): Record<string, T[]> {
  const out: Record<string, T[]> = {}
  for (const [runId, list] of Object.entries(queues)) {
    const next = list.filter((p) => keep.has(p.request_id))
    if (next.length) out[runId] = next
  }
  return out
}

/** The prompt a run's session view answers first: the oldest one. */
export function headOf<T extends PendingLike>(queues: Record<string, T[]>): Record<string, T> {
  const out: Record<string, T> = {}
  for (const [runId, list] of Object.entries(queues)) if (list[0]) out[runId] = list[0]
  return out
}

/**
 * Every open prompt across all runs, flattened for the Inbox. Ordered by when
 * the Controller first saw it (oldest first: the longest-blocked run is the one
 * to unblock); `seenAt` maps request id → ms, missing entries sort last.
 */
export function aggregatePending<T extends PendingLike>(
  queues: Record<string, T[]>,
  seenAt: Record<string, number> = {},
): T[] {
  const all = Object.values(queues).flat()
  const at = (p: T) => seenAt[p.request_id] ?? Number.POSITIVE_INFINITY
  // Array.prototype.sort is stable, so per-run arrival order survives ties.
  return all.sort((a, b) => at(a) - at(b))
}

/** Total prompts waiting (the tab badge). */
export function pendingCount(queues: Record<string, PendingLike[]>): number {
  let n = 0
  for (const list of Object.values(queues)) n += list.length
  return n
}

/**
 * A Host timestamp to epoch ms. Accepts RFC 3339 and SQLite's
 * `YYYY-MM-DD HH:MM:SS` (UTC, no zone); null when absent or unparsable.
 */
export function parseHostTime(ts: string | null | undefined): number | null {
  if (!ts) return null
  let s = ts.trim()
  if (/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}/.test(s)) s = s.replace(' ', 'T')
  if (/^\d{4}-\d{2}-\d{2}T[\d:.]+$/.test(s)) s += 'Z'
  const ms = Date.parse(s)
  return Number.isNaN(ms) ? null : ms
}

const SUCCESS = new Set(['done', 'completed'])
const FAILURE = new Set(['failed', 'error'])

/** `success` / `failure` for a finished run, null for anything else. */
export function finishedKind(status: string): 'success' | 'failure' | null {
  if (SUCCESS.has(status)) return 'success'
  if (FAILURE.has(status)) return 'failure'
  return null
}

/**
 * Runs that finished (done / failed) after `sinceMs`, newest first. A run
 * without a usable timestamp is left out rather than guessed into the list.
 */
export function finishedSince<T extends RunLike>(runs: T[], sinceMs: number): T[] {
  const at = (r: T) => parseHostTime(r.updated_at) ?? parseHostTime(r.created_at)
  return runs
    .filter((r) => finishedKind(r.status) !== null)
    .filter((r) => {
      const t = at(r)
      return t !== null && t > sinceMs
    })
    .sort((a, b) => (at(b) ?? 0) - (at(a) ?? 0))
}

/** How far back the very first visit looks when nothing is stored yet. */
export const FIRST_VISIT_LOOKBACK_MS = 24 * 60 * 60 * 1000

/** Away for less than this (an app switch, a reload) is the same visit. */
export const VISIT_GAP_MS = 5 * 60 * 1000

/** What the Inbox remembers between visits (persisted by the caller). */
export interface VisitState {
  /** "Since last visit" anchor of the current visit (ms). */
  anchor: number | null
  /** When the user last left the app (ms). */
  lastLeftAt: number | null
  /** Finished runs opened since they finished (no unread dot). */
  read: string[]
}

const EMPTY_VISIT: VisitState = { anchor: null, lastLeftAt: null, read: [] }

/** Parse a stored {@link VisitState}; anything malformed reads as empty. */
export function parseVisitState(raw: string | null): VisitState {
  try {
    const o = raw ? JSON.parse(raw) : null
    if (!o || typeof o !== 'object') return { ...EMPTY_VISIT }
    const num = (v: unknown) => (typeof v === 'number' && Number.isFinite(v) ? v : null)
    return {
      anchor: num(o.anchor),
      lastLeftAt: num(o.lastLeftAt),
      read: Array.isArray(o.read) ? o.read.filter((x: unknown) => typeof x === 'string') : [],
    }
  } catch {
    return { ...EMPTY_VISIT }
  }
}

/**
 * The app came to the foreground at `now` (load or un-hide). Away long enough
 * → a new visit anchored at when the user left; otherwise the current visit
 * (and its anchor) carries on. A first-ever visit looks back a day.
 */
export function startVisit(state: VisitState, now: number): VisitState {
  const left = state.lastLeftAt
  if (left !== null && now - left >= VISIT_GAP_MS) return { ...state, anchor: left }
  return { ...state, anchor: state.anchor ?? left ?? now - FIRST_VISIT_LOOKBACK_MS }
}

/** The app went to the background at `now`. */
export function leaveVisit(state: VisitState, now: number): VisitState {
  return { ...state, lastLeftAt: now }
}

/** Cap on remembered read ids so storage cannot grow without bound. */
export const READ_MAX = 200

/** Mark `runId` read (newest last, capped). */
export function markRead(state: VisitState, runId: string): VisitState {
  if (state.read.includes(runId)) return state
  return { ...state, read: [...state.read, runId].slice(-READ_MAX) }
}

/** `then` relative to `now` as the largest whole unit, for
 * `Intl.RelativeTimeFormat` (value ≤ 0 = in the past). */
export function agoParts(
  then: number,
  now: number,
): { value: number; unit: 'second' | 'minute' | 'hour' | 'day' } {
  const sec = Math.max(0, Math.round((now - then) / 1000))
  if (sec < 60) return { value: -sec, unit: 'second' }
  const min = Math.floor(sec / 60)
  if (min < 60) return { value: -min, unit: 'minute' }
  const hr = Math.floor(min / 60)
  if (hr < 24) return { value: -hr, unit: 'hour' }
  return { value: -Math.floor(hr / 24), unit: 'day' }
}
