//! Path safety: every path coming from the source is a `/`-separated path
//! relative to the project root; nothing may leave the root, go through a
//! symlink, or touch an excluded path.

use super::ALWAYS_EXCLUDES;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Gitignore-style exclude matcher over root-relative paths.
pub struct Excludes(Gitignore);

impl Excludes {
    pub fn new(patterns: &[String]) -> Result<Self, String> {
        let mut b = GitignoreBuilder::new("");
        for p in ALWAYS_EXCLUDES.iter().map(|s| s.to_string()).chain(patterns.iter().cloned()) {
            b.add_line(None, &p).map_err(|e| format!("bad exclude pattern {p:?}: {e}"))?;
        }
        Ok(Self(b.build().map_err(|e| format!("bad exclude patterns: {e}"))?))
    }

    /// `rel` (or one of its parent directories) is excluded.
    pub fn is_excluded(&self, rel: &str, is_dir: bool) -> bool {
        !rel.is_empty() && self.0.matched_path_or_any_parents(rel, is_dir).is_ignore()
    }
}

/// Validate a root-relative path from the other side: non-empty, relative,
/// no `.`/`..`/empty components, no backslashes or NUL, never inside `.git`.
pub fn check_rel(rel: &str) -> Result<(), String> {
    let bad = |why: &str| Err(format!("unsafe path {rel:?}: {why}"));
    if rel.is_empty() {
        return bad("empty");
    }
    if rel.starts_with('/') || Path::new(rel).is_absolute() {
        return bad("absolute");
    }
    if rel.contains('\\') || rel.contains('\0') {
        return bad("invalid character");
    }
    for c in rel.split('/') {
        match c {
            "" | "." | ".." => return bad("invalid component"),
            _ if c.eq_ignore_ascii_case(".git") => return bad("inside .git"),
            _ => {}
        }
    }
    // Belt and braces: the std parser must agree it is plain normal parts.
    if !Path::new(rel).components().all(|c| matches!(c, Component::Normal(_))) {
        return bad("invalid component");
    }
    Ok(())
}

/// Repo paths: `""` is the project root itself.
pub fn check_repo_rel(rel: &str) -> Result<(), String> {
    if rel.is_empty() {
        Ok(())
    } else {
        check_rel(rel)
    }
}

pub fn join(root: &Path, rel: &str) -> PathBuf {
    if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    }
}

/// `child` is `prefix` or below it (`prefix` `""` = everything).
pub fn is_under(child: &str, prefix: &str) -> bool {
    prefix.is_empty() || child == prefix || (child.starts_with(prefix) && child.as_bytes().get(prefix.len()) == Some(&b'/'))
}

/// A symlink at `link_rel` pointing to `target` stays inside the root
/// (relative target, never climbing above the root).
pub fn symlink_target_ok(link_rel: &str, target: &str) -> bool {
    if target.is_empty() || target.starts_with('/') || Path::new(target).is_absolute() || target.contains('\0') {
        return false;
    }
    let mut depth: i64 = link_rel.split('/').count() as i64 - 1;
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => depth += 1,
        }
    }
    true
}

/// Every existing ancestor of `rel` below `root` is a real directory (no
/// symlinks, no files). Missing ancestors are fine.
pub fn check_parents(root: &Path, rel: &str) -> Result<(), String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut p = root.to_path_buf();
    for part in &parts[..parts.len().saturating_sub(1)] {
        p.push(part);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Err(format!("{rel}: a parent is a symlink")),
            Ok(m) if !m.is_dir() => return Err(format!("{rel}: a parent is not a directory")),
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
    Ok(())
}

/// Create the missing ancestors of `rel`. An ancestor that is a file or a
/// symlink is removed only when `removable(ancestor_rel)` says so (a
/// receiver-only file already backed up); otherwise it is an error.
pub fn ensure_parents(root: &Path, rel: &str, removable: &dyn Fn(&str) -> bool) -> Result<(), String> {
    let parts: Vec<&str> = rel.split('/').collect();
    let mut p = root.to_path_buf();
    for (i, part) in parts[..parts.len().saturating_sub(1)].iter().enumerate() {
        p.push(part);
        match fs::symlink_metadata(&p) {
            Ok(m) if m.is_dir() => continue,
            Ok(_) => {
                let anc = parts[..=i].join("/");
                if !removable(&anc) {
                    return Err(format!("{rel}: {anc} is in the way"));
                }
                fs::remove_file(&p).map_err(|e| format!("remove {anc}: {e}"))?;
            }
            Err(_) => {}
        }
        fs::create_dir(&p).map_err(|e| format!("create {}: {e}", parts[..=i].join("/")))?;
    }
    Ok(())
}

#[cfg(unix)]
pub fn mode_of(m: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    m.permissions().mode() & 0o777
}
#[cfg(not(unix))]
pub fn mode_of(m: &fs::Metadata) -> u32 {
    if m.permissions().readonly() {
        0o444
    } else {
        0o644
    }
}

#[cfg(unix)]
pub fn set_mode(p: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode & 0o777)).map_err(|e| format!("chmod: {e}"))
}
#[cfg(not(unix))]
pub fn set_mode(_p: &Path, _mode: u32) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
pub fn make_symlink(target: &str, at: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, at).map_err(|e| format!("symlink: {e}"))
}
#[cfg(not(unix))]
pub fn make_symlink(_target: &str, _at: &Path) -> Result<(), String> {
    Err("symlinks are not supported on this platform".into())
}

pub fn read_link(p: &Path) -> Result<String, String> {
    let t = fs::read_link(p).map_err(|e| format!("readlink: {e}"))?;
    t.to_str().map(|s| s.to_string()).ok_or_else(|| "symlink target is not UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_paths() {
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/./b", "a//b", ".git/config", "x/.GIT/hooks/a", "a\\b", "a/"] {
            assert!(check_rel(bad).is_err(), "{bad:?} should be rejected");
        }
        for ok in ["a", "a/b.txt", ".env", "dir/.gitignore", "..a/b..", ".github/x"] {
            assert!(check_rel(ok).is_ok(), "{ok:?} should be accepted");
        }
    }

    #[test]
    fn symlink_targets_stay_inside() {
        assert!(symlink_target_ok("a/link", "../b"));
        assert!(symlink_target_ok("link", "b/c"));
        assert!(!symlink_target_ok("link", "../b"));
        assert!(!symlink_target_ok("a/link", "../../b"));
        assert!(!symlink_target_ok("a/link", "/etc"));
        assert!(!symlink_target_ok("a/link", "b/../../../c"));
    }

    #[test]
    fn excludes_match_gitignore_style() {
        let ex = Excludes::new(&["node_modules".into(), "*.log".into(), "/top-only".into(), "!keep.log".into()]).unwrap();
        assert!(ex.is_excluded("node_modules", true));
        assert!(ex.is_excluded("web/node_modules/x/y.js", false));
        assert!(ex.is_excluded("a/b.log", false));
        assert!(!ex.is_excluded("keep.log", false));
        assert!(ex.is_excluded("top-only", false));
        assert!(!ex.is_excluded("sub/top-only", false));
        assert!(ex.is_excluded(".devdy/sync-backup/x", false));
        assert!(ex.is_excluded("sub/.git", true));
        assert!(!ex.is_excluded("src/main.rs", false));
    }

    #[test]
    fn under() {
        assert!(is_under("a/b", "a"));
        assert!(is_under("a", "a"));
        assert!(!is_under("ab", "a"));
        assert!(is_under("x", ""));
    }
}
