use crate::db::Db;
use crate::secrets;
use anyhow::{anyhow, Result};
use octocrab::Octocrab;
use sqlx::Row;
use std::time::Duration;

/// Build an Octocrab client using the PAT of the GitHub account linked to this project.
pub async fn client_for_project(db: &Db, project_id: &str) -> Result<Octocrab> {
    let row = sqlx::query("SELECT github_account_id FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_one(db)
        .await?;
    let account_id: Option<String> = row.get("github_account_id");
    let account_id = account_id.ok_or_else(|| {
        anyhow!("No GitHub account linked to this project. Link one in project settings.")
    })?;

    let pat = secrets::get_account_pat(&account_id).map_err(|_| {
        anyhow!("No PAT stored for the linked GitHub account. Re-enter it in Settings.")
    })?;

    build_client(pat)
}

/// Without these a stalled connection (e.g. after the Mac slept) never
/// returns, and the PR inbox — which fetches under a single-flight lock —
/// hangs every later refresh and poll behind it.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const READ_TIMEOUT: Duration = Duration::from_secs(30);

fn build_client(pat: String) -> Result<Octocrab> {
    Ok(Octocrab::builder()
        .personal_token(pat)
        .set_connect_timeout(Some(CONNECT_TIMEOUT))
        .set_read_timeout(Some(READ_TIMEOUT))
        .set_write_timeout(Some(READ_TIMEOUT))
        .build()?)
}

/// Build an Octocrab client directly from a GitHub account's stored PAT. Unlike
/// `client_for_project`, this is not tied to any project — used by the PR inbox
/// which aggregates review requests across all configured accounts.
pub fn client_for_account(account_id: &str) -> Result<Octocrab> {
    let pat = secrets::get_account_pat(account_id)
        .map_err(|_| anyhow!("No PAT stored for GitHub account. Re-enter it in Settings."))?;
    build_client(pat)
}
