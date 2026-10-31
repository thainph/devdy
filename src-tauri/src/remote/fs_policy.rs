//! Where a remote desktop may touch this machine's files.
//!
//! Every file command called by the remote-desktop bridge
//! ([`crate::remote::guard::from_remote_bridge`]) goes through a [`RemoteFs`]:
//! an ALLOWLIST, not a denylist — a path is usable only under
//!
//! - HOME, read + write;
//! - a mounted volume (`/Volumes/*`, macOS), read + write;
//! - a registered Devdy project or repo root (from the DB) and the remote upload
//!   dir, read + write — they may live outside HOME;
//! - Devdy's log folder and Slack thread downloads, read only: the log is the
//!   trace a Controller must not erase;
//!
//! and, inside those, never a credential store ([`SecretPaths`]: `~/.ssh`,
//! cloud CLI logins, Devdy's app data, …). Exports may land anywhere writable.
//!
//! A paired remote desktop has full control (it runs agents, edits settings
//! and accounts), so the files a tool executes or loads as config (`.git/hooks`,
//! `.claude/`, `.mcp.json`) are writable like any other: refusing them would
//! not keep code from running here, only get in the user's way.
//!
//! Roots are matched by identity ((device, inode) of the target's ancestors),
//! so `..`, symlinks, case and the `/System/Volumes/Data` firmlink cannot
//! reach outside them; the closest root decides (a read-only folder inside
//! HOME stays read-only). A registered project root that would open far more
//! than a project — HOME's ancestors (`/`, `/Users`), a volume root, `/Volumes`
//! — is ignored.
//!
//! Renaming or deleting ([`RemoteFs::change`]) is held to more: never a root,
//! a mount point, nor a folder holding a credential store, Devdy's app data
//! or the log folder (`~/.claude`, `~/.config`, `~/Library/…`) — moving those
//! away would take them out of the name-based checks, deleting them would
//! wipe them.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tauri::{Manager, Runtime, Webview};

use crate::db::Db;
use crate::remote::guard::{from_remote_bridge, FileId, SecretPaths, DATA_VOLUME};

/// Most entries a copied / moved / imported tree may hold before the check
/// gives up (and refuses).
const MAX_TREE_ENTRIES: usize = 50_000;

/// Where macOS mounts external / network volumes. The boot volume's entry
/// there is a symlink to `/`, resolved (and so not matched) before the check.
#[cfg(target_os = "macos")]
const VOLUMES: &str = "/Volumes";

/// A root, by identity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Root {
    id: FileId,
    writable: bool,
}

/// One remote caller's view of the file system (see module docs).
#[derive(Debug, Clone)]
pub struct RemoteFs {
    roots: Vec<Root>,
    /// Never renamed / deleted: the roots, and every folder a credential store
    /// or the log folder lies in (the stores themselves included).
    fixed: HashSet<FileId>,
    secrets: SecretPaths,
}

/// Roots a [`RemoteFs`] is built from.
#[derive(Debug, Default)]
pub struct Roots {
    /// HOME, mounted volumes and the upload dir.
    pub writable: Vec<PathBuf>,
    /// Registered project / repo roots (writable, unless too wide).
    pub projects: Vec<PathBuf>,
    /// Logs, Slack downloads.
    pub read_only: Vec<PathBuf>,
}

fn refused(raw: &Path, why: &str) -> String {
    // `permission denied` maps to a 403 in the remote file proxy.
    format!("permission denied: {} ({why} from a remote desktop)", raw.display())
}

