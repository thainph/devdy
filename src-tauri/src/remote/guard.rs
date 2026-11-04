//! What a remote-desktop Controller may NOT do on this machine, enforced where
//! the work happens.
//!
//! Policy: a paired remote desktop controls EVERYTHING the person at the
//! keyboard can (accounts, MCP servers, VPS, backups, settings, the secret
//! notes vault, files under HOME) — except Remote Control's own configuration,
//! so a Controller can neither lock the owner out nor erase its traces, and
//! except on-disk credentials, which no remote file read ever returns.
//!
//! A remote desktop's `invoke`s run in the hidden `remote-bridge` webview
//! ([`BRIDGE_LABEL`]), so the webview label is the one fact the backend can
//! trust about a call's origin — unlike the Controller's self-advertised `ipc`
//! feature, or the command name the relay handler sees (a plugin call such as
//! `plugin:event|emit` can make the bridge run something else). Three gates:
//!
//! - [`refuses_invoke`]: the app's `invoke_handler` (lib.rs) refuses every
//!   [`is_local_only_command`] coming from the bridge, whatever route it took.
//!   The relay handler checks the same names first (defence in depth), and
//!   audits those plus the sensitive ones it lets through ([`audited_command`]).
//! - [`remote_may_set_setting`]: `update_setting` from the bridge writes any
//!   setting but Remote Control's own.
//! - [`SecretPaths`]: file reads from the bridge never return on-disk
//!   credentials (Devdy's app data, `~/.ssh`, cloud CLI logins, …).
//!
//! The bridge's plugin calls are bounded by its own capability
//! (`capabilities/remote-bridge.json`).

use std::path::{Component, Path, PathBuf};

use tauri::ipc::Invoke;
use tauri::{Manager, Runtime, Webview};

use crate::remote::ipc::BRIDGE_LABEL;

/// Error of a refused call. The relay handler also uses it as the name of a
/// command that does not exist, so a refused call fails like an unknown one.
pub const DENIED: &str = "remote_denied_local_only";

/// Commands a Controller may never run on this machine.
///
/// Rule: everything that changes who can reach this machine, or wipes the
/// record of who did (Remote Control config, master password, OTP / session
/// links, kicking a Controller, clearing the audit, Web Push recipients), plus
/// the plumbing of the CONTROLLING side. Everything else — credentials
/// included — is the paired device's to manage, like the person at the
/// keyboard ([`audited_command`] keeps a trace of the sensitive part).
const LOCAL_ONLY_COMMANDS: &[&str] = &[
    // Remote Control itself: who may connect, and how.
    "remote_enable",
    "remote_disable",
    "remote_set_config",
    "remote_set_master_password",
    "remote_set_device_alias",
    "remote_reveal_otp",
    "remote_create_session_link",
    "remote_kick_controller",
    "remote_clear_audit",
    // Device sync: pulling here overwrites this machine's data with another
    // device's (and needs that device's master password).
    "device_sync_list_devices",
    "device_sync_pull",
    // Web Push: who gets notified, about what (and a send from this Host).
    "remote_push_remove",
    "remote_push_set_events",
    "remote_push_test",
    // Plumbing of the CONTROLLING side of a remote desktop: a remote window
    // calls these on its own machine (real invoke), never over the bridge. Run
    // here they would read / write arbitrary files of this machine.
    "stat_local_files",
    "read_local_file_chunk",
    "rdfile_register",
    "rdfile_respond",
    "rd_download_create",
    "rd_download_append",
    "rd_copy_secret",
    "start_file_drag",
    // File Transfer window, this machine's half (remote/transfer.rs).
    "local_fs_home", "local_fs_list", "local_fs_mkdir", "local_fs_rename", "local_fs_delete", "local_fs_walk",
    "local_fs_hash", "local_fs_write_begin", "local_fs_write_chunk", "local_fs_write_finish", "local_fs_write_abort",
];

/// Whether `name` is refused to a Controller (see [`LOCAL_ONLY_COMMANDS`]).
pub fn is_local_only_command(name: &str) -> bool {
    LOCAL_ONLY_COMMANDS.contains(&name)
}

/// Whether the calling webview is the remote-desktop bridge.
pub fn from_remote_bridge<R: Runtime>(webview: &Webview<R>) -> bool {
    webview.label() == BRIDGE_LABEL
}

