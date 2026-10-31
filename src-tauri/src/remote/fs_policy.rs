//! Where a remote desktop may touch this machine's files.
//!
//! Every file command called by the remote-desktop bridge
//! ([`crate::remote::guard::from_remote_bridge`]) goes through a [`RemoteFs`]:
//! an ALLOWLIST, not a denylist — a path is usable only under
//!
//! - a registered Devdy project or repo root (from the DB), read + write;
//! - the remote upload dir, read + write;
//! - session transcripts (`~/.claude/projects`, `~/.codex/sessions`, …),
//!   Slack thread downloads and the log folder, read only;
//!
//! and, inside those, never a credential store ([`SecretPaths`]) nor — for a
//! write — a file a tool executes or loads as config ([`is_exec_config`]:
//! `.git/hooks`, `.claude/`, `.mcp.json`, …). Exports may only land in
//! Downloads or the upload dir.
//!
//! Roots are matched by identity ((device, inode) of the target's ancestors),
//! so `..`, symlinks, case and the `/System/Volumes/Data` firmlink cannot
//! reach outside them. A root at or above HOME (or `/`) is ignored: it would
//! open everything.

use std::path::{Path, PathBuf};

use tauri::{Manager, Runtime, Webview};

use crate::db::Db;
use crate::remote::guard::{from_remote_bridge, strip_data_volume, FileId, SecretPaths};

/// Most entries a copied / moved / imported tree may hold before the check
/// gives up (and refuses).
const MAX_TREE_ENTRIES: usize = 50_000;

/// A root, by identity.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Root {
    id: FileId,
}

/// One remote caller's view of the file system (see module docs).
#[derive(Debug, Clone)]
pub struct RemoteFs {
    writable: Vec<Root>,
    read_only: Vec<Root>,
    export: Vec<Root>,
    secrets: SecretPaths,
}

/// Roots a [`RemoteFs`] is built from.
#[derive(Debug, Default)]
pub struct Roots {
    /// Project / repo roots and the upload dir.
    pub writable: Vec<PathBuf>,
    /// Transcripts, Slack downloads, logs.
    pub read_only: Vec<PathBuf>,
    /// Where exports may be written (Downloads, the upload dir).
    pub export: Vec<PathBuf>,
}

fn refused(raw: &Path, why: &str) -> String {
    // `permission denied` maps to a 403 in the remote file proxy.
    format!("permission denied: {} ({why} from a remote desktop)", raw.display())
}

impl RemoteFs {
    pub fn new(roots: Roots, home: Option<&Path>, secrets: SecretPaths) -> Self {
        // HOME and everything above it: a root there would allow everything.
        let too_wide: Vec<FileId> = home
            .map(|h| h.ancestors().filter_map(FileId::of).collect())
            .unwrap_or_default();
        let build = |paths: Vec<PathBuf>| -> Vec<Root> {
            paths
                .into_iter()
                .filter_map(|p| {
                    let path = p.canonicalize().ok()?;
                    let id = FileId::of(&path)?;
                    let wide = too_wide.contains(&id) || path.parent().is_none();
                    (!wide).then_some(Root { id })
                })
                .collect()
        };
        RemoteFs {
            writable: build(roots.writable),
            read_only: build(roots.read_only),
            export: build(roots.export),
            secrets,
        }
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
        let uploads = crate::remote::upload::uploads_root(app).ok();
        let mut roots = Roots { writable: projects.into_iter().map(PathBuf::from).collect(), ..Roots::default() };
        roots.writable.extend(uploads.clone());
        roots.export.extend(uploads);
        if let Some(home) = &home {
            for d in [".claude/projects", ".codex/sessions", ".codex/archived_sessions"] {
                roots.read_only.push(home.join(d));
            }
        }
        if let Ok(dl) = paths.download_dir() {
            roots.export.push(dl);
        }
        if let Some(data) = &app_data {
            roots.read_only.extend([data.join("slack-threads"), data.join("logs")]);
        }
        let secrets = SecretPaths::new(home.as_deref(), app_data.as_deref());
        Ok(Some(Self::new(roots, home.as_deref(), secrets)))
    }

