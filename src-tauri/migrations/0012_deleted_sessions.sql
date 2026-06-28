-- Tombstones for deleted session runs.
--
-- Deleting a `session` run only removes its DB row and local `.devdy` files —
-- it deliberately does NOT touch the shared engine transcript
-- (~/.claude/projects/<cwd>/<id>.jsonl, ~/.codex/sessions/.../rollout-*.jsonl),
-- which the CLI / VS Code extension also own. But the on-open reconcile and the
-- live watcher re-import any transcript that has no matching run, so a deleted
-- session reappeared on the next launch. Record deletions here and skip
-- re-importing those sessions. Keyed to match the import lookup
-- (project_id + session_id); engine kept for diagnostics.
CREATE TABLE IF NOT EXISTS deleted_sessions (
    project_id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    engine     TEXT,
    deleted_at TEXT NOT NULL,
    PRIMARY KEY (project_id, session_id)
);
