//! Symlinks from a project into Devdy's skill/rule library.
//!
//! Skills and rules are installed into projects as symlinks pointing back at the library source,
//! so editing the source updates every project at once. There is deliberately no copy fallback:
//! if a link can't be created the caller gets an error.
//!
//! Projects applied before the switch hold plain copies. A copy that is provably unedited (`Known`
//! says so) is deleted when it is swapped for a link or the item is removed. Anything else is
//! never deleted: it is moved to `<app data>/legacy-backups/<project>/<timestamp>/` first, so the
//! switch-over never blocks and never loses a local edit.
//!
//! Every operation first checks that the folder holding `dest` is a real directory inside the
//! project. If it is reached through a symlink (say `.agents/skills -> ../.claude/skills`), acting
//! on `dest` would actually touch another engine's install — or the library itself.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

static BACKUP_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Set once at startup to `<app data>/legacy-backups`.
pub fn set_backup_root(dir: PathBuf) {
    let _ = BACKUP_ROOT.set(dir);
}

/// Where legacy copies are moved before being replaced or removed.
pub fn backup_root() -> PathBuf {
    BACKUP_ROOT
        .get()
        .cloned()
        .unwrap_or_else(|| std::env::temp_dir().join("devdy-legacy-backups"))
}

/// Backups for one project live under a folder named after its path.
pub fn project_backup_dir(project_root: &Path) -> PathBuf {
    let key: String = project_root
        .to_string_lossy()
        .trim_start_matches(['/', '\\'])
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' })
        .collect();
    backup_root().join(key)
}

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

/// Move a copy Devdy can't vouch for out of the project, keeping its path relative to `root`.
fn back_up(root: &Path, dest: &Path) -> Result<PathBuf, String> {
    let rel = dest.strip_prefix(root).unwrap_or(dest);
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
    let base = project_backup_dir(root);
    let mut target = base.join(&stamp).join(rel);
    let mut n = 1;
    while target.exists() {
        target = base.join(format!("{}-{}", stamp, n)).join(rel);
        n += 1;
    }
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }
    if fs::rename(dest, &target).is_err() {
        // Different volume: copy, then delete the original.
        copy_tree(dest, &target)?;
        remove_copy(dest)?;
    }
    tracing::info!("moved legacy copy {} to {}", dest.display(), target.display());
    Ok(target)
}

fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_file() {
        return fs::copy(from, to).map(|_| ()).map_err(|e| format!("{}: {}", to.display(), e));
    }
    for entry in walkdir::WalkDir::new(from).follow_links(false) {
        let entry = entry.map_err(|e| e.to_string())?;
        let out = to.join(entry.path().strip_prefix(from).map_err(|e| e.to_string())?);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&out).map_err(|e| format!("{}: {}", out.display(), e))?;
        } else if entry.file_type().is_file() {
            fs::copy(entry.path(), &out).map_err(|e| format!("{}: {}", out.display(), e))?;
        }
    }
    Ok(())
}

/// Clear a legacy copy at `dest`: delete it when `known`, otherwise move it to the backups.
fn clear_copy(root: &Path, dest: &Path, known: impl Known) -> Result<(), String> {
    if known(dest) {
        remove_copy(dest)
    } else {
        back_up(root, dest).map(|_| ())
    }
}

/// Make `dest` (inside project `root`) a symlink to `target`.
///
/// * nothing there → create the link (parent dirs included);
/// * already a link to `target` → no-op; a dangling link → replaced;
///   a link to anything else → error (not ours to repoint);
/// * a real file/dir (legacy copy) → deleted if `known(dest)`, else moved to the backups.
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
        Existing::Dir | Existing::File => clear_copy(root, dest, known)?,
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {}", parent.display(), e))?;
    }
    create_symlink(target, dest)
}

/// Remove what Devdy installed at `dest`: a link to `target` (or a dangling one), or a legacy copy
/// (deleted if `known`, else moved to the backups). A link elsewhere is left alone. Missing is
/// fine, and so is a symlinked parent folder (then `dest` isn't ours — nothing is touched).
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
        Existing::Dir | Existing::File => clear_copy(root, dest, known),
    }
}

