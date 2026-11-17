-- The fetched issue/PR's own title and author, copied from its task file's
-- frontmatter, so the History list can tell reviews across repos apart at a
-- glance ("acme/web #42 · @octo") without reading every task file.
-- NULL = not captured yet: `list_runs` backfills it lazily from the task file
-- ('' when the file is gone, so it is not retried).
ALTER TABLE runs ADD COLUMN ref_title TEXT;
ALTER TABLE runs ADD COLUMN ref_author TEXT;
