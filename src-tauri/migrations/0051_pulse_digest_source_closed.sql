-- Project Pulse BR-12: a closed GitHub issue goes to the "done" group at once,
-- while an AI "done" needs 7 days. The digest status alone cannot tell the two
-- apart (a closed issue forces status = 'done'), so record the fact.
ALTER TABLE pulse_digests ADD COLUMN source_closed INTEGER NOT NULL DEFAULT 0;
