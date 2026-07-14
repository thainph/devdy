use anyhow::{anyhow, Result};
use keyring::Entry;

const SERVICE_NAME: &str = "vn.papay.devdy";

pub fn set_account_pat(account_id: &str, pat: &str) -> Result<()> {
    let key = keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    entry.set_password(pat)?;
    Ok(())
}

pub fn get_account_pat(account_id: &str) -> Result<String> {
    let key = keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    entry.get_password().map_err(|e| anyhow!("Failed to get PAT: {}", e))
}

pub fn delete_account_pat(account_id: &str) -> Result<()> {
    let key = keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    let _ = entry.delete_credential();
    Ok(())
}

/// Check whether a PAT exists for the account WITHOUT reading its secret value.
///
/// `get_attributes()` queries only the credential's metadata, which on the macOS
/// Keychain does not trigger the "allow access to confidential information" prompt
/// that `get_password()` does. This lets us show a "stored" badge without pestering
/// the user for their login password.
pub fn has_account_pat(account_id: &str) -> bool {
    let key = keyring_key(account_id);
    Entry::new(SERVICE_NAME, &key)
        .map(|e| e.get_attributes().is_ok())
        .unwrap_or(false)
}

fn keyring_key(account_id: &str) -> String {
    format!("account_{}", account_id)
}

pub fn set_gitlab_account_pat(account_id: &str, pat: &str) -> Result<()> {
    let key = gitlab_keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    entry.set_password(pat)?;
    Ok(())
}

pub fn get_gitlab_account_pat(account_id: &str) -> Result<String> {
    let key = gitlab_keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    entry.get_password().map_err(|e| anyhow!("Failed to get PAT: {}", e))
}

pub fn delete_gitlab_account_pat(account_id: &str) -> Result<()> {
    let key = gitlab_keyring_key(account_id);
    let entry = Entry::new(SERVICE_NAME, &key)?;
    let _ = entry.delete_credential();
    Ok(())
}

/// Check whether a GitLab PAT exists for the account WITHOUT reading its secret
/// value. Mirrors `has_account_pat`: `get_attributes()` reads only metadata and
/// does not trigger the macOS Keychain confidential-access prompt.
pub fn has_gitlab_account_pat(account_id: &str) -> bool {
    let key = gitlab_keyring_key(account_id);
    Entry::new(SERVICE_NAME, &key)
        .map(|e| e.get_attributes().is_ok())
        .unwrap_or(false)
}

fn gitlab_keyring_key(account_id: &str) -> String {
    format!("gitlab_account_{}", account_id)
}
