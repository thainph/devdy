//! Backups taken before anything is overwritten (O5):
//! - git: the current worktree + index state committed onto branch
//!   `devdy-sync-backup/<ts>` (parents: HEAD and an index commit) through a
//!   temporary index, so the checked-out branch, index and worktree are not
//!   touched; local branch tips that the sync rewinds go to
//!   `devdy-sync-backup/<ts>-<branch>`.
//! - non-git files that will be replaced or deleted: copied into
//!   `<root>/.devdy/sync-backup/<ts>/`.
//!
//! Only the [`KEEP_BACKUPS`] most recent backups are kept.

use super::git::{self, Opts};
use super::paths;
use super::KEEP_BACKUPS;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const BRANCH_PREFIX: &str = "devdy-sync-backup/";
pub const FOLDER: &str = ".devdy/sync-backup";
/// Length of a timestamp (`20261030-153012-123`).
const TS_LEN: usize = 19;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct BackupInfo {
    pub ts: String,
    /// `devdy-sync-backup/<ts>` exists in these repos (root-relative).
    pub repos: Vec<String>,
    /// Copied non-git files, when any.
    pub folder: Option<PathBuf>,
    /// Repo → (worktree commit, index commit) of the backup, to restore a
    /// repo whose switch failed half-way.
    #[serde(skip)]
    pub commits: std::collections::BTreeMap<String, (String, Option<String>)>,
}

impl BackupInfo {
    pub fn branch(&self) -> String {
        format!("{BRANCH_PREFIX}{}", self.ts)
    }

    /// Where to find it, for error messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.repos.is_empty() {
            let repos: Vec<&str> = self.repos.iter().map(|r| if r.is_empty() { "." } else { r.as_str() }).collect();
            parts.push(format!("git branch {} in {}", self.branch(), repos.join(", ")));
        }
        if let Some(f) = &self.folder {
            parts.push(format!("files in {}", f.display()));
        }
        if parts.is_empty() {
            "nothing needed a backup".into()
        } else {
            parts.join("; ")
        }
    }
}

pub fn new_ts() -> String {
    chrono::Utc::now().format("%Y%m%d-%H%M%S-%3f").to_string()
}

/// Commit the repo's current worktree + index state onto
/// `devdy-sync-backup/<ts>`. Returns (worktree commit, index commit); `None`
/// for a repo without commits and without anything staged or tracked.
/// Edits hidden by assume-unchanged / skip-worktree are included.
pub fn backup_repo(dir: &Path, ts: &str) -> Result<Option<(String, Option<String>)>, String> {
    let head = git::head(dir);
    let gdir = git::git_dir(dir)?;
    let real_index = gdir.join("index");
    let tmp_index = gdir.join(format!("devdy-sync-backup-{}.index", uuid::Uuid::new_v4().simple()));
    if real_index.exists() {
        fs::copy(&real_index, &tmp_index).map_err(|e| format!("backup index: {e}"))?;
    }
    let env = || vec![("GIT_INDEX_FILE", tmp_index.to_string_lossy().to_string())];
    let wt_tree = (|| {
        // `add -u` skips entries flagged assume-unchanged / skip-worktree:
        // clear the flags (temp index only) where the file is here.
        git::clear_hidden_flags(dir, env(), true)?;
        git::run_with(dir, &["add", "-u", "--", "."], Opts { env: env(), ..Default::default() })?;
        git::run_with(dir, &["write-tree"], Opts { env: env(), ..Default::default() })
    })();
    let _ = fs::remove_file(&tmp_index);
    let wt_tree = String::from_utf8_lossy(&wt_tree?).trim().to_string();
    let empty = git::empty_tree(dir)?;
    // The real index as its own commit (impossible with unresolved conflicts).
    let index_tree = git::text(dir, &["write-tree"]).ok();
    if head.is_none() && wt_tree == empty && index_tree.as_deref().is_none_or(|t| t == empty) {
        return Ok(None);
    }
    let branch = git::current_branch(dir).unwrap_or_else(|| "detached HEAD".into());
    let mut parents: Vec<String> = head.iter().cloned().collect();
    let mut index_commit = None;
    if let Some(t) = index_tree {
        let mut args = vec!["commit-tree".to_string(), t];
        for p in &parents {
            args.extend(["-p".to_string(), p.clone()]);
        }
        args.extend(["-m".into(), format!("devdy sync backup: index of {branch}")]);
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let c = git::text(dir, &a)?;
        index_commit = Some(c.clone());
        parents.push(c);
    }
    let mut args = vec!["commit-tree".to_string(), wt_tree];
    for p in &parents {
        args.extend(["-p".to_string(), p.clone()]);
    }
    args.extend(["-m".into(), format!("devdy sync backup: worktree of {branch}")]);
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    let commit = git::text(dir, &a)?;
    git::run(dir, &["update-ref", &format!("refs/heads/{BRANCH_PREFIX}{ts}"), &commit])?;
    Ok(Some((commit, index_commit)))
}

/// Keep `old` (a branch tip about to be rewound) on a backup branch.
pub fn backup_branch_tip(dir: &Path, ts: &str, branch: &str, old: &str) -> Result<(), String> {
    git::run(dir, &["update-ref", &format!("refs/heads/{BRANCH_PREFIX}{ts}-{branch}"), old]).map(|_| ())
}

