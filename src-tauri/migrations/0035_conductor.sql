-- Conductor: an AI session that orchestrates other sessions ("workers") via the
-- in-process Rust MCP server (`conductor`). A conductor run drives its workers
-- with the session_* tools instead of the frontend acting as the coordinator.
--
-- `role` marks a run as a conductor or a worker so the app can render the tree
-- and reconnect after a restart. Normal runs keep role NULL (treated as 'normal').
ALTER TABLE runs ADD COLUMN role TEXT;

-- The conductor session a worker belongs to (its conductor's run id). NULL for
-- normal runs and for conductors themselves.
ALTER TABLE runs ADD COLUMN conductor_run_id TEXT;

-- One row per conductor session: the goal it was given and coarse status. Kept
-- in SQLite (not localStorage) so an in-flight orchestration survives app close
-- and can be reconnected on next launch.
CREATE TABLE IF NOT EXISTS conductor_sessions (
    id TEXT PRIMARY KEY,                    -- == the conductor run id
    project_id TEXT NOT NULL,
    goal TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL DEFAULT 'running', -- running | done | failed | cancelled
    max_workers INTEGER NOT NULL DEFAULT 6,
    created_at TEXT NOT NULL,
    finished_at TEXT,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

-- Durable audit log of the conductor's control-plane decisions (spawn/send/read/
-- cancel), for observability and post-hoc debugging. Free-form JSON payload.
CREATE TABLE IF NOT EXISTS conductor_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id TEXT NOT NULL,
    ts TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload_json TEXT NOT NULL DEFAULT '{}',
    FOREIGN KEY (session_id) REFERENCES conductor_sessions(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_conductor_events_session ON conductor_events(session_id);
CREATE INDEX IF NOT EXISTS idx_runs_conductor ON runs(conductor_run_id);
