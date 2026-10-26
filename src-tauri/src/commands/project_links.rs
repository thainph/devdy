//! Symlinks from a project into Devdy's skill/rule library.
//!
//! Skills and rules are installed into projects as symlinks pointing back at the library source,
//! so editing the source updates every project at once. There is deliberately no copy fallback:
//! if a link can't be created the caller gets an error.
//!
//! Projects applied before the switch hold plain copies. A copy is only ever deleted — to swap it
//! for a link, or when the item is removed — if it is provably unedited (`Known` says so); an
//! edited copy is left untouched and reported as an error.
//!
//! Every operation first checks that the folder holding `dest` is a real directory inside the
//! project. If it is reached through a symlink (say `.agents/skills -> ../.claude/skills`), acting
//! on `dest` would actually touch another engine's install — or the library itself.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Decides whether a plain file/dir found at a destination is an unedited Devdy copy.
pub trait Known: Fn(&Path) -> bool {}
impl<F: Fn(&Path) -> bool> Known for F {}

/// Nothing at a destination is ever a known copy (used where Devdy never wrote copies).
pub fn no_copies(_: &Path) -> bool {
    false
}

/// What currently sits at a link destination.
enum Existing {
    Nothing,
    Link(PathBuf),
    Dir,
    File,
}

fn inspect(dest: &Path) -> Result<Existing, String> {
    match fs::symlink_metadata(dest) {
        Ok(meta) if meta.file_type().is_symlink() => {
            let target = fs::read_link(dest).map_err(|e| format!("{}: {}", dest.display(), e))?;
            Ok(Existing::Link(target))
        }
        Ok(meta) if meta.is_dir() => Ok(Existing::Dir),
        Ok(_) => Ok(Existing::File),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Existing::Nothing),
        Err(e) => Err(format!("{}: {}", dest.display(), e)),
    }
}

/// Whether every folder between `root` and `dest` is a real directory (no symlinked component).
/// Folders that don't exist yet count as real — `create_dir_all` will make them so.
fn parent_is_real(root: &Path, dest: &Path) -> Result<bool, String> {
    let Some(parent) = dest.parent() else { return Ok(false) };
    let Ok(rel) = parent.strip_prefix(root) else { return Ok(false) };
    let mut dir = root.to_path_buf();
    for part in rel.components() {
        dir.push(part);
        match fs::symlink_metadata(&dir) {
            Ok(meta) if meta.file_type().is_symlink() => return Ok(false),
            Ok(_) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(true),
            Err(e) => return Err(format!("{}: {}", dir.display(), e)),
        }
    }
    Ok(true)
}

fn require_real_parent(root: &Path, dest: &Path) -> Result<(), String> {
    if parent_is_real(root, dest)? {
        Ok(())
    } else {
        Err(format!(
            "{} is reached through a symlinked folder — Devdy won't manage links there",
            dest.display()
        ))
    }
}

fn edited_copy_error(dest: &Path) -> String {
    format!(
        "{} is a local copy that differs from the Devdy source (edited in the project?) — \
         keep or delete it yourself, then try again",
        dest.display()
    )
}

/// Make `dest` (inside project `root`) a symlink to `target`.
///
/// * nothing there → create the link (parent dirs included);
/// * already a link to `target` → no-op; a dangling link → replaced;
///   a link to anything else → error (not ours to repoint);
/// * a real file/dir (legacy copy) → replaced only if `known(dest)`.
pub fn ensure_symlink(root: &Path, dest: &Path, target: &Path, known: impl Known) -> Result<(), String> {
    if !target.exists() {
        return Err(format!("Source not found: {}", target.display()));
    }
    require_real_parent(root, dest)?;
    match inspect(dest)? {
        Existing::Nothing => {}
        Existing::Link(current) => {
            if current == target {
                return Ok(());
            }
            if dest.exists() {
                return Err(format!(
                    "{} already links to {} — remove it, then apply again",
                    dest.display(),
                    current.display()
                ));
            }
            remove_link(dest).map_err(|e| format!("{}: {}", dest.display(), e))?;
        }
        Existing::Dir | Existing::File => {
            if !known(dest) {
                return Err(edited_copy_error(dest));
            }
            remove_copy(dest)?;
        }
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }
    create_symlink(target, dest)
}

/// Remove what Devdy installed at `dest`: a link to `target` (or a dangling one), or an unedited
/// legacy copy. A link elsewhere is left alone; an edited copy is an error. Missing is fine, and
/// so is a symlinked parent folder (then `dest` isn't ours — nothing is touched).
pub fn remove_installed(root: &Path, dest: &Path, target: &Path, known: impl Known) -> Result<(), String> {
    if !parent_is_real(root, dest)? {
        return Ok(());
    }
    match inspect(dest)? {
        Existing::Nothing => Ok(()),
        Existing::Link(current) => {
            if current == target || !dest.exists() {
                remove_link(dest).map_err(|e| format!("{}: {}", dest.display(), e))
            } else {
                Ok(())
            }
        }
        Existing::Dir | Existing::File => {
            if known(dest) {
                remove_copy(dest)
            } else {
                Err(edited_copy_error(dest))
            }
        }
    }
}

