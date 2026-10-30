// Pure helpers + types for on-demand sync (Settings → Sync tabs, the Sync dialog and the sidebar
// popover). Types mirror the Rust contracts of docs/03-design/on-demand-sync.md (§2–§4, snake_case).
// No Tauri / Vue imports, so node:test can load this file directly.

// ---- §2 engine: sync_run / sync://run ----

/** `sync_run(source_device)` (Rust `SyncRunReport`). */
export interface SyncRunReport {
  pulled: number
  applied: number
  last_seq: number
}

/** `flushing`: the source is pushing its changes (`message` = how many so far). */
export type SyncRunPhase = 'requesting' | 'flushing' | 'pulling' | 'applying' | 'done' | 'error'

/** `sync://run` payload. */
export interface SyncRunEvent {
  phase: SyncRunPhase
  message?: string | null
}

// ---- §3 selective: sync_prefs_* ----

/** One project's machine-local sync settings (Rust `ProjectPref`). */
export interface ProjectPref {
  project_id: string
  name: string
  /** The root as recorded on the other devices. */
  synced_path: string
  /** Where it lives (or will live) on this machine; empty / null = no folder chosen. */
  local_path: string | null
  /** `local_path` exists on disk. */
  exists: boolean
  /** Mapped to a local folder (first-join match, or picked by hand). */
  mapped: boolean
  /** Its rows are applied here. */
  enabled: boolean
  /** Its folder is synced (needs `enabled`). */
  folder: boolean
  default_excludes: boolean
  /** Extra gitignore-style patterns. */
  excludes: string[]
  max_file_mb: number
  last_backup_at: string | null
  /** A session of this project is running on this machine (folder sync skips it). */
  running_here: boolean
}

/** `sync_prefs_get` (Rust `SyncPrefs`). */
export interface SyncPrefs {
  source_device: string | null
  categories_off: string[]
  projects: ProjectPref[]
}

/** `sync_project_pref_set(project_id, patch)` (Rust `ProjectPrefPatch`): only the set fields change. */
export interface ProjectPrefPatch {
  enabled?: boolean
  folder?: boolean
  default_excludes?: boolean
  excludes?: string[]
  max_file_mb?: number
  local_path?: string
}

// ---- §4 folder: sync_folder_preview / sync://folder ----

export interface PreviewFile {
  /** Relative to the project root. */
  path: string
  size: number
}

/** One git repository inside a project folder. */
export interface RepoPreview {
  /** Relative to the project root; '' = the root itself. */
  path: string
  /** Missing here: cloned from the bundle. */
  clone: boolean
  /** Current branch here (null = detached / new). */
  branch_from: string | null
  /** The source's current branch (null = detached). */
  branch_to: string | null
  /** The source's HEAD sha. */
  head: string | null
  /** HEAD here (null = no repo / unborn). */
  head_from?: string | null
  /** Local branches whose commits leave the branch (old tips kept on `devdy-sync-backup/…` branches). */
  branches_rewound?: string[]
  /** Commits this machine lacks, shipped as a bundle. */
  commits: number
  /** Staged files restored from the index patch. */
  staged: number
  /** Tracked files with uncommitted changes copied as-is. */
  dirty: number
  bundle_bytes: number
}

/** Something folder sync leaves alone (Rust `scan::SkipItem`). */
export interface SkipItem {
  /** Relative to the project root. */
  path: string
  /** `too_large` | `symlink_outside` | `submodule` | `git_link` | `non_utf8` | `repo_only_here` |
   *  `path_conflict` | `unsafe_path` | `no_repo_here` | `branch_in_worktree` | `lfs` |
   *  `changed_since_preview` | `ref_conflict` | other. */
  reason: string
  size?: number | null
}