/// True when `dest` is a symlink (dangling or not).
#[cfg(test)]
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

/// Slack for files written in the same sync pass as the copy's SKILL.md.
const SYNC_PASS_SLACK: std::time::Duration = std::time::Duration::from_secs(2);

/// True when no file in `dir` (other than `reference` itself) was written after `reference`.
///
/// Copy-era syncs rewrote a skill's whole folder in one pass, SKILL.md included, so every file
/// Devdy wrote has a change time (ctime) at or before that pass — leftovers from older syncs are
/// earlier still. A file edited or added in the project afterwards has a later ctime. ctime is
/// used, not mtime: `fs::copy` keeps the source's mtime, which says nothing about when the copy
/// was made. (No ctime on Windows; mtime is the best available there.)
pub fn untouched_since(dir: &Path, reference: &Path) -> bool {
    untouched_since_within(dir, reference, SYNC_PASS_SLACK)
}

fn untouched_since_within(dir: &Path, reference: &Path, slack: std::time::Duration) -> bool {
    let Some(limit) = fs::metadata(reference).ok().and_then(|m| written_at(&m)) else { return false };
    let limit = limit + slack;
    for entry in walkdir::WalkDir::new(dir).follow_links(false) {
        let Ok(entry) = entry else { return false };
        if entry.file_type().is_symlink() {
            return false;
        }
        if !entry.file_type().is_file() || entry.path() == reference || is_os_junk(entry.path()) {
            continue;
        }
        match entry.metadata().ok().and_then(|m| written_at(&m)) {
            Some(t) if t <= limit => {}
            _ => return false,
        }
    }
    true
}

#[cfg(unix)]
fn written_at(meta: &fs::Metadata) -> Option<std::time::SystemTime> {
    use std::os::unix::fs::MetadataExt;
    let secs = u64::try_from(meta.ctime()).ok()?;
    let nanos = u32::try_from(meta.ctime_nsec()).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::new(secs, nanos))
}

#[cfg(not(unix))]
fn written_at(meta: &fs::Metadata) -> Option<std::time::SystemTime> {
    meta.modified().ok()
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

        // An unrecognised copy is moved to the backups, then linked.
        let edited = root.join("edited.md");
        fs::write(&edited, "local edit").unwrap();
        ensure_symlink(&root, &edited, &src, content_is("synced")).unwrap();
        assert!(is_symlink(&edited));
        assert_eq!(backed_up(&root, "edited.md"), vec!["local edit".to_string()]);

        // Removal follows the same rule.
        let removed = root.join("removed.md");
        fs::write(&removed, "another edit").unwrap();
        remove_installed(&root, &removed, &src, content_is("synced")).unwrap();
        assert!(!removed.exists());
        assert_eq!(backed_up(&root, "removed.md"), vec!["another edit".to_string()]);
    }

    /// Contents of every backup of `rel` taken for project `root`.
    fn backed_up(root: &Path, rel: &str) -> Vec<String> {
        let Ok(stamps) = fs::read_dir(project_backup_dir(root)) else { return Vec::new() };
        stamps
            .flatten()
            .filter_map(|e| fs::read_to_string(e.path().join(rel)).ok())
            .collect()
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
    fn untouched_since_catches_later_writes_only() {
        let root = tmp("ctime");
        fs::create_dir_all(root.join("references")).unwrap();
        let md = root.join("SKILL.md");
        let reference = root.join("references/a.md");
        // A leftover from an older sync, then the pass that wrote SKILL.md.
        fs::write(&reference, "old ref").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&md, "s").unwrap();
        let none = std::time::Duration::ZERO;
        assert!(untouched_since_within(&root, &md, none), "older files are fine");

        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(&reference, "edited ref").unwrap();
        assert!(!untouched_since_within(&root, &md, none), "a later write is caught");
        assert!(untouched_since(&root, &md), "within the sync-pass slack it still counts as one pass");
    }

    #[test]
    fn missing_source_is_an_error() {
        let root = tmp("missing");
        let err = ensure_symlink(&root, &root.join("d"), &root.join("nope"), no_copies).unwrap_err();
        assert!(err.contains("Source not found"), "{err}");
    }
}
