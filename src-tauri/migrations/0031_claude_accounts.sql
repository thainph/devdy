-- Multiple Claude Code accounts, isolated by CLAUDE_CONFIG_DIR.
--
-- Devdy stores only metadata and the profile/config directory. Claude Code owns
-- the actual OAuth credential in its normal config/Keychain store for that dir.
CREATE TABLE IF NOT EXISTS claude_accounts (
    id              TEXT PRIMARY KEY,
    label           TEXT NOT NULL UNIQUE,
    config_dir      TEXT NOT NULL UNIQUE,
    email           TEXT,
    auth_method     TEXT,
    api_provider    TEXT,
    status          TEXT NOT NULL DEFAULT 'unknown',
    is_default      INTEGER NOT NULL DEFAULT 0,
    last_checked_at TEXT,
    created_at      TEXT NOT NULL
);

ALTER TABLE projects ADD COLUMN claude_account_id TEXT
    REFERENCES claude_accounts(id) ON DELETE SET NULL;

ALTER TABLE runs ADD COLUMN claude_account_id TEXT
    REFERENCES claude_accounts(id) ON DELETE SET NULL;

CREATE INDEX IF NOT EXISTS idx_projects_claude_account ON projects(claude_account_id);
CREATE INDEX IF NOT EXISTS idx_runs_claude_account ON runs(claude_account_id);