/** `sync_folder_preview` item (Rust `FolderPreview`): what applying would do, nothing written yet. */
export interface FolderPreview {
  project_id: string
  name: string
  local_path: string | null
  /** The project folder does not exist here yet (created, repos cloned). */
  create: boolean
  /** Why it is skipped: `no_local_path` | `running_session` | other; null = not skipped. */
  skipped: string | null
  repos: RepoPreview[]
  /** Non-git files; each list is capped (the backend keeps the first 2000), `*_count` are exact. */
  overwrite: PreviewFile[]
  add: PreviewFile[]
  delete: PreviewFile[]
  overwrite_count?: number
  add_count?: number
  delete_count?: number
  /** This machine's uncommitted / receiver-only changes that get backed up first. */
  local_changes: string[]
  /** Files over `max_file_mb`, not transferred. */
  too_large: PreviewFile[]
  /** Other things left alone (submodules, symlinks outside, receiver-only repos, path conflicts…). */
  skipped_items?: SkipItem[]
  /** Bytes to download (file contents + bundles). */
  download_bytes: number
  error?: string | null
}

export type FolderPhase = 'manifest' | 'backup' | 'git' | 'files' | 'cleanup' | 'done' | 'error' | 'skipped'

/** `sync://folder` payload. */
export interface FolderProgress {
  project_id: string
  phase: FolderPhase
  done_bytes: number
  total_bytes: number
  files_done: number
  files_total: number
  message?: string | null
}

export const FOLDER_FINAL_PHASES: readonly FolderPhase[] = ['done', 'error', 'skipped']

export function isFolderFinal(p: Pick<FolderProgress, 'phase'> | null | undefined): boolean {
  return !!p && FOLDER_FINAL_PHASES.includes(p.phase)
}

// ---- Categories (§3) ----

export interface SyncCategory {
  id: string
  /** Nested under another category (only shown / effective while the parent is on). */
  parent?: string
}

/** Data categories in display order; `secrets` nests under `accounts`. */
export const SYNC_CATEGORIES: readonly SyncCategory[] = [
  { id: 'config' },
  { id: 'accounts' },
  { id: 'secrets', parent: 'accounts' },
  { id: 'servers' },
  { id: 'mcp' },
  { id: 'library' },
  { id: 'notes' },
  { id: 'slack' },
  { id: 'sessions' },
]

export const SYNC_CATEGORY_IDS: readonly string[] = SYNC_CATEGORIES.map((c) => c.id)

/** The new disabled list after turning `id` on / off (known ids only, display order). */
export function setCategory(off: readonly string[], id: string, enabled: boolean): string[] {
  const next = new Set(off.filter((c) => SYNC_CATEGORY_IDS.includes(c)))
  if (enabled) next.delete(id)
  else next.add(id)
  return SYNC_CATEGORY_IDS.filter((c) => next.has(c))
}

export function allCategoriesOn(off: readonly string[]): boolean {
  return !off.some((c) => SYNC_CATEGORY_IDS.includes(c))
}

/** Categories turned on by going from `before` to `after` (they get re-downloaded on the next Sync). */
export function categoriesEnabled(before: readonly string[], after: readonly string[]): string[] {
  return before.filter((c) => !after.includes(c))
}

// ---- Excludes (O6) ----

export const DEFAULT_EXCLUDES: readonly string[] = [
  'node_modules', 'target', 'dist', 'build', '.next', '.nuxt', '.venv', 'venv', '__pycache__', '.gradle', 'Pods',
  '.DS_Store',
]

export const DEFAULT_MAX_FILE_MB = 500

/** Textarea → patterns: one per line, trimmed, no blanks / `#` comments, no duplicates. */
export function parseExcludes(text: string): string[] {
  const out: string[] = []
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.trim()
    if (!line || line.startsWith('#') || out.includes(line)) continue
    out.push(line)
  }
  return out
}

export function excludesToText(list: readonly string[] | null | undefined): string {
  return (list ?? []).join('\n')
}

