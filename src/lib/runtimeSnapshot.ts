// Runtime Dashboard — the backend contract of `get_runtime_snapshot`
// (spec: runtime-dashboard-contract.md). The Rust side mirrors these types with
// `#[serde(rename_all = "camelCase")]`; keep both in step.
//
// Pure types + names only (no Vue / Tauri) so node:test can import it.

/** Tauri command returning a {@link RuntimeSnapshot}. */
export const CMD_RUNTIME_SNAPSHOT = 'get_runtime_snapshot'

export interface RuntimeSnapshot {
  /** RFC3339. */
  generatedAt: string
  /** One per LIVE run in the RunRegistry (same ids as `list_active_runs`). */
  runs: RuntimeRun[]
  resources: RuntimeResources
  today: RuntimeToday
  external: ExternalSession[]
}

export interface RuntimeRun {
  runId: string
  /** Last stream event drained from the sidecar. */
  lastEventAt: string | null
  /** tool_use without a tool_result yet (latest one); null when none. */
  currentTool: RuntimeTool | null
  /** Exactly 20 buckets, oldest first, last = current minute. */
  eventsPerMinute: number[]
  /** Latest per-message assistant usage (input + cache read + cache creation), reset on compaction. */
  contextTokens: number | null
  /** Model context window if known, else null (the frontend falls back to its own table). */
  contextWindow: number | null
  /** Open AskUserQuestion waiting for the user, if any. */
  awaitingQuestion: AwaitingQuestion | null
  /** Sum over the run's process tree, 0 if unknown. */
  cpuPercent: number
  rssKb: number
}

export interface RuntimeTool {
  /** e.g. "Bash", "Edit", "mcp__devdy__todos_list". */
  name: string
  /** Short human text: command / file path / pattern, max ~120 chars, no secrets. */
  summary: string
  /** RFC3339. */
  startedAt: string
}

export interface AwaitingQuestion {
  question: string
  /** Labels, may be empty. */
  options: string[]
  since: string
}

export interface RuntimeResources {
  /** Total of all Devdy-tracked processes. */
  cpuPercent: number
  rssKb: number
  /** Machine total, 0 if unknown. */
  totalMemKb: number
  processCount: number
  orphanCount: number
  /** null = no source. */
  staleWorktreeCount: number | null
  /** Last ~12 samples (one per snapshot call), oldest first. */
  cpuHistory: number[]
}

export interface RuntimeToday {
  /** Runs that reached done today (local day). */
  finished: number
  /** Runs that reached failed today. */
  failed: number
  /** Latest 3 finished / failed, newest first. */
  recent: RecentRun[]
}

export interface RecentRun {
  runId: string
  projectId: string
  projectName: string
  title: string | null
  status: 'done' | 'failed' | 'cancelled'
  /** Short reason for failed, no secrets. */
  error: string | null
  finishedAt: string
}

export interface ExternalSession {
  /** Stable id (transcript path hash or session id). */
  id: string
  engine: 'claude' | 'codex'
  /** e.g. "claude · lunchbox" (cwd basename). */
  title: string
  cwd: string | null
  /** Matched Devdy project if cwd is inside one. */
  projectId: string | null
  projectName: string | null
  /** Only sessions with activity in the last ~2 minutes are "running". */
  lastActivityAt: string
  /** For "Open transcript"; read-only. */
  transcriptPath: string
}

/** Number of per-minute activity buckets in {@link RuntimeRun.eventsPerMinute}. */
export const ACTIVITY_BUCKETS = 20
