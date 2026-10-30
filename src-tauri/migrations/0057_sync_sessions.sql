-- Cross-device sessions (docs: .workspace/docs/03-design/two-way-sync.md §6).
-- All columns are synced with the run row: the owner + lease are what lets the
-- other devices show a session as "running on <device>" and refuse to drive it.

-- Sync device id of the machine that last started/resumed the run.
ALTER TABLE runs ADD COLUMN owner_device TEXT;
-- Display name of that machine (devices only know their own sync_device row).
ALTER TABLE runs ADD COLUMN device_name TEXT;
-- RFC 3339; renewed by a heartbeat while the sidecar is alive, NULL when idle.
-- An expired lease means the owner died: another device may take the run over.
ALTER TABLE runs ADD COLUMN lease_until TEXT;
-- Git HEAD + dirty flag of the run's working tree when it (re)started, so a
-- device continuing the session can warn when its checkout differs.
ALTER TABLE runs ADD COLUMN git_head TEXT;
ALTER TABLE runs ADD COLUMN git_dirty INTEGER;
