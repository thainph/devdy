-- Central Azure account management (parallel to aws_accounts). Each account is
-- an ISOLATED Azure CLI config dir managed by Devdy at
-- `<app_data>/azure-accounts/<id>` (AZURE_CONFIG_DIR); the az login session and
-- token cache live there, never in SQLite and never in the user's ~/.azure.
-- SQLite stores only metadata for display, validation cache, and run wiring.
CREATE TABLE IF NOT EXISTS azure_accounts (
    id                TEXT PRIMARY KEY,
    label             TEXT NOT NULL,
    tenant            TEXT,           -- user input for `az login --tenant` (id or domain), optional
    tenant_id         TEXT,           -- resolved tenant id from `az account show`
    subscription_id   TEXT,           -- subscription pinned in the account dir
    subscription_name TEXT,
    user_name         TEXT,           -- signed-in user / service principal name
    tags              TEXT,
    last_validated_at TEXT,
    created_at        TEXT NOT NULL
);

-- Project links to one shared Azure account (NULL = not linked).
ALTER TABLE projects ADD COLUMN azure_account_id TEXT
    REFERENCES azure_accounts(id) ON DELETE SET NULL;
