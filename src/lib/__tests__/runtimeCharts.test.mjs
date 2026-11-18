// Unit tests for lib/runtimeCharts.ts (Runtime Dashboard charts: area paths,
// bucket sums, donut / ring maths, countdown, today's timeline).
// Run: node --test src/lib/__tests__/runtimeCharts.test.mjs
import { test } from 'node:test'
import assert from 'node:assert/strict'
import {
  areaChart,
  busiestHour,
  circumference,
  clockLabel,
  donutSegments,
  formatCountdown,
  localDayStart,
  localHourOn,
  quotaLevel,
  ringDash,
  sumBuckets,
  timelineDots,
  timelinePercent,
  timelineTicks,
  timelineWindow,
} from '../runtimeCharts.ts'

/** Local time today-ish: a fixed local date so DST does not matter. */
function at(h, m = 0) {
  return new Date(2026, 10, 18, h, m, 0, 0).getTime()
}

test('areaChart: line, closed area and last point', () => {
  const c = areaChart([0, 5, 10], 100, 21, { pad: 1 })
  assert.equal(c.line, 'M0 20 L50 10.5 L100 1')
  assert.equal(c.area, 'M0 20 L50 10.5 L100 1 L100 21 L0 21 Z')
  assert.equal(c.lastX, 100)
  assert.equal(c.lastY, 1)
  assert.equal(c.empty, false)
})

test('areaChart: fixed max clamps, floor keeps quiet lines low', () => {
  assert.equal(areaChart([200], 10, 11, { max: 100, pad: 0 }).line, 'M0 0 L10 0')
  const quiet = areaChart([1, 1], 10, 11, { floor: 10, pad: 0 })
  assert.equal(quiet.line, 'M0 9 L10 9')
})

test('areaChart: empty and all-zero', () => {
  const none = areaChart([], 100, 20)
  assert.equal(none.line, '')
  assert.equal(none.area, '')
  assert.ok(none.empty)
  const zero = areaChart([0, 0, NaN], 100, 20)
  assert.ok(zero.empty)
  assert.equal(zero.lastY, 19)
})

test('sumBuckets aligns lists on the current minute', () => {
  assert.deepEqual(sumBuckets([[1, 2, 3], [10], []], 4), [0, 1, 2, 13])
  assert.deepEqual(sumBuckets([[1, 2, 3, 4, 5]], 3), [3, 4, 5])
  assert.deepEqual(sumBuckets([], 2), [0, 0])
})

test('donutSegments: proportional, gaps, empty parts dropped', () => {
  const circ = circumference(10)
  const segs = donutSegments([{ key: 'a', n: 1 }, { key: 'b', n: 0 }, { key: 'c', n: 3 }], 10, 2)
  assert.deepEqual(segs.map((s) => s.key), ['a', 'c'])
  const [a, c] = segs
  assert.equal(a.dash, `${Math.round((circ / 4 - 2) * 10) / 10} ${Math.round(circ * 10) / 10}`)
  assert.equal(a.offset, '0')
  assert.equal(c.offset, `${Math.round((-circ / 4) * 10) / 10}`)
  // A single part is a full ring, no gap.
  assert.equal(donutSegments([{ key: 'a', n: 2 }], 10, 2)[0].dash.split(' ')[0], `${Math.round(circ * 10) / 10}`)
  assert.deepEqual(donutSegments([{ key: 'a', n: 0 }], 10), [])
})

test('ringDash clamps and treats null as empty', () => {
  const circ = Math.round(circumference(10) * 10) / 10
  assert.equal(ringDash(50, 10), `${Math.round(circumference(10) * 5) / 10} ${circ}`)
  assert.equal(ringDash(null, 10), `0 ${circ}`)
  assert.equal(ringDash(150, 10), `${circ} ${circ}`)
  assert.equal(ringDash(-5, 10), `0 ${circ}`)
})

test('quotaLevel thresholds', () => {
  assert.equal(quotaLevel(null), 'none')
  assert.equal(quotaLevel(0), 'ok')
  assert.equal(quotaLevel(69.9), 'ok')
  assert.equal(quotaLevel(70), 'warning')
  assert.equal(quotaLevel(90), 'danger')
  assert.equal(quotaLevel(120), 'danger')
})

test('formatCountdown', () => {
  assert.equal(formatCountdown(null), '')
  assert.equal(formatCountdown(-1000), '0:00:00')
  assert.equal(formatCountdown(249_900), '0:04:09')
  assert.equal(formatCountdown(6_130_000), '1:42:10')
})

test('local day helpers and clock label', () => {
  assert.equal(localDayStart(at(14, 20)), at(0))
  assert.equal(localHourOn(at(14, 20), 8), at(8))
  assert.equal(clockLabel(at(9, 5)), '09:05')
})

test('timelineWindow: 08:00 unless a run finished earlier or it is still early', () => {
  assert.deepEqual(timelineWindow(at(9), at(14, 20)), { start: at(8), end: at(14, 20) })
  assert.deepEqual(timelineWindow(null, at(14, 20)), { start: at(8), end: at(14, 20) })
  assert.deepEqual(timelineWindow(at(7, 30), at(14, 20)), { start: at(0), end: at(14, 20) })
  assert.deepEqual(timelineWindow(null, at(6)), { start: at(0), end: at(6) })
  // Right at midnight the window is still at least a minute long.
  assert.equal(timelineWindow(null, at(0)).end, at(0) + 60_000)
})

test('timelinePercent clamps', () => {
  const w = { start: at(8), end: at(12) }
  assert.equal(timelinePercent(at(10), w), 50)
  assert.equal(timelinePercent(at(7), w), 0)
  assert.equal(timelinePercent(at(13), w), 100)
})

test('timelineTicks: whole hours, bounded count, away from the ends', () => {
  const w = { start: at(8), end: at(14, 20) }
  assert.deepEqual(timelineTicks(w).map((t) => t.label), ['10:00', '12:00'])
  const short = { start: at(8), end: at(11, 30) }
  assert.deepEqual(timelineTicks(short).map((t) => t.label), ['09:00', '10:00', '11:00'])
  const day = { start: at(0), end: at(23) }
  const ticks = timelineTicks(day)
  assert.ok(ticks.length <= 5)
  assert.deepEqual(ticks.map((t) => t.label), ['06:00', '12:00', '18:00'])
  // A tick right next to "now" is dropped.
  assert.deepEqual(timelineTicks({ start: at(8), end: at(12, 5) }).map((t) => t.label), ['09:00', '10:00', '11:00'])
})

test('timelineDots: oldest first, unreadable stamps skipped', () => {
  const w = { start: at(8), end: at(12) }
  const runs = [
    { runId: 'b', finishedAt: new Date(at(11)).toISOString() },
    { runId: 'x', finishedAt: 'nope' },
    { runId: 'a', finishedAt: new Date(at(9)).toISOString() },
  ]
  const dots = timelineDots(runs, w)
  assert.deepEqual(dots.map((d) => d.run.runId), ['a', 'b'])
  assert.deepEqual(dots.map((d) => d.pct), [25, 75])
})

test('busiestHour', () => {
  assert.equal(busiestHour([at(9), at(10)]), null)
  assert.deepEqual(busiestHour([at(9, 1), at(10, 5), at(10, 40), at(11), at(11, 30)]), { start: at(10), count: 2 })
  assert.deepEqual(busiestHour([at(9), at(11), at(11, 10), at(11, 50)]), { start: at(11), count: 3 })
})
