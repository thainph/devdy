// Pure logic behind the Remote Controller's Inbox ("needs attention") screen:
// the per-run queue of open permission prompts, the cross-run list the Inbox
// renders, and the sessions the user recently worked on (read state from the
// Host, so every device shows the same).
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
  /** When the last turn ended (a multi-turn session finishes every turn). */
  finished_at?: string | null
  /** When the user last saw the result (Host read state). */
  seen_at?: string | null
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

/** The run's last finish (its newest turn), or null while it has none. */
export function finishedAt(run: RunLike): number | null {
  return finishedKind(run.status) ? parseHostTime(run.finished_at) : null
}

/**
 * Finished after the user last saw it — on any device or at the desktop (the
 * Host's `seen_at`, so every phone agrees). Without read state (an older Host)
 * nothing is unread.
 */
export function isUnread(run: RunLike, hasSeenState: boolean): boolean {
  if (!hasSeenState) return false
  const done = finishedAt(run)
  if (done === null) return false
  const seen = parseHostTime(run.seen_at)
  return seen === null || done > seen
}

/** "Recent" covers sessions the user touched in this window. */
export const RECENT_WINDOW_MS = 3 * 24 * 60 * 60 * 1000
/** At most this many rows (unread ones always make it in). */
export const RECENT_MAX = 15

/**
 * The sessions the user recently worked on, most recent first: ordered by the
 * Host's last USER activity (`updated_at` = start, message, resume — not a
 * turn ending or an agent waking itself), within {@link RECENT_WINDOW_MS}.
 * An unread result is kept whatever its age, so nothing finished goes unseen.
 * The same Host data on every device gives the same list.
 */
export function recentRuns<T extends RunLike>(
  runs: readonly T[],
  nowMs: number,
  hasSeenState: boolean,
  max: number = RECENT_MAX,
): T[] {
  const activity = (r: T) => parseHostTime(r.updated_at) ?? parseHostTime(r.created_at) ?? 0
  const sorted = runs
    .filter((r) => isUnread(r, hasSeenState) || nowMs - activity(r) <= RECENT_WINDOW_MS)
    .sort((a, b) => activity(b) - activity(a))
  const unread = sorted.filter((r) => isUnread(r, hasSeenState))
  const room = Math.max(0, max - unread.length)
  const keep = new Set([...unread, ...sorted.filter((r) => !isUnread(r, hasSeenState)).slice(0, room)])
  return sorted.filter((r) => keep.has(r))
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

/**
 * One `list_pending` sync at a time. `begin` hands out a token (null while a
 * sync is in flight — its ack covers the new request too); only that token's
 * `end` closes it, so an ack from a sync abandoned by `reset` (a reconnect)
 * can never prune the queues of the current one.
 */
export function createSyncGate() {
  let token = 0
  let inFlight = false
  return {
    begin(): number | null {
      if (inFlight) return null
      inFlight = true
      token += 1
      return token
    },
    /** Whether `t` is the sync in flight (and so may apply its result). */
    end(t: number): boolean {
      if (!inFlight || t !== token) return false
      inFlight = false
      return true
    },
    reset(): void {
      inFlight = false
      token += 1
    },
  }
}
