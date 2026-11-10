-- GitHub Project V2 boards are configured once under a GitHub account (URL +
-- Start / Deadline / Status field mapping) and then enabled on any number of
-- projects, instead of being re-entered per project. Board queries run with the
-- owning account's PAT.
-- field_mappings: JSON { boardId, ownerType, owner, number, startFieldId,
-- deadlineFieldId, statusFieldId } (same shape as the old per-project column).
CREATE TABLE IF NOT EXISTS github_boards (
    id TEXT PRIMARY KEY,
    account_id TEXT NOT NULL,
    url TEXT NOT NULL,
    title TEXT NOT NULL DEFAULT '',
    field_mappings TEXT NOT NULL,
    created_at TEXT NOT NULL,                   -- RFC3339
    UNIQUE (account_id, url),
    FOREIGN KEY (account_id) REFERENCES github_accounts(id) ON DELETE CASCADE
);

-- The board enabled on a project (NULL = none).
ALTER TABLE projects ADD COLUMN github_board_id TEXT
    REFERENCES github_boards(id) ON DELETE SET NULL;

-- Existing per-project configs become boards of the project's default GitHub
-- account; projects that pasted the same URL share one board. A config without
-- an account could never be queried, so it is dropped.
INSERT OR IGNORE INTO github_boards (id, account_id, url, title, field_mappings, created_at)
    SELECT lower(hex(randomblob(16))), github_account_id, github_project_board_url, '',
           MAX(github_project_field_mappings), strftime('%Y-%m-%dT%H:%M:%SZ', 'now')
    FROM projects
    WHERE github_account_id IS NOT NULL
      AND github_project_board_url IS NOT NULL AND github_project_board_url <> ''
      AND github_project_field_mappings IS NOT NULL
    GROUP BY github_account_id, github_project_board_url;

UPDATE projects SET github_board_id = (
    SELECT b.id FROM github_boards b
    WHERE b.account_id = projects.github_account_id AND b.url = projects.github_project_board_url
)
WHERE github_project_board_url IS NOT NULL AND github_account_id IS NOT NULL;

ALTER TABLE projects DROP COLUMN github_project_board_url;
ALTER TABLE projects DROP COLUMN github_project_field_mappings;
