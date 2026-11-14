// Focus (docs/focus-board-spec.md): the backend contract — types mirroring the
// Rust `focus_board` / `list_todos` / `focus_settings_*` payloads — plus the
// pure helpers of the "+ Todo" forms: estimate parsing, "✨ Suggest", source
// links. Scoring and bucketing are done by the backend (BR-04/05).
//
// Kept free of Vue / Tauri / `@/` imports so it can be unit-tested with
// `node --test` (src/lib/__tests__/focus.test.mjs).

// ── Contract ────────────────────────────────────────────────────────────────

export type TodoPriority = 1 | 2 | 3
export type TodoStatus = 'todo' | 'doing' | 'waiting'
export type TodoSourceType = 'slack' | 'github' | 'gitlab' | 'run' | 'url'
export type EstimateSource = 'user' | 'ai'

/** A row of `list_todos` (snake_case, as the backend sends it). */
export interface FocusTodo {
  id: string
  text: string
  done: boolean
  position: number
  created_at: string
  project_id: string | null
  /** Local `YYYY-MM-DD`; null = no deadline. */
  due_date: string | null
  /** Local `HH:MM`; only with a `due_date`. */
  due_time: string | null
  priority: TodoPriority
  estimate_min: number | null
  estimate_source: EstimateSource | null
  status: TodoStatus
  /** RFC3339. */
  snoozed_until: string | null
  remind_at: string | null
  source_type: TodoSourceType | null
  /** Normalised source (BR-02), e.g. `github:owner/repo#12`. */
  source_ref: string | null
  source_url: string | null
  started_at: string | null
  completed_at: string | null
  actual_min: number | null
  ai_plan: string | null
  ai_analyzed_at: string | null
  updated_at: string | null
  linked_run_count: number
}

/** `update_todo_fields` patch: absent = unchanged, `null` = clear. */
export interface TodoPatch {
  text?: string
  project_id?: string | null
  due_date?: string | null
  due_time?: string | null
  priority?: TodoPriority
  estimate_min?: number | null
  estimate_source?: EstimateSource | null
  status?: TodoStatus
  snoozed_until?: string | null
  remind_at?: string | null
  ai_plan?: string | null
  actual_min?: number | null
  done?: boolean
}

/** `add_todo` args (camelCase, Tauri converts). */
export interface AddTodoInput {
  text: string
  projectId?: string | null
  runId?: string | null
  dueDate?: string | null
  dueTime?: string | null
  priority?: TodoPriority | null
  estimateMin?: number | null
  sourceUrl?: string | null
}

export type FocusBucket =
  | 'done'
  | 'snoozed'
  | 'waiting'
  | 'doing'
  | 'overdue'
  | 'today'
  | 'tomorrow'
  | 'this_week'
  | 'later'
  | 'no_due'

/**
 * One "why here" fact (BR-05), as the backend's `focus::score` emits it:
 *  - `doing`, `waiting`, `no_due`, `done` — no value (null);
 *  - `snoozed`, `due` — local time `YYYY-MM-DDTHH:MM` (snoozed may be null);
 *  - `overdue` — wall-clock minutes past due;
 *  - `work_left`, `late_by` — working minutes;
 *  - `estimate`, `estimate_missing` — minutes;
 *  - `priority` — 1 | 3 (only when ≠ 2);
 *  - `age_days` — days open (only when ≥ 2).
 * Unknown kinds are rendered generically.
 */
export type FocusReasonKind =
  | 'doing'
  | 'waiting'
  | 'no_due'
  | 'done'
  | 'snoozed'
  | 'due'
  | 'overdue'
  | 'work_left'
  | 'late_by'
  | 'estimate'
  | 'estimate_missing'
  | 'priority'
  | 'age_days'

export interface FocusReason {
  kind: FocusReasonKind | (string & {})
  value?: string | number | null
}

export interface FocusItem {
  todo: FocusTodo
  bucket: FocusBucket
  score: number
  reasons: FocusReason[]
  /** RFC3339; null without a deadline. */
  start_by: string | null
  effective_due: string | null
  effective_estimate_min: number
  estimate_missing: boolean
}

export interface BusyBlock {
  start: string
  end: string
  title?: string
}

export interface FocusCapacity {
  need_min: number
  free_min: number
  busy: BusyBlock[]
}

export interface FocusCalibration {
  /** Applied to estimates (1.0 when calibration is off). */
  factor: number
  samples: number
  /** Settings switch (BR-10). */
  enabled: boolean
  /** The factor computed from history, whether applied or not. */
  measured: number
}

