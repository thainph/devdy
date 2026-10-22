-- Project Pulse: the issue's last known board fields (JSON {start, deadline,
-- status}), reused when the optional board query fails so the content (and
-- its hash) stays stable while comments still trigger a new digest.
ALTER TABLE pulse_watches ADD COLUMN board_json TEXT;
