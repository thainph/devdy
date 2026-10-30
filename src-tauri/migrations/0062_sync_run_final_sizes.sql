-- Cross-device sessions: size in bytes of the run's Devdy log and CLI
-- transcript when the owner's sidecar ended (lease released). Synced with the
-- run row; a device continuing the session waits until its synced copies of
-- both files reached these sizes. NULL while running / unknown.
ALTER TABLE runs ADD COLUMN final_log_size INTEGER;
ALTER TABLE runs ADD COLUMN final_transcript_size INTEGER;
