-- Rule / convention management (parallel to skills, but for CLAUDE rules dir + Codex AGENTS.md)

CREATE TABLE IF NOT EXISTS rules (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT 'both',   -- 'claude' | 'codex' | 'both'
    source_path TEXT NOT NULL,             -- <app_data>/rules/<name>.md
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_rules (
    project_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    target TEXT NOT NULL,                   -- applied target snapshot
    synced_hash_claude TEXT,                -- sha256 of .claude/rules/<name>.md (NULL if not applied to claude)
    synced_hash_codex TEXT,                 -- sha256 of body block in AGENTS.md (NULL if not applied to codex)
    applied_at TEXT NOT NULL,
    PRIMARY KEY (project_id, rule_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (rule_id) REFERENCES rules(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS rule_sync_conflicts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    engine TEXT NOT NULL,                   -- 'claude' | 'codex'
    detected_at TEXT NOT NULL,
    local_hash TEXT NOT NULL,
    source_hash TEXT NOT NULL,
    resolved INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (rule_id) REFERENCES rules(id) ON DELETE CASCADE
);
