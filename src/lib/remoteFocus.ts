// Pure logic behind the Remote Controller's full Focus screens (AI requests,
// day plan, settings, the "how long did it take?" prompt): narrowing the
// Host's Focus payloads, tracking AI requests by what they are for, error
// codes, settings validation. Vue / Tauri / i18n-free so node:test can run it.

import {
  notifyKindsOf,
  type AiAnalyzeResult,
  type AiSuggestion,
  type DayPlan,
  type FocusSettings,
  type FocusTodo,
} from './focus.ts'

// ── AI request keys ─────────────────────────────────────────────────────────

/** What an AI request is for — one at a time per key (the spinner's key). */
export const PLAN_KEY = 'plan'
export const todoAiKey = (todoId: string): string => `todo:${todoId}`
export const suggestAiKey = (sourceUrl: string): string => `suggest:${sourceUrl.trim()}`

/** A fresh `request_id` (≤128 chars, echoed by the Host). */
export function newRequestId(): string {
  const c = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto
  if (c && typeof c.randomUUID === 'function') return c.randomUUID()
  return `r-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 12)}`
}

/**
 * Error code of a failed Focus request, from a `focus_ai_result.error` or a
 * `command_ack.reason`: a bare snake_case code is kept (`project_required`,
 * `busy`, `no_plan`…), `"<code>: detail"` keeps its code, anything else is
 * `failed`.
 */
export function focusErrorCode(reason: string | null | undefined): string {
  const s = (reason ?? '').trim()
  if (/^[a-z_]+$/.test(s)) return s
  const m = /^([a-z_]+):/.exec(s)
  return m ? m[1] : 'failed'
}

// ── Done → actual time ──────────────────────────────────────────────────────

/** BR-11: ask "how long did it take?" after Done only for a started, estimated todo. */
export function shouldAskActual(todo: Pick<FocusTodo, 'done' | 'started_at' | 'estimate_min'>): boolean {
  return !todo.done && !!todo.started_at && !!todo.estimate_min
}

// ── Day plan ────────────────────────────────────────────────────────────────

/** Blocks not yet on Google Calendar. */
export function pendingBlocks(plan: DayPlan | null | undefined): string[] {
  return (plan?.blocks ?? []).filter((b) => !b.calendar_event_id).map((b) => b.id)
}

// ── Settings ────────────────────────────────────────────────────────────────

const HM = /^\d{2}:\d{2}$/

/** Why the settings form can't be saved (mirrors the desktop form), or null. */
export function focusSettingsError(s: FocusSettings): 'hours' | 'days' | 'morning' | null {
  if (!HM.test(s.work_start) || !HM.test(s.work_end) || s.work_end <= s.work_start) return 'hours'
  if (s.work_days.length === 0) return 'days'
  if (!HM.test(s.morning_time)) return 'morning'
  return null
}

/** A detached copy with every reminder kind present and the days sorted. */
export function normaliseFocusSettings(s: FocusSettings): FocusSettings {
  return {
    ...s,
    work_days: [...new Set(s.work_days)].sort((a, b) => a - b),
    calendar_ids: [...s.calendar_ids],
    notify_kinds: notifyKindsOf(s.notify_kinds),
  }
}

/** Flip one ISO weekday in a sorted day list. */
export function toggleWorkDay(days: number[], day: number): number[] {
  const set = new Set(days)
  if (set.has(day)) set.delete(day)
  else set.add(day)
  return [...set].sort((a, b) => a - b)
}

// ── Narrowing (wire → typed) ────────────────────────────────────────────────

type Obj = Record<string, unknown>
const isObj = (v: unknown): v is Obj => !!v && typeof v === 'object' && !Array.isArray(v)
const strOrNull = (v: unknown): string | null => (typeof v === 'string' ? v : null)
const isPriority = (v: unknown): v is 1 | 2 | 3 => v === 1 || v === 2 || v === 3
const strings = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [])

/** A `DayPlan`, malformed blocks / deferred rows dropped; null when unusable. */
export function parseDayPlan(v: unknown): DayPlan | null {
  if (!isObj(v) || typeof v.date !== 'string' || !Array.isArray(v.blocks)) return null
  const blocks = v.blocks
    .filter(isObj)
    .filter((b) => typeof b.id === 'string' && typeof b.todo_id === 'string' && typeof b.start === 'string' && typeof b.end === 'string')
    .map((b) => ({
      id: b.id as string,
      todo_id: b.todo_id as string,
      start: b.start as string,
      end: b.end as string,
      note: typeof b.note === 'string' ? b.note : '',
      calendar_event_id: strOrNull(b.calendar_event_id),
    }))
  const deferred = (Array.isArray(v.deferred) ? v.deferred : [])
    .filter(isObj)
    .filter((d) => typeof d.todo_id === 'string')
    .map((d) => ({ todo_id: d.todo_id as string, reason: typeof d.reason === 'string' ? d.reason : '' }))
  return {
    date: v.date,
    blocks,
    deferred,
    summary: typeof v.summary === 'string' ? v.summary : '',
    dropped: typeof v.dropped === 'number' ? v.dropped : 0,
    created_at: typeof v.created_at === 'string' ? v.created_at : '',
  }
}