/** A whole number of MB in 1..100000, else null. */
export function parseMaxFileMb(v: unknown): number | null {
  const n = typeof v === 'number' ? v : Number(String(v ?? '').trim())
  if (!Number.isInteger(n) || n < 1 || n > 100_000) return null
  return n
}

// ---- Projects tab ----

/** Local folder state: there, will be created by folder sync, or a folder must be picked. */
export type LocalFolderState = 'ok' | 'create' | 'pick'

export function localFolderState(p: Pick<ProjectPref, 'local_path' | 'exists'>): LocalFolderState {
  if (!p.local_path?.trim()) return 'pick'
  return p.exists ? 'ok' : 'create'
}

/** Mapped to a different folder than on the source (show "mapped" next to the path). */
export function isRemapped(p: Pick<ProjectPref, 'local_path' | 'synced_path' | 'mapped'>): boolean {
  return p.mapped && !!p.local_path && p.local_path !== p.synced_path
}

/** Projects whose folders sync (selected + folder sync on). */
export function folderProjects(list: readonly ProjectPref[]): ProjectPref[] {
  return list.filter((p) => p.enabled && p.folder)
}

// ---- Formatting ----

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB']

/** `1536` → `1.5 KB`; one decimal under 10, none above; binary units. */
export function formatBytes(n: number | null | undefined): string {
  if (n == null || !Number.isFinite(n) || n <= 0) return '0 B'
  let v = n
  let i = 0
  while (v >= 1024 && i < UNITS.length - 1) {
    v /= 1024
    i++
  }
  if (i === 0) return `${Math.round(v)} B`
  const s = v < 10 ? v.toFixed(1).replace(/\.0$/, '') : String(Math.round(v))
  return `${s} ${UNITS[i]}`
}

/** Seconds → `45s`, `3m 20s`, `1h 5m`; null when unknown. */
export function formatEta(seconds: number | null | undefined): string | null {
  if (seconds == null || !Number.isFinite(seconds) || seconds < 0) return null
  const s = Math.ceil(seconds)
  if (s < 60) return `${s}s`
  if (s < 3600) {
    const r = s % 60
    return r ? `${Math.floor(s / 60)}m ${r}s` : `${Math.floor(s / 60)}m`
  }
  const m = Math.floor((s % 3600) / 60)
  return m ? `${Math.floor(s / 3600)}h ${m}m` : `${Math.floor(s / 3600)}h`
}

/** 0–100 (integer); a finished project is 100. */
export function folderPercent(p: Pick<FolderProgress, 'phase' | 'done_bytes' | 'total_bytes'> | null | undefined): number {
  if (!p) return 0
  if (p.phase === 'done') return 100
  if (!p.total_bytes || p.total_bytes <= 0) return 0
  return Math.max(0, Math.min(100, Math.floor((p.done_bytes / p.total_bytes) * 100)))
}

/**
 * Seconds left, from the average rate since `startedAt` (`startBytes` already done then):
 * null until a rate can be measured (< 1 s elapsed or nothing moved).
 */
export function etaSeconds(
  doneBytes: number,
  totalBytes: number,
  startedAt: number,
  now: number,
  startBytes = 0,
): number | null {
  const elapsed = (now - startedAt) / 1000
  const moved = doneBytes - startBytes
  if (elapsed < 1 || moved <= 0 || totalBytes <= 0) return null
  return Math.max(0, (totalBytes - doneBytes) / (moved / elapsed))
}

/** Overall percent over several projects (bytes-weighted), for the sidebar dot. */
export function overallPercent(list: ReadonlyArray<Pick<FolderProgress, 'phase' | 'done_bytes' | 'total_bytes'>>): number {
  if (!list.length) return 0
  const total = list.reduce((a, p) => a + Math.max(0, p.total_bytes || 0), 0)
  if (total <= 0) return Math.floor(list.reduce((a, p) => a + folderPercent(p), 0) / list.length)
  const done = list.reduce(
    (a, p) => a + (p.phase === 'done' ? Math.max(0, p.total_bytes || 0) : Math.min(p.done_bytes || 0, p.total_bytes || 0)),
    0,
  )
  return Math.max(0, Math.min(100, Math.floor((done / total) * 100)))
}

