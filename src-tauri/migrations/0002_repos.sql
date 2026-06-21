CREATE TABLE IF NOT EXISTS repos (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    github_owner TEXT,
    github_repo TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

ALTER TABLE runs ADD COLUMN repo_id TEXT REFERENCES repos(id) ON DELETE SET NULL;

-- Migrate existing projects that have github info into repos table
INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at)
SELECT
    id || '_default',
    id,
    name,
    path,
    github_owner,
    github_repo,
    created_at
FROM projects
WHERE github_owner IS NOT NULL OR github_repo IS NOT NULL;