/// Whether an app command `cmd` invoked by webview `label` must be refused.
pub fn refuses_invoke(label: &str, cmd: &str) -> bool {
    label == BRIDGE_LABEL && is_local_only_command(cmd)
}

/// The app's `invoke_handler`: `handler` (the `generate_handler!` dispatcher)
/// behind the [`refuses_invoke`] gate.
pub fn gated<R: Runtime>(handler: impl Fn(Invoke<R>) -> bool + Send + Sync + 'static) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static {
    move |invoke| {
        let message = &invoke.message;
        if refuses_invoke(message.webview_ref().label(), message.command()) {
            tracing::warn!(event = "remote_bridge_invoke_denied", cmd = %message.command());
            invoke.resolver.reject(DENIED);
            return true;
        }
        handler(invoke)
    }
}

/// Whether the bridge may write setting `key`: any but Remote Control's own
/// (`remote_*`: relay URL, enabled, device id / alias, Web Push recipients and
/// events — who can reach this machine).
pub fn remote_may_set_setting(key: &str) -> bool {
    !key.is_empty() && !key.starts_with("remote_")
}

/// Commands that, run by a remote desktop, leave a trace in the Remote Control
/// audit (by name only — never their arguments): they take, mint, change or
/// drop credentials, or reach the secret notes vault. `update_setting` is
/// audited too, with its key (see the relay handler).
const AUDITED_COMMANDS: &[&str] = &[
    // Accounts (tokens, cloud / CLI logins, OAuth clients).
    "create_github_account",
    "update_github_account",
    "validate_github_account",
    "delete_github_account",
    "create_gitlab_account",
    "update_gitlab_account",
    "validate_gitlab_account",
    "delete_gitlab_account",
    "create_aws_account",
    "update_aws_account",
    "validate_aws_account",
    "delete_aws_account",
    "aws_sso_login",
    "aws_sso_login_profile",
    "create_azure_account",
    "update_azure_account",
    "validate_azure_account",
    "delete_azure_account",
    "azure_login",
    "create_claude_account",
    "validate_claude_account",
    "delete_claude_account",
    "login_claude_account",
    "submit_claude_login_code",
    "add_google_account",
    "reauth_google_account",
    "validate_google_account",
    "delete_google_account",
    "google_forget_client",
    // MCP servers: env + header values in, or out in clear.
    "create_mcp_server",
    "update_mcp_server",
    "import_mcp_server",
    "export_mcp_server",
    // VPS servers: SSH key and passphrase.
    "create_vps_server",
    "update_vps_server",
    // Backups carry (or overwrite) every secret above.
    "export_settings_backup",
    "import_settings_backup",
    // Inbox API: its bearer token, its local listener.
    "get_inbox_api_info",
    "regenerate_inbox_api_token",
    "set_inbox_api_enabled",
];

/// Vault commands too frequent or too bland to audit (status polls, the idle
/// lock pushed back while typing, the list of titles).
const UNAUDITED_SECRET_COMMANDS: &[&str] = &["secret_vault_status", "secret_vault_touch", "secret_notes_list"];

/// Whether a remote desktop running `name` is recorded in the audit (see
/// [`AUDITED_COMMANDS`]): those, and the secret notes vault (`secret_*`).
pub fn audited_command(name: &str) -> bool {
    AUDITED_COMMANDS.contains(&name)
        || name == "update_setting"
        || (name.starts_with("secret_") && !UNAUDITED_SECRET_COMMANDS.contains(&name))
}

// ---- On-disk credentials ----------------------------------------------------

/// Directories under HOME holding credentials (keys, CLI logins, tokens).
const SECRET_HOME_DIRS: &[&str] = &[
    ".devdy",
    ".ssh",
    ".aws",
    ".azure",
    ".codex",
    ".gnupg",
    ".kube",
    ".docker",
    ".password-store",
    ".config/gh",
    ".config/glab-cli",
    ".config/gcloud",
    "Library/Keychains",
];

/// Readable parts of a [`SECRET_HOME_DIRS`] entry: Codex session transcripts
/// (the file viewer opens them, like `~/.claude/projects`).
const ALLOWED_HOME_DIRS: &[&str] = &[".codex/sessions", ".codex/archived_sessions"];

/// Credential files under HOME.
const SECRET_HOME_FILES: &[&str] = &[
    ".claude/.credentials.json",
    ".claude/settings.json",
    ".claude/settings.local.json",
    ".netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
];