// ---- Preview summary ----

export interface BranchChange {
  repo: string
  from: string | null
  to: string | null
  /** Short sha. */
  sha: string
}

/** Local branches moved back (their commits stay on a `devdy-sync-backup/…` branch). */
export interface RewoundBranches {
  repo: string
  branches: string[]
}

export interface PreviewSummary {
  skipped: string | null
  create: boolean
  /** Repos cloned from the bundle. */
  clones: number
  /** Branch / HEAD moves, one per repo whose checkout changes. */
  branches: BranchChange[]
  commits: number
  staged: number
  overwrite: number
  add: number
  delete: number
  bytes: number
  /** This machine's own changes backed up first. */
  localChanges: number
  tooLarge: number
  rewound: RewoundBranches[]
  skippedItems: number
  /** Some file list holds fewer entries than its exact count. */
  truncated: boolean
}

/** Exact file count of a list: the backend's `*_count` when present, else the list length. */
export function fileCount(list: readonly PreviewFile[] | null | undefined, count: number | null | undefined): number {
  const n = list?.length ?? 0
  return typeof count === 'number' && Number.isFinite(count) ? Math.max(count, n) : n
}

/** The list shows fewer entries than there are (capped by the backend). */
export function isTruncated(list: readonly PreviewFile[] | null | undefined, count: number | null | undefined): boolean {
  return fileCount(list, count) > (list?.length ?? 0)
}

export const shortSha = (sha: string | null | undefined): string => (sha ?? '').slice(0, 7)

export function summarizePreview(p: FolderPreview): PreviewSummary {
  const repos = p.repos ?? []
  return {
    skipped: p.skipped ?? null,
    create: !!p.create,
    clones: repos.filter((r) => r.clone).length,
    branches: repos
      .filter((r) =>
        !r.clone && (r.branch_from !== r.branch_to || r.commits > 0 || (!!r.head_from && !!r.head && r.head_from !== r.head)))
      .map((r) => ({ repo: r.path, from: r.branch_from, to: r.branch_to, sha: shortSha(r.head) })),
    commits: repos.reduce((a, r) => a + (r.commits || 0), 0),
    staged: repos.reduce((a, r) => a + (r.staged || 0), 0),
    overwrite: fileCount(p.overwrite, p.overwrite_count),
    add: fileCount(p.add, p.add_count),
    delete: fileCount(p.delete, p.delete_count),
    bytes: p.download_bytes || 0,
    localChanges: p.local_changes?.length ?? 0,
    tooLarge: p.too_large?.length ?? 0,
    rewound: repos
      .filter((r) => (r.branches_rewound?.length ?? 0) > 0)
      .map((r) => ({ repo: r.path, branches: [...(r.branches_rewound ?? [])] })),
    skippedItems: p.skipped_items?.length ?? 0,
    truncated: isTruncated(p.overwrite, p.overwrite_count) || isTruncated(p.add, p.add_count) ||
      isTruncated(p.delete, p.delete_count),
  }
}

/** Nothing to change in this project (already identical). */
export function previewIsNoop(s: PreviewSummary): boolean {
  return !s.create && !s.clones && !s.branches.length && !s.commits && !s.staged && !s.overwrite && !s.add &&
    !s.delete && !s.bytes && !s.rewound.length
}

export function canApplyPreview(p: FolderPreview): boolean {
  return !p.skipped && !p.error
}

