// Focus board (docs/focus-board-spec.md): the backend contract — types mirroring
// the Rust `focus_board` / `list_todos` / `focus_settings_*` payloads — plus the
// pure helpers the Focus screen, the "+ Todo" forms and the drawer share:
// estimate parsing, relative due labels, bucket order, filters, snooze presets,
// source links. Scoring and bucketing are done by the backend (BR-04/05); the UI
// only groups by `bucket` and sorts by `score`.
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
  /** Why `capacity` is null: see `CAPACITY_NO_ACCOUNT` / `CAPACITY_LOADING`, else a Calendar error. */
  capacity_error?: string | null
  calibration: FocusCalibration
}

/** `capacity_error`: no Google account linked → "connect Google Calendar". */
export const CAPACITY_NO_ACCOUNT = 'no_account'
/** `capacity_error`: first Calendar fetch still running (`focus:changed` follows). */
export const CAPACITY_LOADING = 'loading'

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
/** Backend → every window: a reminder click asks to show a todo (`{ todo_id }`). */
export const FOCUS_OPEN_TODO = 'focus:open-todo'
/** Backend → main window: create the Focus window (it doesn't exist yet). */
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

/**
 * The drawer's form after its todo changed underneath it (another window, the
 * AI, MCP): every field the user hasn't touched (`form[k] === base[k]`) takes
 * the new value, edited ones are kept. `base` is the form as last loaded.
 */
