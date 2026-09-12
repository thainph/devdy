/**
 * Builds the merged Duo timeline — the two sessions' outputs woven into ONE
 * conversation, read top-to-bottom like a chat between two agents.
 *
 * The backbone is `state.turns`: the orchestrator runs strictly sequentially
 * (A → B → A → B), so that array is already in conversation order and needs no
 * merge-sort. This module's real job is attaching each turn's STREAM DETAIL
 * (tool calls, thinking) — which lives per-run in the liveRuns store — to the
 * right turn.
 *
 * Pure functions only (no Vue), so the mapping rules below stay unit-testable.
 */
import type { StreamEntry } from './streamEvents'
import type { DuoPhase, DuoRole, DuoTurn } from '@/stores/orchestrator'

/** Half-open `[start, end)` slice of a run's entries covering exactly one turn. */
export interface TurnSegment {
  start: number
  end: number
}

/**
 * Cut a run's entries into per-turn segments.
 *
 * Every duo turn begins with exactly one `live.pushUser(...)` (see `startTurn` /
 * `continueTurn` in the orchestrator), so `kind === 'user'` entries are the turn
 * boundaries. A segment starts AFTER its user entry, which conveniently drops
 * the relay prompt from the slice — without that, the other side's reply would
 * appear twice in the merged timeline.
 *
 * Entries before the first user entry (a `system.init`, say) belong to no turn
 * and are skipped. A log with no user entry at all is treated as one segment so
 * odd/legacy transcripts still render something.
 */
export function segmentsOf(entries: StreamEntry[]): TurnSegment[] {
  const starts: number[] = []
  for (let i = 0; i < entries.length; i++) {
    if (entries[i].kind === 'user') starts.push(i + 1)
  }
  if (!starts.length) return entries.length ? [{ start: 0, end: entries.length }] : []
  return starts.map((s, i) => ({ start: s, end: i + 1 < starts.length ? starts[i + 1] - 1 : entries.length }))
}

export type DuoMessage =
  | { kind: 'topic'; id: string; goal: string }
  | { kind: 'round'; id: string; round: number }
  | {
      kind: 'turn'
      id: string
      role: DuoRole
      label: string
      engine: string
      round: number
      text: string
      at?: string
      runId: string | null
      /** Stream detail for this turn, or null when it couldn't be mapped (then
       * the UI renders text only and hides the "details" toggle). */
      entries: StreamEntry[] | null
    }
  | {
      kind: 'live'
      id: string
      role: DuoRole
      label: string
      engine: string
      round: number
      runId: string
      entries: StreamEntry[]
    }
  | { kind: 'notice'; id: string; phase: DuoPhase; reason: string | null }

/** The slice of orchestrator state the builder needs (keeps it store-agnostic). */
export interface DuoConversationInput {
  turns: DuoTurn[]
  goal: string
  round: number
  phase: DuoPhase
  stopReason: string | null
  waiting: DuoRole | null
  active: boolean
  designerLabel: string
  reviewerLabel: string
  designerEngine: string
  reviewerEngine: string
  designerRunId: string | null
  reviewerRunId: string | null
}

/**
 * Map a side's segments onto its completed turns, ALIGNED FROM THE TAIL.
 *
 * Tail alignment (rather than storing absolute indices on each turn) survives
 * two things index-based mapping gets wrong:
 *  - "existing" mode: the picked run already holds its own prior conversation,
 *    so the duo's turns start at some unknown offset;
 *  - an app restart: streams are re-parsed from the on-disk log, which can
 *    differ slightly from what was live (stderr lines, etc).
 *
 * When there are fewer segments than turns the OLDEST turns get `null` and
 * degrade to text-only — never a wrong slice.
 */
function mapSegments(
  entries: StreamEntry[],
  completed: number,
  takeLive: boolean,
): { turns: (StreamEntry[] | null)[]; live: StreamEntry[] | null } {
  const segs = segmentsOf(entries)
  const live = takeLive ? (segs.length ? entries.slice(segs[segs.length - 1].start) : []) : null
  const pool = takeLive && segs.length ? segs.slice(0, -1) : segs
  const used = pool.slice(Math.max(0, pool.length - completed))
  const turns: (StreamEntry[] | null)[] = [
    ...Array(Math.max(0, completed - used.length)).fill(null),
    ...used.map((s) => entries.slice(s.start, s.end)),
  ]
  return { turns, live }
}

