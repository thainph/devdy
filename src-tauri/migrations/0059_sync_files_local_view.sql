-- Two-way sync: transcripts / Codex rollouts embed the sender's absolute cwd,
-- which the receiving device rewrites to its own project path when it writes
-- the file. The record keeps the synced content (size, sha256, chunks_json) as
-- the base; these hold what is actually on disk when it differs (NULL = the
-- file is the synced content byte for byte), so the rewritten file is not
-- mistaken for a local change and pushed back.
ALTER TABLE sync_files ADD COLUMN local_size INTEGER;
ALTER TABLE sync_files ADD COLUMN local_sha256 TEXT;
