// App-wide "what are the agents doing" state, mirrored onto
// `<html data-agent-state>` so the aurora background can react (see
// src/assets/agent-ambient.css and DESIGN_SYSTEM.md §6). Pure logic — the
// Vue side lives in composables/useAgentAmbient.ts.

export type AgentSteadyState = 'idle' | 'running' | 'waiting'
export type AgentTransient = 'error' | 'done'
export type AgentState = AgentSteadyState | AgentTransient

/** The minimal per-run shape the derivation needs (no stream content). */
export interface AgentRunSnapshot {
  id: string
  /** Live run status: `running` | `done` | `failed` | `cancelled` | … */
  status: string
  /** A prompt (tool permission / AskUserQuestion / plan approval) is pending. */
  waiting: boolean
}

/** How long each transient flash stays before falling back to the steady state. */
export const AGENT_TRANSIENT_MS: Record<AgentTransient, number> = {
  error: 4000,
  done: 3000,
}

/** Waiting beats running beats idle, across every run in every project. */
export function steadyAgentState(runs: readonly AgentRunSnapshot[]): AgentSteadyState {
  let running = false
  for (const r of runs) {
    if (r.waiting) return 'waiting'
    if (r.status === 'running') running = true
  }
  return running ? 'running' : 'idle'
}

/**
 * Transient flash triggered between two snapshots: a run that was `running`
 * and is now `failed` → error, now `done` → done. Error wins when both happen
 * at once. Cancelled runs (the user stopped them) flash nothing, and runs that
 * first appear already finished (history being loaded) don't count.
 */
export function detectAgentTransient(
  prev: readonly AgentRunSnapshot[],
  curr: readonly AgentRunSnapshot[],
): AgentTransient | null {
  if (!prev.length) return null
  const before = new Map<string, string>()
  for (const r of prev) before.set(r.id, r.status)
  let result: AgentTransient | null = null
  for (const r of curr) {
    if (before.get(r.id) !== 'running') continue
    if (r.status === 'failed') return 'error'
    if (r.status === 'done') result = 'done'
  }
  return result
}

/**
 * The transient to show when `incoming` arrives while `current` is on screen:
 * a done never cuts an error flash short; anything else replaces it (and the
 * caller restarts the timer).
 */
export function mergeAgentTransient(
  current: AgentTransient | null,
  incoming: AgentTransient,
): AgentTransient {
  return current === 'error' && incoming === 'done' ? 'error' : incoming
}

/**
 * Final state for `data-agent-state`. A pending prompt is actionable, so
 * waiting outranks a flash; otherwise the flash shows over running/idle.
 */
export function resolveAgentState(
  steady: AgentSteadyState,
  transient: AgentTransient | null,
): AgentState {
  if (steady === 'waiting') return 'waiting'
  return transient ?? steady
}

/** One-shot convenience: steady state + the transient this change triggers. */
export function deriveAgentState(
  prev: readonly AgentRunSnapshot[],
  curr: readonly AgentRunSnapshot[],
): { steady: AgentSteadyState; transient: AgentTransient | null } {
  return { steady: steadyAgentState(curr), transient: detectAgentTransient(prev, curr) }
}
