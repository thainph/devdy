/**
 * Per-model context-window limits (max tokens a single conversation can hold)
 * and helpers to compute how full the current run's context is.
 *
 * These are the *context window* sizes — unrelated to the global monthly/weekly
 * token *budget* (that lives in the budget store / stats ledger).
 */

const DEFAULT_LIMIT = 200_000

/**
 * Resolve the context-window limit for a model id.
 *
 * @param model   The model id as seen on the `system.init` event (e.g.
 *                `claude-opus-4-8`, `claude-opus-4-8[1m]`, `gpt-5-codex`).
 * @param override Optional user override from settings (0/undefined = ignore).
 */
export function resolveContextLimit(
  model: string | null | undefined,
  override?: number | null,
): number {
  if (override && override > 0) return override
  const m = (model ?? '').toLowerCase()
  if (!m) return DEFAULT_LIMIT
  // 1M-context variants are tagged `[1m]` in the model id.
  if (m.includes('[1m]') || m.includes('1m]')) return 1_000_000
  // Fable / Mythos ship a 1M context window by default (max == default).
  if (m.includes('fable') || m.includes('mythos')) return 1_000_000
  // Claude family.
  if (m.includes('opus') || m.includes('sonnet') || m.includes('haiku') || m.includes('claude')) {
    return 200_000
  }
  // OpenAI / Codex family (gpt-5 / gpt-5-codex carry a 272k context window).
  if (m.includes('gpt-5') || m.includes('gpt5') || m.includes('codex')) return 272_000
  if (m.includes('o3') || m.includes('o4')) return 200_000
  return DEFAULT_LIMIT
}

/**
 * Reconcile the model id reported on `system.init` with the composer's model
 * selection to resolve the *effective* context model.
 *
 * The Claude CLI/SDK reports the canonical model id (e.g. `claude-opus-4-8`) on
 * `system.init` and drops the `[1m]` context-beta suffix — so a run launched with
 * `opus[1m]` looks identical to a 200K `opus` run in the stream. The composer
 * selection is the only place that still carries `[1m]`, so re-attach it here.
 *
 * @param reported Model id from `system.init` (what actually ran).
 * @param selected Model id from the composer selection (may carry `[1m]`).
 */
export function mergeContextModel(
  reported: string | null | undefined,
  selected: string | null | undefined,
): string | null {
  const has1m = (s: string | null | undefined) => !!s && /1m\]/i.test(s)
  if (has1m(selected) && reported && !has1m(reported)) return `${reported}[1m]`
  return reported || selected || null
}

/** Format a token count compactly, e.g. 45200 -> "45.2k". */
export function formatTokensShort(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`
  return String(n)
}
