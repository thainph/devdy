-- The Batch Board is gone: todos are run one by one with "Start with AI"
-- (linked through todo_runs). Drop its queue and batch tables and the flag.
DROP TABLE IF EXISTS board_queue;
DROP TABLE IF EXISTS todo_batch_items;
ALTER TABLE runs DROP COLUMN todo_batch;
