//! Path safety: every path coming from the source is a `/`-separated path
//! relative to the project root; nothing may leave the root, go through a
//! symlink, or touch an excluded path.

use super::ALWAYS_EXCLUDES;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::fs;
use std::path::{Component, Path, PathBuf};

/// Gitignore-style exclude matcher over root-relative paths.
pub struct Excludes(Gitignore);

/// `.git` / `.devdy` anywhere, in any case (never synced, never written).
fn is_reserved(component: &str) -> bool {
    component.eq_ignore_ascii_case(".git") || component.eq_ignore_ascii_case(".devdy")
}

impl Excludes {
    /// `ci`: match case-insensitively (case-insensitive volumes).
    pub fn new(patterns: &[String], ci: bool) -> Result<Self, String> {
        let mut b = GitignoreBuilder::new("");
        b.case_insensitive(ci).map_err(|e| format!("bad exclude patterns: {e}"))?;
        for p in ALWAYS_EXCLUDES.iter().map(|s| s.to_string()).chain(patterns.iter().cloned()) {
            b.add_line(None, &p).map_err(|e| format!("bad exclude pattern {p:?}: {e}"))?;
        }
        Ok(Self(b.build().map_err(|e| format!("bad exclude patterns: {e}"))?))
    }

    /// `rel` (or one of its parent directories) is excluded.
    pub fn is_excluded(&self, rel: &str, is_dir: bool) -> bool {
        !rel.is_empty() && (rel.split('/').any(is_reserved) || self.0.matched_path_or_any_parents(rel, is_dir).is_ignore())
    }
}

/// Comparison key of a path on this volume: NFC (APFS / HFS+ ignore
/// normalization), lowercased on case-insensitive volumes.
pub fn fold(rel: &str, ci: bool) -> String {
    use unicode_normalization::UnicodeNormalization;
    let n: String = rel.nfc().collect();
    if ci {
        n.to_lowercase()
    } else {
        n
    }
}

/// Whether the volume holding `path` (or its nearest existing ancestor)
/// treats names case-insensitively. Probes an existing entry under another
/// case; without one, assumes the platform default.
pub fn case_insensitive(path: &Path) -> bool {
    let mut p = Some(path);
    while let Some(cur) = p {
        if let (Some(name), Some(parent)) = (cur.file_name().and_then(|n| n.to_str()), cur.parent()) {
            let swapped: String = name.chars().map(|c| if c.is_ascii_lowercase() { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }).collect();
            if swapped != name {
                if let Ok(a) = fs::symlink_metadata(cur) {
                    return same_file(&a, fs::symlink_metadata(parent.join(&swapped)).ok());
                }
            }
        }
        p = cur.parent();
    }
    cfg!(any(target_os = "macos", target_os = "windows"))
}

#[cfg(unix)]
fn same_file(a: &fs::Metadata, b: Option<fs::Metadata>) -> bool {
    use std::os::unix::fs::MetadataExt;
    b.is_some_and(|b| a.dev() == b.dev() && a.ino() == b.ino())
}
#[cfg(not(unix))]
fn same_file(_a: &fs::Metadata, b: Option<fs::Metadata>) -> bool {
    b.is_some()
}

/// Validate a root-relative path from the other side: non-empty, relative,
/// no `.`/`..`/empty components, no backslashes or NUL, never inside
/// `.git` / `.devdy` (any case).
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
            _ if is_reserved(c) => return bad("inside .git or .devdy"),
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
#[cfg(test)]
pub fn symlink_target_ok(link_rel: &str, target: &str) -> bool {
    symlink_target_ok_with(link_rel, target, &|_| false)
}

/// Like [`symlink_target_ok`], also refusing chains: once the target goes
/// through another symlink (`is_link(root-relative path)`), a later `..` is
/// not lexical any more and could leave the root.
pub fn symlink_target_ok_with(link_rel: &str, target: &str, is_link: &dyn Fn(&str) -> bool) -> bool {
    if target.is_empty() || target.starts_with('/') || Path::new(target).is_absolute() || target.contains('\0') {
        return false;
    }
    let mut stack: Vec<&str> = link_rel.split('/').collect();
    stack.pop();
    let mut through_link = false;
    for c in target.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                if through_link || stack.pop().is_none() {
                    return false;
                }
            }
            _ => {
                stack.push(c);
                if !through_link && is_link(&stack.join("/")) {
                    through_link = true;
                }
            }
        }
    }
    true
}

