//! Receiver: compare the source's manifest with this machine's folder
//! (no writes) → what the sync will do, for the preview and the backup.

use super::git;
use super::manifest::{FileEntry, Manifest, RepoManifest};
use super::paths::{self, Excludes};
use super::scan::{self, LocalFile, LocalKind, Scan, SkipItem};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Paths listed per category in the preview (the `*_count` fields are exact).
pub const PREVIEW_PATHS: usize = 2000;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PathSize {
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct RepoPreview {
    /// Root-relative (`""` = the project root).
    pub path: String,
    /// The repo does not exist here yet (created from the bundle).
    pub clone: bool,
    /// Checked-out branch here (`None`: detached / no repo).
    pub branch_from: Option<String>,
    /// The source's checked-out branch (`None`: detached).
    pub branch_to: Option<String>,
    /// The source's HEAD commit.
    pub head: Option<String>,
    /// Commits received from the source.
    pub commits: u64,
    /// Files with staged changes on the source.
    pub staged: u64,
    /// Modified / deleted tracked files on the source.
    pub dirty: u64,
    pub bundle_bytes: u64,
    /// HEAD here.
    pub head_from: Option<String>,
    /// Local branches whose commits leave the branch (kept on backup branches).
    pub branches_rewound: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FolderPreview {
    pub project_id: String,
    /// Project name (filled by the command layer).
    pub name: String,
    pub local_path: String,
    /// The local folder does not exist yet.
    pub create: bool,
    /// Whole project skipped: `no_local_path` | `running_session` | ...
    pub skipped: Option<String>,
    pub repos: Vec<RepoPreview>,
    /// Non-git files (capped at [`PREVIEW_PATHS`] each).
    pub overwrite: Vec<PathSize>,
    pub add: Vec<PathSize>,
    pub delete: Vec<PathSize>,
    pub overwrite_count: u64,
    pub add_count: u64,
    pub delete_count: u64,
    /// This machine's uncommitted tracked changes (root-relative), backed up then replaced.
    pub local_changes: Vec<String>,
    pub too_large: Vec<PathSize>,
    /// Other things left alone (`submodule`, `symlink_outside`, `repo_only_here`, ...).
    pub skipped_items: Vec<SkipItem>,
    /// Contents to download (files + bundles).
    pub download_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn push_capped(v: &mut Vec<PathSize>, n: &mut u64, path: &str, size: u64) {
    *n += 1;
    if v.len() < PREVIEW_PATHS {
        v.push(PathSize { path: path.to_string(), size });
    }
}

/// State of a repo on this machine.
#[derive(Debug, Clone, Default)]
pub struct LocalRepo {
    pub head: Option<String>,
    pub branch: Option<String>,
    pub branches: BTreeMap<String, String>,
    pub remote_refs: BTreeMap<String, String>,
    /// Repo-relative paths with uncommitted tracked changes.
    pub changes: Vec<String>,
}

pub fn local_repo(dir: &Path) -> Result<LocalRepo, String> {
    let out = git::run(dir, &["status", "--porcelain=v1", "-z", "--untracked-files=no", "--no-renames", "--ignore-submodules=all"])?;
    let changes = out
        .split(|b| *b == 0)
        .filter(|r| r.len() > 3)
        .map(|r| String::from_utf8_lossy(&r[3..]).to_string())
        .collect();
    Ok(LocalRepo {
        head: git::head(dir),
        branch: git::current_branch(dir),
        branches: git::branches(dir)?.into_iter().map(|(n, (s, _))| (n, s)).collect(),
        remote_refs: git::remote_refs(dir)?,
        changes,
    })
}

/// Commit tips this machine has, per repo (bundle bases for the source).
pub fn repos_have(root: &Path, sc: &Scan) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    for r in &sc.repos {
        let Ok(l) = local_repo(&paths::join(root, &r.rel)) else { continue };
        let mut v: Vec<String> = l.head.into_iter().chain(l.branches.into_values()).chain(l.remote_refs.into_values()).collect();
        v.sort();
        v.dedup();
        out.insert(r.rel.clone(), v);
    }
    out
}

/// What a receiver-only file looked like when planned; it is deleted only
/// if it still looks the same.
#[derive(Debug, Clone, PartialEq)]
pub struct Stat {
    pub size: u64,
    pub mtime: Option<std::time::SystemTime>,
    pub link: Option<String>,
}

impl Stat {
    fn of(f: &LocalFile) -> Self {
        match &f.kind {
            LocalKind::File { size, .. } => Stat { size: *size, mtime: f.mtime, link: None },
            LocalKind::Symlink { target } => Stat { size: 0, mtime: f.mtime, link: Some(target.clone()) },
        }
    }

    /// The entry at `abs` is still the one planned.
    pub fn unchanged(&self, abs: &Path) -> bool {
        let Ok(m) = fs::symlink_metadata(abs) else { return false };
        match &self.link {
            Some(t) => m.file_type().is_symlink() && paths::read_link(abs).ok().as_deref() == Some(t.as_str()),
            None => m.is_file() && m.len() == self.size && m.modified().ok() == self.mtime,
        }
    }
}

/// The destructive part of a plan, as confirmed in the preview.
#[derive(Debug, Clone, Default)]
pub struct Expected {
    pub deletes: HashSet<String>,
    /// Existing non-git files replaced or deleted (backed up).
    pub replaced: HashSet<String>,
}

/// Everything the apply needs to know besides the manifest.
#[derive(Debug, Default)]
pub struct Plan {
    pub preview: FolderPreview,
    /// Case-insensitive volume.
    pub ci: bool,
    /// Manifest repos to apply (indexes).
    pub repos: Vec<usize>,
    /// Paths (and everything below) that are never touched.
    pub blocked: Vec<String>,
    /// Non-git files of this machine that will be replaced or deleted.
    pub backup_files: Vec<String>,
    /// Receiver-only non-git files (deleted in cleanup if unchanged).
    pub deletes: HashMap<String, Stat>,
    /// Folded paths the source has (or skipped): never deleted.
    pub keep: HashSet<String>,
}

impl Plan {
    pub fn is_blocked(&self, rel: &str) -> bool {
        self.blocked.iter().any(|b| paths::is_under(rel, b))
    }

    /// A planned receiver-only file, still as planned.
    pub fn deletable(&self, root: &Path, rel: &str) -> bool {
        self.deletes.get(rel).is_some_and(|st| st.unchanged(&root.join(rel)))
    }

    pub fn expected(&self) -> Expected {
        Expected { deletes: self.deletes.keys().cloned().collect(), replaced: self.backup_files.iter().cloned().collect() }
    }

    /// Fail when this plan would delete or replace more than `e` (the preview).
    pub fn check_expected(&self, e: &Expected) -> Result<(), String> {
        let more = self.deletes.keys().filter(|d| !e.deletes.contains(*d)).count()
            + self.backup_files.iter().filter(|f| !e.replaced.contains(*f) && !self.deletes.contains_key(*f)).count();
        if more > 0 {
            return Err(format!(
                "plan_changed: the project folder changed since the preview ({more} more file(s) would be replaced or deleted); preview again"
            ));
        }
        Ok(())
    }
}

/// The local file at `rel` already has `want`'s content (and, with
/// `with_mode`, its permission bits).
pub fn identical(root: &Path, rel: &str, want: &FileEntry, with_mode: bool) -> bool {
    let abs = root.join(rel);
    let Ok(m) = fs::symlink_metadata(&abs) else { return false };
    match &want.link {
        Some(t) => m.file_type().is_symlink() && paths::read_link(&abs).ok().as_deref() == Some(t.as_str()),
        None => {
            m.is_file()
                && m.len() == want.size
                && (!with_mode || paths::mode_of(&m) == want.mode)
                && scan::sha256_file(&abs).ok().as_deref() == Some(want.sha256.as_str())
        }
    }
}

/// Files touched by a `git diff --cached` patch.
fn patch_files(r: &RepoManifest) -> u64 {
    let Ok(p) = r.patch_bytes() else { return 0 };
    p.split(|b| *b == b'\n').filter(|l| l.starts_with(b"diff --git ")).count() as u64
}

pub fn plan(root: &Path, project_id: &str, m: &Manifest, sc: &Scan, ex: &Excludes, ci: bool) -> Result<Plan, String> {
    let mut p = Plan { ci, ..Default::default() };
    let fold = |s: &str| paths::fold(s, ci);
    let pv = &mut p.preview;
    pv.project_id = project_id.to_string();
    pv.local_path = root.to_string_lossy().to_string();
    pv.create = !root.is_dir();
    for s in m.skipped.iter().chain(sc.skipped.iter()) {
        if s.reason == "too_large" {
            pv.too_large.push(PathSize { path: s.path.clone(), size: s.size.unwrap_or(0) });
        } else {
            pv.skipped_items.push(s.clone());
        }
    }

    // Never touched: submodules / worktree links here, and repos only this
    // machine has (their files belong to them).
    p.blocked = sc.blocked.clone();
    let source_repos: HashSet<&str> = m.repos.iter().map(|r| r.path.as_str()).collect();
    for r in &sc.repos {
        if !source_repos.contains(r.rel.as_str()) {
            p.blocked.push(r.rel.clone());
            p.preview.skipped_items.push(SkipItem::new(r.rel.clone(), "repo_only_here"));
        }
    }

    let mut need: HashMap<String, u64> = HashMap::new();
    // Sorted by path, so a skipped repo blocks the nested ones after it.
    for (i, r) in m.repos.iter().enumerate() {
        let conflict = !r.path.is_empty()
            && (paths::check_parents(root, &format!("{}/x", r.path)).is_err()
                || fs::symlink_metadata(root.join(&r.path)).is_ok_and(|md| !md.is_dir()));
        if p.is_blocked(&r.path) || conflict {
            p.preview.skipped_items.push(SkipItem::new(r.path.clone(), "path_conflict"));
            p.blocked.push(r.path.clone());
            continue;
        }
        p.repos.push(i);
        let dir = paths::join(root, &r.path);
        let local = if git::is_repo_dir(&dir) { Some(local_repo(&dir)?) } else { None };
        if let Some(l) = &local {
            p.preview.local_changes.extend(l.changes.iter().map(|c| if r.path.is_empty() { c.clone() } else { format!("{}/{c}", r.path) }));
        }
        let (rp, busy) = repo_preview(&dir, r, local.as_ref());
        p.preview.repos.push(rp);
        for b in busy {
            let path = if r.path.is_empty() { b } else { format!("{}:{b}", r.path) };
            p.preview.skipped_items.push(SkipItem::new(path, "branch_in_worktree"));
        }
        if let Some(b) = &r.bundle {
            need.insert(b.sha256.clone(), b.size);
        }
        for d in r.dirty.iter().filter(|d| !d.deleted) {
            p.keep.insert(fold(&d.path));
            if d.link.is_none() && d.size > 0 && !ex.is_excluded(&d.path, false) && !identical(root, &d.path, d, false) {
                need.insert(d.sha256.clone(), d.size);
            }
        }
    }

    let local: HashMap<String, &str> = sc.files.iter().map(|f| (fold(&f.rel), f.rel.as_str())).collect();
    let pv = &mut p.preview;
    for f in &m.files {
        p.keep.insert(fold(&f.path));
        if p.blocked.iter().any(|b| paths::is_under(&f.path, b)) || ex.is_excluded(&f.path, false) {
            continue;
        }
        match fs::symlink_metadata(root.join(&f.path)) {
            Err(_) => push_capped(&mut pv.add, &mut pv.add_count, &f.path, f.size),
            Ok(md) if md.is_dir() => pv.skipped_items.push(SkipItem::new(f.path.clone(), "path_conflict")),
            Ok(_) => {
                if identical(root, &f.path, f, true) {
                    continue;
                }
                push_capped(&mut pv.overwrite, &mut pv.overwrite_count, &f.path, f.size);
                if let Some(l) = local.get(&fold(&f.path)) {
                    p.backup_files.push(l.to_string());
                }
            }
        }
        if f.link.is_none() && f.size > 0 && !identical(root, &f.path, f, false) {
            need.insert(f.sha256.clone(), f.size);
        }
    }
    for s in &m.skipped {
        p.keep.insert(fold(&s.path));
    }
    let dirty_paths: HashSet<String> = m.repos.iter().flat_map(|r| r.dirty.iter().map(|d| fold(&d.path))).collect();
    for f in &sc.files {
        if p.blocked.iter().any(|b| paths::is_under(&f.rel, b)) {
            continue;
        }
        if !p.keep.contains(&fold(&f.rel)) {
            let size = match &f.kind {
                LocalKind::File { size, .. } => *size,
                LocalKind::Symlink { .. } => 0,
            };
            push_capped(&mut pv.delete, &mut pv.delete_count, &f.rel, size);
            p.deletes.insert(f.rel.clone(), Stat::of(f));
            p.backup_files.push(f.rel.clone());
        } else if dirty_paths.contains(&fold(&f.rel)) {
            // Untracked here, tracked (and modified) on the source.
            p.backup_files.push(f.rel.clone());
        }
    }
    p.backup_files.sort();
    p.backup_files.dedup();
    pv.download_bytes = need.values().sum();
    Ok(p)
}

/// The repo's preview, and the source branches checked out in another
/// worktree here (left alone).
fn repo_preview(dir: &Path, r: &RepoManifest, local: Option<&LocalRepo>) -> (RepoPreview, Vec<String>) {
    let mut rp = RepoPreview {
        path: r.path.clone(),
        clone: local.is_none(),
        branch_to: r.branch.clone(),
        head: r.head.clone(),
        commits: r.commits,
        staged: patch_files(r),
        dirty: r.dirty.len() as u64,
        bundle_bytes: r.bundle.as_ref().map(|b| b.size).unwrap_or(0),
        ..Default::default()
    };
    let Some(l) = local else { return (rp, vec![]) };
    let elsewhere = git::branches_checked_out_elsewhere(dir);
    let busy: Vec<String> = r.branches.keys().filter(|b| elsewhere.contains(*b) && l.branches.get(*b) != r.branches.get(*b)).cloned().collect();
    rp.branch_from = l.branch.clone();
    rp.head_from = l.head.clone();
    for (name, sha) in &r.branches {
        match l.branches.get(name) {
            Some(old) if old != sha => {
                let kept = if git::existing_commits(dir, std::slice::from_ref(sha)).is_ok_and(|v| !v.is_empty()) {
                    git::is_ancestor(dir, old, sha)
                } else {
                    r.known_haves.contains(old)
                };
                if !kept {
                    rp.branches_rewound.push(name.clone());
                }
            }
            _ => {}
        }
    }
    (rp, busy)
}