/// Delete all but the [`KEEP_BACKUPS`] newest backup timestamps.
pub fn prune_repo(dir: &Path) -> Result<(), String> {
    let out = git::run(dir, &["for-each-ref", "--format=%(refname)", &format!("refs/heads/{BRANCH_PREFIX}")])?;
    let refs: Vec<String> = String::from_utf8_lossy(&out).lines().map(|s| s.to_string()).collect();
    let ts_of = |r: &str| r.strip_prefix(&format!("refs/heads/{BRANCH_PREFIX}")).and_then(|s| s.get(..TS_LEN)).map(|s| s.to_string());
    let all: BTreeSet<String> = refs.iter().filter_map(|r| ts_of(r)).collect();
    let drop: BTreeSet<&String> = all.iter().rev().skip(KEEP_BACKUPS).collect();
    let mut input = String::new();
    for r in &refs {
        if ts_of(r).is_some_and(|t| drop.contains(&t)) {
            input.push_str(&format!("delete {r}\n"));
        }
    }
    if !input.is_empty() {
        git::run_with(dir, &["update-ref", "--stdin"], Opts { stdin: Some(input.as_bytes()), ..Default::default() })?;
    }
    Ok(())
}

/// Copy `rels` (root-relative files, symlinks or whole directories, never
/// following symlinks) into `<root>/.devdy/sync-backup/<ts>/`. Returns the
/// folder when anything was copied.
pub fn backup_files(root: &Path, ts: &str, rels: &[String]) -> Result<Option<PathBuf>, String> {
    let dest_root = root.join(FOLDER).join(ts);
    let mut any = false;
    for rel in rels {
        paths::check_rel(rel)?;
        paths::check_parents(root, rel)?;
        let src = root.join(rel);
        let Ok(m) = fs::symlink_metadata(&src) else { continue };
        if m.is_dir() {
            for e in walkdir::WalkDir::new(&src).follow_links(false) {
                let e = e.map_err(|e| format!("backup {rel}: {e}"))?;
                if e.file_type().is_dir() {
                    continue;
                }
                let sub = e.path().strip_prefix(root).map_err(|_| "backup: path outside the root")?;
                copy_one(e.path(), &dest_root.join(sub))?;
                any = true;
            }
        } else {
            copy_one(&src, &dest_root.join(rel))?;
            any = true;
        }
    }
    Ok(any.then_some(dest_root))
}

fn copy_one(src: &Path, dst: &Path) -> Result<(), String> {
    // Already backed up in this run (e.g. both replaced and colliding): the
    // first copy is the state before the sync.
    if fs::symlink_metadata(dst).is_ok() {
        return Ok(());
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("backup dir: {e}"))?;
    }
    let m = fs::symlink_metadata(src).map_err(|e| format!("backup: {e}"))?;
    if m.file_type().is_symlink() {
        paths::make_symlink(&paths::read_link(src)?, dst)
    } else if m.is_file() {
        // A real copy (a clone on APFS), independent of later edits.
        fs::copy(src, dst).map(|_| ()).map_err(|e| format!("backup: {e}"))
    } else {
        Ok(())
    }
}

/// Put `rels` back from `<root>/.devdy/sync-backup/<ts>/` (what
/// [`backup_files`] saved there), replacing whatever is at those paths now.
/// Best effort: returns the paths that could not be restored.
pub fn restore_files(root: &Path, ts: &str, rels: &[String]) -> Vec<String> {
    let from = root.join(FOLDER).join(ts);
    let mut failed = Vec::new();
    for rel in rels {
        let res = (|| -> Result<(), String> {
            paths::check_rel(rel)?;
            let src = from.join(rel);
            if fs::symlink_metadata(&src).is_err() {
                return Ok(());
            }
            paths::ensure_parents(root, rel, &|_| false)?;
            let dst = root.join(rel);
            match fs::symlink_metadata(&dst) {
                Ok(m) if m.is_dir() => fs::remove_dir_all(&dst).map_err(|e| e.to_string())?,
                Ok(_) => fs::remove_file(&dst).map_err(|e| e.to_string())?,
                Err(_) => {}
            }
            for e in walkdir::WalkDir::new(&src).follow_links(false) {
                let e = e.map_err(|e| e.to_string())?;
                let sub = e.path().strip_prefix(&from).map_err(|_| "backup path outside the backup")?;
                if e.file_type().is_dir() {
                    fs::create_dir_all(root.join(sub)).map_err(|e| e.to_string())?;
                } else {
                    copy_one(e.path(), &root.join(sub))?;
                }
            }
            Ok(())
        })();
        if res.is_err() {
            failed.push(rel.clone());
        }
    }
    failed
}

pub fn prune_folder(root: &Path) -> Result<(), String> {
    let dir = root.join(FOLDER);
    let Ok(rd) = fs::read_dir(&dir) else { return Ok(()) };
    let mut names: Vec<String> = rd.filter_map(|e| e.ok()?.file_name().to_str().map(|s| s.to_string())).filter(|n| n.len() == TS_LEN).collect();
    names.sort();
    names.reverse();
    for n in names.into_iter().skip(KEEP_BACKUPS) {
        fs::remove_dir_all(dir.join(n)).map_err(|e| format!("prune backup: {e}"))?;
    }
    Ok(())
}
