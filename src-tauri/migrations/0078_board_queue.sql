-- Batch Board queue: todos added from the todo screen ("Add to Batch Board"),
-- waiting for the user to pick how to run them on the board. A todo leaves the
-- queue when a batch starts on it or the user removes it.
CREATE TABLE IF NOT EXISTS board_queue (
    todo_id TEXT PRIMARY KEY,
    added_at TEXT NOT NULL,                     -- RFC3339
    FOREIGN KEY (todo_id) REFERENCES todos(id) ON DELETE CASCADE
);
