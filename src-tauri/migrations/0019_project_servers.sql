-- Per-project VPS assignment (parallel to project_mcp_servers). Maps a managed
-- server (0018_servers.sql) to a project under a deployment role (e.g. staging
-- / production). A single server may be mapped to the same project under
-- multiple roles (one row per role). No secrets here — the passphrase stays in
-- the Keychain consolidated item, and this table only references server ids.
CREATE TABLE IF NOT EXISTS project_servers (
    project_id TEXT NOT NULL,
    server_id  TEXT NOT NULL,
    role       TEXT NOT NULL DEFAULT 'production', -- deployment role (staging|production|…)
    created_at TEXT NOT NULL,                      -- RFC3339
    PRIMARY KEY (project_id, server_id, role),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id)  REFERENCES servers(id) ON DELETE CASCADE
);
