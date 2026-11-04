-- Leftover state of features that are gone; nothing reads it any more.
--
-- `project_secrets`: the phase-1 per-project PAT reference. PATs moved to
-- account-scoped Keychain entries (github_accounts / gitlab_accounts) long ago.
--
-- Settings rows nothing reads:
--   `analyze_issue_prompt`, `review_pr_prompt` — seeded by 0001, the editable
--     issue/PR prompts were removed from Settings;
--   `budget_warn_percent`, `token_budget_period` — the self-imposed token
--     budget, replaced by the plan-based `budget_5h_percent` /
--     `budget_week_percent` guardrail;
--   `claude_models_active_account_key`, `claude_models_cache_by_account` — the
--     old Claude model cache (the Codex one, `codex_models_*`, is still used).

DROP TABLE IF EXISTS project_secrets;

DELETE FROM settings
WHERE key IN (
    'analyze_issue_prompt',
    'review_pr_prompt',
    'budget_warn_percent',
    'token_budget_period',
    'claude_models_active_account_key',
    'claude_models_cache_by_account'
);
