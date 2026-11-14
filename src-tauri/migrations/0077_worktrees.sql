-- Per-task git worktrees: a session can run in its own checkout of the project
-- (`<project>/.devdy/worktrees/<slug>`, branch `devdy/<slug>`) so several
-- sessions edit one repo in parallel. The run still belongs to its project
-- (accounts, MCP, broker, VPS); only its working directory changes.
CREATE TABLE IF NOT EXISTS worktrees (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    branch TEXT NOT NULL,
    base_ref TEXT NOT NULL,                     -- e.g. origin/main, what the branch started from
    todo_id TEXT,                               -- the todo it was made for (no FK: todos can be cleared)
    created_at TEXT NOT NULL,                   -- RFC3339
    removed_at TEXT,                            -- RFC3339; set when the checkout is deleted
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_worktrees_project ON worktrees(project_id);

-- The run's working directory when it is not the project root (NULL = project root).
ALTER TABLE runs ADD COLUMN worktree_path TEXT;

-- Per-project worktree setup.
-- worktree_base_branch: branch new worktrees start from (NULL = the remote's default branch).
-- worktree_setup_cmd: shell command run in a new worktree before the session starts (e.g. `pnpm install`).
ALTER TABLE projects ADD COLUMN worktree_base_branch TEXT;
ALTER TABLE projects ADD COLUMN worktree_setup_cmd TEXT;
