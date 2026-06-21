-- Codex support for skills (parallel to rules): per-engine destinations.
--   Claude  -> .claude/skills/<name>/
--   Codex   -> .codex/skills/<name>/  + pointer block in AGENTS.md

ALTER TABLE skills ADD COLUMN target TEXT NOT NULL DEFAULT 'claude';   -- 'claude' | 'codex' | 'both'

-- Rebuild project_skills to track per-engine sync hashes (mirrors project_rules).
CREATE TABLE IF NOT EXISTS project_skills_new (
    project_id TEXT NOT NULL,
    skill_id TEXT NOT NULL,
    target TEXT NOT NULL DEFAULT 'claude',  -- applied target snapshot
    synced_hash_claude TEXT,                -- sha256 of source SKILL.md synced to .claude/skills (NULL if not applied)
    synced_hash_codex TEXT,                 -- sha256 of source SKILL.md synced to .codex/skills (NULL if not applied)
    applied_at TEXT NOT NULL,
    PRIMARY KEY (project_id, skill_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

INSERT INTO project_skills_new (project_id, skill_id, target, synced_hash_claude, synced_hash_codex, applied_at)
    SELECT project_id, skill_id, 'claude', synced_hash, NULL, applied_at FROM project_skills;

DROP TABLE project_skills;
ALTER TABLE project_skills_new RENAME TO project_skills;

-- Track which engine a skill sync conflict belongs to (existing conflicts are Claude).
ALTER TABLE sync_conflicts ADD COLUMN engine TEXT NOT NULL DEFAULT 'claude';   -- 'claude' | 'codex'
