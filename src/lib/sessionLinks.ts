// Links between sessions (runs) and the things a session works from — Slack
// threads, notes, todos — shared by every surface that shows or makes them:
// the Slack / Notes / Todos windows (LinkedSessionsMenu, "New session from
// this …"), the composer's CapturePicker (auto-link on pick) and RunView's
// linked-items menu.
//
// Sessions live in the main window, so a pop-out that wants to open one, or
// start a new one, asks it with WINDOW_OPEN_RUN / WINDOW_NEW_SESSION (handled
// in App.vue).
import { invoke } from '@/lib/tauri'

/** What a session can be linked to. */
export type LinkKind = 'slack' | 'note' | 'todo'

export interface LinkTarget {
  kind: LinkKind
  id: string
}

/**
 * `manual` (Link session menu) · `picker` (CapturePicker) · `created_from_thread`
 * / `created_from_item` ("New session from this …") · `captured` (a note / todo
 * written from inside the session).
 */
export type SessionLinkSource =
  | 'manual'
  | 'picker'
  | 'created_from_thread'
  | 'created_from_item'
  | 'captured'

/** A session linked to a thread / note / todo, with what its row needs. */
export interface SessionLink {
  runId: string
  /** The run's current project. */
  projectId: string
  source: SessionLinkSource
  linkedAt: string
  title: string | null
  runType: string
  refNumber: number | null
  status: 'fetched' | 'running' | 'done' | 'failed' | 'cancelled'
  engine: string
  lastActivityAt: string | null
}

export interface LinkRunResult {
  /** false = already linked (kept as is). */
  created: boolean
  /** The target had no project and took the run's. */
  projectSet: boolean
}

/** Emitted by a pop-out: show this session (its pop-out if any, else the main window). */
export const WINDOW_OPEN_RUN = 'window:open-run'

/**
 * Emitted by a pop-out: start a new session in `projectId`, its composer
 * prefilled with `text` (the target's reference, not sent), then link `target`
 * to it with source `created_from_thread` / `created_from_item`.
 */
export const WINDOW_NEW_SESSION = 'window:new-session'

export interface WindowOpenRun {
  projectId: string
  runId: string
}

export interface WindowNewSession {
  projectId: string
  text: string
  target: LinkTarget
}

/** Tailwind classes of a linked session's status dot. */
export function runStatusDot(status: string): string {
  switch (status) {
    case 'running':
      return 'bg-emerald-500 animate-pulse'
    case 'failed':
      return 'bg-destructive'
    case 'cancelled':
      return 'bg-amber-500'
    case 'done':
      return 'bg-muted-foreground/60'
    default:
      return 'border border-muted-foreground/60'
  }
}

const REF_TAG: Record<LinkKind, string> = { slack: 'Slack', note: 'Note', todo: 'Todo' }

/**
 * The one-line composer reference the AI resolves through the devdy MCP tools:
 * `[Slack] <title> (id: …)`, `[Note] <title> (id: …)`, `[Todo] (id: …)`.
 */
export function linkReference(kind: LinkKind, id: string, title?: string): string {
  return `[${REF_TAG[kind]}]${title ? ` ${title}` : ''} (id: ${id})`
}

/** Link `target` to a session (idempotent; the first source wins). */
export function linkTargetRun(
  target: LinkTarget,
  runId: string,
  source: SessionLinkSource,
): Promise<LinkRunResult> {
  return target.kind === 'slack'
    ? invoke<LinkRunResult>('link_slack_thread_run', { threadId: target.id, runId, source })
    : invoke<LinkRunResult>('link_item_run', { kind: target.kind, itemId: target.id, runId, source })
}

export function unlinkTargetRun(target: LinkTarget, runId: string): Promise<void> {
  return target.kind === 'slack'
    ? invoke('unlink_slack_thread_run', { threadId: target.id, runId })
    : invoke('unlink_item_run', { kind: target.kind, itemId: target.id, runId })
}

/** Sessions linked to `target` (runs that still exist), most recently active first. */
export function listTargetRuns(target: LinkTarget): Promise<SessionLink[]> {
  return target.kind === 'slack'
    ? invoke<SessionLink[]>('list_slack_thread_runs', { threadId: target.id })
    : invoke<SessionLink[]>('list_item_runs', { kind: target.kind, itemId: target.id })
}

/** The "New session from this …" link source of a kind. */
export function createdFromSource(kind: LinkKind): SessionLinkSource {
  return kind === 'slack' ? 'created_from_thread' : 'created_from_item'
}

/** A note / todo linked to a session (`list_run_items`). */
export interface RunLinkedItem {
  kind: 'note' | 'todo'
  id: string
  /** A note's own title ('' when none); '' for a todo. */
  title: string
  /** Note content / todo text. */
  text: string
  done: boolean
  projectId: string | null
  linkedAt: string
}

export function listRunItems(runId: string): Promise<RunLinkedItem[]> {
  return invoke<RunLinkedItem[]>('list_run_items', { runId })
}
