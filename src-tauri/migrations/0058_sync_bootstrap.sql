-- Note conflict detection (two-way sync, design §0 D2): content hash of the
-- last note body received from another device (`tbl = 'notes'` rows only).
-- Pushed notes carry it as their base, so a receiver can tell an edit made on
-- top of its own version from a concurrent one.
ALTER TABLE sync_rows ADD COLUMN base_hash TEXT;
