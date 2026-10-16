-- Slack threads ↔ sessions (runs), many-to-many. A thread can feed several
-- sessions and a session can reference several threads.
--
-- `run_id` / `project_id` are unenforced (no FK to runs in this DB):
-- `delete_run_inner` removes a run's links, and readers join `runs` so a link
-- whose run vanished another way is simply not listed. `project_id` is the
-- run's project at link time.
CREATE TABLE IF NOT EXISTS slack_thread_runs (
  thread_id  TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  run_id     TEXT NOT NULL,
  project_id TEXT NOT NULL,
  source     TEXT NOT NULL,          -- manual | picker | created_from_thread
  linked_at  TEXT NOT NULL,          -- RFC3339
  PRIMARY KEY (thread_id, run_id)
);
CREATE INDEX IF NOT EXISTS idx_slack_thread_runs_run ON slack_thread_runs(run_id);
