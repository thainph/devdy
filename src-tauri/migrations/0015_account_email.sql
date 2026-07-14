-- Git commit identity email for GitHub accounts (peer to gitlab_accounts.email).
ALTER TABLE github_accounts ADD COLUMN email TEXT;
