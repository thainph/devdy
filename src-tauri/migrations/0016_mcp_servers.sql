-- Central MCP server management (parallel to skills/rules, injected at run launch).
-- Secret env/header VALUEs live in the macOS Keychain (service `devdy-mcp`,
-- account = server_id); SQLite only keeps the KEY names for form rendering.

CREATE TABLE IF NOT EXISTS mcp_servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,      -- valid MCP key (a-zA-Z0-9_-)
    description TEXT,
    transport   TEXT NOT NULL,             -- 'stdio' | 'http' | 'sse'
    command     TEXT,                      -- stdio (non-sensitive)
    args        TEXT,                      -- JSON array (stdio)
    url         TEXT,                      -- http/sse
    env_keys    TEXT,                      -- JSON array of env var KEY names (VALUE in Keychain)
    header_keys TEXT,                      -- JSON array of header KEY names (VALUE in Keychain)
    enabled     INTEGER NOT NULL DEFAULT 1,-- global master switch
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_mcp_servers (
    project_id TEXT NOT NULL,
    server_id  TEXT NOT NULL,
    enabled_at TEXT NOT NULL,
    PRIMARY KEY (project_id, server_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id)  REFERENCES mcp_servers(id) ON DELETE CASCADE
);
