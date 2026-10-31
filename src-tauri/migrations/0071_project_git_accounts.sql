-- Several GitHub / GitLab accounts per project, toggled on/off like servers
-- (parallel to project_servers). A run's gh / glab / git calls may use any
-- enabled account; the agent picks one per command (DEVDY_GIT_ACCOUNT).
-- projects.github_account_id / gitlab_account_id stay as the project's DEFAULT
-- account (used when a command names none, for commit identity and for Devdy's
-- own API calls) and are always one of the enabled accounts.
CREATE TABLE IF NOT EXISTS project_github_accounts (
    project_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    created_at TEXT NOT NULL,                   -- RFC3339
    PRIMARY KEY (project_id, account_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (account_id) REFERENCES github_accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS project_gitlab_accounts (
    project_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    created_at TEXT NOT NULL,                   -- RFC3339
    PRIMARY KEY (project_id, account_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (account_id) REFERENCES gitlab_accounts(id) ON DELETE CASCADE
);

-- Existing links become enabled accounts.
INSERT OR IGNORE INTO project_github_accounts (project_id, account_id, created_at)
    SELECT id, github_account_id, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects WHERE github_account_id IS NOT NULL;

-- The reviewer account is retired (the agent now picks the account itself);
-- keep it available to the project as an enabled account. The column stays
-- (SQLite cannot drop an FK column) but is no longer read.
INSERT OR IGNORE INTO project_github_accounts (project_id, account_id, created_at)
    SELECT id, github_reviewer_account_id, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects WHERE github_reviewer_account_id IS NOT NULL;

INSERT OR IGNORE INTO project_gitlab_accounts (project_id, account_id, created_at)
    SELECT id, gitlab_account_id, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects WHERE gitlab_account_id IS NOT NULL;