    /// The root `canonical` lies in, and its path relative to that root.
    fn locate<'a>(roots: &'a [Root], canonical: &Path) -> Option<(&'a Root, PathBuf)> {
        for ancestor in canonical.ancestors() {
            let Some(id) = FileId::of(ancestor) else { continue };
            if let Some(root) = roots.iter().find(|r| r.id == id) {
                let rel = canonical.strip_prefix(ancestor).map(Path::to_path_buf).unwrap_or_default();
                return Some((root, rel));
            }
        }
        None
    }

    /// An existing path, resolved, that the caller may read.
    pub fn read(&self, raw: &Path) -> Result<PathBuf, String> {
        let canonical = raw.canonicalize().map_err(|_| format!("not found: {}", raw.display()))?;
        let all: Vec<Root> = self.writable.iter().chain(&self.read_only).copied().collect();
        if Self::locate(&all, &canonical).is_none() {
            return Err(refused(raw, "outside the projects, not reachable"));
        }
        if self.secrets.denies(&canonical) {
            return Err(refused(raw, "a credential store, not readable"));
        }
        Ok(canonical)
    }

    /// `raw` (existing or about to be created) resolved, when the caller may
    /// create / change / remove it.
    pub fn write(&self, raw: &Path) -> Result<PathBuf, String> {
        self.write_in(&self.writable, raw)
    }

    /// A file an export may create: in Downloads or the upload dir only.
    pub fn export(&self, raw: &Path) -> Result<PathBuf, String> {
        self.write_in(&self.export, raw)
    }

    fn write_in(&self, roots: &[Root], raw: &Path) -> Result<PathBuf, String> {
        let resolved = resolve_for_write(raw).ok_or_else(|| format!("not found: {}", raw.display()))?;
        let Some((_, rel)) = Self::locate(roots, &resolved) else {
            return Err(refused(raw, "outside the projects, not writable"));
        };
        if self.secrets.denies(&resolved) {
            return Err(refused(raw, "a credential store, not writable"));
        }
        if is_exec_config(&rel) {
            return Err(refused(raw, "a hook / tool config, not writable"));
        }
        Ok(resolved)
    }

    /// Before copying / moving the tree at `src` (already checked itself):
    /// every entry must be readable (a link inside could point at a credential
    /// store — copies follow links), and none may be a hook / tool config
    /// (copying it in would plant one).
    pub fn tree(&self, src: &Path) -> Result<(), String> {
        let name = src.file_name().map(PathBuf::from).unwrap_or_default();
        let mut stack = vec![(src.to_path_buf(), name)];
        let mut seen = 0usize;
        while let Some((path, rel)) = stack.pop() {
            seen += 1;
            if seen > MAX_TREE_ENTRIES {
                return Err(refused(src, "too many entries to check"));
            }
            if is_exec_config(&rel) {
                return Err(refused(&path, "a hook / tool config, not copyable"));
            }
            let canonical = self.read(&path)?;
            if canonical.is_dir() {
                let entries = std::fs::read_dir(&canonical).map_err(|e| format!("read directory: {e}"))?;
                for entry in entries.flatten() {
                    stack.push((entry.path(), rel.join(entry.file_name())));
                }
            }
        }
        Ok(())
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

/// Whether a path (relative to its root) is something a tool runs or loads as
/// config, so writing it means code execution on this machine: git hooks and
/// config (`core.hooksPath`, `core.fsmonitor`), Claude / Codex project config
/// (hooks, MCP servers), `.mcp.json`, VS Code tasks. Also the bare `.git` /
/// `.vscode` folders: renaming a folder to one of those would plant the rest.
pub fn is_exec_config(rel: &Path) -> bool {
    let parts: Vec<String> = strip_data_volume(rel)
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(s) => Some(s.to_string_lossy().to_lowercase()),
            _ => None,
        })
        .collect();
    let any = |names: &[&str]| parts.iter().any(|p| names.contains(&p.as_str()));
    if any(&[".claude", ".codex", ".mcp.json"]) {
        return true;
    }
    let pair = |a: &str, b: &[&str]| parts.windows(2).any(|w| w[0] == a && b.contains(&w[1].as_str()));
    if pair(".git", &["hooks", "config"]) || pair(".vscode", &["tasks.json"]) {
        return true;
    }
    matches!(parts.last().map(String::as_str), Some(".git" | ".vscode"))
}

