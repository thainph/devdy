-- Slack Thread Translate: Slack threads leave the shared `captures` core and get
-- their own tables. Threads are fetched from Slack by link and stored per message
-- (original + Vietnamese). Existing kind='slack' captures are carried over as
-- legacy snapshots (old markdown + files kept viewable) and re-fetched by the app.
-- Files stay where they are: <app_data>/slack-threads/<id>/files/… (same dir).

-- 0040 renamed the 0038 table to `captures`, but SQLite keeps index names on
-- rename: `idx_slack_threads_position` / `_project` / `_url` still exist (on
-- `captures`) and would turn the CREATE INDEX IF NOT EXISTS below into no-ops.
-- Re-create them under `captures` names first.
DROP INDEX IF EXISTS idx_slack_threads_position;
DROP INDEX IF EXISTS idx_slack_threads_project;
DROP INDEX IF EXISTS idx_slack_threads_url;
CREATE INDEX IF NOT EXISTS idx_captures_position ON captures(position);
CREATE INDEX IF NOT EXISTS idx_captures_project  ON captures(project_id);

CREATE TABLE IF NOT EXISTS slack_threads (
  id               TEXT PRIMARY KEY,
  title            TEXT NOT NULL DEFAULT '',
  title_custom     INTEGER NOT NULL DEFAULT 0,     -- 1 = user-chosen, kept on refresh / re-import
  project_id       TEXT,
  position         INTEGER NOT NULL DEFAULT 0,     -- ascending; new rows = MIN(position) - 1
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  workspace_domain TEXT,                            -- link host, e.g. malme.slack.com
  team_id          TEXT,                            -- auth.test team_id
  workspace        TEXT,                            -- auth.test team (name)
  channel_id       TEXT,                            -- C… / G…; NULL only for unparsable legacy rows
  channel          TEXT,                            -- channel name WITHOUT '#'
  thread_ts        TEXT,                            -- thread root ts
  thread_url       TEXT,                            -- canonical link (§4)
  fetch_status     TEXT NOT NULL DEFAULT 'pending', -- pending|fetching|translating|ready|error
  fetch_error_code TEXT,                            -- §8.3
  fetch_error      TEXT,                            -- English detail
  fetched_at       TEXT,                            -- RFC3339 of the last successful fetch
  reply_count      INTEGER NOT NULL DEFAULT 0,
  last_reply_ts    TEXT,                            -- newest message ts
  last_seen_ts     TEXT,                            -- newest ts when the user last viewed it
  warnings         TEXT NOT NULL DEFAULT '[]',      -- JSON array of warning codes (§8.4)
  search_text      TEXT NOT NULL DEFAULT '',        -- generated plain transcript (vi + original) for search / preview
  legacy           INTEGER NOT NULL DEFAULT 0,      -- 1 = old zip/markdown snapshot not yet re-fetched
  legacy_content   TEXT                             -- old markdown body; NULL once re-fetched
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_slack_threads_key
  ON slack_threads(channel_id, thread_ts) WHERE channel_id IS NOT NULL AND thread_ts IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_slack_threads_position ON slack_threads(position);
CREATE INDEX IF NOT EXISTS idx_slack_threads_project  ON slack_threads(project_id);
CREATE INDEX IF NOT EXISTS idx_slack_threads_status   ON slack_threads(fetch_status);

-- One row per Slack message (seq 0 = parent).
CREATE TABLE IF NOT EXISTS slack_messages (
  thread_id        TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  ts               TEXT NOT NULL,
  seq              INTEGER NOT NULL,                -- 0 = parent, then chronological
  user_id          TEXT,
  bot_id           TEXT,
  author_name      TEXT,                            -- bot username / fallback name
  author_icon_url  TEXT,                            -- bot icon URL
  subtype          TEXT,
  text_raw         TEXT NOT NULL DEFAULT '',        -- Slack mrkdwn as returned
  text_vi          TEXT,                            -- Vietnamese, same mrkdwn syntax; NULL = not translated
  src_hash         TEXT NOT NULL,                   -- sha256 hex of text_raw
  translate_status TEXT NOT NULL DEFAULT 'pending', -- pending|done|skipped|error
  translate_error  TEXT,
  edited_ts        TEXT,
  reactions        TEXT NOT NULL DEFAULT '[]',      -- JSON [{"name":"+1","count":2}]
  file_ids         TEXT NOT NULL DEFAULT '[]',      -- JSON array of slack_files.id, message order
  PRIMARY KEY (thread_id, ts)
);
CREATE INDEX IF NOT EXISTS idx_slack_messages_seq ON slack_messages(thread_id, seq);

-- Files of a thread: Slack files (slack_file_id set) and carried-over legacy zip files (NULL).
CREATE TABLE IF NOT EXISTS slack_files (
  id              TEXT PRIMARY KEY,
  thread_id       TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  slack_file_id   TEXT,                             -- F…; NULL = legacy zip file
  message_ts      TEXT,                             -- first message carrying it; NULL = legacy
  name            TEXT NOT NULL,
  title           TEXT,
  mime            TEXT,
  filetype        TEXT,
  size            INTEGER NOT NULL DEFAULT 0,
  file_path       TEXT,                             -- RELATIVE to <app_data>; NULL until downloaded
  remote_updated  INTEGER,                          -- file.updated (or created), unix seconds
  permalink       TEXT,
  width           INTEGER,
  height          INTEGER,
  download_status TEXT NOT NULL DEFAULT 'pending',  -- done|pending|error|too_large|unavailable
  download_error  TEXT,
  created_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_files_thread ON slack_files(thread_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_slack_files_remote
  ON slack_files(thread_id, slack_file_id) WHERE slack_file_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS slack_users (
  team_id      TEXT NOT NULL,
  user_id      TEXT NOT NULL,
  display_name TEXT,
  real_name    TEXT,
  avatar_url   TEXT,                -- profile.image_72
  avatar_path  TEXT,                -- RELATIVE to <app_data>; NULL until downloaded
  is_bot       INTEGER NOT NULL DEFAULT 0,
  fetched_at   TEXT NOT NULL,
  PRIMARY KEY (team_id, user_id)
);

CREATE TABLE IF NOT EXISTS slack_channels (
  team_id    TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  name       TEXT,
  kind       TEXT,                  -- public|private|im|mpim|unknown
  fetched_at TEXT NOT NULL,
  PRIMARY KEY (team_id, channel_id)
);

-- Ids of merged-away duplicate legacy rows: the app deletes their
-- <app_data>/slack-threads/<id>/ dirs once, then clears this table.
CREATE TABLE IF NOT EXISTS slack_legacy_orphans (id TEXT PRIMARY KEY);

-- ---- Carry over kind='slack' captures ---------------------------------------
-- Best-effort parse of thread_url (/archives/<channel>/p<16 digits>); the app
-- re-parses every legacy row with the full parser before fetching.
CREATE TEMP TABLE _slack_legacy AS
WITH src AS (
  SELECT c.*,
         CASE WHEN instr(c.thread_url, '/archives/') > 0
              THEN substr(c.thread_url, instr(c.thread_url, '/archives/') + 10) END AS rest
  FROM captures c
  WHERE c.kind = 'slack'
),
parsed AS (
  SELECT src.*,
         CASE WHEN instr(rest, '/p') > 1 THEN substr(rest, 1, instr(rest, '/p') - 1) END AS ch,
         CASE WHEN instr(rest, '/p') > 1 THEN substr(rest, instr(rest, '/p') + 2, 16) END AS pd
  FROM src
),
keyed AS (
  SELECT parsed.*,
         CASE WHEN ch GLOB '[CG][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]*'
                   AND ch NOT GLOB '*[^A-Z0-9]*'
                   AND length(pd) = 16 AND pd NOT GLOB '*[^0-9]*'
              THEN ch END AS k_channel,
         CASE WHEN ch GLOB '[CG][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]*'
                   AND ch NOT GLOB '*[^A-Z0-9]*'
                   AND length(pd) = 16 AND pd NOT GLOB '*[^0-9]*'
              THEN substr(pd, 1, 10) || '.' || substr(pd, 11, 6) END AS k_ts
  FROM parsed
)
SELECT keyed.*,
       ROW_NUMBER() OVER (
         PARTITION BY COALESCE(k_channel || '|' || k_ts, id)
         ORDER BY updated_at DESC, created_at DESC, id ASC
       ) AS rn
FROM keyed;

INSERT INTO slack_threads (
  id, title, title_custom, project_id, position, created_at, updated_at,
  workspace_domain, workspace, channel_id, channel, thread_ts, thread_url,
  fetch_status, search_text, legacy, legacy_content
)
SELECT id, title, title_custom, project_id, position, created_at, updated_at,
       NULL, workspace, k_channel, ltrim(channel, '#'), k_ts, thread_url,
       'pending', title || char(10) || COALESCE(content, ''), 1, content
FROM _slack_legacy WHERE rn = 1;

INSERT INTO slack_files (id, thread_id, name, mime, size, file_path, download_status, created_at)
SELECT a.id, a.capture_id, a.name, a.mime, a.size, a.file_path, 'done', a.created_at
FROM capture_attachments a
JOIN _slack_legacy l ON l.id = a.capture_id AND l.rn = 1;

INSERT INTO slack_legacy_orphans (id) SELECT id FROM _slack_legacy WHERE rn > 1;

-- Remove the Slack rows from the captures core (children first; no file I/O here).
DELETE FROM capture_attachments WHERE capture_id IN (SELECT id FROM _slack_legacy);
DELETE FROM captures WHERE id IN (SELECT id FROM _slack_legacy);

DROP TABLE _slack_legacy;