export function rebaseForm<T extends Record<string, unknown>>(form: T, base: T, next: T): T {
  const out = { ...form }
  for (const k of Object.keys(next) as (keyof T)[]) {
    if (form[k] === base[k]) out[k] = next[k]
  }
  return out
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

// ── Buckets ─────────────────────────────────────────────────────────────────

/** Section order of the "by time" view (spec §3.2 mockup). */
export const BUCKET_ORDER: FocusBucket[] = [
  'doing',
  'overdue',
  'today',
  'tomorrow',
  'this_week',
  'later',
  'no_due',
  'waiting',
  'snoozed',
  'done',
]

/** Sections open by default. */
export const DEFAULT_OPEN_BUCKETS: FocusBucket[] = ['doing', 'overdue', 'today']

/** Open work: not done, not snoozed. Waiting is still open (someone else's ball). */
export function isActiveBucket(b: FocusBucket): boolean {
  return b !== 'done' && b !== 'snoozed'
}

function compareItems(a: FocusItem, b: FocusItem): number {
  if (a.score !== b.score) return a.score - b.score
  const da = a.effective_due ? Date.parse(a.effective_due) : Infinity
  const db = b.effective_due ? Date.parse(b.effective_due) : Infinity
  if (da !== db) return da - db
  return a.todo.position - b.todo.position
}

/** Items sorted by score (ties: earlier due, then position). */
export function sortByScore(items: FocusItem[]): FocusItem[] {
  return items.slice().sort(compareItems)
}

/** Non-empty buckets in BUCKET_ORDER, each sorted by score. */
export function groupByBucket(items: FocusItem[]): { bucket: FocusBucket; items: FocusItem[] }[] {
  const map = new Map<FocusBucket, FocusItem[]>()
  for (const it of items) {
    const list = map.get(it.bucket)
    if (list) list.push(it)
    else map.set(it.bucket, [it])
  }
  const out: { bucket: FocusBucket; items: FocusItem[] }[] = []
  for (const bucket of BUCKET_ORDER) {
    const list = map.get(bucket)
    if (list?.length) out.push({ bucket, items: sortByScore(list) })
  }
  return out
}

/** "Làm ngay": the backend's `top` ids resolved against the (filtered) items. */
export function topItems(items: FocusItem[], top: string[]): FocusItem[] {
  const byId = new Map(items.map((it) => [it.todo.id, it]))
  return top.map((id) => byId.get(id)).filter((it): it is FocusItem => !!it)
}

export interface ProjectColumn {
  projectId: string | null
  items: FocusItem[]
  /** Σ effective estimate of the column. */
  estimateMin: number
  overdue: number
}

/**
 * "By project" view: one column per project with open work (no project last),
 * ordered by the best score in it; done / snoozed items are left out.
 */
export function groupByProject(items: FocusItem[]): ProjectColumn[] {
  const map = new Map<string | null, FocusItem[]>()
  for (const it of items) {
    if (!isActiveBucket(it.bucket)) continue
    const key = it.todo.project_id
    const list = map.get(key)
    if (list) list.push(it)
    else map.set(key, [it])
  }
  const cols: ProjectColumn[] = [...map.entries()].map(([projectId, list]) => {
    const sorted = sortByScore(list)
    return {
      projectId,
      items: sorted,
      estimateMin: sorted.reduce((n, it) => n + it.effective_estimate_min, 0),
      overdue: sorted.filter((it) => it.bucket === 'overdue').length,
    }
  })
  return cols.sort((a, b) => {
    if ((a.projectId === null) !== (b.projectId === null)) return a.projectId === null ? 1 : -1
    return compareItems(a.items[0], b.items[0])
  })
}

// ── Filters ─────────────────────────────────────────────────────────────────

/** Filter value meaning "no project" / "no source". */
export const FILTER_NONE = '__none__'

export interface FocusFilter {
  /** '' = all, FILTER_NONE = without project, else a project id. */
  projectId: string
  /** '' = all, FILTER_NONE = without source, else a source type. */
  sourceType: string
}

export function filterItems(items: FocusItem[], f: FocusFilter): FocusItem[] {
  return items.filter((it) => {
    const p = it.todo.project_id
    if (f.projectId === FILTER_NONE ? p !== null : f.projectId && p !== f.projectId) return false
    const s = it.todo.source_type
    if (f.sourceType === FILTER_NONE ? s !== null : f.sourceType && s !== f.sourceType) return false
    return true
  })
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

/** A span as its largest sensible unit, for "quá hạn 2 ngày" / "còn 3h" wording. */
export type Span = { unit: 'm' | 'h' | 'd'; value: number }

export function spanOf(minutes: number): Span {
  const m = Math.max(0, Math.round(Math.abs(minutes)))
  if (m < 60) return { unit: 'm', value: m }
  if (m < 24 * 60) return { unit: 'h', value: Math.round(m / 60) }
  return { unit: 'd', value: Math.round(m / (24 * 60)) }
}

// ── Dates & due labels ──────────────────────────────────────────────────────

const pad = (n: number) => String(n).padStart(2, '0')

/** Local `YYYY-MM-DD` of an instant. */
export function ymdOf(t: number | Date): string {
  const d = new Date(t)
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`
}

/** Local `HH:MM` of an instant. */
export function hmOf(t: number | Date): string {
  const d = new Date(t)
  return `${pad(d.getHours())}:${pad(d.getMinutes())}`
}

function dayNumber(ymd: string): number {
  const [y, m, d] = ymd.split('-').map(Number)
  return Date.UTC(y, m - 1, d) / 86_400_000
}

/** Calendar days from `now`'s local date to `t`'s (negative = past). */
export function dayDiff(t: number, now: number): number {
  return Math.round(dayNumber(ymdOf(t)) - dayNumber(ymdOf(now)))
}

/**
 * What a row says about its timing, most pressing first:
 *  - `overdue`  — effective due passed (`minutes` late);
 *  - `start_by` — the latest start time falls today and the todo isn't started
 *                 (`time`, `late` when already past);
 *  - `due`      — the deadline itself: today / tomorrow / weekday-or-date, with the
 *                 time when the user gave one.
 */
export type DueLabel =
  | { kind: 'overdue'; minutes: number }
  | { kind: 'start_by'; time: string; late: boolean }
  | { kind: 'due'; day: 'today' | 'tomorrow' | 'date'; date: string; time: string | null }

export function dueLabel(item: FocusItem, now: number): DueLabel | null {
  const { todo } = item
  if (todo.done || !todo.due_date) return null
  const due = item.effective_due ? Date.parse(item.effective_due) : NaN
  if (Number.isFinite(due) && due < now) return { kind: 'overdue', minutes: Math.round((now - due) / 60_000) }
  const startBy = item.start_by ? Date.parse(item.start_by) : NaN
  if (todo.status === 'todo' && Number.isFinite(startBy) && dayDiff(startBy, now) === 0) {
    return { kind: 'start_by', time: hmOf(startBy), late: startBy <= now }
  }
  const diff = dayNumber(todo.due_date) - dayNumber(ymdOf(now))
  return {
    kind: 'due',
    day: diff === 0 ? 'today' : diff === 1 ? 'tomorrow' : 'date',
    date: todo.due_date,
    time: todo.due_time,
  }
}

/** The 7 local dates (Mon → Sun) of the week containing `now`. */
export function weekDays(now: number): string[] {
  const d = new Date(now)
  const iso = d.getDay() === 0 ? 7 : d.getDay()
  const monday = new Date(d.getFullYear(), d.getMonth(), d.getDate() - (iso - 1))
  return Array.from({ length: 7 }, (_, i) =>
    ymdOf(new Date(monday.getFullYear(), monday.getMonth(), monday.getDate() + i)),
  )
}

export interface WeekColumn {
  date: string
  items: FocusItem[]
  estimateMin: number
}

/** "Week" view: open todos placed on their due date within this week. */
export function groupByWeek(items: FocusItem[], now: number): WeekColumn[] {
  return weekDays(now).map((date) => {
    const list = sortByScore(items.filter((it) => isActiveBucket(it.bucket) && it.todo.due_date === date))
    return { date, items: list, estimateMin: list.reduce((n, it) => n + it.effective_estimate_min, 0) }
  })
}

/** RFC3339 → `<input type="datetime-local">` value (local). */
export function toLocalInput(iso: string | null | undefined): string {
  const t = iso ? Date.parse(iso) : NaN
  return Number.isFinite(t) ? `${ymdOf(t)}T${hmOf(t)}` : ''
}

/** `<input type="datetime-local">` value → RFC3339 (UTC), null when empty / invalid. */
export function fromLocalInput(value: string | null | undefined): string | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2})/.exec(value ?? '')
  if (!m) return null
  return new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5]).toISOString()
}

// ── Snooze presets ──────────────────────────────────────────────────────────

export type SnoozePreset = 'hour' | 'afternoon' | 'tomorrow' | 'nextWeek'

function atLocal(base: Date, dayOffset: number, hm: string): Date {
  const [h, m] = hm.split(':').map(Number)
  return new Date(base.getFullYear(), base.getMonth(), base.getDate() + dayOffset, h || 0, m || 0)
}

/**
 * Snooze menu instants (RFC3339): +1h · today 14:00 (only before 14:00) ·
 * tomorrow at work_start · next Monday at work_start.
 */
export function snoozePresets(now: number, workStart = '09:00'): { preset: SnoozePreset; at: string }[] {
  const d = new Date(now)
  const out: { preset: SnoozePreset; at: string }[] = [{ preset: 'hour', at: new Date(now + 3_600_000).toISOString() }]
  const afternoon = atLocal(d, 0, '14:00')
  if (afternoon.getTime() > now) out.push({ preset: 'afternoon', at: afternoon.toISOString() })
  out.push({ preset: 'tomorrow', at: atLocal(d, 1, workStart).toISOString() })
  const iso = d.getDay() === 0 ? 7 : d.getDay()
  out.push({ preset: 'nextWeek', at: atLocal(d, 8 - iso, workStart).toISOString() })
  return out
}

// ── Reasons ─────────────────────────────────────────────────────────────────

/** A reason decoded for display; the UI words it (useFocusText). */
export type ReasonView =
  | { kind: 'doing' | 'waiting' | 'no_due' | 'done' }
  | { kind: 'snoozed' | 'due'; date: string | null; time: string | null }
  | { kind: 'overdue' | 'work_left' | 'late_by' | 'estimate' | 'estimate_missing'; minutes: number }
  | { kind: 'priority' | 'age_days'; n: number }
  | { kind: 'unknown'; name: string; value: string | null }

const FLAG_REASONS = ['doing', 'waiting', 'no_due', 'done'] as const
const TIME_REASONS = ['snoozed', 'due'] as const
const MINUTE_REASONS = ['overdue', 'work_left', 'late_by', 'estimate', 'estimate_missing'] as const
const COUNT_REASONS = ['priority', 'age_days'] as const

function includes<T extends string>(list: readonly T[], v: string): v is T {
  return (list as readonly string[]).includes(v)
}

/** Decode one backend reason; anything off-contract falls back to `unknown`. */
export function parseReason(r: FocusReason): ReasonView {
  const v = r.value
  const unknown = (): ReasonView => ({ kind: 'unknown', name: r.kind, value: v == null ? null : String(v) })
  if (includes(FLAG_REASONS, r.kind)) return { kind: r.kind }
  if (includes(TIME_REASONS, r.kind)) {
    if (v == null) return { kind: r.kind, date: null, time: null }
    const m = /^(\d{4}-\d{2}-\d{2})(?:[T ](\d{2}:\d{2}))?/.exec(String(v))
    return m ? { kind: r.kind, date: m[1], time: m[2] ?? null } : unknown()
  }
  const n = typeof v === 'number' ? v : Number(v)
  if (v == null || v === '' || !Number.isFinite(n)) return unknown()
  if (includes(MINUTE_REASONS, r.kind)) return { kind: r.kind, minutes: n }
  if (includes(COUNT_REASONS, r.kind)) return { kind: r.kind, n }
  return unknown()
}

// ── Calibration ─────────────────────────────────────────────────────────────

/** BR-10 hint factor ("you usually take 1.4× longer"), or null when not worth saying. */
export function calibrationHint(c: FocusCalibration | null | undefined): number | null {
  if (!c || !c.enabled || c.samples < 5 || !Number.isFinite(c.measured)) return null
  const f = Math.round(c.measured * 10) / 10
  return f === 1 ? null : f
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

/** `focus_ai_analyze` structured result (the markdown goes to `ai_plan`). */
export interface AiAnalysis {
  summary: string
  steps: string[]
  estimate_min: number
  estimate_low_min: number
  estimate_high_min: number
  confidence: 'low' | 'medium' | 'high'
  risks: string[]
  questions: string[]
  suggested_priority: TodoPriority | null
  suggested_due_date: string | null
  suggested_due_time: string | null
}

export interface AiAnalyzeResult {
  analysis: AiAnalysis
  todo: FocusTodo
}

export interface DayPlanBlock {
  /** `b1`, `b2`… in time order. */
  id: string
  todo_id: string
  /** Local `HH:MM`. */
  start: string
  end: string
  note: string
  calendar_event_id: string | null
}

/** `focus_ai_plan_today` / `focus_day_plan_get`. */
export interface DayPlan {
  date: string
  blocks: DayPlanBlock[]
  deferred: { todo_id: string; reason: string }[]
  summary: string
  /** Blocks the backend dropped (unknown todo, outside free time, overlap). */
  dropped: number
  created_at: string
}

/** What "Apply" of an analysis puts into the drawer's form. */
export interface AiApply {
  estimateMin?: number
  priority?: TodoPriority
  dueDate?: string
  dueTime?: string
}

export type AiRequestKind = 'suggest' | 'analyze' | 'plan'

/** `focus:ai-progress` payload. */
export interface AiProgress {
  request_id: string
  kind: AiRequestKind
  todo_id: string | null
  stage: 'started' | 'done' | 'error' | 'cancelled'
  error: string | null
}

export const FOCUS_AI_PROGRESS = 'focus:ai-progress'

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

/** Minutes between two `HH:MM`. */
export function blockMinutes(b: Pick<DayPlanBlock, 'start' | 'end'>): number {
  const m = (hm: string) => {
    const [h, mm] = hm.split(':').map(Number)
    return h * 60 + mm
  }
  return Math.max(0, m(b.end) - m(b.start))
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

/** GitHub issue URL of `owner/repo` + number. */
export function githubIssueUrl(owner: string, repo: string, number: number): string {
  return `https://github.com/${owner}/${repo}/issues/${number}`
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

// ── Project chips ───────────────────────────────────────────────────────────

/** Stable hue (0–359) for a project id: projects have no colour of their own. */
export function projectHue(id: string): number {
  let h = 0
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) >>> 0
  return h % 360
}
