-- Slack Thread Inbox: threads pushed by the Chrome extension over the local
-- inbox HTTP API (or imported manually), plus their stored attachments. Files
-- live under <app_data>/slack-threads/<thread_id>/files/.
CREATE TABLE IF NOT EXISTS slack_threads (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL DEFAULT '',
  content     TEXT NOT NULL DEFAULT '',      -- markdown body (no front matter)
  workspace   TEXT,
  channel     TEXT,
  thread_url  TEXT,
  thread_ts   TEXT,
  exported_at TEXT,
  project_id  TEXT,
  position    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_threads_position ON slack_threads(position);
CREATE INDEX IF NOT EXISTS idx_slack_threads_project  ON slack_threads(project_id);
CREATE INDEX IF NOT EXISTS idx_slack_threads_url      ON slack_threads(thread_url);

CREATE TABLE IF NOT EXISTS slack_thread_attachments (
  id         TEXT PRIMARY KEY,
  thread_id  TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  name       TEXT NOT NULL,      -- original relative path inside the zip
  file_path  TEXT NOT NULL,      -- path RELATIVE to <app_data>, e.g. slack-threads/<id>/files/x.png
  size       INTEGER NOT NULL,
  mime       TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_thread_att_thread ON slack_thread_attachments(thread_id);
