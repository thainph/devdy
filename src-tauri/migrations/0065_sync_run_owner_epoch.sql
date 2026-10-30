-- Cross-device sessions: ownership generation of a run. Bumped whenever
-- ownership moves to another device (take-over, or a start on a run owned
-- elsewhere whose lease expired). Synced with the run row; a synced change
-- carrying a lower epoch never replaces the ownership fields (owner, lease,
-- status, final sizes) of a higher one, so an old owner that kept renewing
-- its lease while cut off cannot win back a run by HLC.
ALTER TABLE runs ADD COLUMN owner_epoch INTEGER NOT NULL DEFAULT 0;
