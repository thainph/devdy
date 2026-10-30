//! What the source sends: one manifest per project folder (built here, on
//! the source) describing every repo and non-git file.

use super::git::{self, Opts};
use super::paths::{self, Excludes};
use super::scan::{self, LocalKind, SkipItem};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

pub const MANIFEST_VERSION: u32 = 1;
/// Receiver commit tips used as bundle `--not` bases (per repo).
pub const MAX_HAVES: usize = 1000;

/// `folder_manifest` request body.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ManifestRequest {
    pub project_id: String,
    /// Repo path → commit shas the receiver has (tips of its refs).
    #[serde(default)]
    pub repos_have: BTreeMap<String, Vec<String>>,
    /// The receiver's exclude patterns (the source adds the always-excludes).
    #[serde(default)]
    pub excludes: Vec<String>,
    pub max_file_bytes: u64,
    /// Build bundles (apply); a preview only needs the commit counts.
    #[serde(default)]
    pub with_bundles: bool,
    /// The receiver's volume ignores case: match excludes that way.
    #[serde(default)]
    pub case_insensitive: bool,
}

/// A file (or symlink) with its content address. Paths are root-relative.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    #[serde(default)]
    pub size: u64,
    /// Permission bits (`0o644`, `0o755`, ...).
    #[serde(default)]
    pub mode: u32,
    /// Hex sha256 of the content (empty for symlinks / deletions).
    #[serde(default)]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    /// Tracked file deleted from the source's worktree.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Upstream {
    pub remote: String,
    pub merge: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BundleRef {
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RepoManifest {
    /// Root-relative (`""` = the project root).
    pub path: String,
    pub object_format: String,
    /// `None`: unborn branch (no commit yet).
    pub head: Option<String>,
    /// `None`: detached HEAD.
    pub branch: Option<String>,
    pub branches: BTreeMap<String, String>,
    #[serde(default)]
    pub upstreams: BTreeMap<String, Upstream>,
    /// `refs/remotes/...` → sha.
    #[serde(default)]
    pub remote_refs: BTreeMap<String, String>,
    #[serde(default)]
    pub remotes: BTreeMap<String, String>,
    /// base64 of `git diff --cached --binary` (empty: nothing staged).
    #[serde(default)]
    pub index_patch: String,
    /// Tracked files whose worktree differs from HEAD (written / deleted).
    #[serde(default)]
    pub dirty: Vec<FileEntry>,
    /// Commits the receiver lacks.
    #[serde(default)]
    pub commits: u64,
    #[serde(default)]
    pub bundle: Option<BundleRef>,
    /// Receiver tips the source has too.
    #[serde(default)]
    pub known_haves: Vec<String>,
}

impl RepoManifest {
    pub fn patch_bytes(&self) -> Result<Vec<u8>, String> {
        base64::engine::general_purpose::STANDARD.decode(&self.index_patch).map_err(|e| format!("bad index patch: {e}"))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub v: u32,
    pub repos: Vec<RepoManifest>,
    /// Non-git files (untracked + ignored, not excluded).
    pub files: Vec<FileEntry>,
    pub skipped: Vec<SkipItem>,
}

/// A content id: lowercase hex sha256 (it names staging files).
pub fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

impl Manifest {
    /// Reject anything that would leave the receiver's root or name a file
    /// outside the staging area.
    pub fn validate(&self) -> Result<(), String> {
        if self.v != MANIFEST_VERSION {
            return Err(format!("unsupported folder manifest version {}", self.v));
        }
        for r in &self.repos {
            paths::check_repo_rel(&r.path)?;
            for d in &r.dirty {
                paths::check_rel(&d.path)?;
                if !paths::is_under(&d.path, &r.path) {
                    return Err(format!("unsafe path {:?}: outside its repo", d.path));
                }
            }
            if let Some(b) = &r.bundle {
                if !is_sha256(&b.sha256) {
                    return Err(format!("bad content id for the bundle of {:?}", r.path));
                }
            }
        }
        let all = || self.files.iter().chain(self.repos.iter().flat_map(|r| r.dirty.iter()));
        let links: std::collections::HashSet<&str> = all().filter(|f| f.link.is_some()).map(|f| f.path.as_str()).collect();
        let is_link = |p: &str| links.contains(p);
        for f in all() {
            paths::check_rel(&f.path)?;
            let mut prefix = String::new();
            for part in f.path.split('/').collect::<Vec<_>>().split_last().map(|(_, rest)| rest.to_vec()).unwrap_or_default() {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if is_link(&prefix) {
                    return Err(format!("unsafe path {:?}: through a symlink", f.path));
                }
            }
            match &f.link {
                Some(t) => {
                    if !paths::symlink_target_ok_with(&f.path, t, &is_link) {
                        return Err(format!("unsafe symlink {:?}", f.path));
                    }
                }
                None if !f.deleted && !is_sha256(&f.sha256) => return Err(format!("bad content id for {:?}", f.path)),
                None => {}
            }
        }
        Ok(())
    }
}

/// A content the source can upload on `folder_fetch`.
#[derive(Debug, Clone)]
pub struct Content {
    pub path: PathBuf,
    pub size: u64,
}

/// Temporary files (bundles), deleted on drop — also when the request is
/// aborted while the manifest is being built.
#[derive(Default)]
pub struct TempFiles(pub Vec<PathBuf>);

impl Drop for TempFiles {
    fn drop(&mut self) {
        for t in &self.0 {
            let _ = fs::remove_file(t);
        }
    }
}

pub struct Built {
    pub manifest: Manifest,
    pub contents: HashMap<String, Content>,
    /// Bundle files, deleted once no longer served.
    pub temps: TempFiles,
}

pub fn build(root: &Path, req: &ManifestRequest, tmp_dir: &Path) -> Result<Built, String> {
    if !root.is_dir() {
        return Err("project folder is missing on the source device".into());
    }
    let ex = Excludes::new(&req.excludes, req.case_insensitive)?;
    let sc = scan::scan(root, &ex, req.max_file_bytes)?;
    let mut built = Built { manifest: Manifest { v: MANIFEST_VERSION, ..Default::default() }, contents: HashMap::new(), temps: TempFiles::default() };
    // On error `built` (and its bundle files) is dropped.
    for f in &sc.files {
        let entry = local_entry(root, &f.rel, &f.kind)?;
        if entry.link.is_none() {
            built.contents.insert(entry.sha256.clone(), Content { path: root.join(&f.rel), size: entry.size });
        }
        built.manifest.files.push(entry);
    }
    built.manifest.skipped = sc.skipped.clone();
    for r in &sc.repos {
        let rm = repo_manifest(root, &r.rel, req, &ex, tmp_dir, &mut built)?;
        built.manifest.repos.push(rm);
    }
    Ok(built)
}

fn local_entry(root: &Path, rel: &str, kind: &LocalKind) -> Result<FileEntry, String> {
    Ok(match kind {
        LocalKind::Symlink { target } => FileEntry { path: rel.into(), link: Some(target.clone()), ..Default::default() },
        LocalKind::File { size, mode } => FileEntry {
            path: rel.into(),
            size: *size,
            mode: *mode,
            sha256: scan::sha256_file(&root.join(rel)).map_err(|e| format!("{rel}: {e}"))?,
            ..Default::default()
        },
    })
}

fn repo_manifest(root: &Path, rel: &str, req: &ManifestRequest, ex: &Excludes, tmp_dir: &Path, built: &mut Built) -> Result<RepoManifest, String> {
    let dir = paths::join(root, rel);
    let head = git::head(&dir);
    let mut rm = RepoManifest {
        path: rel.to_string(),
        object_format: git::object_format(&dir),
        head: head.clone(),
        branch: git::current_branch(&dir),
        remote_refs: git::remote_refs(&dir)?,
        remotes: git::remotes(&dir)?,
        ..Default::default()
    };
    for (name, (sha, up)) in git::branches(&dir)? {
        // The source's own sync backups stay on the source.
        if name.starts_with(super::backup::BRANCH_PREFIX) {
            continue;
        }
        if let Some((remote, merge)) = up {
            rm.upstreams.insert(name.clone(), Upstream { remote, merge });
        }
        rm.branches.insert(name, sha);
    }

    // Staged state, relative to HEAD (or to nothing on an unborn branch).
    let patch = git::run(
        &dir,
        &[
            "diff", "--cached", "--binary", "--full-index", "--no-renames", "--no-ext-diff", "--no-textconv",
            "--no-color", "--no-relative", "--ignore-submodules=all", "--src-prefix=a/", "--dst-prefix=b/",
        ],
    )?;
    rm.index_patch = base64::engine::general_purpose::STANDARD.encode(&patch);

    // Worktree vs HEAD for tracked paths.
    let base = match &head {
        Some(h) => h.clone(),
        None => git::empty_tree(&dir)?,
    };
    let out = git::run(&dir, &["diff", &base, "--name-status", "-z", "--no-renames", "--no-ext-diff", "--no-relative", "--ignore-submodules=all"])?;
    let mut it = out.split(|b| *b == 0).filter(|s| !s.is_empty());
    while let (Some(status), Some(p)) = (it.next(), it.next()) {
        let p = String::from_utf8_lossy(p).to_string();
        let full = if rel.is_empty() { p } else { format!("{rel}/{p}") };
        // The receiver never touches excluded paths, tracked or not.
        if ex.is_excluded(&full, false) {
            continue;
        }
        let abs = root.join(&full);
        let entry = match (status.first(), fs::symlink_metadata(&abs)) {
            (_, Err(_)) => FileEntry { path: full, deleted: true, ..Default::default() },
            // Removed from the index but still here: an untracked file now,
            // listed with the non-git files.
            (Some(b'D'), Ok(_)) => continue,
            (_, Ok(m)) if m.file_type().is_symlink() => {
                let target = paths::read_link(&abs)?;
                if !paths::symlink_target_ok_with(&full, &target, &|p| paths::is_symlink_at(root, p)) {
                    built.manifest.skipped.push(SkipItem::new(full, "symlink_outside"));
                    continue;
                }
                FileEntry { path: full, link: Some(target), ..Default::default() }
            }
            (_, Ok(m)) if m.is_file() => {
                let e = local_entry(root, &full, &LocalKind::File { size: m.len(), mode: paths::mode_of(&m) })?;
                built.contents.insert(e.sha256.clone(), Content { path: abs, size: e.size });
                e
            }
            _ => continue,
        };
        rm.dirty.push(entry);
    }

    // Commits the receiver lacks: everything reachable from our refs, minus
    // what it already has.
    let mut revs: Vec<String> = Vec::new();
    if head.is_some() {
        revs.push("HEAD".into());
    }
    if !rm.branches.is_empty() {
        revs.push("--branches".into());
    }
    if !rm.remote_refs.is_empty() {
        revs.push("--remotes".into());
    }
    if revs.is_empty() {
        return Ok(rm);
    }
    let mut have = req.repos_have.get(rel).cloned().unwrap_or_default();
    have.retain(|s| s.len() >= 40 && s.chars().all(|c| c.is_ascii_hexdigit()));
    have.sort();
    have.dedup();
    have.truncate(MAX_HAVES);
    let have = git::existing_commits(&dir, &have)?;
    rm.known_haves = have.clone();
    if !have.is_empty() {
        revs.push("--not".into());
        revs.extend(have);
    }
    let mut count_args: Vec<&str> = vec!["rev-list", "--count"];
    count_args.extend(revs.iter().map(String::as_str));
    rm.commits = git::text(&dir, &count_args)?.parse().unwrap_or(0);
    if !req.with_bundles || rm.commits == 0 {
        return Ok(rm);
    }
    fs::create_dir_all(tmp_dir).map_err(|e| format!("temp dir: {e}"))?;
    let file = tmp_dir.join(format!("{}.bundle", uuid::Uuid::new_v4().simple()));
    let file_s = file.to_str().ok_or("temp path is not UTF-8")?.to_string();
    let mut args: Vec<&str> = vec!["bundle", "create", "-q", &file_s];
    args.extend(revs.iter().map(String::as_str));
    let o = git::exec(&dir, &args, Opts::default())?;
    if !o.ok {
        let _ = fs::remove_file(&file);
        if o.stderr.contains("empty bundle") {
            return Ok(rm);
        }
        return Err(format!("git bundle failed: {}", o.stderr.lines().next().unwrap_or("")));
    }
    built.temps.0.push(file.clone());
    let sha = scan::sha256_file(&file)?;
    let size = fs::metadata(&file).map_err(|e| format!("bundle: {e}"))?.len();
    built.contents.insert(sha.clone(), Content { path: file, size });
    rm.bundle = Some(BundleRef { sha256: sha, size });
    Ok(rm)
}
