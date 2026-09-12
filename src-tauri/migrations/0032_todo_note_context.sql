-- Capture context for the quick-capture surfaces. A todo/note jotted down while
-- working on a project should remember WHERE it came from, so the list screens
-- can filter by project and offer a backlink to the AI run that produced it.
--
-- Both columns are nullable and unenforced (no FKs in this DB): a todo/note
-- created from the global quick-capture has neither, and a deleted project/run
-- simply leaves a dangling id that the UI resolves to "no project" / no link.
ALTER TABLE todos ADD COLUMN project_id TEXT;
ALTER TABLE todos ADD COLUMN run_id TEXT;
ALTER TABLE notes ADD COLUMN run_id TEXT;

CREATE INDEX IF NOT EXISTS idx_todos_project ON todos(project_id);
