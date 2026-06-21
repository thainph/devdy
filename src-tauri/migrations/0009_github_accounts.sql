-- Multiple shared GitHub accounts (label + PAT in Keychain), selectable per project.
-- Replaces the per-project PAT model (projects.has_pat, Keychain key pat_<project_id>).
CREATE TABLE IF NOT EXISTS github_accounts (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    username TEXT,            -- GitHub login, filled after validation
    scopes TEXT,             -- comma-separated scopes, cached at validation
    created_at TEXT NOT NULL
);

-- Project links to one shared account (NULL = not linked). PAT lives in Keychain
-- under key account_<account_id>, never per-project anymore.
ALTER TABLE projects ADD COLUMN github_account_id TEXT
    REFERENCES github_accounts(id) ON DELETE SET NULL;

-- Drop the legacy per-project PAT cache flag; superseded by github_account_id.
ALTER TABLE projects DROP COLUMN has_pat;
