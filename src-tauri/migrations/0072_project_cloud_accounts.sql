-- Several AWS / Azure accounts per project, toggled on/off like the git
-- accounts (0071). A run's aws / az / terraform calls may use any enabled
-- account; the agent picks one per command (DEVDY_AWS_ACCOUNT /
-- DEVDY_AZURE_ACCOUNT). projects.aws_account_id / azure_account_id stay as the
-- project's DEFAULT account (used when a command names none, and for the run's
-- AWS sandbox region) and are always one of the enabled accounts.
CREATE TABLE IF NOT EXISTS project_aws_accounts (
    project_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    created_at TEXT NOT NULL,                   -- RFC3339
    PRIMARY KEY (project_id, account_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (account_id) REFERENCES aws_accounts(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS project_azure_accounts (
    project_id TEXT NOT NULL,
    account_id TEXT NOT NULL,
    created_at TEXT NOT NULL,                   -- RFC3339
    PRIMARY KEY (project_id, account_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (account_id) REFERENCES azure_accounts(id) ON DELETE CASCADE
);

-- Existing links become enabled accounts.
INSERT OR IGNORE INTO project_aws_accounts (project_id, account_id, created_at)
    SELECT id, aws_account_id, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects WHERE aws_account_id IS NOT NULL;

INSERT OR IGNORE INTO project_azure_accounts (project_id, account_id, created_at)
    SELECT id, azure_account_id, strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects WHERE azure_account_id IS NOT NULL;
