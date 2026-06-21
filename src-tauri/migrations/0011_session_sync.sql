-- Session-sync hardening.
--
-- 1. Cache the resolved transcript path per run (so Codex log views don't have
--    to re-walk ~/.codex/sessions to locate a rollout) and remember how many
--    bytes of the transcript we've already mirrored. Because transcripts are
--    append-only, an exact byte-size comparison detects external continuations
--    precisely — replacing the mtime+5s margin that could miss a short final
--    tail.
ALTER TABLE runs ADD COLUMN transcript_path TEXT;
ALTER TABLE runs ADD COLUMN transcript_synced_size INTEGER;

-- 2. Forbid duplicate session runs. The import path was SELECT-then-INSERT with
--    no unique constraint, so the watcher and the on-open reconcile could race
--    and both insert a row for the same session. Collapse any pre-existing
--    duplicates (keep the earliest per project+engine+session), drop the
--    losers' usage rows so stats don't double-count, then add the index.
DELETE FROM run_usage WHERE run_id IN (
    SELECT id FROM runs
    WHERE session_id IS NOT NULL
      AND rowid NOT IN (
          SELECT MIN(rowid) FROM runs
          WHERE session_id IS NOT NULL
          GROUP BY project_id, engine, session_id
      )
);

DELETE FROM runs
WHERE session_id IS NOT NULL
  AND rowid NOT IN (
      SELECT MIN(rowid) FROM runs
      WHERE session_id IS NOT NULL
      GROUP BY project_id, engine, session_id
  );

CREATE UNIQUE INDEX IF NOT EXISTS idx_runs_project_engine_session
    ON runs(project_id, engine, session_id)
    WHERE session_id IS NOT NULL;
