-- Skill / rule grouping: bundle items into groups and flip a whole group on or
-- off for a project in one action. Skill groups and rule groups are separate
-- namespaces; membership is many-to-many (an item may sit in several groups).

CREATE TABLE IF NOT EXISTS skill_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    color TEXT,                             -- badge color, NULL = default
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS skill_group_members (
    group_id TEXT NOT NULL,
    skill_id TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, skill_id),
    FOREIGN KEY (group_id) REFERENCES skill_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (skill_id) REFERENCES skills(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_skill_group_members_skill ON skill_group_members(skill_id);

-- A project remembers which groups it enabled, so skills added to the group
-- later can propagate to every project already running that group.
CREATE TABLE IF NOT EXISTS project_skill_groups (
    project_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    enabled_at TEXT NOT NULL,
    PRIMARY KEY (project_id, group_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES skill_groups(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS rule_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    color TEXT,
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rule_group_members (
    group_id TEXT NOT NULL,
    rule_id TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, rule_id),
    FOREIGN KEY (group_id) REFERENCES rule_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (rule_id) REFERENCES rules(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_rule_group_members_rule ON rule_group_members(rule_id);

CREATE TABLE IF NOT EXISTS project_rule_groups (
    project_id TEXT NOT NULL,
    group_id TEXT NOT NULL,
    enabled_at TEXT NOT NULL,
    PRIMARY KEY (project_id, group_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (group_id) REFERENCES rule_groups(id) ON DELETE CASCADE
);

-- Tell apart "user flipped this item on by hand" from "a group brought it in".
-- Disabling a group only removes items it brought in (manual = 0) that no other
-- enabled group still claims. Rows that predate grouping were all manual.
ALTER TABLE project_skills ADD COLUMN manual INTEGER NOT NULL DEFAULT 1;
ALTER TABLE project_rules ADD COLUMN manual INTEGER NOT NULL DEFAULT 1;
