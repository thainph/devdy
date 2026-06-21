//! Estimated API-equivalent token pricing.
//!
//! Used to fill in `cost_usd` when the engine doesn't report it directly:
//! - Claude (Agent SDK) already returns `total_cost_usd`, so this is only a
//!   fallback for Claude.
//! - Codex (subscription) never reports a dollar cost, so its cost is always
//!   derived here and flagged `cost_estimated = 1`.
//!
//! Owner runs on subscriptions, so these dollar figures are a notional
//! "if this were billed per-token via API" estimate, not an actual charge.
//! Prices are USD per 1,000,000 tokens; matched by model-id prefix. Update as
//! published pricing changes.

struct ModelPrice {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

/// Resolve a price table by matching the model id against known prefixes.
/// Falls back to Sonnet-class pricing for unknown Claude-ish ids and to a
/// generic mid-tier rate for everything else.
fn price_for(model: &str) -> ModelPrice {
    let m = model.to_ascii_lowercase();

    // ---- Anthropic Claude ----
    if m.contains("opus") {
        return ModelPrice { input: 15.0, output: 75.0, cache_write: 18.75, cache_read: 1.5 };
    }
    if m.contains("haiku") {
        return ModelPrice { input: 0.80, output: 4.0, cache_write: 1.0, cache_read: 0.08 };
    }
    if m.contains("sonnet") {
        return ModelPrice { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.30 };
    }

    // ---- OpenAI / Codex (gpt-5 family) ----
    if m.contains("gpt-5") || m.contains("codex") || m.starts_with("o3") || m.starts_with("o4") {
        return ModelPrice { input: 1.25, output: 10.0, cache_write: 1.25, cache_read: 0.125 };
    }

    // ---- Fallback (mid-tier) ----
    ModelPrice { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.30 }
}

/// Estimate cost in USD from per-category token counts.
pub fn estimate_cost(
    model: &str,
    input_tokens: i64,
    output_tokens: i64,
    cache_creation_tokens: i64,
    cache_read_tokens: i64,
) -> f64 {
    let p = price_for(model);
    let per = 1_000_000.0;
    (input_tokens as f64) / per * p.input
        + (output_tokens as f64) / per * p.output
        + (cache_creation_tokens as f64) / per * p.cache_write
        + (cache_read_tokens as f64) / per * p.cache_read
}
