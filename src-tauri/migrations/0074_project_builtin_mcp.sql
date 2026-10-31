-- Built-in MCP servers (`devdy`, `google`) switched off for one project. They
-- are on everywhere by default, so only the opt-outs are stored; the app-wide
-- switch stays in settings (`mcp_builtin_<name>_enabled`).
CREATE TABLE IF NOT EXISTS project_builtin_mcp_disabled (
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,                         -- `devdy` | `google`
    created_at TEXT NOT NULL,                   -- RFC3339
    PRIMARY KEY (project_id, name),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);