/// `Ok(path)` unchanged for a local caller, else `policy.read(path)`.
pub fn read(policy: Option<&RemoteFs>, path: &Path) -> Result<PathBuf, String> {
    match policy {
        Some(p) => p.read(path),
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
        for d in [
            proj.join("src"),
            proj.join(".git/hooks"),
            home.join(".ssh"),
            home.join("Downloads"),
            home.join("Documents"),
            home.join(".claude/projects/p"),
            base.join("uploads"),
        ] {
            fs::create_dir_all(d).unwrap();
        }
        for f in ["work/proj/src/a.rs", "work/proj/.env", ".ssh/id_rsa", "Documents/private.txt", ".claude/projects/p/s.jsonl"] {
            fs::write(home.join(f), b"x").unwrap();
        }
        let roots = Roots {
            // HOME itself registered as a project must not open HOME.
            writable: vec![proj.clone(), home.clone(), PathBuf::from("/"), base.join("uploads")],
            read_only: vec![home.join(".claude/projects")],
            export: vec![home.join("Downloads"), base.join("uploads")],
        };
        let fs = RemoteFs::new(roots, Some(&home), SecretPaths::new(Some(&home), None));
        Fx { base, home, proj, fs }
    }

    #[test]
    fn reads_only_inside_the_allowed_roots() {
        let f = fx();
        assert!(f.fs.read(&f.proj.join("src/a.rs")).is_ok());
        assert!(f.fs.read(&f.proj.join(".env")).is_ok());
        assert!(f.fs.read(&f.home.join(".claude/projects/p/s.jsonl")).is_ok());
        for p in [f.home.join("Documents/private.txt"), f.home.join(".ssh/id_rsa"), PathBuf::from("/etc/hosts")] {
            assert!(f.fs.read(&p).unwrap_err().starts_with("permission denied"), "{}", p.display());
        }
        // `..` out of the project.
        assert!(f.fs.read(&f.proj.join("../../Documents/private.txt")).is_err());
        // Read-only roots are not writable.
        assert!(f.fs.write(&f.home.join(".claude/projects/p/new.jsonl")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_a_project_are_followed_then_refused() {
        let f = fx();
        std::os::unix::fs::symlink(f.home.join(".ssh"), f.proj.join("keys")).unwrap();
        std::os::unix::fs::symlink(f.home.join("Documents/private.txt"), f.proj.join("doc.png")).unwrap();
        assert!(f.fs.read(&f.proj.join("keys/id_rsa")).is_err());
        assert!(f.fs.read(&f.proj.join("doc.png")).is_err());
        assert!(f.fs.write(&f.proj.join("keys/new")).is_err());
        // A tree holding such a link cannot be copied.
        assert!(f.fs.tree(&f.proj).is_err());
        fs::remove_file(f.proj.join("keys")).unwrap();
        fs::remove_file(f.proj.join("doc.png")).unwrap();
    }

    #[test]
    fn the_data_volume_alias_of_a_root_is_the_same_root() {
        let f = fx();
        let alias = Path::new("/System/Volumes/Data").join(f.proj.join("src/a.rs").strip_prefix("/").unwrap());
        if alias.exists() {
            // macOS: matched by identity, not by the literal prefix.
            assert!(f.fs.read(&alias).is_ok());
            let doc = Path::new("/System/Volumes/Data").join(f.home.join("Documents/private.txt").strip_prefix("/").unwrap());
            assert!(f.fs.read(&doc).is_err());
        }
    }

    #[test]
    fn writes_never_plant_hooks_or_tool_config() {
        let f = fx();
        assert!(f.fs.write(&f.proj.join("src/new.rs")).is_ok());
        assert!(f.fs.write(&f.proj.join("src/a.rs")).is_ok());
        for rel in [
            ".git/hooks/pre-commit",
            ".git/hooks",
            ".git/config",
            ".git",
            ".claude/settings.json",
            ".claude",
            "src/.claude/settings.local.json",
            ".mcp.json",
            ".codex/config.toml",
            ".vscode/tasks.json",
            ".vscode",
            ".GIT/HOOKS/post-checkout",
        ] {
            assert!(f.fs.write(&f.proj.join(rel)).is_err(), "{rel}");
        }
        // Not under a root at all.
        assert!(f.fs.write(&f.home.join("Documents/x.txt")).is_err());
        assert!(f.fs.write(&f.home.join(".zshrc")).is_err());
    }

    #[test]
    fn copying_a_tree_with_tool_config_is_refused() {
        let f = fx();
        assert!(f.fs.tree(&f.proj.join("src")).is_ok());
        // The project root holds `.git/hooks`.
        assert!(f.fs.tree(&f.proj).is_err());
    }

    #[test]
    fn exports_go_to_downloads_or_uploads_only() {
        let f = fx();
        assert!(f.fs.export(&f.home.join("Downloads/rule.md")).is_ok());
        assert!(f.fs.export(&f.base.join("uploads/s.zip")).is_ok());
        assert!(f.fs.export(&f.proj.join("rule.md")).is_err());
        assert!(f.fs.export(&f.home.join(".zshrc")).is_err());
        assert!(f.fs.export(&f.home.join("Downloads/.claude")).is_err());
    }

    #[test]
    fn local_callers_are_untouched() {
        assert_eq!(read(None, Path::new("/etc/hosts")).unwrap(), PathBuf::from("/etc/hosts"));
        assert_eq!(write(None, Path::new("/x/y")).unwrap(), PathBuf::from("/x/y"));
    }
}
