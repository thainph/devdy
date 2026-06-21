ALTER TABLE runs ADD COLUMN input_path TEXT;

UPDATE runs SET input_path = output_path WHERE status = 'fetched' AND input_path IS NULL;
