-- Add provider identity to repos so fetch can branch GitHub vs GitLab (BR-001, DATA-001).
-- Existing rows default to 'github' so the current GitHub flow is unchanged (NFR-001).
ALTER TABLE repos ADD COLUMN provider TEXT NOT NULL DEFAULT 'github';

-- GitLab identity: display path (namespace/project) and numeric project id used
-- as the preferred `:id` for the REST API (INT-004). Both NULL for GitHub repos.
ALTER TABLE repos ADD COLUMN gitlab_project_path TEXT;
ALTER TABLE repos ADD COLUMN gitlab_project_id INTEGER;

-- Backfill: make the default explicit for any pre-existing rows.
UPDATE repos SET provider = 'github' WHERE provider IS NULL OR provider = '';
