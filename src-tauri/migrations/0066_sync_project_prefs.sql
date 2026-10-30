-- Selective sync (on-demand-sync.md §3), machine-local, never synced.
--
-- Which synced projects this machine takes (rows of an unselected project are
-- skipped on apply) and how its folder sync runs.
CREATE TABLE sync_project_prefs (
    project_id       TEXT PRIMARY KEY,
    enabled          INTEGER NOT NULL DEFAULT 0,
    folder           INTEGER NOT NULL DEFAULT 0,
    default_excludes INTEGER NOT NULL DEFAULT 1,
    excludes         TEXT NOT NULL DEFAULT '[]',
    max_file_mb      INTEGER NOT NULL DEFAULT 500,
    last_backup_at   TEXT
);

-- Existing projects keep syncing; the startup hook (`sync::prefs::seed`)
-- unselects the ones whose folder is not on this machine, then clears the flag.
INSERT INTO sync_project_prefs (project_id, enabled) SELECT id, 1 FROM projects;
INSERT INTO sync_state (key, value) VALUES ('prefs_seed_pending', '1')
    ON CONFLICT(key) DO UPDATE SET value = excluded.value;

-- A project created here is selected; one arriving from another device (the
-- apply guard is on) is not, unless `prefs_synced_project_enabled` says so
-- (test hook).
CREATE TRIGGER sync_prefs_project_ins AFTER INSERT ON projects BEGIN
    INSERT OR IGNORE INTO sync_project_prefs (project_id, enabled) VALUES (
        NEW.id,
        CASE WHEN (SELECT active FROM sync_apply_guard WHERE id = 1) = 1
             THEN COALESCE((SELECT CAST(value AS INTEGER) FROM sync_state WHERE key = 'prefs_synced_project_enabled'), 0)
             ELSE 1 END
    );
END;

-- A re-keyed project (bootstrap merge, unique-key reconcile) keeps its prefs.
CREATE TRIGGER sync_prefs_project_rekey AFTER UPDATE OF id ON projects WHEN OLD.id != NEW.id BEGIN
    UPDATE OR IGNORE sync_project_prefs SET project_id = NEW.id WHERE project_id = OLD.id;
    DELETE FROM sync_project_prefs WHERE project_id = OLD.id;
END;

CREATE TRIGGER sync_prefs_project_del AFTER DELETE ON projects BEGIN
    DELETE FROM sync_project_prefs WHERE project_id = OLD.id;
END;

-- Pulled rows of an unselected project that other rows (or files) point at:
-- lets their children and logs/transcripts be told apart without the parent.
-- Cleared when the project is selected (the cursor reset re-pulls them).
CREATE TABLE sync_skipped_rows (
    tbl        TEXT NOT NULL,
    id         TEXT NOT NULL,
    project_id TEXT NOT NULL,
    -- runs: the CLI session id (transcripts / rollouts are named after it).
    session_id TEXT,
    PRIMARY KEY (tbl, id)
);
CREATE INDEX idx_sync_skipped_rows_session ON sync_skipped_rows(session_id);

-- Applied rows whose reference to a row of a disabled category (absent here)
-- was cut to NULL: `refs` = JSON {column: original value}. Put back when the
-- row is pushed (so this machine never erases them), and the row is re-applied
-- after a category is enabled.
CREATE TABLE sync_stripped_refs (
    tbl  TEXT NOT NULL,
    pk   TEXT NOT NULL,
    refs TEXT NOT NULL,
    PRIMARY KEY (tbl, pk)
);
