-- Multiple shared GitLab accounts (label + PAT in Keychain), selectable per project.
-- Peer to github_accounts; a project may link one of each independently.
CREATE TABLE IF NOT EXISTS gitlab_accounts (
    id TEXT PRIMARY KEY,
    label TEXT NOT NULL,
    username TEXT,            -- GitLab username, filled after validation
    host TEXT,                -- GitLab host (e.g. gitlab.com or self-hosted)
    email TEXT,               -- git commit identity email
    scopes TEXT,              -- comma-separated scopes, cached at validation
    created_at TEXT NOT NULL
);

-- Project links to one shared GitLab account (NULL = not linked). PAT lives in
-- Keychain under key gitlab_account_<account_id>.
ALTER TABLE projects ADD COLUMN gitlab_account_id TEXT
    REFERENCES gitlab_accounts(id) ON DELETE SET NULL;
