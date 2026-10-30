-- Relay is a pipe (on-demand-sync.md §P2): the relay stores nothing, so each
-- device serves its own changes. Every synced row, file and Keychain item
-- with a known version (`sync_rows`: local changes once stamped, and rows
-- applied from the other device) gets a local, monotonically increasing
-- change seq; one entry per row (its latest state, deletes = tombstones).
-- The other device pulls `changes{since}` by this seq.
CREATE TABLE sync_feed (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl TEXT NOT NULL,
    pk  TEXT NOT NULL,
    UNIQUE (tbl, pk)
);

-- What this device already knows goes into the feed once, in a stable order.
INSERT INTO sync_feed (tbl, pk) SELECT tbl, pk FROM sync_rows ORDER BY hlc, tbl, pk;

-- A new version of a row moves it to the end of the feed (AUTOINCREMENT never
-- hands a seq out twice). Delete + insert, not INSERT OR REPLACE: inside a
-- trigger the outer statement's conflict policy (an upsert's ABORT) wins.
CREATE TRIGGER sync_feed_rows_ins AFTER INSERT ON sync_rows
BEGIN
    DELETE FROM sync_feed WHERE tbl = NEW.tbl AND pk = NEW.pk;
    INSERT INTO sync_feed (tbl, pk) VALUES (NEW.tbl, NEW.pk);
END;

CREATE TRIGGER sync_feed_rows_upd AFTER UPDATE OF hlc, deleted ON sync_rows
WHEN NEW.hlc IS NOT OLD.hlc OR NEW.deleted IS NOT OLD.deleted
BEGIN
    DELETE FROM sync_feed WHERE tbl = NEW.tbl AND pk = NEW.pk;
    INSERT INTO sync_feed (tbl, pk) VALUES (NEW.tbl, NEW.pk);
END;

CREATE TRIGGER sync_feed_rows_del AFTER DELETE ON sync_rows
BEGIN
    DELETE FROM sync_feed WHERE tbl = OLD.tbl AND pk = OLD.pk;
END;

-- Relay blob bookkeeping is gone with the relay store.
DROP TABLE sync_blob_release;
DROP TABLE sync_blobs;

-- The relay pull cursor and history mark are replaced by one cursor per
-- source device (`peer_cursor:<device>`), starting from 0.
DELETE FROM sync_state WHERE key IN ('pull_cursor', 'relay_mark');