impl RemoteFs {
    pub fn new(roots: Roots, home: Option<&Path>, secrets: SecretPaths) -> Self {
        // Above HOME: a project root there would open other users' homes, or `/`.
        let mut too_wide: Vec<FileId> = home
            .map(|h| h.ancestors().skip(1).filter_map(FileId::of).collect())
            .unwrap_or_default();
        #[cfg(target_os = "macos")]
        too_wide.extend(FileId::of(Path::new(VOLUMES)));
        too_wide.extend(FileId::of(Path::new(DATA_VOLUME)));
        let build = |paths: Vec<PathBuf>, writable: bool, project: bool| -> Vec<Root> {
            paths
                .into_iter()
                .filter_map(|p| {
                    let path = p.canonicalize().ok()?;
                    let id = FileId::of(&path)?;
                    let wide = path.parent().is_none() || (project && (too_wide.contains(&id) || is_mount_point(&path)));
                    (!wide).then_some(Root { id, writable })
                })
                .collect()
        };
        let read_only = roots.read_only.clone();
        let mut all = build(roots.read_only, false, false);
        all.extend(build(roots.writable, true, false));
        all.extend(build(roots.projects, true, true));
        let mut fixed: HashSet<FileId> = all.iter().map(|r| r.id).collect();
        for p in secrets.protected().into_iter().chain(read_only) {
            let Ok(p) = p.canonicalize() else { continue };
            fixed.extend(p.ancestors().filter_map(FileId::of));
        }
        RemoteFs { roots: all, fixed, secrets }
    }

    /// The policy for a call from `webview`, or `None` when it is not the
    /// remote-desktop bridge (the local UI keeps its own rules).
    pub async fn for_caller<R: Runtime>(webview: &Webview<R>, db: &Db) -> Result<Option<Self>, String> {
        if !from_remote_bridge(webview) {
            return Ok(None);
        }
        let app = webview.app_handle();
        let projects: Vec<String> = sqlx::query_scalar("SELECT path FROM projects UNION SELECT path FROM repos")
            .fetch_all(db)
            .await
            .map_err(|e| e.to_string())?;
        let paths = app.path();
        let home = paths.home_dir().ok();
        let app_data = paths.app_data_dir().ok();
        let mut roots = Roots { projects: projects.into_iter().map(PathBuf::from).collect(), ..Roots::default() };
        roots.writable.extend(home.clone());
        #[cfg(target_os = "macos")]
        roots.writable.push(PathBuf::from(VOLUMES));
        roots.writable.extend(crate::remote::upload::uploads_root(app).ok());
        if let Some(data) = &app_data {
            roots.read_only.extend([data.join("slack-threads"), data.join("logs")]);
        }
        let secrets = SecretPaths::new(home.as_deref(), app_data.as_deref());
        Ok(Some(Self::new(roots, home.as_deref(), secrets)))
    }

    /// The closest root `canonical` lies in.
    fn locate(&self, canonical: &Path) -> Option<Root> {
        for ancestor in canonical.ancestors() {
            let Some(id) = FileId::of(ancestor) else { continue };
            // Read-only first: a folder registered both ways stays read-only.
            if let Some(root) = self.roots.iter().filter(|r| r.id == id).min_by_key(|r| r.writable) {
                return Some(*root);
            }
        }
        None
    }

    /// An existing path, resolved, that the caller may read.
    pub fn read(&self, raw: &Path) -> Result<PathBuf, String> {
        let canonical = raw.canonicalize().map_err(|_| format!("not found: {}", raw.display()))?;
        if self.locate(&canonical).is_none() {
            return Err(refused(raw, "outside HOME and the projects, not reachable"));
        }
        if self.secrets.denies(&canonical) {
            return Err(refused(raw, "a credential store, not readable"));
        }
        Ok(canonical)
    }

    /// `raw` (existing or about to be created) resolved, when the caller may
    /// create / change / remove it — exports included.
    pub fn write(&self, raw: &Path) -> Result<PathBuf, String> {
        let resolved = resolve_for_write(raw).ok_or_else(|| format!("not found: {}", raw.display()))?;
        match self.locate(&resolved) {
            None => return Err(refused(raw, "outside HOME and the projects, not writable")),
            Some(root) if !root.writable => return Err(refused(raw, "read only")),
            Some(_) => {}
        }
        if self.secrets.denies(&resolved) {
            return Err(refused(raw, "a credential store, not writable"));
        }
        Ok(resolved)
    }

