import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref, reactive } from 'vue'
import type { ImageAttachment } from '@/lib/streamEvents'

/**
 * Fingerprint of the file(s) backing a run's log, from `get_run_log_revision`.
 *
 * `transcript_*` matters for mirrored Claude/Codex sessions: those can be
 * continued outside Devdy (the CLI, the IDE extension), and the conventional
 * `.devdy/runs/<id>.log` would NOT change when that happens. Watching only the
 * target file's own size/mtime would silently serve a stale log.
 */
export interface RunLogRevision {
  source: 'conventional' | 'transcript' | 'output' | null
  path: string | null
  size: number
  /** Nanoseconds. Second resolution is too coarse for a streaming run. */
  mtime_ns: number
  transcript_size: number | null
  transcript_mtime_ns: number | null
}

export interface RunLogPage {
  /** Complete records, oldest first — never a partial line. */
  records: string[]
  /** Byte offset the first record starts at; pass back as `before` to go older. */
  cursor: number
  has_more: boolean
  /** The `system` init record, present when the window doesn't reach the file top. */
  preamble: string | null
  revision: RunLogRevision
}

/** True when two fingerprints describe the same bytes, so a re-read is pointless. */
export function sameRunLogRevision(
  a: RunLogRevision | null,
  b: RunLogRevision | null,
): boolean {
  if (!a || !b) return false
  return (
    a.source === b.source &&
    a.path === b.path &&
    a.size === b.size &&
    a.mtime_ns === b.mtime_ns &&
    a.transcript_size === b.transcript_size &&
    a.transcript_mtime_ns === b.transcript_mtime_ns
  )
}

export interface RunRecord {
  id: string
  project_id: string
  repo_id: string | null
  run_type: string
  ref_number: number | null
  status: 'fetched' | 'running' | 'done' | 'failed' | 'cancelled'
  engine: string
  input_path: string | null
  output_path: string | null
  session_id: string | null
  claude_account_id: string | null
  started_at: string | null
  finished_at: string | null
  created_at: string
  /**
   * Last time the session was worked on — start/resume, a follow-up turn, a
   * finished turn, or a transcript touched by the CLI outside Devdy. `list_runs`
   * always fills it (COALESCE on the backend); the single-run endpoints may
   * return null, so read it through `runActivityAt`.
   */
  last_activity_at: string | null
  title: string | null
  pinned: boolean
}

/**
 * The timestamp the History list sorts and labels by. Mirrors the backend's
 * COALESCE so a record from an endpoint that doesn't compute it still lands in
 * the right place instead of falling back to the epoch.
 */
export function runActivityAt(run: RunRecord): string {
  return run.last_activity_at ?? run.finished_at ?? run.started_at ?? run.created_at
}

export interface RunOutput {
  run_id: string
  line: string
  is_stderr: boolean
  /** Diagnostic level for codex tracing/notes: error|warn|info|debug|trace. Absent for plain output. */
  level?: string
}

export interface ProjectEntry {
  path: string
  is_dir: boolean
}

export interface FileContent {
  path: string
  content: string
  truncated: boolean
}

export interface DirEntry {
  name: string
  path: string
  is_dir: boolean
  /** Covered by .gitignore — still listed, shown dimmed in the tree. */
  ignored: boolean
}

export interface HandoffResult {
  run: RunRecord
  context_path: string
}


