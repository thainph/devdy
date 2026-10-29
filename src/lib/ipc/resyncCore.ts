/** Pure part of ./resync.ts (unit-tested in plain Node). */

/** Local (this machine only) event: every remote window should resync. */
export const RESYNC_EVENT = 'remote-desktop://resync'
/** At most one resync reload per window this often. */
export const RESYNC_MIN_GAP_MS = 30_000

/** How long to wait before reloading, given the last reload (ms since epoch, or null). */
export function resyncDelay(now: number, last: number | null, minGapMs = RESYNC_MIN_GAP_MS): number {
  if (last === null || !Number.isFinite(last) || last > now) return 0
  return Math.max(0, last + minGapMs - now)
}