/// HOME file-name prefixes holding credentials (`~/.claude.json` and its
/// `.backup` copies carry the Claude account).
const SECRET_HOME_FILE_PREFIXES: &[&str] = &[".claude.json"];

/// Devdy's own app data folders (prod and dev builds, all their accounts,
/// server keys, sandboxed CLI logins) are named after this identifier.
const APP_ID_PREFIX: &str = "vn.papay.devdy";

/// Readable parts of this app's data dir: Slack thread downloads (previews)
/// and the log folder (`remote_fs_log_dir`). Never credentials.
const ALLOWED_APP_DIRS: &[&str] = &["slack-threads", "logs"];

/// Backup stores, anywhere (a Time Machine disk holds copies of every
/// credential store above): folder names, and bundle extensions.
const BACKUP_NAMES: &[&str] = &["backups.backupdb", ".timemachine", ".mobilebackups", "com.apple.timemachine.localsnapshots"];
const BACKUP_SUFFIXES: &[&str] = &[".backupbundle", ".sparsebundle"];

/// macOS: the data volume's firmlink. `/Users/x` and
/// `/System/Volumes/Data/Users/x` are the same files, and `canonicalize` keeps
/// whichever form it was given.
pub const DATA_VOLUME: &str = "/System/Volumes/Data";

/// A file's identity: (device, inode). Matching on it sees through every
/// alias of a path (firmlinks, hard-linked directories, case, bind mounts).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId(u64, u64);

impl FileId {
    /// The device the file lives on.
    pub fn dev(&self) -> u64 {
        self.0
    }

    /// `None` when `path` does not exist (or on a platform without inodes).
    pub fn of(path: &Path) -> Option<FileId> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(path).ok().map(|m| FileId(m.dev(), m.ino()))
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            None
        }
    }
}

/// `path` without the [`DATA_VOLUME`] prefix (`/System/Volumes/Data/Users/x`
/// → `/Users/x`); unchanged otherwise.
pub fn strip_data_volume(path: &Path) -> PathBuf {
    match path.strip_prefix(DATA_VOLUME) {
        Ok(rest) => Path::new("/").join(rest),
        Err(_) => path.to_path_buf(),
    }
}

/// The paths a remote-desktop read must never return.
///
/// Matching is on canonical paths, component by component and
/// case-insensitively (macOS volumes usually are), so `..`, symlinks and case
/// games all land on the real location first.
#[derive(Debug, Clone)]
pub struct SecretPaths {
    home: Option<PathBuf>,
    dirs: Vec<PathBuf>,
    allowed: Vec<PathBuf>,
    files: Vec<PathBuf>,
    /// Folders whose children named `vn.papay.devdy*` are denied (where the
    /// app data dir lives).
    app_parents: Vec<PathBuf>,
    /// The same stores by identity (those that exist), for [`Self::denies`]'s
    /// ancestor walk.
    dir_ids: Vec<FileId>,
    allowed_ids: Vec<FileId>,
    file_ids: Vec<FileId>,
    app_parent_ids: Vec<FileId>,
}

impl SecretPaths {
    /// Policy for a given HOME and this app's data dir.
    pub fn new(home: Option<&Path>, app_data: Option<&Path>) -> Self {
        let mut p = SecretPaths {
            home: None,
            dirs: Vec::new(),
            allowed: Vec::new(),
            files: Vec::new(),
            app_parents: Vec::new(),
            dir_ids: Vec::new(),
            allowed_ids: Vec::new(),
            file_ids: Vec::new(),
            app_parent_ids: Vec::new(),
        };
        if let Some(home) = home {
            for h in both_forms(home) {
                p.dirs.extend(SECRET_HOME_DIRS.iter().flat_map(|d| both_forms(&h.join(d))));
                p.allowed.extend(ALLOWED_HOME_DIRS.iter().flat_map(|d| both_forms(&h.join(d))));
                p.files.extend(SECRET_HOME_FILES.iter().flat_map(|f| both_forms(&h.join(f))));
            }
            p.home = Some(canonical_or_self(home));
        }
        if let Some(app_data) = app_data {
            p.dirs.extend(both_forms(app_data));
            p.allowed.extend(ALLOWED_APP_DIRS.iter().flat_map(|d| both_forms(&app_data.join(d))));
            p.app_parents.extend(app_data.parent().into_iter().flat_map(both_forms));
        }
        let ids = |paths: &[PathBuf]| paths.iter().filter_map(|x| FileId::of(x)).collect::<Vec<_>>();
        p.dir_ids = ids(&p.dirs);
        p.allowed_ids = ids(&p.allowed);
        p.file_ids = ids(&p.files);
        p.app_parent_ids = ids(&p.app_parents);
        p
    }

