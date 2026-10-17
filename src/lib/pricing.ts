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

// Anthropic: cache writes 1.25x input (5-minute TTL). OpenAI: cache writes
// billed as plain input.
const claude = (input: number, output: number, cacheRead: number): ModelPrice => ({
  input,
  output,
  cacheWrite: input * 1.25,
  cacheRead,
})
const openai = (input: number, output: number, cacheRead: number): ModelPrice => ({
  input,
  output,
  cacheWrite: input,
  cacheRead,
})

// USD per 1,000,000 tokens. First matching substring wins, so more specific
// ids come first (`opus-5-5` before `opus-5`, `gpt-5.5-pro` before `gpt-5.5`).
const PRICES: [string, ModelPrice][] = [
  ['fable-5-1', claude(10, 50, 0.25)],
  ['mythos-5-1', claude(10, 50, 0.25)],
  ['fable', claude(10, 50, 1)],
  ['mythos', claude(10, 50, 1)],
  ['opus-5-5', claude(4, 20, 0.2)],
  ['opus-5', claude(5, 25, 0.5)],
  ['opus-4-8', claude(5, 25, 0.5)],
  ['opus-4-7', claude(5, 25, 0.5)],
  ['opus-4-6', claude(5, 25, 0.5)],
  ['opus-4-5', claude(5, 25, 0.5)],
  ['opus', claude(15, 75, 1.5)],
  ['sonnet-5', claude(2, 10, 0.2)],
  ['sonnet', claude(3, 15, 0.3)],
  ['haiku-4-5', claude(1, 5, 0.1)],
  ['haiku', claude(0.8, 4, 0.08)],
  ['gpt-6-astra', openai(10, 50, 1)],
  ['gpt-5.6-terra', openai(2, 12, 0.2)],
  ['gpt-5.6-sol', openai(2, 10, 0.2)],
  ['gpt-5.6-luna', openai(0.2, 1.2, 0.02)],
  ['gpt-5.5-pro', openai(30, 180, 30)],
  ['gpt-5.5', openai(5, 30, 0.5)],
  ['gpt-5.3-codex', openai(1.75, 14, 0.175)],
  ['gpt-5', openai(1.25, 10, 0.125)],
  ['codex', openai(1.25, 10, 0.125)],
]
const FALLBACK = claude(3, 15, 0.3)

function priceFor(model: string): ModelPrice {
  const m = model.toLowerCase()
  if (m.startsWith('o3') || m.startsWith('o4')) return openai(1.25, 10, 0.125)
  return PRICES.find(([key]) => m.includes(key))?.[1] ?? FALLBACK
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
