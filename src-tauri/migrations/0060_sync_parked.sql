-- Two-way sync: local changes the relay permanently refused (op larger than
-- the relay accepts, malformed). They are taken out of sync_outbox so they stop
-- blocking every later push, and recorded here so Settings → Sync can show
-- them. A newer change of the same row is queued and pushed as usual; pushing
-- it clears the record.
CREATE TABLE sync_parked (
    tbl        TEXT NOT NULL,
    pk         TEXT NOT NULL,
    reason     TEXT NOT NULL,
    parked_at  INTEGER NOT NULL,
    PRIMARY KEY (tbl, pk)
);