    /// The policy of this machine.
    pub fn for_app<R: Runtime>(app: &tauri::AppHandle<R>) -> Self {
        let home = app.path().home_dir().ok();
        let app_data = app.path().app_data_dir().ok();
        Self::new(home.as_deref(), app_data.as_deref())
    }

    /// Whether `path` (already canonical) is or lies under a credential store:
    /// by name (also with the [`DATA_VOLUME`] prefix dropped) or by identity —
    /// the closest ancestor that is a store, or an allowed part of one, decides.
    pub fn denies(&self, path: &Path) -> bool {
        self.denies_by_name(path) || self.denies_by_name(&strip_data_volume(path)) || self.denies_by_id(path)
    }

    fn denies_by_id(&self, path: &Path) -> bool {
        let mut first = true;
        for ancestor in path.ancestors() {
            let Some(id) = FileId::of(ancestor) else {
                first = false;
                continue;
            };
            if first && self.file_ids.contains(&id) {
                return true;
            }
            first = false;
            if self.allowed_ids.contains(&id) {
                return false;
            }
            if self.dir_ids.contains(&id) {
                return true;
            }
            let named_app = ancestor
                .file_name()
                .is_some_and(|n| n.to_string_lossy().to_lowercase().starts_with(APP_ID_PREFIX));
            if named_app && ancestor.parent().and_then(FileId::of).is_some_and(|pid| self.app_parent_ids.contains(&pid)) {
                return true;
            }
        }
        false
    }

    fn denies_by_name(&self, path: &Path) -> bool {
        if self.in_backup_or_other_home(path) {
            return true;
        }
        if self.files.iter().any(|f| same_path(path, f)) {
            return true;
        }
        if let Some(home) = &self.home {
            let in_home = path.parent().is_some_and(|parent| same_path(parent, home));
            let name = path.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            if in_home && SECRET_HOME_FILE_PREFIXES.iter().any(|pre| name.starts_with(pre)) {
                return true;
            }
        }
        let allowed = self.allowed.iter().any(|a| is_under(path, a));
        for parent in &self.app_parents {
            if let Some(child) = child_under(path, parent) {
                if child.to_lowercase().starts_with(APP_ID_PREFIX) && !allowed {
                    return true;
                }
            }
        }
        let denied = self.dirs.iter().any(|d| is_under(path, d));
        denied && !allowed
    }

    /// Whether `path` is in a backup store, or in a credential store of a home
    /// other than this user's (`…/Users/<name>/.ssh`, `…/Users/<name>/Library/
    /// Application Support/vn.papay.devdy*`, …) — the internal disk of another
    /// Mac or a backup mounted under `/Volumes`. By name: those are not ours.
    fn in_backup_or_other_home(&self, path: &Path) -> bool {
        let p = parts(&strip_data_volume(path));
        if p.iter().any(|c| BACKUP_NAMES.contains(&c.as_str()) || BACKUP_SUFFIXES.iter().any(|s| c.ends_with(s))) {
            return true;
        }
        let own = self.home.as_deref().map(parts);
        let split = |rel: &str| -> Vec<String> { rel.split('/').map(str::to_lowercase).collect() };
        let starts = |rest: &[String], rel: &str| {
            let r = split(rel);
            rest.len() >= r.len() && rest[..r.len()] == r[..]
        };
        for i in 0..p.len().saturating_sub(2) {
            if p[i] != "users" || own.as_deref() == Some(&p[..i + 2]) {
                continue;
            }
            let rest = &p[i + 2..];
            if ALLOWED_HOME_DIRS.iter().any(|d| starts(rest, d)) {
                continue;
            }
            let app = starts(rest, "library/application support")
                && rest.get(2).is_some_and(|n| n.starts_with(APP_ID_PREFIX));
            if app
                || SECRET_HOME_DIRS.iter().any(|d| starts(rest, d))
                || SECRET_HOME_FILES.iter().any(|f| split(f) == rest)
                || (rest.len() == 1 && SECRET_HOME_FILE_PREFIXES.iter().any(|pre| rest[0].starts_with(pre)))
            {
                return true;
            }
        }
        false
    }