/** Footer totals over the checked projects. */
export function previewTotals(list: readonly FolderPreview[], selected: readonly string[]): {
  count: number
  bytes: number
  backup: boolean
  /** Exact file changes (overwrite + add + delete) over the checked projects. */
  files: number
} {
  const picked = list.filter((p) => canApplyPreview(p) && selected.includes(p.project_id))
  return {
    count: picked.length,
    bytes: picked.reduce((a, p) => a + (p.download_bytes || 0), 0),
    backup: picked.some((p) =>
      (p.local_changes?.length ?? 0) > 0 || fileCount(p.overwrite, p.overwrite_count) > 0 ||
      fileCount(p.delete, p.delete_count) > 0 ||
      (p.repos ?? []).some((r) => (r.branches_rewound?.length ?? 0) > 0)),
    files: picked.reduce(
      (a, p) => a + fileCount(p.overwrite, p.overwrite_count) + fileCount(p.add, p.add_count) +
        fileCount(p.delete, p.delete_count),
      0,
    ),
  }
}

/** Projects checked by default in the preview: every applicable one. */
export function defaultSelection(list: readonly FolderPreview[]): string[] {
  return list.filter(canApplyPreview).map((p) => p.project_id)
}

/**
 * Apply refused because the source's folder changed beyond the confirmed preview (the folder
 * worker aborts instead of writing more than was shown): the UI offers "Review" (preview again).
 */
export function isPlanChanged(message: unknown): boolean {
  const m = String(message ?? '').toLowerCase()
  return /(^|[^a-z_])(plan_changed|preview_required|plan_grew|preview_stale|preview_changed)($|[^a-z_])/.test(m) ||
    m.includes('beyond the confirmed preview') || m.includes('preview is out of date')
}

// ---- Last run (machine-local, kept by the store) ----

export interface ProjectRunError {
  project_id: string
  name: string
  message: string
}

export interface LastSyncRun {
  /** Unix millis. */
  at: number
  source_device: string
  ok: boolean
  error: string | null
  /** Rows applied from the source. */
  applied: number
  /** Folder sync: projects done / attempted (0 / 0 = data only). */
  projects_ok: number
  projects_total: number
  project_errors: ProjectRunError[]
}

/** Parse a stored LastSyncRun, null when missing or malformed. */
export function parseLastRun(raw: string | null | undefined): LastSyncRun | null {
  if (!raw) return null
  try {
    const v = JSON.parse(raw) as Partial<LastSyncRun>
    if (typeof v?.at !== 'number' || typeof v.source_device !== 'string') return null
    return {
      at: v.at,
      source_device: v.source_device,
      ok: !!v.ok,
      error: typeof v.error === 'string' ? v.error : null,
      applied: Number(v.applied) || 0,
      projects_ok: Number(v.projects_ok) || 0,
      projects_total: Number(v.projects_total) || 0,
      project_errors: Array.isArray(v.project_errors) ? v.project_errors : [],
    }
  } catch {
    return null
  }
}

/** A `local_path` joined with the folder-backup dir (O5). */
export function backupDir(localPath: string): string {
  return `${localPath.replace(/\/+$/, '')}/.devdy/sync-backup`
}

/** Unix millis of a backend timestamp (RFC 3339 text, or millis / seconds as text); null if unknown. */
export function parseTimestamp(v: string | number | null | undefined): number | null {
  if (v == null || v === '') return null
  const n = typeof v === 'number' ? v : Number(v)
  if (Number.isFinite(n)) return n < 1e12 ? n * 1000 : n
  const t = Date.parse(String(v))
  return Number.isFinite(t) ? t : null
}

/** Default source: the saved one if it is still another device of the group, else an online one, else the first. */
export function pickSource(
  saved: string | null | undefined,
  devices: ReadonlyArray<{ id: string; is_self: boolean }>,
  online: readonly string[] | null,
): string | null {
  const others = devices.filter((d) => !d.is_self)
  if (saved && others.some((d) => d.id === saved)) return saved
  return others.find((d) => online?.includes(d.id))?.id ?? others[0]?.id ?? null
}
