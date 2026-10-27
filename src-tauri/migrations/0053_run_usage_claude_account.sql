-- Attribute usage ledger rows to the Claude account that produced them, so the
-- Stats screen can filter and break down usage per account.
--
-- Stamped from `runs.claude_account_id` at insert time (trigger below) instead
-- of threading the id through every insert site. Snapshotting it on the ledger
-- row keeps the attribution when the run is deleted, the run is later moved to
-- another account, or the account itself is removed. NULL = no managed account
-- (global ~/.claude) or a non-Claude engine.
ALTER TABLE run_usage ADD COLUMN claude_account_id TEXT;

UPDATE run_usage
   SET claude_account_id = (SELECT r.claude_account_id FROM runs r WHERE r.id = run_usage.run_id)
 WHERE engine = 'claude' AND run_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_run_usage_claude_account ON run_usage(claude_account_id);

CREATE TRIGGER IF NOT EXISTS trg_run_usage_claude_account
AFTER INSERT ON run_usage
WHEN NEW.engine = 'claude' AND NEW.claude_account_id IS NULL AND NEW.run_id IS NOT NULL
BEGIN
    UPDATE run_usage
       SET claude_account_id = (SELECT r.claude_account_id FROM runs r WHERE r.id = NEW.run_id)
     WHERE id = NEW.id;
END;