/// `rel` exists below `root` as a symlink.
pub fn is_symlink_at(root: &Path, rel: &str) -> bool {
    fs::symlink_metadata(join(root, rel)).is_ok_and(|m| m.file_type().is_symlink())
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

/// On case-insensitive volumes, renames existing entries whose name differs
/// from the source's spelling only by case / normalization, so the result is
/// spelled like the source (writing `foo.txt` over `Foo.txt` would keep the
/// old name). Directory listings are cached.
pub struct Speller {
    ci: bool,
    cache: std::collections::HashMap<PathBuf, std::collections::HashMap<String, String>>,
}

impl Speller {
    pub fn new(ci: bool) -> Self {
        Self { ci, cache: Default::default() }
    }

    /// Respell the existing components of `rel`. `may_rename(actual_rel)`
    /// guards each rename (never blocked or excluded entries).
    pub fn respell(&mut self, root: &Path, rel: &str, may_rename: &dyn Fn(&str) -> bool) -> Result<(), String> {
        if !self.ci {
            return Ok(());
        }
        let parts: Vec<&str> = rel.split('/').collect();
        let mut dir = root.to_path_buf();
        for (i, part) in parts.iter().enumerate() {
            if !self.cache.contains_key(&dir) {
                let Ok(rd) = fs::read_dir(&dir) else { return Ok(()) };
                let names = rd.filter_map(|e| e.ok()?.file_name().to_str().map(|n| (fold(n, true), n.to_string()))).collect();
                self.cache.insert(dir.clone(), names);
            }
            let names = self.cache.get_mut(&dir).unwrap();
            let Some(actual) = names.get(&fold(part, true)).cloned() else { return Ok(()) };
            if actual != *part {
                let prefix = parts[..i].join("/");
                let actual_rel = if prefix.is_empty() { actual.clone() } else { format!("{prefix}/{actual}") };
                if !may_rename(&actual_rel) {
                    return Err(format!("{rel}: {actual_rel} is spelled differently here"));
                }
                fs::rename(dir.join(&actual), dir.join(part)).map_err(|e| format!("rename {actual_rel}: {e}"))?;
                names.insert(fold(part, true), part.to_string());
                if let Some(sub) = self.cache.remove(&dir.join(&actual)) {
                    self.cache.insert(dir.join(part), sub);
                }
            }
            if i + 1 < parts.len() {
                dir.push(part);
                if !fs::symlink_metadata(&dir).is_ok_and(|m| m.is_dir()) {
                    return Ok(());
                }
            }
        }
        Ok(())
    }
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
        for bad in ["", "/etc/passwd", "../x", "a/../../x", "a/./b", "a//b", ".git/config", "x/.GIT/hooks/a", "a\\b", "a/", ".devdy/x", "a/.DEVDY/sync-tmp/w"] {
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
        let ex = Excludes::new(&["node_modules".into(), "*.log".into(), "/top-only".into(), "!keep.log".into()], false).unwrap();
        assert!(ex.is_excluded("node_modules", true));
        assert!(ex.is_excluded("web/node_modules/x/y.js", false));
        assert!(ex.is_excluded("a/b.log", false));
        assert!(!ex.is_excluded("keep.log", false));
        assert!(ex.is_excluded("top-only", false));
        assert!(!ex.is_excluded("sub/top-only", false));
        assert!(ex.is_excluded(".devdy/sync-backup/x", false));
        assert!(ex.is_excluded("sub/.git", true));
        assert!(!ex.is_excluded("src/main.rs", false));
        assert!(ex.is_excluded("a/.DEVDY/x", false));
        assert!(ex.is_excluded("sub/.Git/config", false));
        assert!(!ex.is_excluded("NODE_MODULES/x", false));
        let ci = Excludes::new(&["node_modules".into(), "conf/.env.local".into()], true).unwrap();
        assert!(ci.is_excluded("NODE_MODULES/x", false));
        assert!(ci.is_excluded("Conf/.ENV.local", false));
    }

    #[test]
    fn symlink_chains_cannot_escape() {
        let links = ["a/l1"];
        let is_link = |p: &str| links.contains(&p);
        // a/l1 -> .. is fine on its own; a/l2 -> l1/.. climbs out through it.
        assert!(symlink_target_ok_with("a/l1", "..", &is_link));
        assert!(symlink_target_ok("a/l2", "l1/.."));
        assert!(!symlink_target_ok_with("a/l2", "l1/..", &is_link));
        assert!(symlink_target_ok_with("a/l3", "l1/x", &is_link));
    }

    #[test]
    fn folding() {
        assert_eq!(fold("Docs/Foo.TXT", true), "docs/foo.txt");
        assert_eq!(fold("Docs/Foo.TXT", false), "Docs/Foo.TXT");
        assert_eq!(fold("cafe\u{301}", false), fold("caf\u{e9}", false));
    }

    #[test]
    fn under() {
        assert!(is_under("a/b", "a"));
        assert!(is_under("a", "a"));
        assert!(!is_under("ab", "a"));
        assert!(is_under("x", ""));
    }
}
