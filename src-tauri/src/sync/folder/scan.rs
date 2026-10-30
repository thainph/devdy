//! Walk a project folder: find git repos (root + nested), and the non-git
//! files (untracked + ignored) that are not excluded. Never follows
//! symlinks; submodules and linked worktrees (`.git` file) are not entered.

use super::git;
use super::paths::{self, Excludes};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum LocalKind {
    File { size: u64, mode: u32 },
    Symlink { target: String },
}

#[derive(Debug, Clone)]
pub struct LocalFile {
    pub rel: String,
    pub kind: LocalKind,
}

#[derive(Debug, Clone)]
pub struct ScanRepo {
    /// Root-relative path (`""` = the project root).
    pub rel: String,
}

/// Something left out of the sync, with why.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SkipItem {
    pub path: String,
    /// `too_large` | `symlink_outside` | `submodule` | `git_link` | `non_utf8`
    /// | `repo_only_here` | `path_conflict` | `unsafe_path` | `no_repo_here`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

impl SkipItem {
    pub fn new(path: impl Into<String>, reason: &str) -> Self {
        Self { path: path.into(), reason: reason.into(), size: None }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Scan {
    pub repos: Vec<ScanRepo>,
    /// Non-git files and symlinks, sorted by path.
    pub files: Vec<LocalFile>,
    pub skipped: Vec<SkipItem>,
    /// Directories not walked (submodules, linked worktrees): never touched.
    pub blocked: Vec<String>,
}

impl Scan {
    pub fn is_blocked(&self, rel: &str) -> bool {
        self.blocked.iter().any(|b| paths::is_under(rel, b))
    }
}

struct RepoCtx {
    tracked: HashSet<String>,
    gitlinks: HashSet<String>,
}

pub fn scan(root: &Path, ex: &Excludes, max_bytes: u64) -> Result<Scan, String> {
    let mut out = Scan::default();
    if !root.is_dir() {
        return Ok(out);
    }
    let mut repos: Vec<RepoCtx> = Vec::new();
    // (dir rel, innermost repo index)
    let mut stack: Vec<(String, Option<usize>)> = vec![(String::new(), None)];
    while let Some((rel, mut repo)) = stack.pop() {
        let abs = paths::join(root, &rel);
        match fs::symlink_metadata(abs.join(".git")) {
            Ok(m) if m.is_dir() => {
                let mut ctx = RepoCtx { tracked: HashSet::new(), gitlinks: HashSet::new() };
                for (p, gitlink) in git::ls_files(&abs)? {
                    let full = if rel.is_empty() { p } else { format!("{rel}/{p}") };
                    if gitlink {
                        ctx.gitlinks.insert(full);
                    } else {
                        ctx.tracked.insert(full);
                    }
                }
                repos.push(ctx);
                repo = Some(repos.len() - 1);
                out.repos.push(ScanRepo { rel: rel.clone() });
            }
            Ok(_) => {
                out.blocked.push(rel.clone());
                out.skipped.push(SkipItem::new(rel.clone(), "git_link"));
                continue;
            }
            Err(_) => {}
        }
        let ctx = repo.map(|i| &repos[i]);
        let mut entries: Vec<_> = fs::read_dir(&abs)
            .map_err(|e| format!("read {}: {e}", if rel.is_empty() { "." } else { &rel }))?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries.into_iter().rev() {
            let name = e.file_name();
            let Some(name) = name.to_str() else {
                let lossy = name.to_string_lossy();
                out.skipped.push(SkipItem::new(if rel.is_empty() { lossy.to_string() } else { format!("{rel}/{lossy}") }, "non_utf8"));
                continue;
            };
            if name == ".git" {
                continue;
            }
            let crel = if rel.is_empty() { name.to_string() } else { format!("{rel}/{name}") };
            let Ok(ft) = e.file_type() else { continue };
            let tracked = ctx.is_some_and(|c| c.tracked.contains(&crel));
            if ft.is_dir() {
                if ex.is_excluded(&crel, true) {
                    continue;
                }
                if ctx.is_some_and(|c| c.gitlinks.contains(&crel)) {
                    out.blocked.push(crel.clone());
                    out.skipped.push(SkipItem::new(crel, "submodule"));
                    continue;
                }
                stack.push((crel, repo));
            } else if ft.is_symlink() {
                if tracked || ex.is_excluded(&crel, false) {
                    continue;
                }
                let target = paths::read_link(&e.path())?;
                if !paths::symlink_target_ok(&crel, &target) {
                    out.skipped.push(SkipItem::new(crel, "symlink_outside"));
                    continue;
                }
                out.files.push(LocalFile { rel: crel, kind: LocalKind::Symlink { target } });
            } else if ft.is_file() {
                if tracked || ex.is_excluded(&crel, false) {
                    continue;
                }
                let m = e.metadata().map_err(|e| format!("stat {crel}: {e}"))?;
                if m.len() > max_bytes {
                    out.skipped.push(SkipItem { path: crel, reason: "too_large".into(), size: Some(m.len()) });
                    continue;
                }
                out.files.push(LocalFile { rel: crel, kind: LocalKind::File { size: m.len(), mode: paths::mode_of(&m) } });
            }
        }
    }
    out.files.sort_by(|a, b| a.rel.cmp(&b.rel));
    out.repos.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// Lowercase hex sha256 of a file's content.
pub fn sha256_file(p: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = fs::File::open(p).map_err(|e| format!("open: {e}"))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf).map_err(|e| format!("read: {e}"))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}