    /// Every credential store this policy knows that exists, Devdy's app data
    /// folders included: what a rename / delete must never take along.
    pub fn protected(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = self.dirs.iter().chain(&self.files).filter(|p| p.exists()).cloned().collect();
        for parent in &self.app_parents {
            let Ok(entries) = std::fs::read_dir(parent) else { continue };
            out.extend(
                entries
                    .flatten()
                    .filter(|e| e.file_name().to_string_lossy().to_lowercase().starts_with(APP_ID_PREFIX))
                    .map(|e| e.path()),
            );
        }
        if let Some(home) = &self.home {
            if let Ok(entries) = std::fs::read_dir(home) {
                out.extend(
                    entries
                        .flatten()
                        .filter(|e| {
                            let n = e.file_name().to_string_lossy().to_lowercase();
                            SECRET_HOME_FILE_PREFIXES.iter().any(|pre| n.starts_with(pre))
                        })
                        .map(|e| e.path()),
                );
            }
        }
        out
    }

    /// Resolve `raw` (symlinks, `..`, relative to the process cwd) and refuse
    /// a credential store. Returns the canonical path: open THAT, not `raw`,
    /// so a link swapped in after the check is not followed.
    pub fn check(&self, raw: &str) -> Result<PathBuf, String> {
        let canonical = Path::new(raw).canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("not found: {raw}"),
            std::io::ErrorKind::PermissionDenied => format!("permission denied: {raw}"),
            _ => format!("{raw}: {e}"),
        })?;
        if self.denies(&canonical) {
            // `permission denied` maps to a 403 in the remote file proxy.
            return Err(format!("permission denied: {raw} (not readable from a remote desktop)"));
        }
        Ok(canonical)
    }
}

/// The path as given and its canonical form (when it exists and differs), so a
/// symlinked HOME or store is matched either way.
fn both_forms(path: &Path) -> Vec<PathBuf> {
    let mut out = vec![path.to_path_buf()];
    if let Ok(c) = path.canonicalize() {
        if c != path {
            out.push(c);
        }
    }
    out
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Normal components, lower-cased (`/`, `.` and prefixes dropped).
fn parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            Component::ParentDir => Some("..".to_string()),
            _ => None,
        })
        .collect()
}

fn is_under(path: &Path, root: &Path) -> bool {
    let (p, r) = (parts(path), parts(root));
    p.len() >= r.len() && p[..r.len()] == r[..]
}

fn same_path(a: &Path, b: &Path) -> bool {
    parts(a) == parts(b)
}

