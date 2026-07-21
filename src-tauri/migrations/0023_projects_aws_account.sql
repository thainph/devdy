-- Project links to one shared AWS account (NULL = not linked), mirroring the
-- GitHub/GitLab account model. Secrets remain account-scoped in Keychain.
ALTER TABLE projects ADD COLUMN aws_account_id TEXT
    REFERENCES aws_accounts(id) ON DELETE SET NULL;
