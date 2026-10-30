-- Two-way sync: resumable SHA-256 state of a tracked file, so a grown
-- append-only file (run log, transcript, rollout) is hashed from where it was
-- last hashed instead of from the start, and a peer can append the new bytes
-- in place. JSON `{ino, synced: {off, h}, disk: {off, h} | null}`; NULL = no
-- state (the next change re-reads the whole file).
ALTER TABLE sync_files ADD COLUMN hash_state TEXT;
