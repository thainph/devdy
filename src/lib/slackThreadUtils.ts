// Pure helpers of the Slack threads window: Slack-style message grouping, the
// sidebar's workspace → channel tree, avatar initials / colors, the client-side
// thread-link check (spec §4) and small formatters.
// Pure module (relative type imports only): unit-tested with `node --test`.
import type { SlackMessage, SlackThreadSummary } from '../stores/slackThreads.ts'

// ── Message grouping ────────────────────────────────────────────────────────

/** Consecutive messages of one sender within this window collapse (no avatar / name). */
export const GROUP_WINDOW_MS = 5 * 60 * 1000

export interface MessageRow {
  message: SlackMessage
  /** seq 0: the thread's parent message. */
  isParent: boolean
  /** Same sender as the previous row, ≤ 5 minutes later: hide avatar + name. */
  continued: boolean
}

function senderKey(m: SlackMessage): string {
  return m.userId ?? (m.botId ? `bot:${m.botId}` : `name:${m.authorName}`)
}

/**
 * Rows in seq order. The parent never groups with anything, and the first
 * reply never groups with the parent (the "N replies" divider sits between).
 */
export function groupMessages(messages: SlackMessage[]): MessageRow[] {
  const sorted = messages.slice().sort((a, b) => a.seq - b.seq)
  const rows: MessageRow[] = []
  for (let i = 0; i < sorted.length; i++) {
    const message = sorted[i]
    const isParent = i === 0 && message.seq === 0
    const prev = i > 0 ? sorted[i - 1] : null
    const prevIsParent = i === 1 && rows[0]?.isParent
    const gap = prev ? Date.parse(message.postedAt) - Date.parse(prev.postedAt) : Infinity
    const continued =
      !isParent &&
      !!prev &&
      !prevIsParent &&
      senderKey(prev) === senderKey(message) &&
      gap >= 0 &&
      gap <= GROUP_WINDOW_MS
    rows.push({ message, isParent, continued })
  }
  return rows
}

// ── Sidebar tree ────────────────────────────────────────────────────────────

export interface ChannelGroup {
  key: string
  /** `#name`, or null for "Other". */
  label: string | null
  threads: SlackThreadSummary[]
}

export interface WorkspaceGroup {
  key: string
  /** Workspace name / domain, or null for "Other". */
  label: string | null
  channels: ChannelGroup[]
}

/**
 * workspace (`workspace` ?? `workspaceDomain`) → channel (`#channel` ??
 * `#channelId`); groups ordered by their smallest position, threads by position.
 */
