// Work Dashboard — Slack widget: which tracked threads need the user.
//
// Pure logic (no Vue / Tauri) for node:test. Inputs are the thread summaries
// (`list_slack_threads`) and the per-thread activity (`list_slack_thread_activity`:
// latest human message, latest message of a user marked as a customer).
//
// A thread is "waiting on me" when its latest human message (bots skipped)
// comes from a customer, "waiting on the customer" when someone else spoke
// last after a customer took part, and unlabelled when no customer ever spoke.
// Independently it is unread when it has new messages — or was never opened.

/** The `SlackThreadSummary` fields read here. */
export interface SlackThreadLite {
  id: string
  title: string
  projectId: string | null
  channel: string | null
  lastReplyTs: string | null
  lastReplyAt: string | null
  /** Messages newer than the last view; 0 when never viewed. */
  newCount: number
  preview: string
}

/** Mirrors the Rust `SlackActivityMessage`. */
export interface SlackActivityMessage {
  userId: string
  displayName: string
  ts: string
  postedAt: string | null
  isCustomer: boolean
  preview: string
}

/** Mirrors the Rust `SlackThreadActivity`. */
export interface SlackThreadActivity {
  threadId: string
  lastSeenTs: string | null
  lastHuman: SlackActivityMessage | null
  lastCustomer: SlackActivityMessage | null
}

export type SlackWaitLabel = 'waitingOnMe' | 'waitingOnCustomer'
export type WaitLevel = 'ok' | 'warn' | 'late'

/** A customer waiting longer than this is amber… */
export const SLACK_WAIT_WARN_MS = 4 * 3_600_000
/** …and longer than this red. */
export const SLACK_WAIT_LATE_MS = 24 * 3_600_000

export interface SlackDashboardRow {
  thread: SlackThreadLite
  /** null = no customer took part (only "new messages" is shown). */
  label: SlackWaitLabel | null
  unread: boolean
  /** Customer of the thread (latest one who wrote). */
  customer: SlackActivityMessage | null
  /** Since the customer's message, while waiting on me. */
  waitingMs: number | null
  waitLevel: WaitLevel
  /** Text to show: the customer's latest message, else the latest human one, else the parent. */
  preview: string
}

/** Message time in ms: ISO `postedAt`, else the Slack `ts` (seconds.micro). */
export function messageTime(m: Pick<SlackActivityMessage, 'postedAt' | 'ts'>): number | null {
  const iso = m.postedAt ? Date.parse(m.postedAt) : Number.NaN
  if (!Number.isNaN(iso)) return iso
  const secs = Number.parseFloat(m.ts)
  return Number.isFinite(secs) ? secs * 1000 : null
}

export function waitLevel(ms: number | null): WaitLevel {
  if (ms === null) return 'ok'
  if (ms > SLACK_WAIT_LATE_MS) return 'late'
  if (ms > SLACK_WAIT_WARN_MS) return 'warn'
  return 'ok'
}

/**
 * New messages since the last view. `newCount` is 0 for a thread never opened
 * (the backend has no `last_seen_ts` to compare with), so such a thread with
 * any message counts as unread.
 */
export function isUnread(thread: SlackThreadLite, activity: SlackThreadActivity | undefined): boolean {
  if (thread.newCount > 0) return true
  return !!activity && activity.lastSeenTs === null && !!(thread.lastReplyTs || activity.lastHuman)
}

export function classifySlackThread(
  thread: SlackThreadLite,
  activity: SlackThreadActivity | undefined,
  now: number,
): SlackDashboardRow {
  const last = activity?.lastHuman ?? null
  const customer = activity?.lastCustomer ?? null
  let label: SlackWaitLabel | null = null
  let waitingMs: number | null = null
  if (customer && last) {
    if (last.isCustomer) {
      label = 'waitingOnMe'
      const at = messageTime(last)
      waitingMs = at === null ? null : Math.max(0, now - at)
    } else {
      label = 'waitingOnCustomer'
    }
  }
  return {
    thread,
    label,
    unread: isUnread(thread, activity),
    customer,
    waitingMs,
    waitLevel: label === 'waitingOnMe' ? waitLevel(waitingMs) : 'ok',
    preview: customer?.preview || last?.preview || thread.preview,
  }
}

function rank(r: SlackDashboardRow): number {
  if (r.label === 'waitingOnMe') return 0
  if (r.unread) return 1
  return 2
}

/** Waiting on me (longest wait first) → unread → the rest, newest activity first. */
export function compareSlackRows(a: SlackDashboardRow, b: SlackDashboardRow): number {
  const r = rank(a) - rank(b)
  if (r !== 0) return r
  if (rank(a) === 0) {
    const w = (b.waitingMs ?? -1) - (a.waitingMs ?? -1)
    if (w !== 0) return w
  }
  const at = a.thread.lastReplyTs ?? ''
  const bt = b.thread.lastReplyTs ?? ''
  if (at !== bt) return bt.localeCompare(at, undefined, { numeric: true })
  return a.thread.id.localeCompare(b.thread.id)
}

export function needsAttention(r: SlackDashboardRow): boolean {
  return r.label === 'waitingOnMe' || r.unread
}

/** Every thread classified and ordered (see `compareSlackRows`). */
export function buildSlackRows(
  threads: SlackThreadLite[],
  activity: SlackThreadActivity[],
  now: number,
): SlackDashboardRow[] {
  const byId = new Map(activity.map((a) => [a.threadId, a]))
  return threads.map((t) => classifySlackThread(t, byId.get(t.id), now)).sort(compareSlackRows)
}
