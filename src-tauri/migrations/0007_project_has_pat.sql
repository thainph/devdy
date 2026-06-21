-- Cache "project has a GitHub PAT" as a DB flag so the projects list never has to
-- read the macOS Keychain (which prompts for the login password on every secret read).
-- The flag is kept in sync with the Keychain at startup via a prompt-free attribute check.
ALTER TABLE projects ADD COLUMN has_pat INTEGER NOT NULL DEFAULT 0;
