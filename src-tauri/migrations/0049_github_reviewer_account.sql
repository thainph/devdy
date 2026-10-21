-- Per-project GitHub reviewer account + per-run GitHub account override.
--
-- projects.github_reviewer_account_id: optional second account used for review
-- work (PR reviews/comments). NULL = fall back to projects.github_account_id.
--
-- runs.github_account_id: pins a run to one GitHub account for every gh call
-- (e.g. a review started from the PR inbox as the account that was asked to
-- review). NULL = resolve from the project's default/reviewer accounts.
ALTER TABLE projects ADD COLUMN github_reviewer_account_id TEXT
    REFERENCES github_accounts(id) ON DELETE SET NULL;

ALTER TABLE runs ADD COLUMN github_account_id TEXT
    REFERENCES github_accounts(id) ON DELETE SET NULL;