    /// The existing entry `raw` resolved, when the caller may rename, move or
    /// delete it: writable, and none of the [`RemoteFs::fixed`] folders (a
    /// root, a mount point, or a folder holding a credential store / the logs).
    pub fn change(&self, raw: &Path) -> Result<PathBuf, String> {
        let resolved = self.write(raw)?;
        let held = FileId::of(&resolved).is_some_and(|id| self.fixed.contains(&id));
        if held || is_mount_point(&resolved) {
            return Err(refused(raw, "a root or a folder holding credentials / logs, not movable"));
        }
        Ok(resolved)
    }

    /// Before copying / moving the tree at `src` (already checked itself):
    /// every entry must be readable — a link inside could point at a
    /// credential store, and copies follow links.
    pub fn tree(&self, src: &Path) -> Result<(), String> {
        let mut stack = vec![src.to_path_buf()];
        let mut seen = 0usize;
        while let Some(path) = stack.pop() {
            seen += 1;
            if seen > MAX_TREE_ENTRIES {
                return Err(refused(src, "too many entries to check"));
            }
            let canonical = self.read(&path)?;
            if canonical.is_dir() {
                let entries = std::fs::read_dir(&canonical).map_err(|e| format!("read directory: {e}"))?;
                stack.extend(entries.flatten().map(|entry| entry.path()));
            }
        }
        Ok(())
    }
}

/// Whether `path` is where a file system is mounted (its device differs from
/// its parent's): a volume root.
fn is_mount_point(path: &Path) -> bool {
    match (FileId::of(path), path.parent().and_then(FileId::of)) {
        (Some(id), Some(parent)) => id.dev() != parent.dev(),
        _ => false,
    }
}

/// `raw` resolved: itself when it exists, else its (existing) parent resolved
/// plus its name.
fn resolve_for_write(raw: &Path) -> Option<PathBuf> {
    if let Ok(c) = raw.canonicalize() {
        return Some(c);
    }
    let name = raw.file_name()?;
    let parent = raw.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    Some(parent.canonicalize().ok()?.join(name))
}

/// `Ok(path)` unchanged for a local caller, else `policy.read(path)`.
pub fn read(policy: Option<&RemoteFs>, path: &Path) -> Result<PathBuf, String> {
    match policy {
        Some(p) => p.read(path),
        None => Ok(path.to_path_buf()),
    }
}

/// `Ok(path)` unchanged for a local caller, else `policy.change(path)`.
pub fn change(policy: Option<&RemoteFs>, path: &Path) -> Result<PathBuf, String> {
    match policy {
        Some(p) => p.change(path),
        None => Ok(path.to_path_buf()),
    }
}