export const useRunsStore = defineStore('runs', () => {
  const runs = ref<RunRecord[]>([])
  const loading = ref(false)
  // The project whose runs currently occupy the shared `runs` array. This is a
  // single global list (one project at a time), so with the multi-project tabs
  // feature a background project's run finishing must NOT refetch over the
  // foreground project's list — callers gate on this id to avoid clobbering the
  // view of whatever project is on screen.
  const loadedProjectId = ref<string | null>(null)

  // Metadata for every run we've loaded this session, keyed by run id and kept
  // across project switches. The `runs` array only ever holds ONE project's
  // runs (it gets replaced on every tab switch), so app-wide surfaces like the
  // active-runs dock can't rely on it to label a background project's run. They
  // read titles from this accumulated cache instead.
  const runMeta = reactive(new Map<string, RunRecord>())

  function rememberRuns(list: RunRecord[]) {
    for (const r of list) runMeta.set(r.id, r)
  }

  async function fetchRuns(project_id: string) {
    loading.value = true
    try {
      runs.value = await invoke<RunRecord[]>('list_runs', { projectId: project_id })
      loadedProjectId.value = project_id
      rememberRuns(runs.value)
    } finally {
      loading.value = false
    }
  }

  async function fetchIssue(project_id: string, repo_id: string, issue_number: number): Promise<RunRecord> {
    return invoke<RunRecord>('fetch_issue', { projectId: project_id, repoId: repo_id, issueNumber: issue_number })
  }

  async function fetchPr(
    project_id: string,
    repo_id: string,
    pr_number: number,
    linked_issue?: number,
    allow_missing_issue?: boolean,
  ): Promise<RunRecord> {
    return invoke<RunRecord>('fetch_pr', {
      projectId: project_id,
      repoId: repo_id,
      prNumber: pr_number,
      linkedIssue: linked_issue ?? null,
      allowMissingIssue: allow_missing_issue ?? false,
    })
  }

  async function startRun(
    run_id: string,
    engine_override?: string,
    permission_mode_override?: string,
    prompt_override?: string,
    model_override?: string,
    images?: ImageAttachment[],
    override_budget?: boolean,
  ): Promise<void> {
    await invoke('start_run', {
      payload: {
        run_id,
        engine_override: engine_override ?? null,
        permission_mode_override: permission_mode_override ?? null,
        prompt_override: prompt_override ?? null,
        model_override: model_override ?? null,
        images: images ?? [],
        override_budget: override_budget ?? false,
      },
    })
    touchRun(run_id)
  }

  async function respondPermission(
    run_id: string,
    request_id: string,
    decision: 'allow' | 'deny' | 'ask',
    reason?: string,
    extra?: { answers?: Record<string, string>; response?: string; remember?: boolean },
  ): Promise<void> {
    await invoke('respond_permission', {
      payload: {
        run_id,
        request_id,
        decision,
        reason: reason ?? null,
        answers: extra?.answers ?? null,
        response: extra?.response ?? null,
        // "Always" — lets an engine with its own session approval cache (codex)
        // stop re-asking instead of round-tripping to the UI every time.
        remember: extra?.remember ?? false,
      },
    })
  }

  async function sendUserMessage(
    run_id: string,
    content: string,
    images?: ImageAttachment[],
    override_budget?: boolean,
  ): Promise<void> {
    await invoke('send_user_message', {
      payload: { run_id, content, images: images ?? [], override_budget: override_budget ?? false },
    })
    touchRun(run_id)
  }

  async function endRunInput(run_id: string): Promise<void> {
    await invoke('end_run_input', { runId: run_id })
  }

  async function rerunRun(run_id: string): Promise<RunRecord> {
    return invoke<RunRecord>('rerun_run', { runId: run_id })
  }

  // Re-fetch fresh PR/issue content from GitHub and overwrite the run's input
  // markdown in place. The AI output/session are preserved. Returns the run.
  async function refetchRun(run_id: string): Promise<RunRecord> {
    return invoke<RunRecord>('refetch_run', { runId: run_id })
  }

  async function cancelRun(run_id: string): Promise<void> {
    await invoke('cancel_run', { runId: run_id })
  }

  /**
   * Whole log as one string. Real logs reach ~29MB, so this is deliberately NOT
   * the path the run viewer takes any more — use `getRunLogPage`. Kept for the
   * few actions that genuinely need everything at once (plain-text export, the
   * "files changed" scan), which load it on demand and drop it again.
   */
  async function getRunLog(run_id: string): Promise<string> {
    const result = await invoke<{ content: string }>('get_run_log', { runId: run_id })
    return result.content
  }

  /**
   * Cheap "did this log change?" fingerprint — no file read, no session upsert.
   * Compare with `sameRunLogRevision` before paying for a real read.
   */
  async function getRunLogRevision(run_id: string): Promise<RunLogRevision> {
    return invoke<RunLogRevision>('get_run_log_revision', { runId: run_id })
  }

  /**
   * Every tool call in the run that touched a file, pruned to just the fields
   * the "files changed" list reads. Scanned in Rust so the whole log never
   * crosses IPC — see `get_run_tool_records`.
   */
  async function getRunToolRecords(run_id: string): Promise<string[]> {
    const r = await invoke<{ records: string[] }>('get_run_tool_records', { runId: run_id })
    return r.records
  }

  /**
   * One window of log records, newest last. Omit `before` for the newest page;
   * pass a previous response's `cursor` to walk backwards.
   */
  async function getRunLogPage(
    run_id: string,
    before?: number,
    limit?: number,
  ): Promise<RunLogPage> {
    return invoke<RunLogPage>('get_run_log_page', { runId: run_id, before, limit })
  }

  // Absolute path of the log file backing a run/session (`.devdy/runs/<id>.log`
  // or the mirrored Claude/Codex transcript). Null when no log file exists yet.
  async function getRunLogPath(run_id: string): Promise<string | null> {
    const result = await invoke<{ path: string | null }>('get_run_log_path', { runId: run_id })
    return result.path
  }

  async function readRunInput(run_id: string): Promise<string> {
    return invoke<string>('read_run_input', { runId: run_id })
  }

  async function resumeRun(
    run_id: string,
    permission_mode_override?: string,
    model_override?: string,
    override_budget?: boolean,
  ): Promise<void> {
    await invoke('resume_run', {
      runId: run_id,
      permissionModeOverride: permission_mode_override ?? null,
      modelOverride: model_override ?? null,
      overrideBudget: override_budget ?? false,
    })
    touchRun(run_id)
  }

  async function listProjectFiles(project_path: string): Promise<ProjectEntry[]> {
    return invoke<ProjectEntry[]>('list_project_files', { projectPath: project_path })
  }

  async function readProjectFile(project_path: string, file_path: string): Promise<FileContent> {
    return invoke<FileContent>('read_project_file', { projectPath: project_path, filePath: file_path })
  }

  // Overwrite a project text file with edited contents from the file viewer.
  async function writeProjectFile(project_path: string, file_path: string, content: string): Promise<void> {
    await invoke('write_project_file', { projectPath: project_path, filePath: file_path, content })
  }

  // List one level of a directory (lazy tree loading for the file-tree panel).
  async function listDir(project_path: string, rel_dir: string): Promise<DirEntry[]> {
    return invoke<DirEntry[]>('list_dir', { projectPath: project_path, relDir: rel_dir })
  }

  // ── File-tree mutations (context-menu actions). Each returns the new relative
  // path (except delete) so the caller can refresh/expand the affected subtree.
  async function createDir(project_path: string, rel_dir: string, name: string): Promise<string> {
    return invoke<string>('create_dir', { projectPath: project_path, relDir: rel_dir, name })
  }
  async function createFile(project_path: string, rel_dir: string, name: string): Promise<string> {
    return invoke<string>('create_file', { projectPath: project_path, relDir: rel_dir, name })
  }
  async function renameEntry(project_path: string, rel_path: string, new_name: string): Promise<string> {
    return invoke<string>('rename_entry', { projectPath: project_path, relPath: rel_path, newName: new_name })
  }
  async function deleteEntry(project_path: string, rel_path: string): Promise<void> {
    await invoke('delete_entry', { projectPath: project_path, relPath: rel_path })
  }
  async function copyEntry(project_path: string, src_rel: string, dest_dir: string): Promise<string> {
    return invoke<string>('copy_entry', { projectPath: project_path, srcRel: src_rel, destDir: dest_dir })
  }
  async function moveEntry(project_path: string, src_rel: string, dest_dir: string): Promise<string> {
    return invoke<string>('move_entry', { projectPath: project_path, srcRel: src_rel, destDir: dest_dir })
  }

  async function createSessionRun(project_id: string, engine_override?: string): Promise<RunRecord> {
    return invoke<RunRecord>('create_session_run', {
      projectId: project_id,
      engineOverride: engine_override ?? null,
    })
  }

  // Mirror every Claude session for the project's working dir into runs
  // (importing externally-created ones, refreshing existing ones). Returns the
  // number of runs imported or refreshed.
  async function reconcileClaudeSessions(project_id: string): Promise<number> {
    return invoke<number>('reconcile_claude_sessions', { projectId: project_id })
  }

  // Same, for Codex rollout sessions (matched to the project by cwd).
  async function reconcileCodexSessions(project_id: string): Promise<number> {
    return invoke<number>('reconcile_codex_sessions', { projectId: project_id })
  }

  async function createHandoffRun(
    run_id: string,
    target_engine: string,
    transcript: string,
  ): Promise<HandoffResult> {
    return invoke<HandoffResult>('create_handoff_run', {
      runId: run_id,
      targetEngine: target_engine,
      transcript,
    })
  }

  async function deleteRun(run_id: string): Promise<void> {
    await invoke('delete_run', { runId: run_id })
    runs.value = runs.value.filter(r => r.id !== run_id)
    runMeta.delete(run_id)
  }

  async function deleteAllRuns(project_id: string): Promise<number> {
    const count = await invoke<number>('delete_all_runs', { projectId: project_id })
    runs.value = runs.value.filter(r => r.status === 'running')
    return count
  }

  // Keep the local list ordered the same way the backend does: pinned runs
  // first, then by last activity descending. Called after pin toggles and after
  // `touchRun` so the row jumps without a round-trip refetch.
  function sortRuns() {
    runs.value.sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
      return runActivityAt(b).localeCompare(runActivityAt(a))
    })
  }

  /**
   * Mark a run as just-interacted-with locally. The backend stamps the same
   * thing, but only the next `list_runs` would reveal it — this moves the row to
   * the top the instant the user starts/resumes a run or sends a turn.
   */
  function touchRun(run_id: string) {
    const now = new Date().toISOString()
    const run = runs.value.find(r => r.id === run_id)
    if (run) {
      run.last_activity_at = now
      sortRuns()
    }
    const cached = runMeta.get(run_id)
    if (cached) cached.last_activity_at = now
  }

  async function renameRun(run_id: string, title: string): Promise<void> {
    await invoke('rename_run', { runId: run_id, title })
    const next = title.trim() || null
    const run = runs.value.find(r => r.id === run_id)
    if (run) run.title = next
    const cached = runMeta.get(run_id)
    if (cached) cached.title = next
  }

  async function setRunPinned(run_id: string, pinned: boolean): Promise<void> {
    await invoke('set_run_pinned', { runId: run_id, pinned })
    const run = runs.value.find(r => r.id === run_id)
    if (run) {
      run.pinned = pinned
      sortRuns()
    }
  }

  async function setRunClaudeAccount(run_id: string, account_id: string | null): Promise<void> {
    await invoke('set_run_claude_account', { runId: run_id, accountId: account_id })
    const next = account_id?.trim() || null
    const run = runs.value.find(r => r.id === run_id)
    if (run) run.claude_account_id = next
    const cached = runMeta.get(run_id)
    if (cached) cached.claude_account_id = next
  }

  return {
    runs, loading, loadedProjectId, runMeta,
    fetchRuns, fetchIssue, fetchPr,
    startRun, rerunRun, refetchRun, cancelRun, resumeRun,
    getRunLog, getRunLogRevision, getRunLogPage, getRunLogPath, getRunToolRecords, readRunInput,
    respondPermission, sendUserMessage, endRunInput,
    listProjectFiles, readProjectFile, writeProjectFile, listDir,
    createDir, createFile, renameEntry, deleteEntry, copyEntry, moveEntry,
    createHandoffRun, createSessionRun,
    reconcileClaudeSessions, reconcileCodexSessions,
    deleteRun, deleteAllRuns, renameRun, setRunClaudeAccount, setRunPinned, touchRun,
  }
})
