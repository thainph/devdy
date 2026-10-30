-- Two-way sync of files (docs: .workspace/docs/03-design/two-way-sync.md §4, §9b).
-- Library files, run logs, CLI transcripts and managed SSH keys travel as
-- `file` ops whose content is stored on the relay as sealed 1 MiB chunks.

-- One row per tracked file, keyed like the op pk: (kind, portable path).
-- `chunks_json` = [{"sha": plaintext chunk sha256, "blob": sealed blob sha, "len": n}].
-- origin = 'local' when the version was scanned here, 'remote' when sync wrote
-- it (the session watcher ignores those while their size is unchanged).
CREATE TABLE sync_files (
    kind        TEXT NOT NULL,
    path        TEXT NOT NULL,
    local_path  TEXT NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0,
    sha256      TEXT NOT NULL DEFAULT '',
    chunks_json TEXT NOT NULL DEFAULT '[]',
    mtime       INTEGER NOT NULL DEFAULT 0,  -- ms, of the version recorded here
    mode        INTEGER,
    hlc         TEXT,
    deleted     INTEGER NOT NULL DEFAULT 0,
    origin      TEXT NOT NULL DEFAULT 'local',
    PRIMARY KEY (kind, path)
);
CREATE INDEX idx_sync_files_local ON sync_files(local_path);

-- Upload progress / dedupe: plaintext chunk sha256 -> sealed blob already on
-- the relay. Sealing uses a random nonce, so the same plaintext would
-- otherwise get a new blob every time.
CREATE TABLE sync_blobs (
    chunk_sha   TEXT PRIMARY KEY,
    blob_sha    TEXT NOT NULL,
    len         INTEGER NOT NULL,
    uploaded_at INTEGER NOT NULL
);

-- Pulled `file` / `secret` ops that could not be applied yet (blob download
-- failed, Keychain locked, ...). Retried on every sync cycle.
CREATE TABLE sync_extra_deferred (
    tbl         TEXT NOT NULL,
    pk          TEXT NOT NULL,
    op          TEXT NOT NULL,      -- JSON of the Op
    reason      TEXT NOT NULL,
    attempts    INTEGER NOT NULL DEFAULT 0,
    deferred_at INTEGER NOT NULL,
    PRIMARY KEY (tbl, pk)
);
