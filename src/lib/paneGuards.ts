// In-pane navigation guards for the right half of a split (components/
// PaneRouterView). The pane navigates without the app router, so it runs the
// guards the router would for an in-place change itself: the leave guards of
// the records it leaves, then the update guards of the records it keeps —
// `onBeforeRouteLeave` / `onBeforeRouteUpdate` registered by the screen in the
// pane, and `beforeRouteLeave` / `beforeRouteUpdate` component options. Global
// `beforeEach` guards are the app router's (they steer the window, e.g.
// tabActions `routeSplitNavigation`) and are not run here.
//
// Pure logic only — no Vue / router imports — so it is unit-tested directly
// (src/lib/__tests__/paneGuards.test.mjs).

/** A route record as the guards see it (vue-router's RouteRecordNormalized). */
export interface GuardRecord {
  aliasOf?: GuardRecord | undefined
  leaveGuards: Set<GuardFn>
  updateGuards: Set<GuardFn>
  components?: Record<string, unknown> | null
  instances: Record<string, unknown>
}

export type GuardFn = (to: unknown, from: unknown, next?: (v?: unknown) => void) => unknown

/** What a guard queue decided: go on, cancel, or go somewhere else instead. */
export type GuardOutcome = { kind: 'ok' } | { kind: 'abort'; error?: unknown } | { kind: 'redirect'; to: unknown }

/** The same record, through aliases (vue-router `isSameRouteRecord`). */
export function sameRecord(a: GuardRecord, b: GuardRecord): boolean {
  return (a.aliasOf || a) === (b.aliasOf || b)
}

/** Records of `from` that `to` leaves, and those it keeps (vue-router `extractChangingRecords`). */
export function changingRecords<R extends GuardRecord>(
  to: readonly R[],
  from: readonly R[],
): { leaving: R[]; updating: R[] } {
  const leaving: R[] = []
  const updating: R[] = []
  for (const r of from) {
    if (to.some((x) => sameRecord(x, r))) updating.push(r)
    else leaving.push(r)
  }
  return { leaving, updating }
}

/** A route location as a guard may return it to redirect. */
function isLocation(v: unknown): boolean {
  return typeof v === 'string' || (!!v && typeof v === 'object' && !(v instanceof Error))
}

/**
 * Call one guard the way vue-router does: its return value, or the `next`
 * callback when it declares a third parameter.
 */
export async function callGuard(guard: GuardFn, to: unknown, from: unknown, thisArg?: unknown): Promise<GuardOutcome> {
  let settle!: (v: unknown) => void
  const viaNext = new Promise<unknown>((resolve) => {
    settle = resolve
  })
  let value: unknown
  try {
    const ret = guard.call(thisArg, to, from, (v?: unknown) => settle(v))
    value = guard.length < 3 ? await ret : await viaNext
  } catch (error) {
    return { kind: 'abort', error }
  }
  if (value === false) return { kind: 'abort' }
  if (value instanceof Error) return { kind: 'abort', error: value }
  if (value === undefined || value === true || typeof value === 'function') return { kind: 'ok' }
  return isLocation(value) ? { kind: 'redirect', to: value } : { kind: 'ok' }
}

/** Run guards in order; the first one that doesn't let the navigation through decides. */
export async function runGuardQueue(guards: readonly (() => Promise<GuardOutcome>)[]): Promise<GuardOutcome> {
  for (const g of guards) {
    const out = await g()
    if (out.kind !== 'ok') return out
  }
  return { kind: 'ok' }
}

function componentGuards(records: readonly GuardRecord[], option: 'beforeRouteLeave' | 'beforeRouteUpdate', to: unknown, from: unknown) {
  const out: (() => Promise<GuardOutcome>)[] = []
  for (const record of records) {
    for (const name in record.components ?? {}) {
      const instance = record.instances[name]
      // Only mounted components guard a leave / update (as the router does).
      if (!instance) continue
      const raw = record.components![name] as Record<string, unknown> | null
      const opts = (raw && ((raw.__vccOpts as Record<string, unknown>) || raw)) ?? null
      const guard = opts?.[option]
      if (typeof guard === 'function') out.push(() => callGuard(guard as GuardFn, to, from, instance))
    }
  }
  return out
}

/**
 * The guards of an in-pane navigation from `from` to `to` (both with the
 * pane's own records): leave guards (deepest first, like the router), then
 * update guards.
 */
export function paneNavigationGuards(
  to: { matched: readonly GuardRecord[] },
  from: { matched: readonly GuardRecord[] },
): (() => Promise<GuardOutcome>)[] {
  const { leaving, updating } = changingRecords(to.matched, from.matched)
  const leavingDeepFirst = [...leaving].reverse()
  return [
    ...componentGuards(leavingDeepFirst, 'beforeRouteLeave', to, from),
    ...leavingDeepFirst.flatMap((r) => [...r.leaveGuards].map((g) => () => callGuard(g, to, from))),
    ...componentGuards(updating, 'beforeRouteUpdate', to, from),
    ...updating.flatMap((r) => [...r.updateGuards].map((g) => () => callGuard(g, to, from))),
  ]
}

/**
 * The pane's own copy of a route record: it reads through to the shared one
 * (path, components, meta…) but keeps its own guards and mounted instances, so
 * a screen in the pane never registers guards the app router would run for the
 * left half (or the reverse). `aliasOf` keeps it the "same record" as the
 * original for RouterLink's active state and the checks above.
 */
export function paneRecordCopy<R extends GuardRecord>(record: R): R {
  return Object.create(record, {
    aliasOf: { value: record.aliasOf || record, enumerable: true },
    leaveGuards: { value: new Set(), enumerable: true, writable: true },
    updateGuards: { value: new Set(), enumerable: true, writable: true },
    instances: { value: {}, enumerable: true, writable: true },
    enterCallbacks: { value: {}, enumerable: true, writable: true },
  }) as R
}
