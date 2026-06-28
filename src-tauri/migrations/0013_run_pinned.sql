-- Allow pinning a run/session to the top of the History list.
-- Pinned runs sort before unpinned ones; within each group the existing
-- created_at DESC order is kept.
ALTER TABLE runs ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
