// Runtime Dashboard — the maths behind its charts: area charts (activity, CPU,
// RAM, sparklines), the session donut, the 5-hour limit rings and countdown,
// and today's timeline. The components only bind the strings computed here.
//
// Pure logic (no Vue / Tauri / `@/` imports) for node:test. Times are epoch ms;
// the timeline's hour marks follow the local time zone.

// ── Area charts ─────────────────────────────────────────────────────────────

export interface AreaChart {
  /** SVG path of the line (`M x y L …`), empty when there are no values. */
  line: string
  /** The same line closed down to the baseline, for the gradient fill. */
  area: string
  /** The last point (the "now" dot). */
  lastX: number
  lastY: number
  /** Every value is 0 (or there are none). */
  empty: boolean
}

export interface AreaOptions {
  /** Fixed top of the scale (e.g. 100 for a percentage); default: the largest value. */
  max?: number
  /** Smallest top of the scale when it follows the values, so a quiet line stays low. */
  floor?: number
  /** Room kept above the highest point (stroke width), in chart units. */
  pad?: number
}

function r1(n: number): number {
  return Math.round(n * 10) / 10
}

/**
 * A line + filled area in a `width` × `height` box (y grows downwards), values
 * oldest first spread evenly across the width. One value draws a flat line.
 */
export function areaChart(values: readonly number[], width: number, height: number, o: AreaOptions = {}): AreaChart {
  const clean = values.map((v) => (Number.isFinite(v) ? Math.max(0, v) : 0))
  if (!clean.length) return { line: '', area: '', lastX: width, lastY: height, empty: true }
  const max = o.max !== undefined && o.max > 0 ? o.max : Math.max(o.floor ?? 1, ...clean)
  const pad = o.pad ?? 2
  const base = height - 1
  const y = (v: number) => r1(base - (Math.min(v, max) / max) * (base - pad))
  const pts = clean.length === 1
    ? [[0, y(clean[0])], [width, y(clean[0])]]
    : clean.map((v, i) => [r1((i * width) / (clean.length - 1)), y(v)])
  const line = pts.map(([px, py], i) => `${i ? 'L' : 'M'}${px} ${py}`).join(' ')
  const [lastX, lastY] = pts[pts.length - 1]
  return { line, area: `${line} L${width} ${height} L0 ${height} Z`, lastX, lastY, empty: clean.every((v) => v === 0) }
}

/**
 * Per-minute buckets of many sessions added up, aligned on the current minute
 * (each list's last value is "now"); `n` buckets, oldest first.
 */
export function sumBuckets(lists: readonly (readonly number[])[], n = 20): number[] {
  const out = new Array<number>(n).fill(0)
  for (const list of lists) {
    const offset = n - list.length
    list.forEach((v, i) => {
      if (offset + i >= 0 && Number.isFinite(v)) out[offset + i] += v
    })
  }
  return out
}

// ── Donut / rings ───────────────────────────────────────────────────────────

export function circumference(radius: number): number {
  return 2 * Math.PI * radius
}

export interface DonutSegment<K extends string> {
  key: K
  n: number
  /** `stroke-dasharray` for a full circle of the radius. */
  dash: string
  /** `stroke-dashoffset` (where the segment starts). */
  offset: string
}

/**
 * The segments of a donut (one circle stroke per part, drawn from 12 o'clock
 * clockwise when rotated −90°). Empty parts are dropped; `gap` is left between
 * segments when there are several.
 */
export function donutSegments<K extends string>(parts: readonly { key: K; n: number }[], radius: number, gap = 0): DonutSegment<K>[] {
  const shown = parts.filter((p) => p.n > 0)
  const total = shown.reduce((sum, p) => sum + p.n, 0)
  if (!total) return []
  const circ = circumference(radius)
  let acc = 0
  return shown.map((p) => {
    const len = (p.n / total) * circ
    const drawn = shown.length > 1 ? Math.max(0, len - gap) : len
    const seg = { key: p.key, n: p.n, dash: `${r1(drawn)} ${r1(circ)}`, offset: `${r1(-acc)}` }
    acc += len
    return seg
  })
}

/** `stroke-dasharray` of a progress ring filled to `pct` (0-100; null = empty). */
export function ringDash(pct: number | null, radius: number): string {
  const circ = circumference(radius)
  const p = pct === null || !Number.isFinite(pct) ? 0 : Math.min(100, Math.max(0, pct))
  return `${r1((p / 100) * circ)} ${r1(circ)}`
}

export const QUOTA_WARN_PERCENT = 70
export const QUOTA_DANGER_PERCENT = 90

export type QuotaLevel = 'none' | 'ok' | 'warning' | 'danger'