export function groupThreads(threads: SlackThreadSummary[]): WorkspaceGroup[] {
  const sorted = threads.slice().sort((a, b) => a.position - b.position)
  const workspaces = new Map<string, WorkspaceGroup & { byChannel: Map<string, ChannelGroup> }>()
  for (const th of sorted) {
    const wsLabel = th.workspace?.trim() || th.workspaceDomain?.trim() || null
    const wsKey = `ws:${wsLabel ?? ''}`
    let ws = workspaces.get(wsKey)
    if (!ws) {
      ws = { key: wsKey, label: wsLabel, channels: [], byChannel: new Map() }
      workspaces.set(wsKey, ws)
    }
    const name = th.channel?.trim().replace(/^#/, '') || th.channelId || null
    const chLabel = name ? `#${name}` : null
    const chKey = `${wsKey}/ch:${chLabel ?? ''}`
    let ch = ws.byChannel.get(chKey)
    if (!ch) {
      ch = { key: chKey, label: chLabel, threads: [] }
      ws.byChannel.set(chKey, ch)
      ws.channels.push(ch)
    }
    ch.threads.push(th)
  }
  // Threads were visited by position, so insertion order already is min(position) order.
  return [...workspaces.values()].map(({ key, label, channels }) => ({ key, label, channels }))
}

// ── Avatars ─────────────────────────────────────────────────────────────────

/** Up to two initials of a display name ("Nguyen Thai" → "NT"), '?' when empty. */
export function initials(name: string): string {
  const words = name.trim().replace(/^@/, '').split(/[\s._-]+/).filter(Boolean)
  if (words.length === 0) return '?'
  const letters = words.length === 1 ? [...words[0]].slice(0, 2) : [[...words[0]][0], [...words[1]][0]]
  return letters.join('').toUpperCase()
}

/** Avatar backgrounds: the app's solid chip tones (components/ui/Badge.vue). */
export const AVATAR_TONES = [
  'bg-slate-600',
  'bg-blue-600',
  'bg-emerald-600',
  'bg-red-600',
  'bg-amber-600',
  'bg-violet-600',
  'bg-indigo-600',
]

/** Stable tone for a user id (djb2). */
export function avatarTone(key: string): string {
  let h = 5381
  for (let i = 0; i < key.length; i++) h = ((h << 5) + h + key.charCodeAt(i)) >>> 0
  return AVATAR_TONES[h % AVATAR_TONES.length]
}

// ── Thread link (spec §4) ───────────────────────────────────────────────────

export type LinkCheck = 'ok' | 'invalid_link' | 'dm_unsupported'

/** `^[a-z0-9][a-z0-9-]*(\.enterprise)?\.slack\.com$` (no port). */
function isSlackHost(host: string): boolean {
  if (!host.endsWith('.slack.com')) return false
  let label = host.slice(0, -'.slack.com'.length)
  if (label.endsWith('.enterprise')) label = label.slice(0, -'.enterprise'.length)
  return /^[a-z0-9][a-z0-9-]*$/.test(label)
}

/** `%XX` decoding of a query value (invalid escapes kept), `+` = space. */
function percentDecode(v: string): string {
  try {
    return decodeURIComponent(v.replace(/\+/g, ' '))
  } catch {
    return v.replace(/\+/g, ' ')
  }
}

/**
 * Early client-side check of a Slack thread link. A line-by-line port of
 * `parse_thread_link` (src-tauri/src/slack_threads/link.rs) so both sides give
 * the same verdict: DM is decided from the path before the query, a malformed
 * `thread_ts` is ignored (the path ts is used), a `cid` different from the path
 * channel (even empty) is invalid, and a host with a port is invalid. The
 * backend stays the authority.
 */
export function checkThreadLink(input: string): LinkCheck {
  let s = input.trim()
  if (s.startsWith('<') && s.endsWith('>')) s = s.slice(1, -1).trim()
  const sep = s.indexOf('://')
  if (sep === -1) return 'invalid_link'
  const scheme = s.slice(0, sep).toLowerCase()
  if (scheme !== 'https' && scheme !== 'http') return 'invalid_link'
  const rest = s.slice(sep + 3).split('#')[0]
  const authEnd = rest.search(/[/?]/)
  const host = (authEnd === -1 ? rest : rest.slice(0, authEnd)).toLowerCase()
  if (!isSlackHost(host)) return 'invalid_link'
  const tail = authEnd === -1 ? '' : rest.slice(authEnd)
  const q = tail.indexOf('?')
  let path = q === -1 ? tail : tail.slice(0, q)
  const query = q === -1 ? '' : tail.slice(q + 1)

  if (path.endsWith('/')) path = path.slice(0, -1)
  const m = /^\/archives\/([^/]*)\/p(.*)$/.exec(path)
  if (!m) return 'invalid_link'
  const [, channel, digits] = m
  if (!/^[CGD][A-Z0-9]{8,}$/.test(channel) || !/^\d{16}$/.test(digits)) return 'invalid_link'
  if (channel.startsWith('D')) return 'dm_unsupported'

  for (const pair of query.split('&').filter(Boolean)) {
    const eq = pair.indexOf('=')
    const key = eq === -1 ? pair : pair.slice(0, eq)
    const value = percentDecode(eq === -1 ? '' : pair.slice(eq + 1))
    if (key === 'cid' && value !== channel) return 'invalid_link'
  }
  return 'ok'
}

// ── Mark seen ───────────────────────────────────────────────────────────────

/**
 * Remembers which (thread, newest reply) pairs were already marked seen by this
 * window, so `mark_slack_thread_seen` fires at most once per pair: marking seen
 * emits `slack-threads://changed`, which reloads the thread, which must not mark
 * it again. A new reply (new `lastReplyTs`) makes the pair new again.
 */
export function createSeenGuard() {
  const marked = new Set<string>()
  return {
    /** True (and remembered) the first time for this pair; null ts = nothing to mark. */
    take(id: string, lastReplyTs: string | null): boolean {
      if (!lastReplyTs) return false
      const key = `${id}|${lastReplyTs}`
      if (marked.has(key)) return false
      marked.add(key)
      return true
    },
    /** Forget a pair (the request failed), so a later view retries. */
    release(id: string, lastReplyTs: string | null) {
      marked.delete(`${id}|${lastReplyTs}`)
    },
  }
}

// ── Formatters ──────────────────────────────────────────────────────────────

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`
  const kb = bytes / 1024
  if (kb < 1024) return `${kb.toFixed(kb < 10 ? 1 : 0)} KB`
  const mb = kb / 1024
  return `${mb.toFixed(mb < 10 ? 1 : 0)} MB`
}

/** `HH:mm` in local time. */
export function formatClock(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return ''
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

export function formatFullDate(iso: string | null): string {
  if (!iso) return ''
  const d = new Date(iso)
  return Number.isNaN(d.getTime()) ? iso : d.toLocaleString()
}
