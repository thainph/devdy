//! Azure CLI config dirs managed by Devdy.
//!
//! Every Devdy Azure account is an ISOLATED Azure CLI config dir
//! (`AZURE_CONFIG_DIR`) at `<app_data>/azure-accounts/<account_id>`. The account
//! is signed in (`az login`) and its default subscription set (`az account set`)
//! inside that dir, so the user's own `~/.azure` is never read or written.
//!
//! A run never sees an account dir directly: the sidecar's `AZURE_CONFIG_DIR`
//! points at a shared, EMPTY sandbox (`<app_data>/azure-projects/_empty`), and
//! only the broker-gated `az` / `terraform` shims hand the linked account's dir
//! to their child process.
//!
//! Limitation: an account dir contains az's plaintext MSAL token cache (refresh
//! tokens). Owner-only permissions keep other OS users out, but any process of
//! the same user that knows the path can read it — the same exposure as AWS
//! profile mode's `~/.aws` SSO cache. The broker gates `az` calls, not the files.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

const AZURE_ACCOUNTS_DIR: &str = "azure-accounts";
const AZURE_PROJECTS_DIR: &str = "azure-projects";
const EMPTY_DIR: &str = "_empty";

/// Root of the account dirs, recorded once at startup so the broker (which has
/// no `AppHandle` in unit tests) can locate an account's config dir.
static ACCOUNTS_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Record the account root (`<app_data>/azure-accounts`). First call wins.
pub fn init_accounts_root(app_data_dir: &Path) {
    let _ = ACCOUNTS_ROOT.set(accounts_root(app_data_dir));
}

/// The account root recorded at startup, if any.
pub fn registered_accounts_root() -> Option<&'static Path> {
    ACCOUNTS_ROOT.get().map(|p| p.as_path())
}

pub fn accounts_root(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(AZURE_ACCOUNTS_DIR)
}

/// Account ids are Devdy-generated UUIDs; reject anything else so a stray id can
/// never escape the account root via path traversal.
fn is_safe_account_id(account_id: &str) -> bool {
    !account_id.is_empty()
        && account_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// `<root>/<account_id>`; `None` for an id that is not a plain identifier.
pub fn account_dir_in(root: &Path, account_id: &str) -> Option<PathBuf> {
    is_safe_account_id(account_id).then(|| root.join(account_id))
}

/// The account's isolated config dir: `<app_data>/azure-accounts/<account_id>`.
pub fn account_dir(app_data_dir: &Path, account_id: &str) -> Option<PathBuf> {
    account_dir_in(&accounts_root(app_data_dir), account_id)
}

/// The shared empty config dir every run's sidecar gets as `AZURE_CONFIG_DIR`.
pub fn empty_azure_dir(app_data_dir: &Path) -> PathBuf {
    app_data_dir.join(AZURE_PROJECTS_DIR).join(EMPTY_DIR)
}

/// Reset the shared `_empty` dir to a fresh, empty state. Called at startup:
/// `az` writes cache/profile files into whatever dir it is given, so wiping it
/// guarantees no session (e.g. a login made by bypassing the shim) survives
/// into later runs.
pub fn ensure_empty_sandbox(app_data_dir: &Path) {
    let dir = empty_azure_dir(app_data_dir);
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);
}

/// Create the account dir (owner-only on unix). Idempotent.
pub fn create_account_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

/// Remove an account dir (on account deletion / failed creation).
pub fn remove_account_dir(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

/// Cheap "is this dir signed in?" check: `azureProfile.json` lists at least one
/// subscription (or tenant-level account). Does not prove the refresh token is
/// still valid — `az account show` / a real call does that.
pub fn is_logged_in(dir: &Path) -> bool {
    let Ok(bytes) = std::fs::read(dir.join("azureProfile.json")) else {
        return false;
    };
    // az writes this file as UTF-8 with a BOM.
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|v| {
            v.get("subscriptions")
                .and_then(|s| s.as_array())
                .map(|a| !a.is_empty())
        })
        .unwrap_or(false)
}

/// Startup: record the account root and reset the shared empty sandbox.
pub fn sync_all(app_data_dir: &Path) {
    init_accounts_root(app_data_dir);
    let _ = std::fs::create_dir_all(accounts_root(app_data_dir));
    ensure_empty_sandbox(app_data_dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_dir_rejects_traversal() {
        let root = Path::new("/tmp/devdy-az");
        assert_eq!(
            account_dir_in(root, "6f1c-ab_9").as_deref(),
            Some(Path::new("/tmp/devdy-az/6f1c-ab_9"))
        );
        for bad in ["", "..", "../x", "a/b", "a b", "."] {
            assert!(
                account_dir_in(root, bad).is_none(),
                "id {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn is_logged_in_reads_bom_profile() {
        let dir = std::env::temp_dir().join(format!("devdy-az-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_logged_in(&dir), "missing profile");
        std::fs::write(
            dir.join("azureProfile.json"),
            b"\xEF\xBB\xBF{\"installationId\": \"x\"}",
        )
        .unwrap();
        assert!(!is_logged_in(&dir), "no subscriptions key");
        std::fs::write(
            dir.join("azureProfile.json"),
            b"\xEF\xBB\xBF{\"installationId\": \"x\", \"subscriptions\": [{\"id\": \"s1\"}]}",
        )
        .unwrap();
        assert!(is_logged_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
