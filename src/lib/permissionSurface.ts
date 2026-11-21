// Where a pending permission / question request is shown, and how the in-app
// card stack lays itself out. Pure logic (no Vue / Tauri) so it can be unit-tested;
// the stateful side lives in composables/usePermissionDecisions.ts.
//
// - The main window is in front → requests are answered INSIDE it (a card stack in
//   the bottom-right corner); no standalone permission windows.
// - Another Devdy window is in front (a popped-out session, a permission window) →
//   the small standalone permission windows, as before.
// - Devdy is in the background → the permission windows too, unless the user
//   turned `permission_float_when_background` off; then only the native
//   notification (PermissionNotifier) alerts them.

/** Which window, if any, the user is looking at. */
export type AppForeground = 'main' | 'other' | 'background'

/** How pending requests are presented right now. */
export type PermissionSurface = 'inline' | 'window' | 'none'

/** App setting key: float permission windows while Devdy is in the background. */
export const PERMISSION_FLOAT_SETTING = 'permission_float_when_background'

/** The setting is on unless explicitly "false" (default true, like other booleans). */
export function floatWhenBackgroundEnabled(value: string | null | undefined): boolean {
  return value !== 'false'
}

/**
 * Which window is in front, from the focus of Devdy's windows. One exception:
 * focus moving from the main window into a permission window the user popped
 * out of a split RunView's dock (`poppedOutLabels`) keeps the main window "in
 * front" (`held`). That window belongs to the main window's flow — answering it
 * must not flip every other pending request out of the dock / card stack into
 * windows of their own. Anywhere else (another app, another Devdy window) ends
 * the hold.
 */
export function resolveForeground(
  prev: AppForeground,
  mainFocused: boolean,
  focusedOthers: readonly string[],
  poppedOutLabels: ReadonlySet<string>,
): { foreground: AppForeground; held: boolean } {
  if (mainFocused) return { foreground: 'main', held: false }
  if (!focusedOthers.length) return { foreground: 'background', held: false }
  if (prev === 'main' && focusedOthers.every((l) => poppedOutLabels.has(l))) return { foreground: 'main', held: true }
  return { foreground: 'other', held: false }
}

export function permissionSurface(
  foreground: AppForeground,
  floatWhenBackground: boolean,
): PermissionSurface {
  if (foreground === 'main') return 'inline'
  // Devdy is in front, just not its main window: a window beside it is right.
  if (foreground === 'other') return 'window'
  return floatWhenBackground ? 'window' : 'none'
}

/**
 * The runs that should have a standalone permission window right now.
 *
 * - `window`: every run with a pending request.
 * - `inline`: none — the main window shows them, so open windows are closed
 *   (programmatically, which never counts as a deny) — except the runs the user
 *   popped out on purpose (`poppedOut`, from a prompt docked in a split RunView).
 * - `none`: no new windows, but a window already open for a still-pending run
 *   stays (closing it under the user's cursor would be worse than leaving it);
 *   popped-out runs get theirs too.
 *
 * `dismissed`: runs whose popped-out window the user just closed — the request
 * went back to the main window, so it gets no window again (not even while
 * Devdy is in the background) until the main window has been in front.
 */
export function desiredWindowRuns(
  surface: PermissionSurface,
  pending: Iterable<string>,
  tracked: Iterable<string>,
  poppedOut: Iterable<string> = [],
  dismissed: Iterable<string> = [],
): Set<string> {
  const pend = new Set(pending)
  for (const runId of dismissed) pend.delete(runId)
  if (surface === 'window') return pend
  const out = new Set<string>()
  if (surface === 'none') for (const runId of tracked) if (pend.has(runId)) out.add(runId)
  for (const runId of poppedOut) if (pend.has(runId)) out.add(runId)
  return out
}

/**
 * The requests the in-app card stack shows: everything pending except a run whose
 * request is docked in a RunView (split view answers it in place of the composer)
 * and a request the user popped out into its own window — so no request is ever
 * on screen twice.
 */
