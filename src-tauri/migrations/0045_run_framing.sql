-- Durable per-run system framing, so it survives resumes.
--
-- `system_instruction`: a worker's role instruction (session_spawn
-- `instruction`). Re-applied on every resume — the Claude SDK does not persist
-- an appended system prompt, and session_send to a stopped worker IS a resume.
--
-- `framing_seeded`: 1 once a Codex thread carries the run's framing in its own
-- history (developer instructions on thread/start, or a one-time
-- <session-framing> block prepended to a resumed turn). Codex ignores
-- developerInstructions on thread/resume, so a thread that was never started
-- with framing (legacy plain session) must be seeded through the prompt once.
ALTER TABLE runs ADD COLUMN system_instruction TEXT;
ALTER TABLE runs ADD COLUMN framing_seeded INTEGER NOT NULL DEFAULT 0;
