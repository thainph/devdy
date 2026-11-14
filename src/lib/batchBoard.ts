// Batch Board (views/BatchBoardView.vue): types of the board_* commands and the
// pure rules that sort a batch's sessions into lanes.

/**
 * Emitted by a pop-out (the todo list) with `{ path }`; the main window focuses
 * itself and opens the board there (handled in App.vue).
 */
export const BOARD_OPEN_ROUTE = 'board:open-route'

/** A todo offered in the "Hand to AI" dialog. */
export interface BatchCandidate {
  id: string
  title: string
  projectId: string | null
}

/** The board's route for a batch. */
export const boardPath = (batchRunId: string) => `/board?batch=${encodeURIComponent(batchRunId)}`

export interface BoardTodo {
  id: string
  text: string
  status: string
  done: boolean
}

export interface BoardWorktree {
  id: string
  projectId: string
  path: string
  branch: string
  baseRef: string
  todoId: string | null
  createdAt: string
  removedAt: string | null
}

export interface DiffStat {
  commits: number
  files: number
  insertions: number
  deletions: number
  uncommitted: number
}

export interface BoardItem {
  runId: string
  title: string | null
  role: 'conductor' | 'worker'
  projectId: string
  projectName: string
  engine: string
  /** runs.status: fetched | running | done | failed | cancelled */
  status: string
  createdAt: string
  startedAt: string | null
  finishedAt: string | null
  costUsd: number
  todo: BoardTodo | null
  worktree: BoardWorktree | null
  diff: DiffStat | null
}

export interface BoardQueuedTodo {
  todo: BoardTodo
  projectId: string | null
  projectName: string | null
}

export interface BoardData {
  items: BoardItem[]
  queued: BoardQueuedTodo[]
}

export interface BatchSummary {
  runId: string
  title: string | null
  projectId: string
  projectName: string
  status: string
  createdAt: string
  workerCount: number
}

/** Live state of a run, from the liveRuns store. */
export interface LiveState {
  /** Pending permission prompts / questions. */
  pending: number
  /** Liveness notices (quiet for long, tool running long…). */
  attention: number
  /** Live status when the store knows the run (overrides the DB snapshot). */
  status?: string
}

export type Lane = 'needs' | 'alert' | 'running' | 'review' | 'failed' | 'closed'

/** Lanes in board order: what needs the user first. */
export const LANES: Lane[] = ['needs', 'alert', 'running', 'review', 'failed', 'closed']

/**
 * Which lane a worker belongs in. A prompt beats everything (the session is
 * blocked on the user); a finished worker waits for review until its todo is
 * done or its worktree is gone.
 */
export function laneOf(item: BoardItem, live: LiveState | undefined): Lane {
  if (live && live.pending > 0) return 'needs'
  const status = live?.status ?? item.status
  if (status === 'running') return live && live.attention > 0 ? 'alert' : 'running'
  if (item.todo?.done) return 'closed'
  if (item.worktree?.removedAt) return 'closed'
  if (status === 'failed' || status === 'cancelled') return 'failed'
  if (status === 'fetched') return 'running'
  return 'review'
}

/** Workers grouped by lane, each lane in spawn order. */
export function groupByLane(
  items: BoardItem[],
  liveOf: (runId: string) => LiveState | undefined,
): Record<Lane, BoardItem[]> {
  const out = Object.fromEntries(LANES.map((l) => [l, [] as BoardItem[]])) as Record<Lane, BoardItem[]>
  for (const item of items) {
    if (item.role === 'conductor') continue
    out[laneOf(item, liveOf(item.runId))].push(item)
  }
  return out
}

/** `[implementer] Fix login…` → `implementer`; null without a role prefix. */
export function roleLabel(title: string | null): string | null {
  const m = /^\[([^\]]{1,40})\]/.exec(title ?? '')
  return m ? m[1] : null
}

/** A single line saying what the session did last, from its stream entries. */
export function lastActivity(
  entries: ReadonlyArray<{ kind: string; text?: string; name?: string; displayName?: string }>,
  max = 140,
): string | null {
  for (let i = entries.length - 1; i >= 0; i--) {
    const e = entries[i]
    let line: string | null = null
    if (e.kind === 'tool') line = `⚙ ${e.displayName || e.name || 'tool'}`
    else if ((e.kind === 'text' || e.kind === 'result' || e.kind === 'error') && e.text?.trim()) line = e.text
    if (line) {
      const flat = line.replace(/\s+/g, ' ').trim()
      return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat
    }
  }
  return null
}

/** `+12 −3 · 4 files · 2 commits · 1 uncommitted` parts, empty ones dropped. */
export function diffParts(d: DiffStat | null): DiffStat | null {
  if (!d) return null
  if (!d.files && !d.commits && !d.uncommitted) return null
  return d
}

/** Commit message for "commit all": the todo's first line. */
export function commitMessageFor(item: BoardItem): string {
  const first = (item.todo?.text ?? item.title ?? '').split('\n')[0].trim()
  return first.slice(0, 72) || 'Devdy task'
}