export function buildDuoConversation(
  input: DuoConversationInput,
  getEntries: (runId: string) => StreamEntry[],
  isRunning: (runId: string) => boolean,
): DuoMessage[] {
  const sides: Record<DuoRole, { label: string; engine: string; runId: string | null }> = {
    designer: { label: input.designerLabel, engine: input.designerEngine, runId: input.designerRunId },
    reviewer: { label: input.reviewerLabel, engine: input.reviewerEngine, runId: input.reviewerRunId },
  }

  const mapped: Record<DuoRole, { turns: (StreamEntry[] | null)[]; live: StreamEntry[] | null }> = {
    designer: { turns: [], live: null },
    reviewer: { turns: [], live: null },
  }
  for (const role of ['designer', 'reviewer'] as DuoRole[]) {
    const runId = sides[role].runId
    const completed = input.turns.filter((t) => t.role === role).length
    if (!runId) {
      mapped[role] = { turns: Array(completed).fill(null), live: null }
      continue
    }
    // Only the side the orchestrator is waiting on can have a turn in flight;
    // `isRunning` (liveRuns status) pins the exact moment it stops, which avoids
    // double-rendering the last segment as both "completed" and "live".
    const takeLive = input.active && input.waiting === role && isRunning(runId)
    mapped[role] = mapSegments(getEntries(runId), completed, takeLive)
  }

  const out: DuoMessage[] = []
  if (input.goal.trim()) out.push({ kind: 'topic', id: 'topic', goal: input.goal.trim() })

  const seen: Record<DuoRole, number> = { designer: 0, reviewer: 0 }
  let lastRound = 0
  for (let i = 0; i < input.turns.length; i++) {
    const turn = input.turns[i]
    const side = sides[turn.role]
    if (turn.round !== lastRound) {
      out.push({ kind: 'round', id: `round-${turn.round}`, round: turn.round })
      lastRound = turn.round
    }
    out.push({
      kind: 'turn',
      id: `turn-${i}`,
      role: turn.role,
      label: side.label,
      engine: turn.engine || side.engine,
      round: turn.round,
      text: turn.text,
      at: turn.at,
      runId: side.runId,
      entries: mapped[turn.role].turns[seen[turn.role]] ?? null,
    })
    seen[turn.role] += 1
  }

  // A turn is only recorded once it finishes, so anything in flight lives here.
  // `state.round` counts COMPLETED rounds, hence +1 for the turn under way.
  if (input.waiting) {
    const role = input.waiting
    const liveEntries = mapped[role].live
    const runId = sides[role].runId
    if (liveEntries && runId) {
      const round = input.round + 1
      if (round !== lastRound) {
        out.push({ kind: 'round', id: `round-${round}`, round })
        lastRound = round
      }
      out.push({
        kind: 'live',
        id: `live-${role}-${round}`,
        role,
        label: sides[role].label,
        engine: sides[role].engine,
        round,
        runId,
        entries: liveEntries,
      })
    }
  }

  if (!input.active && (input.phase === 'done' || input.phase === 'stopped' || input.phase === 'error')) {
    out.push({ kind: 'notice', id: `notice-${input.phase}`, phase: input.phase, reason: input.stopReason })
  }
  return out
}

/** Render the conversation as a markdown transcript (the "copy" action). */
export function conversationToMarkdown(input: DuoConversationInput): string {
  const lines: string[] = []
  if (input.goal.trim()) lines.push(`# ${input.goal.trim()}`, '')
  for (const turn of input.turns) {
    const label = turn.role === 'designer' ? input.designerLabel : input.reviewerLabel
    lines.push(`## ${label} · ${turn.engine} · round ${turn.round}`, '', turn.text.trim(), '')
  }
  return lines.join('\n').trim() + '\n'
}
