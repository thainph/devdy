// Estimated API-equivalent token pricing for the live result summary.
//
// Claude (Agent SDK) reports `total_cost_usd` directly, so this is only used
// for Codex, whose subscription runs never report a dollar cost. The figure is
// a notional "if billed per-token via API" estimate, not an actual charge.
//
// Mirrors src-tauri/src/runs/pricing.rs — keep the two tables in sync. The Rust
// side remains the source of truth for persisted usage rows; this only powers
// the live on-screen estimate.

interface ModelPrice {
  input: number
  output: number
  cacheWrite: number
  cacheRead: number
}

// USD per 1,000,000 tokens, matched by model-id prefix.
function priceFor(model: string): ModelPrice {
  const m = model.toLowerCase()
  if (m.includes('opus')) return { input: 15.0, output: 75.0, cacheWrite: 18.75, cacheRead: 1.5 }
  if (m.includes('haiku')) return { input: 0.8, output: 4.0, cacheWrite: 1.0, cacheRead: 0.08 }
  if (m.includes('sonnet')) return { input: 3.0, output: 15.0, cacheWrite: 3.75, cacheRead: 0.3 }
  if (m.includes('gpt-5') || m.includes('codex') || m.startsWith('o3') || m.startsWith('o4'))
    return { input: 1.25, output: 10.0, cacheWrite: 1.25, cacheRead: 0.125 }
  return { input: 3.0, output: 15.0, cacheWrite: 3.75, cacheRead: 0.3 }
}

export function estimateCost(
  model: string,
  inputTokens: number,
  outputTokens: number,
  cacheCreationTokens: number,
  cacheReadTokens: number,
): number {
  const p = priceFor(model)
  const per = 1_000_000
  return (
    (inputTokens / per) * p.input +
    (outputTokens / per) * p.output +
    (cacheCreationTokens / per) * p.cacheWrite +
    (cacheReadTokens / per) * p.cacheRead
  )
}
