-- Quick notes: a personal markdown scratchpad. Each note has a short title and a
-- markdown body, and may optionally belong to a project (`project_id` nullable).
-- Foreign-key enforcement is off in this DB, so a deleted project can leave a
-- dangling `project_id`; the UI resolves the project name from the projects list
-- and treats an unknown id as "no project". `position` is the display order
-- (ascending: lower = higher / top); drag-and-drop rewrites positions.
CREATE TABLE IF NOT EXISTS notes (
    id         TEXT PRIMARY KEY,
    title      TEXT NOT NULL DEFAULT '',
    content    TEXT NOT NULL DEFAULT '',
    project_id TEXT,
    position   INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_notes_position ON notes(position);
CREATE INDEX IF NOT EXISTS idx_notes_project ON notes(project_id);
