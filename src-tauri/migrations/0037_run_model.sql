-- Persist the model a run actually executes with, resolved at start/resume
-- (per-run override, else the engine's default). NULL means the engine picked
-- its own default. Lets the History list and composer show the applied model
-- without re-reading each run's log.
ALTER TABLE runs ADD COLUMN model TEXT;
