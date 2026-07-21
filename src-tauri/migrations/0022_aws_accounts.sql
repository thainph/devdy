-- Central AWS account management (parallel to github_accounts/gitlab_accounts).
-- Secret Access Key and Session Token live in the consolidated Keychain item
-- (devdy_secret_store_v1), map `aws`, keyed by account id. SQLite stores only
-- metadata needed for display, validation cache, and per-run credential wiring.
CREATE TABLE IF NOT EXISTS aws_accounts (
    id                TEXT PRIMARY KEY,
    label             TEXT NOT NULL,
    auth_method       TEXT NOT NULL,  -- 'keys' | 'profile'
    account_id        TEXT,           -- AWS 12-digit account id from STS
    arn               TEXT,           -- caller ARN from STS
    region            TEXT NOT NULL,
    access_key_id     TEXT,           -- keys auth only; not the secret key
    profile_name      TEXT,           -- profile auth only
    tags              TEXT,
    last_validated_at TEXT,
    created_at        TEXT NOT NULL
);