/** `FocusSettings`, unknown / missing fields defaulted; null when unusable. */
export function parseFocusSettings(v: unknown): FocusSettings | null {
  if (!isObj(v) || typeof v.work_start !== 'string' || typeof v.work_end !== 'string' || !Array.isArray(v.work_days)) {
    return null
  }
  const kinds: Record<string, boolean> = {}
  if (isObj(v.notify_kinds)) {
    for (const [k, on] of Object.entries(v.notify_kinds)) if (typeof on === 'boolean') kinds[k] = on
  }
  return {
    work_start: v.work_start,
    work_end: v.work_end,
    work_days: v.work_days.filter((d): d is number => Number.isInteger(d) && (d as number) >= 1 && (d as number) <= 7),
    morning_time: typeof v.morning_time === 'string' ? v.morning_time : '09:00',
    notify_enabled: v.notify_enabled !== false,
    notify_kinds: kinds,
    calendar_account_id: strOrNull(v.calendar_account_id),
    calendar_ids: strings(v.calendar_ids),
    calibration_enabled: v.calibration_enabled !== false,
    ai_engine: typeof v.ai_engine === 'string' ? v.ai_engine : '',
    ai_model: typeof v.ai_model === 'string' ? v.ai_model : '',
  }
}

/** `focus_ai_suggest` result, or null. */
export function parseAiSuggestion(v: unknown): AiSuggestion | null {
  if (!isObj(v) || typeof v.title !== 'string' || typeof v.estimate_min !== 'number') return null
  return {
    title: v.title,
    due_date: strOrNull(v.due_date),
    due_time: strOrNull(v.due_time),
    due_evidence: strOrNull(v.due_evidence),
    priority: isPriority(v.priority) ? v.priority : 2,
    estimate_min: v.estimate_min,
    reason: typeof v.reason === 'string' ? v.reason : '',
  }
}

/** `focus_ai_analyze` result `{ analysis, todo }`, or null. */
export function parseAiAnalyzeResult(v: unknown): AiAnalyzeResult | null {
  if (!isObj(v) || !isObj(v.analysis) || !isObj(v.todo) || typeof v.todo.id !== 'string') return null
  const a = v.analysis
  const n = (x: unknown): number => (typeof x === 'number' && Number.isFinite(x) ? x : 0)
  const estimate = n(a.estimate_min)
  return {
    analysis: {
      summary: typeof a.summary === 'string' ? a.summary : '',
      steps: strings(a.steps),
      estimate_min: estimate,
      estimate_low_min: n(a.estimate_low_min) || estimate,
      estimate_high_min: n(a.estimate_high_min) || estimate,
      confidence: a.confidence === 'low' || a.confidence === 'high' ? a.confidence : 'medium',
      risks: strings(a.risks),
      questions: strings(a.questions),
      suggested_priority: isPriority(a.suggested_priority) ? a.suggested_priority : null,
      suggested_due_date: strOrNull(a.suggested_due_date),
      suggested_due_time: strOrNull(a.suggested_due_time),
    },
    todo: v.todo as unknown as FocusTodo,
  }
}

/** Settings only the desktop edits (AI engine, Calendar choice); the Host
 * ignores them from a phone, so they are never sent. */
export const DESKTOP_ONLY_SETTINGS = ['ai_engine', 'ai_model', 'calendar_account_id', 'calendar_ids'] as const

/** `settings` without the {@link DESKTOP_ONLY_SETTINGS}: what a phone saves. */
export function phoneSettings<T extends object>(settings: T): Partial<T> {
  const out: Partial<T> = { ...settings }
  for (const k of DESKTOP_ONLY_SETTINGS) delete (out as Record<string, unknown>)[k]
  return out
}

export const ACTUAL_MIN = 1
export const ACTUAL_MAX = 14_400

/** An actual time typed like an estimate (`30m`, `2h`, `1h30`, `90`) but in
 * [1, 14400] minutes: null when empty, undefined when unreadable / out of range. */
export function parseActual(input: string | null | undefined): number | null | undefined {
  const s = (input ?? '').trim().toLowerCase().replace(/\s+/g, '').replace(',', '.')
  if (!s) return null
  let m: RegExpExecArray | null
  let min: number
  if ((m = /^(\d+)$/.exec(s))) min = +m[1]
  else if ((m = /^(\d+(?:\.\d+)?)(?:m|min|p|ph|phút)$/.exec(s))) min = +m[1]
  else if ((m = /^(\d+(?:\.\d+)?)(?:h|g|giờ)$/.exec(s))) min = +m[1] * 60
  else if ((m = /^(\d+)(?:h|g)(\d{1,2})(?:m|min|p|ph)?$/.exec(s))) min = +m[1] * 60 + +m[2]
  else return undefined
  min = Math.round(min)
  return min >= ACTUAL_MIN && min <= ACTUAL_MAX ? min : undefined
}
