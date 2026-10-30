-- Focus board (docs/focus-board-spec.md §5.1): todos gain due time, priority,
-- estimate, status, source, snooze / reminder, AI plan and actual time; Project
-- Pulse is removed.
ALTER TABLE todos ADD COLUMN due_time TEXT;              -- HH:MM, NULL = cả ngày
ALTER TABLE todos ADD COLUMN priority INTEGER NOT NULL DEFAULT 2;
ALTER TABLE todos ADD COLUMN estimate_min INTEGER;
ALTER TABLE todos ADD COLUMN estimate_source TEXT;       -- user | ai
ALTER TABLE todos ADD COLUMN status TEXT NOT NULL DEFAULT 'todo';  -- todo|doing|waiting
ALTER TABLE todos ADD COLUMN snoozed_until TEXT;         -- RFC3339
ALTER TABLE todos ADD COLUMN remind_at TEXT;             -- RFC3339
ALTER TABLE todos ADD COLUMN source_type TEXT;           -- slack|github|gitlab|run|url
ALTER TABLE todos ADD COLUMN source_ref TEXT;
ALTER TABLE todos ADD COLUMN source_url TEXT;
ALTER TABLE todos ADD COLUMN started_at TEXT;
ALTER TABLE todos ADD COLUMN completed_at TEXT;
ALTER TABLE todos ADD COLUMN actual_min INTEGER;
ALTER TABLE todos ADD COLUMN ai_plan TEXT;
ALTER TABLE todos ADD COLUMN ai_analyzed_at TEXT;
ALTER TABLE todos ADD COLUMN updated_at TEXT;

CREATE INDEX IF NOT EXISTS idx_todos_due ON todos(done, due_date);

-- Local only (excluded from sync): which reminders already fired (BR-07).
CREATE TABLE IF NOT EXISTS todo_notifications (
    todo_id  TEXT NOT NULL,
    kind     TEXT NOT NULL,
    fire_key TEXT NOT NULL,
    fired_at TEXT NOT NULL,
    PRIMARY KEY (todo_id, kind, fire_key)
);

CREATE TABLE IF NOT EXISTS focus_day_plans (
    date       TEXT PRIMARY KEY,   -- YYYY-MM-DD
    plan_json  TEXT NOT NULL,
    created_at TEXT NOT NULL
);

DROP TABLE IF EXISTS pulse_requests;
DROP TABLE IF EXISTS pulse_digests;
DROP TABLE IF EXISTS pulse_watches;
