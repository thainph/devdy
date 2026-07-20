-- AI-driven Deploy (GĐ3). Adds the per-(project, server, role) deploy playbook
-- table and two nullable columns on `runs` so a deploy run can carry its target
-- server + role. No secrets here — the passphrase stays in the Keychain; a
-- playbook only holds public connection metadata (remote_path/branch) and the
-- natural-language instructions for the agent.
CREATE TABLE IF NOT EXISTS deploy_playbooks (
    id           TEXT PRIMARY KEY,                    -- UUID v4
    project_id   TEXT NOT NULL,
    server_id    TEXT NOT NULL,
    role         TEXT NOT NULL DEFAULT 'production',  -- deployment role (staging|production|…)
    remote_path  TEXT,                                -- deploy directory on the VPS
    branch       TEXT,                                -- git branch to deploy
    instructions TEXT,                                -- natural-language playbook for the agent
    created_at   TEXT NOT NULL,                       -- RFC3339
    updated_at   TEXT NOT NULL,                       -- RFC3339
    UNIQUE (project_id, server_id, role),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id)  REFERENCES servers(id) ON DELETE CASCADE
);

-- A deploy run is a `session` run tagged with its target server + role. NULL on
-- both columns marks an ordinary (non-deploy) run. SQLite cannot ADD COLUMN with
-- an inline FK, so these stay loose (nullable), matching the other runs columns.
ALTER TABLE runs ADD COLUMN server_id TEXT;
ALTER TABLE runs ADD COLUMN deploy_role TEXT;
