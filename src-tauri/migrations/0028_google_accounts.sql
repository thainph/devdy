-- Connected Google accounts (Drive + Gmail). Metadata only; the long-lived
-- refresh_token lives in the consolidated Keychain item (keyed by id), and the
-- shared OAuth client_id/secret is stored there too. Multiple accounts may be
-- connected at once; exactly one is flagged is_default (used when a tool call
-- omits its `account` argument).
CREATE TABLE google_accounts (
    id         TEXT PRIMARY KEY,
    label      TEXT NOT NULL UNIQUE,
    email      TEXT,
    scope      TEXT,
    is_default INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
