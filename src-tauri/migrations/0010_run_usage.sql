-- Token/cost usage ledger. One row per `result` stream event (per turn); a run
-- that is resumed produces multiple rows.
--
-- Intentionally has NO foreign keys to runs/projects: usage is independent
-- historical data that must survive deletion of the originating run or project.
-- Every column needed for stats is snapshotted at write time. The only path that
-- clears this table is the explicit `reset_usage_stats` command.
CREATE TABLE IF NOT EXISTS run_usage (
    id                      TEXT PRIMARY KEY,
    run_id                  TEXT,
    project_id              TEXT,
    project_name            TEXT,
    engine                  TEXT NOT NULL,
    model                   TEXT,
    input_tokens            INTEGER NOT NULL DEFAULT 0,
    output_tokens           INTEGER NOT NULL DEFAULT 0,
    cache_creation_tokens   INTEGER NOT NULL DEFAULT 0,
    cache_read_tokens       INTEGER NOT NULL DEFAULT 0,
    total_tokens            INTEGER NOT NULL DEFAULT 0,
    cost_usd                REAL,
    cost_estimated          INTEGER NOT NULL DEFAULT 0,
    num_turns               INTEGER,
    duration_ms             INTEGER,
    created_at              TEXT NOT NULL,
    deleted_run             INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_run_usage_created ON run_usage(created_at);
CREATE INDEX IF NOT EXISTS idx_run_usage_project ON run_usage(project_id);
CREATE INDEX IF NOT EXISTS idx_run_usage_engine  ON run_usage(engine);
CREATE INDEX IF NOT EXISTS idx_run_usage_run     ON run_usage(run_id);
