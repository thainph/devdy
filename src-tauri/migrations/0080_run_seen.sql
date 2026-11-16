-- When the user last saw a run's result (opened it on the phone, or had it on
-- screen at the desktop). A run that finished after this is unread on the
-- Remote Control Inbox, on every paired device alike.
-- Backfill: everything that already finished counts as seen, so upgrading does
-- not light up the whole history.
ALTER TABLE runs ADD COLUMN seen_at TEXT;

UPDATE runs SET seen_at = finished_at WHERE finished_at IS NOT NULL;
