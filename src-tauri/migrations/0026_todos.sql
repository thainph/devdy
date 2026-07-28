-- Simple global "quick note" todo list. Personal scratchpad, not tied to any
-- project. `position` is the priority order (ascending: lower = higher / top);
-- drag-and-drop in the UI rewrites positions via reorder_todos.
CREATE TABLE IF NOT EXISTS todos (
    id         TEXT PRIMARY KEY,
    text       TEXT NOT NULL,
    done       INTEGER NOT NULL DEFAULT 0,
    position   INTEGER NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_todos_position ON todos(position);
