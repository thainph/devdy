-- Project Pulse (docs/project-pulse-spec.md §5): Slack threads / GitHub issues
-- the user watches per project, the AI digests of each, MCP → app requests
-- (BR-11) and the Slack user id behind each workspace token (BR-07).

CREATE TABLE IF NOT EXISTS pulse_watches (
  id              TEXT PRIMARY KEY,
  project_id      TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  source_type     TEXT NOT NULL CHECK (source_type IN ('slack', 'github_issue')),
  source_ref      TEXT NOT NULL,                     -- BR-02: <channel_id>:<thread_ts> | <owner>/<repo>#<n>
  source_url      TEXT NOT NULL,                     -- canonical link to open
  slack_thread_id TEXT REFERENCES slack_threads(id) ON DELETE SET NULL,
  title           TEXT,
  active          INTEGER NOT NULL DEFAULT 1,        -- 0 = unwatched (digests kept)
  added_via       TEXT NOT NULL CHECK (added_via IN ('mcp', 'button', 'paste')),
  added_run_id    TEXT,                              -- run that called the MCP tool
  resolve_status  TEXT NOT NULL DEFAULT 'pending' CHECK (resolve_status IN ('pending', 'ready', 'error')),
  last_error      TEXT,                              -- latest error code
  checked_at      TEXT,                              -- last refresh (changed or not)
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_pulse_watches_source ON pulse_watches(source_type, source_ref);
CREATE INDEX IF NOT EXISTS idx_pulse_watches_project ON pulse_watches(project_id, active);
CREATE INDEX IF NOT EXISTS idx_pulse_watches_pending
  ON pulse_watches(resolve_status) WHERE resolve_status = 'pending';

-- AI digest of one watch; the 5 newest per watch are kept.
CREATE TABLE IF NOT EXISTS pulse_digests (
  id             TEXT PRIMARY KEY,
  watch_id       TEXT NOT NULL REFERENCES pulse_watches(id) ON DELETE CASCADE,
  content_hash   TEXT NOT NULL,                      -- sha256 of the normalised source (BR-05)
  status         TEXT NOT NULL CHECK (status IN ('on_track', 'at_risk', 'blocked', 'waiting', 'done')),
  ball           TEXT NOT NULL CHECK (ball IN ('me', 'customer', 'team', 'none')),
  ball_since     TEXT,
  summary        TEXT NOT NULL DEFAULT '',
  next_action    TEXT,
  deadlines      TEXT NOT NULL DEFAULT '[]',         -- JSON [{title, due_at, all_day, evidence}]
  decisions      TEXT NOT NULL DEFAULT '[]',         -- JSON [{text, decided_at, evidence}]
  open_questions TEXT NOT NULL DEFAULT '[]',         -- JSON [{text, asked_by, evidence}]
  truncated      INTEGER NOT NULL DEFAULT 0,
  model          TEXT,
  created_at     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_pulse_digests_watch ON pulse_digests(watch_id, created_at DESC);

-- MCP → app requests (BR-11): the app polls rows with done_at IS NULL.
-- watch_id / project_id narrow a refresh; both NULL = everything.
CREATE TABLE IF NOT EXISTS pulse_requests (
  id         TEXT PRIMARY KEY,
  kind       TEXT NOT NULL CHECK (kind IN ('resolve', 'refresh')),
  watch_id   TEXT,
  project_id TEXT,
  created_at TEXT NOT NULL,
  done_at    TEXT
);
CREATE INDEX IF NOT EXISTS idx_pulse_requests_open
  ON pulse_requests(created_at) WHERE done_at IS NULL;

-- Slack user behind the token of each workspace (auth.test), for "ball is mine".
CREATE TABLE IF NOT EXISTS slack_identities (
  team_id    TEXT PRIMARY KEY,
  user_id    TEXT NOT NULL,
  fetched_at TEXT NOT NULL
);

-- The BR-11 poller only runs while a session is running: keep that check on an index.
CREATE INDEX IF NOT EXISTS idx_runs_running ON runs(status) WHERE status = 'running';