/// `Ok(path)` unchanged for a local caller, else `policy.write(path)`.
pub fn write(policy: Option<&RemoteFs>, path: &Path) -> Result<PathBuf, String> {
    match policy {
        Some(p) => p.write(path),
        None => Ok(path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Fx {
        base: PathBuf,
        home: PathBuf,
        proj: PathBuf,
        /// A stand-in for a mounted volume (`/Volumes/USB`).
        volumes: PathBuf,
        fs: RemoteFs,
    }

    impl Drop for Fx {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn fx() -> Fx {
        let base = std::env::temp_dir().canonicalize().unwrap().join(format!("devdy-rfs-{}", uuid::Uuid::new_v4()));
        let home = base.join("home");
        let proj = home.join("work/proj");
        let volumes = base.join("Volumes");
        let app_data = home.join("Library/Application Support/vn.papay.devdy");
        for d in [
            home.join(".config/gh"),
            proj.join("src"),
            proj.join(".git/hooks"),
            proj.join(".claude"),
            home.join(".ssh"),
            home.join("Downloads"),
            home.join("Documents"),
            home.join(".claude/projects/p"),
            app_data.join("logs"),
            app_data.join("server-keys"),
            volumes.join("USB/photos"),
            volumes.join("USB/Users/bob/.ssh"),
            volumes.join("TM/Backups.backupdb/mac"),
            base.join("outside"),
            base.join("uploads"),
            base.join("elsewhere-proj"),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        for f in [
            "work/proj/src/a.rs",
            "work/proj/.env",
            ".ssh/id_rsa",
            "Documents/private.txt",
            ".claude/projects/p/s.jsonl",
            ".netrc",
            ".claude/.credentials.json",
            ".config/gh/hosts.yml",
            "Library/Application Support/vn.papay.devdy/logs/devdy.log",
            "Library/Application Support/vn.papay.devdy/server-keys/k",
        ] {
            fs::write(home.join(f), b"x").unwrap();
        }
        for f in ["USB/photos/a.jpg", "USB/Users/bob/notes.txt", "USB/Users/bob/.ssh/id_rsa", "TM/Backups.backupdb/mac/f"] {
            fs::write(volumes.join(f), b"x").unwrap();
        }
        fs::write(base.join("outside/x.txt"), b"x").unwrap();
        fs::write(base.join("elsewhere-proj/main.rs"), b"x").unwrap();
        let roots = Roots {
            writable: vec![home.clone(), volumes.clone(), PathBuf::from("/"), base.join("uploads")],
            // A project above HOME (here: the temp base) must not open it all.
            projects: vec![proj.clone(), base.join("elsewhere-proj"), base.clone(), PathBuf::from("/")],
            read_only: vec![app_data.join("logs")],
        };
        let fs = RemoteFs::new(roots, Some(&home), SecretPaths::new(Some(&home), Some(&app_data)));
        Fx { base, home, proj, volumes, fs }
    }

    #[test]
    fn home_files_are_readable_and_writable() {
        let f = fx();
        for p in [f.proj.join("src/a.rs"), f.proj.join(".env"), f.home.join("Documents/private.txt"), f.home.join(".claude/projects/p/s.jsonl")] {
            assert!(f.fs.read(&p).is_ok(), "{}", p.display());
            assert!(f.fs.write(&p).is_ok(), "{}", p.display());
        }
        assert!(f.fs.write(&f.home.join("Documents/new.txt")).is_ok());
        assert!(f.fs.write(&f.home.join(".zshrc")).is_ok());
    }

    #[test]
    fn credential_stores_are_refused() {
        let f = fx();
        for p in [
            f.home.join(".ssh/id_rsa"),
            f.home.join(".ssh"),
            f.home.join(".netrc"),
            f.home.join("Library/Application Support/vn.papay.devdy/server-keys/k"),
        ] {
            assert!(f.fs.read(&p).unwrap_err().starts_with("permission denied"), "{}", p.display());
            assert!(f.fs.write(&p).is_err(), "{}", p.display());
        }
        assert!(f.fs.write(&f.home.join(".ssh/authorized_keys")).is_err());
        // `..` from a project.
        assert!(f.fs.read(&f.proj.join("../../.ssh/id_rsa")).is_err());
    }

    #[test]
    fn outside_home_only_volumes_and_projects() {
        let f = fx();
        assert!(f.fs.read(&f.volumes.join("USB/photos/a.jpg")).is_ok());
        assert!(f.fs.write(&f.volumes.join("USB/photos/b.jpg")).is_ok());
        assert!(f.fs.read(&f.base.join("elsewhere-proj/main.rs")).is_ok());
        assert!(f.fs.write(&f.base.join("uploads/s.zip")).is_ok());
        for p in [f.base.join("outside/x.txt"), PathBuf::from("/etc/hosts")] {
            assert!(f.fs.read(&p).unwrap_err().starts_with("permission denied"), "{}", p.display());
            assert!(f.fs.write(&p).is_err(), "{}", p.display());
        }
        assert!(f.fs.write(&f.base.join("outside/new.txt")).is_err());
    }

    #[test]
    fn other_homes_and_backups_on_a_volume_are_refused() {
        let f = fx();
        assert!(f.fs.read(&f.volumes.join("USB/Users/bob/notes.txt")).is_ok());
        for rel in ["USB/Users/bob/.ssh/id_rsa", "USB/Users/bob/.ssh", "TM/Backups.backupdb/mac/f"] {
            assert!(f.fs.read(&f.volumes.join(rel)).unwrap_err().starts_with("permission denied"), "{rel}");
        }
        assert!(f.fs.write(&f.volumes.join("USB/Users/bob/.ssh/authorized_keys")).is_err());
    }

    #[test]
    fn folders_holding_credentials_or_logs_cannot_be_moved_or_deleted() {
        let f = fx();
        for p in [
            f.home.clone(),
            f.volumes.clone(),
            f.home.join(".claude"),
            f.home.join(".config"),
            f.home.join(".ssh"),
            f.home.join("Library"),
            f.home.join("Library/Application Support"),
            f.home.join("Library/Application Support/vn.papay.devdy"),
            f.home.join("Library/Application Support/vn.papay.devdy/logs"),
        ] {
            assert!(f.fs.change(&p).unwrap_err().starts_with("permission denied"), "{}", p.display());
        }
        // Ordinary entries, files next to a store included, still move.
        for p in [f.home.join("Documents"), f.home.join("Documents/private.txt"), f.proj.join("src"), f.home.join(".claude/projects/p")] {
            assert!(f.fs.change(&p).is_ok(), "{}", p.display());
        }
    }

    #[test]
    fn wide_project_roots_are_ignored() {
        let home = std::env::temp_dir().canonicalize().unwrap().join(format!("devdy-rfs-wide-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let mut projects = vec![PathBuf::from("/"), home.parent().unwrap().to_path_buf()];
        projects.push(PathBuf::from(DATA_VOLUME));
        #[cfg(target_os = "macos")]
        projects.push(PathBuf::from(VOLUMES));
        let policy = RemoteFs::new(Roots { projects, ..Roots::default() }, Some(&home), SecretPaths::new(Some(&home), None));
        assert!(policy.roots.is_empty(), "{:?}", policy.roots);
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn the_log_folder_stays_read_only() {
        let f = fx();
        let log = f.home.join("Library/Application Support/vn.papay.devdy/logs/devdy.log");
        assert!(f.fs.read(&log).is_ok());
        assert!(f.fs.write(&log).unwrap_err().contains("read only"));
        assert!(f.fs.write(&log.with_file_name("new.log")).is_err());
    }

    #[test]
    fn tool_config_is_writable_under_full_control() {
        let f = fx();
        for rel in [".git/hooks/pre-commit", ".git/config", ".claude/settings.json", ".mcp.json"] {
            assert!(f.fs.write(&f.proj.join(rel)).is_ok(), "{rel}");
        }
        assert!(f.fs.tree(&f.proj).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn links_into_a_credential_store_are_followed_then_refused() {
        let f = fx();
        std::os::unix::fs::symlink(f.home.join(".ssh"), f.proj.join("keys")).unwrap();
        std::os::unix::fs::symlink(f.home.join(".ssh/id_rsa"), f.home.join("Documents/key.png")).unwrap();
        std::os::unix::fs::symlink(f.base.join("outside/x.txt"), f.proj.join("out.txt")).unwrap();
        assert!(f.fs.read(&f.proj.join("keys/id_rsa")).is_err());
        assert!(f.fs.read(&f.home.join("Documents/key.png")).is_err());
        assert!(f.fs.write(&f.proj.join("keys/new")).is_err());
        assert!(f.fs.read(&f.proj.join("out.txt")).is_err());
        // A tree holding such a link cannot be copied.
        assert!(f.fs.tree(&f.proj).is_err());
        assert!(f.fs.tree(&f.proj.join("src")).is_ok());
    }

    #[test]
    fn the_data_volume_alias_of_a_root_is_the_same_root() {
        let f = fx();
        let alias = |p: PathBuf| Path::new("/System/Volumes/Data").join(p.strip_prefix("/").unwrap());
        let file = alias(f.proj.join("src/a.rs"));
        if file.exists() {
            // macOS: matched by identity, not by the literal prefix.
            assert!(f.fs.read(&file).is_ok());
            assert!(f.fs.read(&alias(f.home.join(".ssh/id_rsa"))).is_err());
        }
    }

    #[test]
    fn local_callers_are_untouched() {
        assert_eq!(read(None, Path::new("/etc/hosts")).unwrap(), PathBuf::from("/etc/hosts"));
        assert_eq!(write(None, Path::new("/x/y")).unwrap(), PathBuf::from("/x/y"));
    }
}