/// True when `dest` is a symlink (dangling or not).
pub fn is_symlink(dest: &Path) -> bool {
    fs::symlink_metadata(dest).is_ok_and(|m| m.file_type().is_symlink())
}

/// Finder metadata files, never part of a skill — ignored when comparing copies.
fn is_os_junk(path: &Path) -> bool {
    path.file_name().is_some_and(|n| n == ".DS_Store")
}

/// True when the legacy copy `copy` holds exactly the files of `source` with the same bytes —
/// nothing edited, added or removed in the project — leaving out the file at relative path `skip`
/// (checked separately by the caller). Used for skill folders, where a SKILL.md hash alone would
/// miss edits to references/ or scripts/.
pub fn same_tree_except(copy: &Path, source: &Path, skip: Option<&Path>) -> bool {
    fn files(dir: &Path, skip: Option<&Path>) -> Option<Vec<PathBuf>> {
        let mut out = Vec::new();
        for entry in walkdir::WalkDir::new(dir).follow_links(false) {
            let entry = entry.ok()?;
            if entry.file_type().is_symlink() {
                return None;
            }
            if entry.file_type().is_file() {
                let rel = entry.path().strip_prefix(dir).ok()?.to_path_buf();
                if Some(rel.as_path()) != skip && !is_os_junk(&rel) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Some(out)
    }
    let (Some(a), Some(b)) = (files(copy, skip), files(source, skip)) else { return false };
    a == b && a.iter().all(|rel| match (fs::read(copy.join(rel)), fs::read(source.join(rel))) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    })
}

/// True when no file in `dir` (other than `reference` itself) was modified after `reference`.
///
/// Copy-era syncs rewrote a skill's whole folder, and `fs::copy` keeps each source file's mtime,
/// so everything Devdy wrote is dated no later than the SKILL.md it wrote in the same pass. A file
/// changed or added in the project after that sync is newer — that is what this catches, while a
/// copy that is merely stale (the source moved on without a sync) passes.
pub fn nothing_newer_than(dir: &Path, reference: &Path) -> bool {
    let Ok(limit) = fs::metadata(reference).and_then(|m| m.modified()) else { return false };
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let Ok(entry) = entry else { return false };
        if entry.file_type().is_symlink() {
            return false;
        }
        if !entry.file_type().is_file() || entry.path() == reference || is_os_junk(entry.path()) {
            continue;
        }
        match entry.metadata().ok().and_then(|m| m.modified().ok()) {
            Some(t) if t <= limit => {}
            _ => return false,
        }
    }
    true
}

/// True when `a` and `b` resolve to the same existing file — e.g. a rename that only changes
/// letter case on a case-insensitive file system.
pub fn same_file(a: &Path, b: &Path) -> bool {
    matches!((fs::canonicalize(a), fs::canonicalize(b)), (Ok(x), Ok(y)) if x == y)
}

fn remove_copy(dest: &Path) -> Result<(), String> {
    let result = if dest.is_dir() { fs::remove_dir_all(dest) } else { fs::remove_file(dest) };
    result.map_err(|e| format!("{}: {}", dest.display(), e))
}

#[cfg(unix)]
fn create_symlink(target: &Path, dest: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, dest)
        .map_err(|e| format!("Cannot create symlink {} → {}: {}", dest.display(), target.display(), e))
}

#[cfg(windows)]
fn create_symlink(target: &Path, dest: &Path) -> Result<(), String> {
    let result = if target.is_dir() {
        std::os::windows::fs::symlink_dir(target, dest)
    } else {
        std::os::windows::fs::symlink_file(target, dest)
    };
    result.map_err(|e| {
        format!(
            "Cannot create symlink {} → {}: {} (Windows needs Developer Mode enabled to create symlinks)",
            dest.display(),
            target.display(),
            e
        )
    })
}

#[cfg(unix)]
fn remove_link(dest: &Path) -> std::io::Result<()> {
    fs::remove_file(dest)
}

