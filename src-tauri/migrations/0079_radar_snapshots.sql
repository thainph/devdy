-- Delivery Radar snapshots: the last `list_milestone_board` result of each
-- project, so the built-in `devdy` MCP server (which reads this DB directly and
-- has no GitHub credentials) can answer radar questions. Refreshed whenever the
-- radar window or an MCP refresh (via the broker) fetches the board.
-- board: JSON MilestoneBoard (camelCase, same shape the UI receives).
-- saved_at: unix epoch milliseconds.
CREATE TABLE IF NOT EXISTS radar_snapshots (
    project_id TEXT PRIMARY KEY,
    board TEXT NOT NULL,
    saved_at INTEGER NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);
