-- Optional human label for runs, primarily for standalone `session` runs that
-- aren't tied to a GitHub issue/PR (derived from the first user message).
ALTER TABLE runs ADD COLUMN title TEXT;
