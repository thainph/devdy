-- Work Dashboard: Slack users the user marked as customers (by hand). A thread
-- whose latest human message comes from one of them is "waiting on me".
-- Kept apart from `slack_users`, which is a cache rewritten on every refresh.
CREATE TABLE IF NOT EXISTS slack_customers (
  team_id    TEXT NOT NULL,
  user_id    TEXT NOT NULL,
  project_id TEXT REFERENCES projects(id) ON DELETE SET NULL,
  note       TEXT NOT NULL DEFAULT '',
  created_at TEXT NOT NULL,
  PRIMARY KEY (team_id, user_id)
);
