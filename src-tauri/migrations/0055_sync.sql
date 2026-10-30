-- Two-way sync bookkeeping (docs: .workspace/docs/03-design/two-way-sync.md §2).
-- Change capture triggers on the synced tables are NOT created here: they are
-- generated from the table registry (src-tauri/src/sync/schema.rs) and
-- (re)installed at startup by sync::capture, so a registry change never needs a
-- migration. Nothing in this file may reference those triggers.

-- This machine's stable sync identity (exactly one row).
CREATE TABLE sync_device (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL
);

-- Small key/value store: last HLC, pull cursor, capture trigger hash, ...
CREATE TABLE sync_state (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Local changes not yet pushed. Written by the capture triggers (also for
-- writes made by sidecar-mcp through node:sqlite). `pk` is the canonical JSON
-- of the primary key: a JSON string, or an object with sorted keys for
-- composite keys. `changed_at` is wall time in ms.
CREATE TABLE sync_outbox (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    tbl        TEXT NOT NULL,
    pk         TEXT NOT NULL,
    op         TEXT NOT NULL,      -- 'I' | 'U' | 'D' (informative; the current row decides)
    changed_at INTEGER NOT NULL
);
CREATE INDEX idx_sync_outbox_row ON sync_outbox(tbl, pk);

-- Last known HLC per synced row (local or remote); deleted = 1 is a tombstone.
CREATE TABLE sync_rows (
    tbl     TEXT NOT NULL,
    pk      TEXT NOT NULL,
    hlc     TEXT NOT NULL,
    deleted INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tbl, pk)
);

-- Remote ops that could not be applied yet (missing parent row, unknown table,
-- newer schema, constraint error). Retried on every apply.
CREATE TABLE sync_deferred (
    tbl         TEXT NOT NULL,
    pk          TEXT NOT NULL,
    hlc         TEXT NOT NULL,
    op          TEXT NOT NULL,     -- JSON of the Op
    reason      TEXT NOT NULL,
    deferred_at INTEGER NOT NULL,
    PRIMARY KEY (tbl, pk)
);

-- active = 1 while remote ops are being applied, so the capture triggers do not
-- echo them back into the outbox. Only ever set inside the apply transaction.
CREATE TABLE sync_apply_guard (
    id     INTEGER PRIMARY KEY CHECK (id = 1),
    active INTEGER NOT NULL DEFAULT 0
);
INSERT INTO sync_apply_guard (id, active) VALUES (1, 0);
