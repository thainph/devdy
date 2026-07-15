import { defineStore } from 'pinia'
import { invoke } from '@/lib/tauri'
import { ref } from 'vue'
import type { ImageAttachment } from '@/lib/streamEvents'

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
  started_at: string | null
  finished_at: string | null
  created_at: string
  title: string | null
  pinned: boolean
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

  async function fetchRuns(project_id: string) {
    loading.value = true
    try {
      runs.value = await invoke<RunRecord[]>('list_runs', { projectId: project_id })
      loadedProjectId.value = project_id
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
  ): Promise<RunRecord> {
    return invoke<RunRecord>('fetch_pr', {
      projectId: project_id,
      repoId: repo_id,
      prNumber: pr_number,
      linkedIssue: linked_issue ?? null,
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
  }

  async function respondPermission(
    run_id: string,
    request_id: string,
    decision: 'allow' | 'deny' | 'ask',
    reason?: string,
    extra?: { answers?: Record<string, string>; response?: string },
  ): Promise<void> {
    await invoke('respond_permission', {
      payload: {
        run_id,
        request_id,
        decision,
        reason: reason ?? null,
        answers: extra?.answers ?? null,
        response: extra?.response ?? null,
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

  async function getRunLog(run_id: string): Promise<string> {
    const result = await invoke<{ content: string }>('get_run_log', { runId: run_id })
    return result.content
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
  }

  async function listProjectFiles(project_path: string): Promise<ProjectEntry[]> {
    return invoke<ProjectEntry[]>('list_project_files', { projectPath: project_path })
  }

  async function readProjectFile(project_path: string, file_path: string): Promise<FileContent> {
    return invoke<FileContent>('read_project_file', { projectPath: project_path, filePath: file_path })
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
  }

  async function deleteAllRuns(project_id: string): Promise<number> {
    const count = await invoke<number>('delete_all_runs', { projectId: project_id })
    runs.value = runs.value.filter(r => r.status === 'running')
    return count
  }

  // Keep the local list ordered the same way the backend does: pinned runs
  // first, then by created_at descending. Called after pin toggles so the row
  // jumps to/from the top without a round-trip refetch.
  function sortRuns() {
    runs.value.sort((a, b) => {
      if (a.pinned !== b.pinned) return a.pinned ? -1 : 1
      return b.created_at.localeCompare(a.created_at)
    })
  }

  async function renameRun(run_id: string, title: string): Promise<void> {
    await invoke('rename_run', { runId: run_id, title })
    const run = runs.value.find(r => r.id === run_id)
    if (run) run.title = title.trim() || null
  }

  async function setRunPinned(run_id: string, pinned: boolean): Promise<void> {
    await invoke('set_run_pinned', { runId: run_id, pinned })
    const run = runs.value.find(r => r.id === run_id)
    if (run) {
      run.pinned = pinned
      sortRuns()
    }
  }

  return {
    runs, loading, loadedProjectId,
    fetchRuns, fetchIssue, fetchPr,
    startRun, rerunRun, refetchRun, cancelRun, resumeRun,
    getRunLog, getRunLogPath, readRunInput,
    respondPermission, sendUserMessage, endRunInput,
    listProjectFiles, readProjectFile, createHandoffRun, createSessionRun,
    reconcileClaudeSessions, reconcileCodexSessions,
    deleteRun, deleteAllRuns, renameRun, setRunPinned,
  }
})