#[cfg(windows)]
fn remove_link(dest: &Path) -> std::io::Result<()> {
    // Directory symlinks are directories to the Windows API.
    fs::remove_file(dest).or_else(|_| fs::remove_dir(dest))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-links-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn content_is(expected: &'static str) -> impl Fn(&Path) -> bool {
        move |p| fs::read_to_string(p).is_ok_and(|c| c == expected)
    }

    #[test]
    fn creates_link_and_is_idempotent() {
        let root = tmp("create");
        let src = root.join("src.md");
        fs::write(&src, "a").unwrap();
        let dest = root.join("proj/.claude/rules/x.md");

        ensure_symlink(&root, &dest, &src, no_copies).unwrap();
        ensure_symlink(&root, &dest, &src, no_copies).unwrap();
        assert_eq!(fs::read_link(&dest).unwrap(), src);
        assert_eq!(fs::read_to_string(&dest).unwrap(), "a");
    }

    #[test]
    fn replaces_known_copy_but_keeps_edited_copy() {
        let root = tmp("copy");
        let src = root.join("src.md");
        fs::write(&src, "source").unwrap();

        let known = root.join("known.md");
        fs::write(&known, "synced").unwrap();
        ensure_symlink(&root, &known, &src, content_is("synced")).unwrap();
        assert!(is_symlink(&known));

        let edited = root.join("edited.md");
        fs::write(&edited, "local edit").unwrap();
        let err = ensure_symlink(&root, &edited, &src, content_is("synced")).unwrap_err();
        assert!(err.contains("local copy"), "{err}");
        assert_eq!(fs::read_to_string(&edited).unwrap(), "local edit");

        // Removal follows the same rule.
        assert!(remove_installed(&root, &edited, &src, content_is("synced")).is_err());
        assert!(edited.exists());
    }

    #[test]
    fn refuses_foreign_link_but_replaces_dangling_one() {
        let root = tmp("foreign");
        let src = root.join("src.md");
        let other = root.join("other.md");
        fs::write(&src, "a").unwrap();
        fs::write(&other, "b").unwrap();

        let dest = root.join("dest.md");
        std::os::unix::fs::symlink(&other, &dest).unwrap();
        assert!(ensure_symlink(&root, &dest, &src, no_copies).is_err());
        // Removal leaves a link it doesn't own.
        remove_installed(&root, &dest, &src, no_copies).unwrap();
        assert!(is_symlink(&dest));

        fs::remove_file(&other).unwrap();
        ensure_symlink(&root, &dest, &src, no_copies).unwrap();
        assert_eq!(fs::read_link(&dest).unwrap(), src);
    }

    #[test]
    fn removing_a_dir_link_keeps_the_source() {
        let root = tmp("remove");
        let src = root.join("skill");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "x").unwrap();
        let dest = root.join("proj/.claude/skills/skill");
        ensure_symlink(&root, &dest, &src, no_copies).unwrap();

        remove_installed(&root, &dest, &src, no_copies).unwrap();
        assert!(!is_symlink(&dest));
        assert!(src.join("SKILL.md").exists());
    }

    #[test]
    fn symlinked_parent_folder_is_never_touched() {
        let root = tmp("shared-parent");
        let src = root.join("lib/skill");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("SKILL.md"), "x").unwrap();
        let claude = root.join(".claude/skills/skill");
        ensure_symlink(&root, &claude, &src, no_copies).unwrap();
        // `.agents/skills` is the same folder as `.claude/skills`.
        fs::create_dir_all(root.join(".agents")).unwrap();
        std::os::unix::fs::symlink(root.join(".claude/skills"), root.join(".agents/skills")).unwrap();
        let codex = root.join(".agents/skills/skill");

        assert!(ensure_symlink(&root, &codex, &src, no_copies).is_err());
        remove_installed(&root, &codex, &src, no_copies).unwrap();
        assert!(is_symlink(&claude), "Claude's link survives");
    }

    #[test]
    fn same_tree_spots_edits_outside_skill_md() {
        let root = tmp("tree");
        let src = root.join("src");
        let copy = root.join("copy");
        for d in [&src, &copy] {
            fs::create_dir_all(d.join("references")).unwrap();
            fs::write(d.join("SKILL.md"), "s").unwrap();
            fs::write(d.join("references/a.md"), "ref").unwrap();
        }
        assert!(same_tree_except(&copy, &src, None));
        fs::write(copy.join("references/a.md"), "edited").unwrap();
        assert!(!same_tree_except(&copy, &src, None));
        fs::write(copy.join("references/a.md"), "ref").unwrap();
        fs::write(copy.join("notes.md"), "added").unwrap();
        assert!(!same_tree_except(&copy, &src, None));
    }

    #[test]
    fn nothing_newer_than_catches_later_edits_only() {
        use std::time::{Duration, SystemTime};
        let root = tmp("mtime");
        fs::create_dir_all(root.join("references")).unwrap();
        let set = |p: &Path, secs_ago: u64| {
            fs::File::options()
                .write(true)
                .open(p)
                .unwrap()
                .set_modified(SystemTime::now() - Duration::from_secs(secs_ago))
                .unwrap();
        };
        let md = root.join("SKILL.md");
        let reference = root.join("references/a.md");
        fs::write(&md, "s").unwrap();
        fs::write(&reference, "old ref").unwrap();
        set(&md, 100);
        set(&reference, 1000);
        assert!(nothing_newer_than(&root, &md), "older files from the sync pass are fine");

        fs::write(&reference, "edited ref").unwrap();
        assert!(!nothing_newer_than(&root, &md), "a later edit is caught");
    }

    #[test]
    fn missing_source_is_an_error() {
        let root = tmp("missing");
        let err = ensure_symlink(&root, &root.join("d"), &root.join("nope"), no_copies).unwrap_err();
        assert!(err.contains("Source not found"), "{err}");
    }
}
