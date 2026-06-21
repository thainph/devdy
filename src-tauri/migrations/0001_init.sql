CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL UNIQUE,
    github_owner TEXT,
    github_repo TEXT,
    default_engine TEXT NOT NULL DEFAULT 'claude',
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_secrets (
    project_id TEXT PRIMARY KEY,
    pat_keyring_key TEXT NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS skills (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    source_path TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_skills (
    project_id TEXT NOT NULL,
    skill_id TEXT NOT NULL,
    synced_hash TEXT NOT NULL,
    applied_at TEXT NOT NULL,
    PRIMARY KEY (project_id, skill_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS sync_conflicts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    skill_id TEXT NOT NULL,
    detected_at TEXT NOT NULL,
    local_hash TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    type TEXT NOT NULL,
    ref_number INTEGER,
    status TEXT NOT NULL DEFAULT 'fetched',
    engine TEXT NOT NULL DEFAULT 'claude',
    output_path TEXT,
    started_at TEXT,
    finished_at TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

INSERT OR IGNORE INTO settings (key, value) VALUES
    ('default_engine', 'claude'),
    ('claude_path', 'claude'),
    ('codex_path', 'codex'),
    ('extra_args', ''),
    ('theme', 'system'),
    ('analyze_issue_prompt', 'Please analyze the GitHub issue described in the file and create a detailed implementation plan.'),
    ('review_pr_prompt', 'Please review the pull request described in the file according to the configured skills.');