/// The first component of `path` below `root`, if `path` is under it.
fn child_under(path: &Path, root: &Path) -> Option<String> {
    let (p, r) = (parts(path), parts(root));
    (p.len() > r.len() && p[..r.len()] == r[..]).then(|| p[r.len()].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn only_remote_control_config_and_plumbing_are_local_only() {
        for name in [
            "remote_set_master_password",
            "remote_reveal_otp",
            "remote_create_session_link",
            "remote_kick_controller",
            "remote_set_config",
            "remote_set_device_alias",
            "remote_enable",
            "remote_disable",
            "remote_clear_audit",
            "device_sync_list_devices",
            "device_sync_pull",
            "remote_push_remove",
            "remote_push_set_events",
            "remote_push_test",
            "read_local_file_chunk",
            "stat_local_files",
            "rdfile_register",
            "rdfile_respond",
            "rd_download_create",
            "rd_download_append",
            "rd_copy_secret",
            "start_file_drag",
        ] {
            assert!(is_local_only_command(name), "{name}");
        }
    }

    #[test]
    fn everything_else_is_available_over_ipc() {
        // Full control: credentials, the vault, backups and settings included.
        for name in [
            "list_runs",
            "start_run",
            "send_user_message",
            "list_github_accounts",
            "create_github_account",
            "validate_gitlab_account",
            "update_aws_account",
            "aws_sso_login_profile",
            "azure_login",
            "login_claude_account",
            "submit_claude_login_code",
            "add_google_account",
            "reauth_google_account",
            "google_forget_client",
            "delete_google_account",
            "list_mcp_servers",
            "update_mcp_server",
            "export_mcp_server",
            "create_vps_server",
            "export_settings_backup",
            "import_settings_backup",
            "get_inbox_api_info",
            "regenerate_inbox_api_token",
            "secret_notes_list",
            "secret_note_reveal",
            "secret_vault_unlock",
            "secret_note_from_note",
            "remote_status",
            "remote_end_session",
            "remote_fs_read",
            "add_todo",
            "update_setting",
        ] {
            assert!(!is_local_only_command(name), "{name}");
        }
        // Exact names only: no accidental prefix match.
        assert!(!is_local_only_command("remote_enable_extra"));
    }

    #[test]
    fn sensitive_commands_are_audited() {
        for name in [
            "create_github_account",
            "delete_claude_account",
            "aws_sso_login",
            "update_mcp_server",
            "export_mcp_server",
            "update_vps_server",
            "export_settings_backup",
            "regenerate_inbox_api_token",
            "secret_note_reveal",
            "secret_note_copy",
            "secret_vault_unlock",
            "update_setting",
        ] {
            assert!(audited_command(name), "{name}");
        }
        for name in ["list_runs", "list_github_accounts", "secret_vault_touch", "secret_vault_status", "secret_notes_list"] {
            assert!(!audited_command(name), "{name}");
        }
    }

    #[test]
    fn local_only_and_audited_commands_are_registered() {
        // A renamed command would silently fall off the list.
        let lib = include_str!("../lib.rs");
        let handler = &lib[lib.find("generate_handler![").expect("generate_handler! in lib.rs")..];
        for name in LOCAL_ONLY_COMMANDS.iter().chain(AUDITED_COMMANDS) {
            assert!(
                handler.lines().any(|l| l.trim().trim_end_matches(',').rsplit("::").next() == Some(name)),
                "{name} is not in generate_handler!"
            );
        }
    }

    #[test]
    fn the_invoke_gate_refuses_only_the_bridge() {
        assert!(refuses_invoke(BRIDGE_LABEL, "remote_set_master_password"));
        assert!(refuses_invoke(BRIDGE_LABEL, "read_local_file_chunk"));
        assert!(!refuses_invoke(BRIDGE_LABEL, "export_settings_backup"));
        assert!(!refuses_invoke(BRIDGE_LABEL, "secret_note_reveal"));
        assert!(!refuses_invoke(BRIDGE_LABEL, "list_runs"));
        // The bridge's own plumbing must keep working.
        assert!(!refuses_invoke(BRIDGE_LABEL, "remote_bridge_ready"));
        assert!(!refuses_invoke(BRIDGE_LABEL, "remote_bridge_batch"));
        // The local UI (and the remote-desktop windows on the controlling side)
        // are not the bridge.
        for label in ["main", "remote-desktop", "remote-secret-notes", "secret-notes", "remote-bridge-x"] {
            assert!(!refuses_invoke(label, "remote_set_master_password"), "{label}");
        }
    }

    #[test]
    fn the_invoke_gate_is_installed() {
        let lib = include_str!("../lib.rs");
        assert!(lib.contains(".invoke_handler(remote::guard::gated(tauri::generate_handler!["), "lib.rs must wrap generate_handler! with guard::gated");
    }

    /// Tauri capability windows: exact labels or a trailing-`*` glob here.
    fn window_matches(pattern: &str, label: &str) -> bool {
        match pattern.strip_suffix('*') {
            Some(prefix) => label.starts_with(prefix),
            None => pattern == label,
        }
    }

    #[test]
    fn the_bridge_gets_only_its_own_minimal_capability() {
        let caps: [(&str, serde_json::Value); 2] = [
            ("default", serde_json::from_str(include_str!("../../capabilities/default.json")).unwrap()),
            ("remote-bridge", serde_json::from_str(include_str!("../../capabilities/remote-bridge.json")).unwrap()),
        ];
        for (name, cap) in &caps {
            let windows: Vec<&str> = cap["windows"].as_array().unwrap().iter().map(|w| w.as_str().unwrap()).collect();
            let matches = windows.iter().any(|w| window_matches(w, BRIDGE_LABEL));
            assert_eq!(matches, *name == "remote-bridge", "{name} windows: {windows:?}");
            if *name == "remote-bridge" {
                assert_eq!(cap["permissions"], serde_json::json!(["core:event:allow-listen", "core:event:allow-unlisten"]));
            } else {
                // The remote-desktop windows (controlling side) keep theirs.
                for label in ["remote-desktop", "remote-session-abc", "remote-fileviewer-x", "remote-item-list-todo"] {
                    assert!(windows.iter().any(|w| window_matches(w, label)), "{label}");
                }
            }
        }
    }

    #[test]
    fn remote_settings_are_all_but_remote_controls_own() {
        for key in [
            "theme",
            "language",
            "default_engine",
            "claude_model",
            "extra_args",
            "default_permission_mode",
            "claude_path",
            "codex_path",
            "node_path",
            "inbox_api_enabled",
            "project_path_map",
            "member",
            "secret_salt",
        ] {
            assert!(remote_may_set_setting(key), "{key}");
        }
        for key in [
            "remote_enabled",
            "remote_relay_url",
            "remote_device_id",
            "remote_device_alias",
            "remote_push_events",
            "remote_anything",
            "",
        ] {
            assert!(!remote_may_set_setting(key), "{key}");
        }
    }

    /// A fake HOME + app data dir under a temp dir.
    struct Fixture {
        root: PathBuf,
        home: PathBuf,
        policy: SecretPaths,
    }

    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("devdy-guard-{}", uuid::Uuid::new_v4())).canonicalize_parent();
            let home = root.join("home");
            let support = home.join("Library/Application Support");
            let app_data = support.join("vn.papay.devdy");
            for d in [
                home.join(".ssh"),
                home.join(".aws"),
                home.join(".claude/projects/p"),
                home.join(".codex/sessions/2026"),
                home.join("proj/src"),
                app_data.join("server-keys"),
                app_data.join("claude-accounts/a1"),
                support.join("vn.papay.devdy.dev/azure-accounts"),
                support.join("Other App"),
                home.join("Library/Keychains"),
            ] {
                fs::create_dir_all(&d).unwrap();
            }
            for f in [
                ".ssh/id_ed25519",
                ".aws/credentials",
                ".claude/.credentials.json",
                ".claude.json",
                ".claude.json.backup",
                ".claude/projects/p/s.jsonl",
                ".codex/auth.json",
                ".codex/sessions/2026/r.jsonl",
                ".netrc",
                "proj/src/main.rs",
                "Library/Application Support/vn.papay.devdy/server-keys/k",
                "Library/Application Support/vn.papay.devdy/claude-accounts/a1/.credentials.json",
                "Library/Application Support/vn.papay.devdy.dev/azure-accounts/t",
                "Library/Application Support/Other App/x",
                "Library/Keychains/login.keychain-db",
            ] {
                fs::write(home.join(f), b"x").unwrap();
            }
            let policy = SecretPaths::new(Some(&home), Some(&app_data));
            Fixture { root, home, policy }
        }

        fn p(&self, rel: &str) -> String {
            self.home.join(rel).to_string_lossy().into_owned()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    trait CanonParent {
        fn canonicalize_parent(self) -> PathBuf;
    }
    impl CanonParent for PathBuf {
        /// The temp dir itself may sit behind a symlink (`/var` → `/private/var`).
        fn canonicalize_parent(self) -> PathBuf {
            let parent = self.parent().unwrap().canonicalize().unwrap();
            parent.join(self.file_name().unwrap())
        }
    }

    #[test]
    fn credential_stores_are_not_readable() {
        let fx = Fixture::new();
        for rel in [
            ".ssh/id_ed25519",
            ".ssh",
            ".aws/credentials",
            ".claude/.credentials.json",
            ".claude.json",
            ".claude.json.backup",
            ".codex/auth.json",
            ".netrc",
            "Library/Application Support/vn.papay.devdy/server-keys/k",
            "Library/Application Support/vn.papay.devdy/claude-accounts/a1/.credentials.json",
            // The other build's data dir too.
            "Library/Application Support/vn.papay.devdy.dev/azure-accounts/t",
            "Library/Keychains/login.keychain-db",
        ] {
            let err = fx.policy.check(&fx.p(rel)).unwrap_err();
            assert!(err.starts_with("permission denied"), "{rel}: {err}");
        }
    }

    #[test]
    fn ordinary_files_and_transcripts_stay_readable() {
        let fx = Fixture::new();
        for rel in [
            "proj/src/main.rs",
            ".claude/projects/p/s.jsonl",
            ".codex/sessions/2026/r.jsonl",
            "Library/Application Support/Other App/x",
        ] {
            let got = fx.policy.check(&fx.p(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
            assert_eq!(got, fx.home.join(rel).canonicalize().unwrap());
        }
        assert!(fx.policy.check(&fx.p("proj/nope.txt")).unwrap_err().starts_with("not found"));
    }

    #[test]
    fn dot_dot_and_case_tricks_resolve_first() {
        let fx = Fixture::new();
        for rel in ["proj/src/../../.ssh/id_ed25519", "proj/../.aws/./credentials", ".codex/sessions/../auth.json"] {
            assert!(fx.policy.check(&fx.p(rel)).is_err(), "{rel}");
        }
        // Case-insensitive volumes (macOS) open `.SSH` as `.ssh`.
        assert!(fx.policy.denies(&fx.home.join(".SSH/id_ed25519")));
        assert!(fx.policy.denies(&fx.home.join("library/application support/VN.PAPAY.DEVDY/x")));
    }

    #[test]
    fn the_data_volume_firmlink_is_the_same_place() {
        let fx = Fixture::new();
        let aliased = Path::new(DATA_VOLUME).join(fx.home.join(".ssh/id_ed25519").strip_prefix("/").unwrap());
        // By name, whether or not the alias exists on this machine.
        assert!(fx.policy.denies(&aliased), "{}", aliased.display());
        assert_eq!(strip_data_volume(&aliased), fx.home.join(".ssh/id_ed25519"));
        // And through the real alias, where there is one (macOS).
        if aliased.exists() {
            assert!(fx.policy.check(&aliased.to_string_lossy()).is_err());
        }
    }

    #[cfg(unix)]
    #[test]
    fn stores_are_matched_by_identity_too() {
        let fx = Fixture::new();
        // A second name for HOME the policy was not built with (a bind mount /
        // firmlink stand-in): identity still finds `.aws` above the file.
        let alias = fx.root.join("alias-home");
        std::os::unix::fs::symlink(&fx.home, &alias).unwrap();
        let via_alias = alias.join(".aws/credentials");
        assert!(fx.policy.denies_by_id(&via_alias));
        // Allowed parts win when they are closer.
        assert!(!fx.policy.denies_by_id(&alias.join(".codex/sessions/2026/r.jsonl")));
        assert!(fx.policy.denies_by_id(&alias.join(".codex/auth.json")));
        // App-data exceptions: Slack downloads stay readable.
        let app = fx.home.join("Library/Application Support/vn.papay.devdy");
        fs::create_dir_all(app.join("slack-threads/t1")).unwrap();
        fs::write(app.join("slack-threads/t1/a.png"), b"x").unwrap();
        let policy = SecretPaths::new(Some(&fx.home), Some(&app));
        assert!(policy.check(&app.join("slack-threads/t1/a.png").to_string_lossy()).is_ok());
        assert!(policy.check(&app.join("server-keys/k").to_string_lossy()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_into_a_store_are_refused() {
        let fx = Fixture::new();
        let link = fx.home.join("proj/innocent.txt");
        std::os::unix::fs::symlink(fx.home.join(".ssh/id_ed25519"), &link).unwrap();
        assert!(fx.policy.check(&link.to_string_lossy()).is_err());
        // A linked directory too.
        let dir_link = fx.home.join("proj/keys");
        std::os::unix::fs::symlink(fx.home.join("Library/Application Support/vn.papay.devdy"), &dir_link).unwrap();
        assert!(fx.policy.check(&fx.p("proj/keys/server-keys/k")).is_err());
        // A store that is itself a link elsewhere is matched at its target.
        let real_aws = fx.root.join("elsewhere-aws");
        fs::create_dir_all(&real_aws).unwrap();
        fs::write(real_aws.join("credentials"), b"x").unwrap();
        fs::remove_dir_all(fx.home.join(".aws")).unwrap();
        std::os::unix::fs::symlink(&real_aws, fx.home.join(".aws")).unwrap();
        let policy = SecretPaths::new(Some(&fx.home), None);
        assert!(policy.check(&real_aws.join("credentials").to_string_lossy()).is_err());
        assert!(policy.check(&fx.p(".aws/credentials")).is_err());
    }
}
