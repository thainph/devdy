// Per-turn usage from the Claude SDK's cumulative `modelUsage`.
//
// Mirrors src-tauri/src/runs/model_usage.rs — keep the two in sync. A result's
// `usage` covers the main agent loop only (no subagents / compaction);
// `modelUsage` holds, per model, the session's RUNNING totals of every call.
// They are cumulative within a CLI process and, since CLI 2.1.288, across
// resumes — so a turn is the per-model difference from the previous result.

export interface ModelTotals {
  tokens: number
  cost: number
}

export type ModelUsageMap = Record<string, ModelTotals>

const CARRY_OVER_SINCE = [2, 1, 288]

/** Whether a CLI version's running totals continue across resumes. */
export function carriesOver(version: string | undefined): boolean {
  if (!version) return false
  const parts = version.match(/\d+/g)?.slice(0, 3).map(Number)
  if (!parts || parts.length < 3) return false
  for (let i = 0; i < 3; i++) {
    if (parts[i] !== CARRY_OVER_SINCE[i]) return parts[i] > CARRY_OVER_SINCE[i]
  }
  return true
}

/** Read `modelUsage` off a `result` event; undefined when absent or all-zero. */
export function parseModelUsage(e: Record<string, unknown>): ModelUsageMap | undefined {
  const raw = e.modelUsage
  if (!raw || typeof raw !== 'object') return undefined
  const num = (m: Record<string, unknown>, k: string) => (typeof m[k] === 'number' ? (m[k] as number) : 0)
  const out: ModelUsageMap = {}
  let any = false
  for (const [model, v] of Object.entries(raw as Record<string, unknown>)) {
    if (!v || typeof v !== 'object') continue
    const m = v as Record<string, unknown>
    const tokens =
      num(m, 'inputTokens') + num(m, 'outputTokens') + num(m, 'cacheReadInputTokens') + num(m, 'cacheCreationInputTokens')
    const cost = num(m, 'costUSD')
    out[model] = { tokens, cost }
    if (tokens || cost) any = true
  }
  return any ? out : undefined
}

/**
 * This turn's totals across models. With `prev` (same running total) each
 * model is differenced on its own; a model whose cost went down restarted, so
 * its whole figure counts. Token counters can dip on resume while the cost
 * keeps growing, so tokens never go negative.
 */
export function turnTotals(cur: ModelUsageMap, prev: ModelUsageMap | undefined): ModelTotals {
  let tokens = 0
  let cost = 0
  for (const [model, u] of Object.entries(cur)) {
    const p = prev?.[model]
    if (p && u.cost >= p.cost - 1e-9) {
      tokens += Math.max(0, u.tokens - p.tokens)
      cost += Math.max(0, u.cost - p.cost)
    } else {
      tokens += u.tokens
      cost += u.cost
    }
  }
  return { tokens, cost }
}
