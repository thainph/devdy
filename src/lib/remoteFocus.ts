// Pure logic behind the Remote Controller's Focus settings (working hours &
// reminders): narrowing the Host's settings payload, error codes, settings
// validation. Vue / Tauri / i18n-free so node:test can run it.

import { notifyKindsOf, type FocusSettings } from './focus.ts'

/**
 * Error code of a refused request, from a `command_ack.reason`: a bare snake_case code is kept (`project_required`,
 * `busy`, `no_plan`…), `"<code>: detail"` keeps its code, anything else is
 * `failed`.
 */
export function focusErrorCode(reason: string | null | undefined): string {
  const s = (reason ?? '').trim()
  if (/^[a-z_]+$/.test(s)) return s
  const m = /^([a-z_]+):/.exec(s)
  return m ? m[1] : 'failed'
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
const strings = (v: unknown): string[] => (Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [])

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

/** Settings only the desktop edits (AI engine, Calendar choice); the Host
 * ignores them from a phone, so they are never sent. */
export const DESKTOP_ONLY_SETTINGS = ['ai_engine', 'ai_model', 'calendar_account_id', 'calendar_ids'] as const

/** `settings` without the {@link DESKTOP_ONLY_SETTINGS}: what a phone saves. */
export function phoneSettings<T extends object>(settings: T): Partial<T> {
  const out: Partial<T> = { ...settings }
  for (const k of DESKTOP_ONLY_SETTINGS) delete (out as Record<string, unknown>)[k]
  return out
}
