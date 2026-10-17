-- Per-run baseline of the SDK's cumulative per-model usage.
--
-- A Claude `result` event's `modelUsage` maps each model to its RUNNING totals
-- (tokens + costUSD) for every call in the session — main loop, Task
-- subagents, compaction — whereas `usage` covers the main loop only. Since CLI
-- 2.1.288 those totals also carry over resumes, i.e. across Devdy turns. The
-- ledger records each turn's per-model difference from the run's previous
-- `modelUsage`, which is kept here (see runs::model_usage).
--
-- `model_usage`: JSON object, model id → {input, output, cache_read,
--   cache_creation, cost}.
-- `carries_over`: 1 when that result came from a CLI whose totals continue
--   across resumes, so the next resumed turn may be differenced against it.
CREATE TABLE IF NOT EXISTS run_usage_baseline (
    run_id       TEXT PRIMARY KEY,
    model_usage  TEXT NOT NULL,
    carries_over INTEGER NOT NULL,
    updated_at   TEXT NOT NULL
);
