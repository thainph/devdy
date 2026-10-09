-- Generic "captures" core: Slack threads (kind 'slack') and web pages
-- (kind 'web') share one table + one attachments table. 0038/0039 already
-- shipped to dev DBs, so this migration renames/extends instead of editing them.
-- SQLite >= 3.26 (legacy_alter_table off) rewrites the attachments FK to point
-- at `captures` on rename.
ALTER TABLE slack_threads RENAME TO captures;
ALTER TABLE captures ADD COLUMN kind TEXT NOT NULL DEFAULT 'slack';   -- 'slack' | 'web'
ALTER TABLE captures ADD COLUMN source_url  TEXT;   -- web: page URL as sent
ALTER TABLE captures ADD COLUMN url_key     TEXT;   -- web: normalized URL (dedup key)
ALTER TABLE captures ADD COLUMN site_name   TEXT;
ALTER TABLE captures ADD COLUMN author      TEXT;
ALTER TABLE captures ADD COLUMN published_at TEXT;
ALTER TABLE captures ADD COLUMN description TEXT;
ALTER TABLE captures ADD COLUMN selection   INTEGER NOT NULL DEFAULT 0; -- 1 = only the user's selection
ALTER TABLE slack_thread_attachments RENAME TO capture_attachments;
ALTER TABLE capture_attachments RENAME COLUMN thread_id TO capture_id;
CREATE INDEX IF NOT EXISTS idx_captures_kind    ON captures(kind);
CREATE INDEX IF NOT EXISTS idx_captures_url_key ON captures(url_key);