export interface FocusBoard {
  now: string
  items: FocusItem[]
  /** Ids of "Làm ngay" (at most 3), in order. */
  top: string[]
  /** Today's load against Google Calendar (BR-08); null when unavailable. */
  capacity: FocusCapacity | null
  /** Why `capacity` is null: `no_account`, `loading`, else a Calendar error. */
  capacity_error?: string | null
  calibration: FocusCalibration
}

export interface FocusSettings {
  work_start: string
  work_end: string
  /** ISO weekdays, 1 = Monday … 7 = Sunday. */
  work_days: number[]
  morning_time: string
  notify_enabled: boolean
  notify_kinds: Record<string, boolean>
  calendar_account_id: string | null
  calendar_ids: string[]
  calibration_enabled: boolean
  /** '' = follow the translate engine / model. */
  ai_engine: string
  ai_model: string
}

export const DEFAULT_FOCUS_SETTINGS: FocusSettings = {
  work_start: '09:00',
  work_end: '18:00',
  work_days: [1, 2, 3, 4, 5],
  morning_time: '09:00',
  notify_enabled: true,
  notify_kinds: {},
  calendar_account_id: null,
  calendar_ids: [],
  calibration_enabled: true,
  ai_engine: '',
  ai_model: '',
}

/** Emitted by the backend after every Focus write. */
export const FOCUS_CHANGED = 'focus:changed'
/** Backend → main window: open the Todos window on a todo (reminder click, tray row). */
export const FOCUS_OPEN_WINDOW = 'focus:open-window'
export interface FocusOpenTodo {
  todo_id: string | null
}

/** Reminder kinds (spec §3.4), the keys of `notify_kinds`. */
export const NOTIFY_KINDS = ['morning', 'start_by', 'due_soon', 'overdue', 'remind', 'snooze_end'] as const
export type NotifyKind = (typeof NOTIFY_KINDS)[number]

/** Every reminder kind, on unless the backend says otherwise. */
export function notifyKindsOf(kinds: Record<string, boolean> | null | undefined): Record<NotifyKind, boolean> {
  const out = {} as Record<NotifyKind, boolean>
  for (const k of NOTIFY_KINDS) out[k] = kinds?.[k] ?? true
  return out
}

// ── "+ Todo" draft ──────────────────────────────────────────────────────────

/** The capture form's fields (TodoCaptureFields), as typed. */
export interface TodoDraft {
  text: string
  /** '' = no project. */
  projectId: string
  /** `YYYY-MM-DD` or ''. */
  dueDate: string
  /** `HH:MM` or ''; ignored without a date. */
  dueTime: string
  priority: TodoPriority
  /** Free text, see parseEstimate. */
  estimate: string
  sourceUrl: string
}

/** What a "+ Todo" entry point knows about its context. */
export interface TodoPrefill {
  text?: string
  projectId?: string | null
  /** Session the todo is captured from (a `captured` link). */
  runId?: string | null
  sourceUrl?: string | null
  dueDate?: string | null
}

export function emptyTodoDraft(over: Partial<TodoDraft> = {}): TodoDraft {
  return { text: '', projectId: '', dueDate: '', dueTime: '', priority: 2, estimate: '', sourceUrl: '', ...over }
}

/** `add_todo` args of a draft, or null when it can't be saved (no text / unreadable estimate). */
export function draftToInput(d: TodoDraft, runId: string | null = null): AddTodoInput | null {
  const text = d.text.trim()
  const estimate = parseEstimate(d.estimate)
  if (!text || estimate === undefined) return null
  return {
    text,
    projectId: d.projectId || null,
    runId,
    dueDate: d.dueDate || null,
    dueTime: d.dueDate && d.dueTime ? d.dueTime : null,
    priority: d.priority,
    estimateMin: estimate,
    sourceUrl: d.sourceUrl.trim() || null,
  }
}

// ── Estimates ───────────────────────────────────────────────────────────────

/** BR-01 bounds of `estimate_min`. */
export const ESTIMATE_MIN = 5
export const ESTIMATE_MAX = 2400

/**
 * Parse an estimate typed as `30m`, `2h`, `1h30`, `1h30m`, `1.5h` or bare
 * minutes (`90`). Returns minutes, `null` for an empty field, or `undefined`
 * when it can't be read or falls outside [5, 2400] (BR-01).
 */