/** Colour level of a 5-hour limit reading. */
export function quotaLevel(pct: number | null): QuotaLevel {
  if (pct === null || !Number.isFinite(pct)) return 'none'
  if (pct >= QUOTA_DANGER_PERCENT) return 'danger'
  if (pct >= QUOTA_WARN_PERCENT) return 'warning'
  return 'ok'
}

/** Time left as "h:mm:ss" ("0:04:09"); empty when unknown. Never negative. */
export function formatCountdown(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return ''
  const s = Math.max(0, Math.floor(ms / 1000))
  const h = Math.floor(s / 3600)
  const m = String(Math.floor((s % 3600) / 60)).padStart(2, '0')
  return `${h}:${m}:${String(s % 60).padStart(2, '0')}`
}

// ── Today's timeline ────────────────────────────────────────────────────────

/** Local start of the day `t` falls in. */
export function localDayStart(t: number): number {
  const d = new Date(t)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** `hour`:00 local time on the day `t` falls in. */
export function localHourOn(t: number, hour: number): number {
  const d = new Date(t)
  d.setHours(hour, 0, 0, 0)
  return d.getTime()
}

/** "HH:MM", local, 24-hour. */
export function clockLabel(t: number): string {
  const d = new Date(t)
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`
}

export interface TimelineWindow {
  start: number
  end: number
}

/** The timeline's working-day start. */
export const TIMELINE_MORNING_HOUR = 8

/**
 * The span the timeline covers: from 08:00 to now — from the day start instead
 * when a run finished before 08:00 (or it is not 08:00 yet).
 */
export function timelineWindow(firstAt: number | null, now: number): TimelineWindow {
  const dayStart = localDayStart(now)
  const morning = localHourOn(now, TIMELINE_MORNING_HOUR)
  const early = now <= morning || (firstAt !== null && firstAt < morning)
  const start = early ? dayStart : morning
  return { start, end: Math.max(now, start + 60_000) }
}

/** Position (0-100 %) of `t` across the window, clamped. */
export function timelinePercent(t: number, w: TimelineWindow): number {
  const span = w.end - w.start
  if (span <= 0) return 100
  return r1(Math.min(100, Math.max(0, ((t - w.start) / span) * 100)))
}

export interface TimelineMark {
  at: number
  pct: number
  label: string
}

const TICK_STEPS_H = [1, 2, 3, 4, 6, 12]

/**
 * Whole-hour marks inside the window (at most `max`, a step of 1-12 h), none
 * closer than `margin` % to either end — the ends carry their own labels.
 */
export function timelineTicks(w: TimelineWindow, max = 5, margin = 8): TimelineMark[] {
  const hours = (w.end - w.start) / 3_600_000
  const step = TICK_STEPS_H.find((s) => hours / s <= max) ?? 24
  const d = new Date(w.start)
  d.setMinutes(0, 0, 0)
  if (d.getTime() <= w.start) d.setHours(d.getHours() + 1)
  while (d.getHours() % step !== 0) d.setHours(d.getHours() + 1)
  const out: TimelineMark[] = []
  for (let guard = 0; d.getTime() < w.end && guard < 48; guard++) {
    const pct = timelinePercent(d.getTime(), w)
    if (pct >= margin && pct <= 100 - margin) out.push({ at: d.getTime(), pct, label: clockLabel(d.getTime()) })
    d.setHours(d.getHours() + step)
  }
  return out
}

export interface TimelineDot<T> {
  run: T
  at: number
  pct: number
}

/** One dot per run with a readable `finishedAt`, oldest first. */
export function timelineDots<T extends { finishedAt: string }>(runs: readonly T[], w: TimelineWindow): TimelineDot<T>[] {
  return runs
    .map((run) => ({ run, at: Date.parse(run.finishedAt) }))
    .filter((d) => !Number.isNaN(d.at))
    .sort((a, b) => a.at - b.at)
    .map((d) => ({ ...d, pct: timelinePercent(d.at, w) }))
}

/** The local hour in which the most runs finished (at least two), the earliest on a tie. */
export function busiestHour(times: readonly number[]): { start: number; count: number } | null {
  const counts = new Map<number, number>()
  for (const t of times) {
    if (!Number.isFinite(t)) continue
    const d = new Date(t)
    d.setMinutes(0, 0, 0)
    counts.set(d.getTime(), (counts.get(d.getTime()) ?? 0) + 1)
  }
  let best: { start: number; count: number } | null = null
  for (const [start, count] of [...counts].sort((a, b) => a[0] - b[0])) {
    if (count >= 2 && (!best || count > best.count)) best = { start, count }
  }
  return best
}
