-- Sort the History list by the last time a session was actually worked on, not
-- by when its row was created. `created_at` is stamped once and never moves, so
-- a session resumed days later stayed buried under newer but idle ones.
--
-- Nullable on purpose: INSERTs don't set it (a brand-new run has no activity
-- beyond its own creation), and readers fall back with
-- COALESCE(last_activity_at, finished_at, started_at, created_at). The backfill
-- below seeds exactly that expression for existing rows.
ALTER TABLE runs ADD COLUMN last_activity_at TEXT;

UPDATE runs SET last_activity_at = COALESCE(finished_at, started_at, created_at);