export function parseEstimate(input: string | null | undefined): number | null | undefined {
  const s = (input ?? '').trim().toLowerCase().replace(/\s+/g, '').replace(',', '.')
  if (!s) return null
  let min: number
  let m: RegExpExecArray | null
  if ((m = /^(\d+)$/.exec(s))) min = +m[1]
  else if ((m = /^(\d+(?:\.\d+)?)(?:m|min|p|ph|phút)$/.exec(s))) min = +m[1]
  else if ((m = /^(\d+(?:\.\d+)?)(?:h|g|giờ)$/.exec(s))) min = +m[1] * 60
  else if ((m = /^(\d+)(?:h|g)(\d{1,2})(?:m|min|p|ph)?$/.exec(s))) min = +m[1] * 60 + +m[2]
  else return undefined
  min = Math.round(min)
  return min >= ESTIMATE_MIN && min <= ESTIMATE_MAX ? min : undefined
}

/** `30m`, `2h`, `1h30` — the inverse of parseEstimate. */
export function formatEstimate(min: number | null | undefined): string {
  if (min == null || !Number.isFinite(min)) return ''
  const total = Math.max(0, Math.round(min))
  const h = Math.floor(total / 60)
  const m = total % 60
  if (h === 0) return `${m}m`
  if (m === 0) return `${h}h`
  return `${h}h${String(m).padStart(2, '0')}`
}

// ── AI (P3, on demand) ──────────────────────────────────────────────────────

/** `focus_ai_suggest` result: fills the "+ Todo" form, nothing is saved. */
export interface AiSuggestion {
  title: string
  /** Only with a deadline quoted from the source (`due_evidence`). */
  due_date: string | null
  due_time: string | null
  due_evidence: string | null
  priority: TodoPriority
  estimate_min: number
  reason: string
}

/** Command errors are `"<code>: <detail>"`; `cancelled` is bare. */
export function aiErrorCode(e: unknown): { code: string | null; detail: string } {
  const msg = String(e instanceof Error ? e.message : e)
  if (msg === 'cancelled') return { code: 'cancelled', detail: '' }
  const m = /^([a-z_]+):\s*(.*)$/s.exec(msg)
  return m ? { code: m[1], detail: m[2] } : { code: null, detail: msg }
}

/** The form after "✨ Suggest": AI values fill the fields; the user's own text / deadline win when set. */
export function applySuggestion(d: TodoDraft, s: AiSuggestion): TodoDraft {
  const next = { ...d, priority: s.priority, estimate: formatEstimate(s.estimate_min) }
  if (!d.text.trim()) next.text = s.title
  if (!d.dueDate && s.due_date) {
    next.dueDate = s.due_date
    next.dueTime = s.due_time ?? ''
  }
  return next
}

// ── Sources ─────────────────────────────────────────────────────────────────

/** Source type of a link, guessed client-side for the "+ Todo" chip (the backend normalises for real, BR-02). */
export function guessSourceType(url: string | null | undefined): TodoSourceType | null {
  const s = (url ?? '').trim()
  if (!s) return null
  let host = ''
  try {
    host = new URL(s).hostname.toLowerCase()
  } catch {
    return 'url'
  }
  if (host === 'slack.com' || host.endsWith('.slack.com')) return 'slack'
  if (host === 'github.com' || host === 'www.github.com') return 'github'
  if (host === 'gitlab.com' || host.startsWith('gitlab.')) return 'gitlab'
  return 'url'
}

/** GitHub issue / PR ref `owner/repo#n` from its URL; null for anything else. */
export function githubRef(url: string | null | undefined): string | null {
  const m = /^https?:\/\/(?:www\.)?github\.com\/([A-Za-z0-9-]+)\/([A-Za-z0-9._-]+)\/(?:issues|pull)\/0*([1-9][0-9]{0,9})(?:[/?#]|$)/i.exec(
    (url ?? '').trim(),
  )
  return m ? `${m[1].toLowerCase()}/${m[2].toLowerCase()}#${m[3]}` : null
}

/** Short label of a source for a row: `#123` / `!12` for issues, MRs and PRs; null otherwise (icon only). */
export function sourceShortLabel(type: TodoSourceType | null, ref: string | null, url?: string | null): string | null {
  const r = ref ?? ''
  if (type === 'github') {
    const m = /#(\d+)$/.exec(r) ?? /#(\d+)$/.exec(githubRef(url) ?? '')
    return m ? `#${m[1]}` : null
  }
  if (type === 'gitlab') {
    const m = /([#!]\d+)$/.exec(r)
    return m ? m[1] : null
  }
  return null
}
