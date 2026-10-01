-- Protected runs are skipped by "Clear all" so an important session survives a
-- bulk cleanup. Independent of `pinned` (which only controls sort order).
ALTER TABLE runs ADD COLUMN protected INTEGER NOT NULL DEFAULT 0;
