-- Central VPS/server management (parallel to github_accounts / mcp_servers).
-- The private-key passphrase (if any) lives in the macOS Keychain consolidated
-- item (devdy_secret_store_v1), map `servers`, keyed by server id. SQLite never
-- stores the passphrase.
CREATE TABLE IF NOT EXISTS servers (
    id               TEXT PRIMARY KEY,           -- UUID v4
    label            TEXT NOT NULL,              -- display name
    host             TEXT NOT NULL,              -- IP or domain
    port             INTEGER NOT NULL DEFAULT 22,-- SSH port
    username         TEXT NOT NULL,              -- SSH user
    auth_method      TEXT NOT NULL,              -- 'agent' | 'key' (never 'password')
    private_key_path TEXT,                       -- required only when auth_method='key'
    tags             TEXT,                       -- CSV for filtering/grouping
    status           TEXT,                       -- last-known: 'online'|'offline'|'unknown'
    last_checked_at  TEXT,                       -- RFC3339, updated after each test
    created_at       TEXT NOT NULL               -- RFC3339
);
