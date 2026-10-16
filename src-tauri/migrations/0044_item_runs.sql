-- Notes / todos ↔ sessions (runs), many-to-many — same shape as
-- `slack_thread_runs` (0043). A note or todo can feed several sessions and a
-- session can reference several of them.
--
-- `run_id` / `project_id` are unenforced (no FK to runs in this DB):
-- `delete_run_inner` / `remove_project` remove the links, and readers join
-- `runs`, so a link whose run vanished another way is never listed.
-- `project_id` is the run's project at link time.
CREATE TABLE IF NOT EXISTS note_runs (
  note_id    TEXT NOT NULL REFERENCES notes(id) ON DELETE CASCADE,
  run_id     TEXT NOT NULL,
  project_id TEXT NOT NULL,
  source     TEXT NOT NULL,          -- manual | picker | created_from_item | captured
  linked_at  TEXT NOT NULL,          -- RFC3339
  PRIMARY KEY (note_id, run_id)
);
CREATE INDEX IF NOT EXISTS idx_note_runs_run ON note_runs(run_id);

CREATE TABLE IF NOT EXISTS todo_runs (
  todo_id    TEXT NOT NULL REFERENCES todos(id) ON DELETE CASCADE,
  run_id     TEXT NOT NULL,
  project_id TEXT NOT NULL,
  source     TEXT NOT NULL,          -- manual | picker | created_from_item | captured
  linked_at  TEXT NOT NULL,          -- RFC3339
  PRIMARY KEY (todo_id, run_id)
);
CREATE INDEX IF NOT EXISTS idx_todo_runs_run ON todo_runs(run_id);

-- The single capture backlink (0032) becomes a 'captured' link. Dangling ids
-- (run deleted since) are dropped. The old `run_id` columns stay but are no
-- longer read or written by the app.
INSERT OR IGNORE INTO note_runs (note_id, run_id, project_id, source, linked_at)
SELECT n.id, n.run_id, r.project_id, 'captured', n.created_at
FROM notes n JOIN runs r ON r.id = n.run_id
WHERE n.run_id IS NOT NULL;

INSERT OR IGNORE INTO todo_runs (todo_id, run_id, project_id, source, linked_at)
SELECT t.id, t.run_id, r.project_id, 'captured', t.created_at
FROM todos t JOIN runs r ON r.id = t.run_id
WHERE t.run_id IS NOT NULL;