export function stackEntries<T extends PendingEntry>(
  entries: readonly T[],
  dockedRuns: ReadonlySet<string>,
  poppedOutRequestIds: ReadonlySet<string>,
): T[] {
  return entries.filter((e) => !dockedRuns.has(e.runId) && !poppedOutRequestIds.has(e.requestId))
}

export interface PendingEntry {
  runId: string
  requestId: string
}

export interface StackLayout<T extends PendingEntry> {
  /** Cards on screen; the first is the expanded one with the full prompt. */
  visible: T[]
  /** Not collapsed, but beyond the visible limit. */
  overflow: T[]
  /** Collapsed by the user (still pending). */
  collapsed: T[]
}

/**
 * Lay out the in-app card stack. `promoted` lists run ids the user brought to the
 * front (most recent first); they lead, the rest keep arrival order. Collapsing is
 * keyed by request id, so a NEW request from a collapsed run shows up again.
 */
export function stackLayout<T extends PendingEntry>(
  entries: readonly T[],
  collapsedRequestIds: ReadonlySet<string>,
  promoted: readonly string[],
  max = 3,
): StackLayout<T> {
  const rank = (e: T) => {
    const i = promoted.indexOf(e.runId)
    return i === -1 ? promoted.length : i
  }
  const ordered = entries
    .map((e, i) => ({ e, i }))
    .sort((a, b) => rank(a.e) - rank(b.e) || a.i - b.i)
    .map(({ e }) => e)
  const open = ordered.filter((e) => !collapsedRequestIds.has(e.requestId))
  return {
    visible: open.slice(0, Math.max(0, max)),
    overflow: open.slice(Math.max(0, max)),
    collapsed: ordered.filter((e) => collapsedRequestIds.has(e.requestId)),
  }
}

/** Move `runId` to the front of the promoted list (deduped, bounded). */
export function promote(promoted: readonly string[], runId: string, limit = 20): string[] {
  return [runId, ...promoted.filter((id) => id !== runId)].slice(0, limit)
}

function asObject(v: unknown): Record<string, unknown> {
  return v && typeof v === 'object' ? (v as Record<string, unknown>) : {}
}

function oneLine(s: string, max: number): string {
  const flat = s.replace(/\s+/g, ' ').trim()
  return flat.length > max ? `${flat.slice(0, max - 1)}…` : flat
}

/**
 * A one-line summary of what a request wants to do, for a compact card: the
 * command / file / URL / pattern, the first question of an AskUserQuestion, or a
 * short JSON preview as a last resort.
 */
export function summarizePermissionInput(toolName: string, toolInput: unknown, max = 120): string {
  if (typeof toolInput === 'string') return oneLine(toolInput, max)
  const inp = asObject(toolInput)
  if (toolName === 'AskUserQuestion') {
    const qs = Array.isArray(inp['questions']) ? (inp['questions'] as unknown[]) : []
    const first = asObject(qs[0])
    const q = typeof first['question'] === 'string' ? (first['question'] as string) : ''
    if (!q) return ''
    return oneLine(qs.length > 1 ? `${q} (+${qs.length - 1})` : q, max)
  }
  if (toolName === 'ExitPlanMode') {
    const plan = typeof inp['plan'] === 'string' ? (inp['plan'] as string) : ''
    const firstLine = plan.split('\n').find((l) => l.trim()) ?? ''
    return oneLine(firstLine.replace(/^#+\s*/, ''), max)
  }
  for (const k of ['command', 'file_path', 'path', 'url', 'pattern', 'query', 'description']) {
    const v = inp[k]
    if (typeof v === 'string' && v.trim()) return oneLine(v, max)
  }
  const keys = Object.keys(inp)
  if (!keys.length) return ''
  try {
    return oneLine(JSON.stringify(inp), max)
  } catch {
    return ''
  }
}
