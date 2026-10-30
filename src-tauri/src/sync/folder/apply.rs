//! Receiver: preview (no writes) and apply (backup → git → files → cleanup).
//!
//! Git LFS: the receiver never downloads LFS objects (no network), so the
//! git phase runs with `GIT_LFS_SKIP_SMUDGE`. Real contents this worktree
//! already has for the new pointers are kept; other LFS paths stay pointer
//! files and are listed as `lfs`.

use super::backup::{self, BackupInfo};
use super::git::{self, Opts};
use super::manifest::{is_sha256, FileEntry, Manifest, ManifestRequest, RepoManifest};
use super::paths::{self, Excludes, Speller};
use super::plan::{self, Expected, FolderPreview, Plan};
use super::scan::{self, SkipItem};
use super::transfer::{self, Downloader};
use super::{blocking, Cancel, FolderOptions, FolderProgress, ProgressSink};
use crate::sync::peer::PeerLink;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Staging area for downloads and atomic writes (always excluded).
pub const STAGING: &str = ".devdy/sync-tmp";
/// Sha256 of empty content.
const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ApplyReport {
    pub backup: BackupInfo,
    pub files_written: u64,
    pub files_deleted: u64,
    pub bytes_downloaded: u64,
    pub skipped: Vec<SkipItem>,
}

/// Re-checked right before the first destructive step (a session of the
/// project may have started since the preview).
pub type Precheck = Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

#[derive(Clone, Default)]
pub struct ApplyGuard {
    /// What the user confirmed in the preview: abort when more would be
    /// deleted or replaced now.
    pub expected: Option<Expected>,
    pub precheck: Option<Precheck>,
}

/// Local root as an absolute path (resolved when it exists).
fn resolve_root(opts: &FolderOptions) -> Result<PathBuf, String> {
    let r = &opts.local_root;
    if !r.is_absolute() {
        return Err("the local project path must be absolute".into());
    }
    Ok(r.canonicalize().unwrap_or_else(|_| r.clone()))
}

struct Local {
    root: PathBuf,
    ci: bool,
    ex: Arc<Excludes>,
    scan: scan::Scan,
}

async fn local_state(opts: &FolderOptions) -> Result<Local, String> {
    let root = resolve_root(opts)?;
    let ci = paths::case_insensitive(&root);
    let ex = Arc::new(Excludes::new(&opts.patterns(), ci)?);
    let (r2, e2) = (root.clone(), ex.clone());
    // Everything here counts, whatever its size (receiver-only files are deleted).
    let scan = blocking(move || scan::scan(&r2, &e2, u64::MAX)).await?;
    Ok(Local { root, ci, ex, scan })
}

async fn fetch_manifest(link: &Arc<dyn PeerLink>, source: &str, opts: &FolderOptions, local: &Local) -> Result<Manifest, String> {
    let (root, sc) = (local.root.clone(), local.scan.clone());
    let have = blocking(move || Ok(plan::repos_have(&root, &sc))).await?;
    let req = ManifestRequest {
        project_id: opts.project_id.clone(),
        repos_have: have,
        excludes: opts.patterns(),
        max_file_bytes: opts.max_file_bytes(),
        with_bundles: true,
        case_insensitive: local.ci,
    };
    transfer::request_manifest(link, source, &req).await
}

async fn make_plan(opts: &FolderOptions, local: &Local, m: Manifest) -> Result<(Manifest, Plan), String> {
    let (root, ex, pid, ci) = (local.root.clone(), local.ex.clone(), opts.project_id.clone(), local.ci);
    let sc = local.scan.clone();
    blocking(move || {
        let p = plan::plan(&root, &pid, &m, &sc, &ex, ci)?;
        Ok((m, p))
    })
    .await
}

/// What a sync from `source` would do to this project folder. No writes.
#[cfg(test)]
pub async fn preview(link: Arc<dyn PeerLink>, source: &str, opts: &FolderOptions) -> Result<FolderPreview, String> {
    Ok(preview_expected(link, source, opts).await?.0)
}

/// The preview, and what it commits to (give it back to [`apply`] through
/// [`ApplyGuard::expected`]).
pub async fn preview_expected(link: Arc<dyn PeerLink>, source: &str, opts: &FolderOptions) -> Result<(FolderPreview, Expected), String> {
    let local = local_state(opts).await?;
    let m = fetch_manifest(&link, source, opts, &local).await?;
    // Bundles were built for their size only; apply asks for a fresh manifest.
    let (_, p) = make_plan(opts, &local, m).await?;
    let e = p.expected();
    Ok((p.preview, e))
}

struct Emitter {
    sink: ProgressSink,
    base: FolderProgress,
}

impl Emitter {
    fn phase(&mut self, phase: &str) {
        self.base.phase = phase.into();
        self.base.done_bytes = 0;
        self.base.total_bytes = 0;
        self.base.files_done = 0;
        self.base.files_total = 0;
        self.base.message = None;
        (self.sink)(&self.base);
    }
    fn emit(&self) {
        (self.sink)(&self.base);
    }
}

/// Make the project folder identical to the source's. Emits progress for
/// every phase except the final `done` / `error` (the caller's).
pub async fn apply(
    link: Arc<dyn PeerLink>,
    source: &str,
    opts: &FolderOptions,
    sink: ProgressSink,
    cancel: Cancel,
    guard: ApplyGuard,
) -> Result<ApplyReport, String> {
    let mut em = Emitter { sink, base: FolderProgress { project_id: opts.project_id.clone(), ..Default::default() } };
    em.phase("manifest");
    let local = local_state(opts).await?;
    let m = fetch_manifest(&link, source, opts, &local).await?;
    let (m, plan) = make_plan(opts, &local, m).await?;
    if let Some(e) = &guard.expected {
        plan.check_expected(e)?;
    }
    let (m, plan) = (Arc::new(m), Arc::new(plan));
    let root = local.root.clone();
    cancel.check()?;

    em.phase("backup");
    let info = {
        let (root, m, plan) = (root.clone(), m.clone(), plan.clone());
        blocking(move || take_backup(&root, &m, &plan)).await?
    };
    let mut report = ApplyReport { backup: info, ..Default::default() };

    let res = async {
        if let Some(pc) = &guard.precheck {
            pc().await?;
        }
        run_after_backup(&link, source, &local, &root, m, plan, &mut em, &cancel, &mut report).await
    }
    .await;
    let _ = fs::remove_dir_all(root.join(STAGING));
    if let Err(e) = res {
        // The git phase may have added files to the backup since.
        note_folder(&mut report, &root);
        return Err(format!("{e} (backup: {})", report.backup.describe()));
    }
    Ok(report)
}

fn take_backup(root: &Path, m: &Manifest, plan: &Plan) -> Result<BackupInfo, String> {
    let ts = backup::new_ts();
    let mut info = BackupInfo { ts: ts.clone(), ..Default::default() };
    fs::create_dir_all(root).map_err(|e| format!("create project folder: {e}"))?;
    for &i in &plan.repos {
        let rel = &m.repos[i].path;
        let dir = paths::join(root, rel);
        if !git::is_repo_dir(&dir) {
            continue;
        }
        if let Some(commits) = backup::backup_repo(&dir, &ts)? {
            info.repos.push(rel.clone());
            info.commits.insert(rel.clone(), commits);
            backup::prune_repo(&dir)?;
        }
    }
    info.folder = backup::backup_files(root, &ts, &plan.backup_files)?;
    backup::prune_folder(root)?;
    Ok(info)
}

/// Adds the backup folder to `info` when the git phase backed up more files.
fn note_folder(report: &mut ApplyReport, root: &Path) {
    let f = root.join(backup::FOLDER).join(&report.backup.ts);
    if report.backup.folder.is_none() && f.is_dir() {
        report.backup.folder = Some(f);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_after_backup(
    link: &Arc<dyn PeerLink>,
    source: &str,
    local: &Local,
    root: &Path,
    m: Arc<Manifest>,
    plan: Arc<Plan>,
    em: &mut Emitter,
    cancel: &Cancel,
    report: &mut ApplyReport,
) -> Result<(), String> {
    cancel.check()?;
    let project_id = em.base.project_id.clone();
    let staging = root.join(STAGING);
    let _ = fs::remove_dir_all(&staging);
    let mut dl = Downloader::new(&staging)?;

    // The git phase reverts dirty files: keep the ones that already have
    // the source's content so they are not downloaded again.
    {
        let (root2, m2, plan2, st) = (root.to_path_buf(), m.clone(), plan.clone(), staging.clone());
        blocking(move || preserve_dirty(&root2, &m2, &plan2, &st)).await?;
    }

    // ---- git
    em.phase("git");
    let bundles: Vec<(String, u64)> = plan.repos.iter().filter_map(|&i| m.repos[i].bundle.as_ref().map(|b| (b.sha256.clone(), b.size))).collect();
    em.base.total_bytes = bundles.iter().map(|(_, n)| n).sum();
    em.base.files_total = plan.repos.len() as u64;
    em.emit();
    {
        let sink = em.sink.clone();
        let base = Arc::new(Mutex::new(em.base.clone()));
        let b2 = base.clone();
        let mut on_progress = move |n: u64| {
            let mut b = b2.lock().unwrap();
            b.done_bytes += n;
            sink(&b);
        };
        dl.fetch(link, source, &project_id, &bundles, cancel, &mut on_progress, &mut |_, _| Ok(())).await?;
        report.bytes_downloaded += base.lock().unwrap().done_bytes;
        em.base.done_bytes = em.base.total_bytes;
    }
    let mut skipped_repos: Vec<String> = Vec::new();
    for &i in &plan.repos {
        cancel.check()?;
        let r = m.repos[i].clone();
        let bundle = match &r.bundle {
            Some(b) => Some(dl.done_path(&b.sha256)?),
            None => None,
        };
        let cx = RepoCtx {
            root: root.to_path_buf(),
            ts: report.backup.ts.clone(),
            staging: staging.clone(),
            blocked: plan.blocked.clone(),
            backup: report.backup.commits.get(&r.path).cloned(),
        };
        let path = r.path.clone();
        let res = blocking(move || apply_repo(&cx, &r, bundle.as_deref())).await;
        note_folder(report, root);
        let out = res?;
        if !out.applied {
            skipped_repos.push(path);
        }
        report.skipped.extend(out.skips);
        em.base.files_done += 1;
        em.emit();
    }

    // A repo left alone: its files are not touched either (nested repos are).
    let plan = if skipped_repos.is_empty() {
        plan
    } else {
        let mut p = (*plan).clone();
        p.skipped_repos = skipped_repos;
        Arc::new(p)
    };

    // ---- files (non-git + the source's dirty tracked files)
    em.phase("files");
    cancel.check()?;
    let speller = Arc::new(Mutex::new(Speller::new(local.ci)));
    let (root2, m2, plan2, ex2, sp2) = (root.to_path_buf(), m.clone(), plan.clone(), local.ex.clone(), speller.clone());
    let todo = blocking(move || files_todo(&root2, &m2, &plan2, &ex2, &mut sp2.lock().unwrap())).await?;
    report.skipped.extend(todo.skipped.iter().cloned());
    em.base.files_total = todo.local.len() as u64 + todo.dests.values().map(|v| v.len() as u64).sum::<u64>() + todo.deletes.len() as u64;
    em.base.total_bytes = todo.wanted.iter().map(|(_, n)| n).sum();
    em.emit();
    let links = Arc::new(todo.links.clone());
    for (rel, kind) in &todo.local {
        cancel.check()?;
        match place_local(root, rel, kind, &plan, &links) {
            Ok(()) => report.files_written += 1,
            Err(Conflict::Skip(s)) => report.skipped.push(s),
            Err(Conflict::Fail(e)) => return Err(e),
        }
        em.base.files_done += 1;
    }
    for rel in &todo.deletes {
        cancel.check()?;
        em.base.files_done += 1;
        if paths::check_parents(root, rel).is_err() {
            report.skipped.push(SkipItem::new(rel.clone(), "path_conflict"));
            continue;
        }
        let abs = root.join(rel);
        if fs::symlink_metadata(&abs).is_ok_and(|md| !md.is_dir()) {
            fs::remove_file(&abs).map_err(|e| format!("delete {rel}: {e}"))?;
            report.files_deleted += 1;
        }
    }
    em.emit();
    {
        let state = Arc::new(Mutex::new((em.base.clone(), 0u64, Vec::<SkipItem>::new())));
        let (s1, s2) = (state.clone(), state.clone());
        let sink = em.sink.clone();
        let sink2 = sink.clone();
        let mut on_progress = move |n: u64| {
            let mut st = s1.lock().unwrap();
            st.0.done_bytes += n;
            sink(&st.0);
        };
        let dests = todo.dests.clone();
        let root2 = root.to_path_buf();
        let plan2 = plan.clone();
        let mut on_done = move |sha: &str, staged: &Path| -> Result<(), String> {
            let mut st = s2.lock().unwrap();
            for (rel, mode) in dests.get(sha).into_iter().flatten() {
                match place_file(&root2, rel, staged, *mode, &plan2) {
                    Ok(()) => st.1 += 1,
                    Err(Conflict::Skip(s)) => st.2.push(s),
                    Err(Conflict::Fail(e)) => return Err(e),
                }
                st.0.files_done += 1;
            }
            let _ = fs::remove_file(staged);
            sink2(&st.0);
            Ok(())
        };
        // Contents kept from before the git phase need no download.
        let mut wanted = Vec::new();
        for (sha, size) in &todo.wanted {
            let kept = dl.done_path(sha)?;
            if kept.is_file() {
                cancel.check()?;
                on_done(sha, &kept)?;
            } else {
                wanted.push((sha.clone(), *size));
            }
        }
        let res = dl.fetch(link, source, &project_id, &wanted, cancel, &mut on_progress, &mut on_done).await;
        let st = state.lock().unwrap();
        report.files_written += st.1;
        report.bytes_downloaded += st.0.done_bytes;
        report.skipped.extend(st.2.iter().cloned());
        em.base = st.0.clone();
        res?;
    }

    // ---- cleanup: the receiver-only files planned (and still unchanged)
    em.phase("cleanup");
    cancel.check()?;
    let (root2, m2, plan2, ex2) = (root.to_path_buf(), m.clone(), plan.clone(), local.ex.clone());
    let (n, skips) = blocking(move || cleanup(&root2, &m2, &plan2, &ex2)).await?;
    report.files_deleted += n;
    report.skipped.extend(skips);
    Ok(())
}

fn valid_ref_part(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control()) && !s.starts_with('-')
}

/// What one repo's git step needs besides the manifest.
pub struct RepoCtx {
    pub root: PathBuf,
    pub ts: String,
    pub staging: PathBuf,
    /// Never touched (submodules, repos only this machine has, ...).
    pub blocked: Vec<String>,
    /// (worktree commit, index commit) of this repo's backup.
    pub backup: Option<(String, Option<String>)>,
}

/// Result of one repo's git step.
pub struct RepoOutcome {
    /// `false`: left alone as a whole (see `skips`).
    pub applied: bool,
    pub skips: Vec<SkipItem>,
}

/// Make one repo match the source (objects come from `bundle`). A repo
/// whose new tree would write into a blocked path (`path_conflict`) or whose
/// refs cannot be set here (`ref_conflict`) is left alone, unchanged.
pub fn apply_repo(cx: &RepoCtx, r: &RepoManifest, bundle: Option<&Path>) -> Result<RepoOutcome, String> {
    let root = &cx.root;
    let label = if r.path.is_empty() { "." } else { r.path.as_str() };
    let ctx = |e: String| format!("{label}: {e}");
    let full = |p: &str| if r.path.is_empty() { p.to_string() } else { format!("{}/{p}", r.path) };
    let dir = paths::join(root, &r.path);
    if !r.path.is_empty() {
        paths::ensure_parents(root, &format!("{}/x", r.path), &|_| false).map_err(ctx)?;
    }
    if !git::is_repo_dir(&dir) {
        if fs::symlink_metadata(dir.join(".git")).is_ok() {
            return Err(ctx("not a plain git repo here".into()));
        }
        let mut args = vec!["init", "-q"];
        let fmt = format!("--object-format={}", r.object_format);
        if !r.object_format.is_empty() && r.object_format != "sha1" {
            args.push(&fmt);
        }
        git::run(&dir, &args).map_err(ctx)?;
    }

    for (name, sha) in r.branches.iter().map(|(n, s)| (format!("refs/heads/{n}"), s)).chain(r.remote_refs.iter().map(|(n, s)| (n.clone(), s))) {
        if !valid_ref_part(&name) || !(name.starts_with("refs/heads/") || name.starts_with("refs/remotes/")) || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(ctx(format!("invalid ref {name:?}")));
        }
    }
    if let Some(b) = &r.branch {
        if !valid_ref_part(b) {
            return Err(ctx(format!("invalid branch {b:?}")));
        }
    }
    if r.head.as_ref().is_some_and(|h| !h.chars().all(|c| c.is_ascii_hexdigit())) {
        return Err(ctx("invalid HEAD".into()));
    }

    if let Some(b) = bundle {
        let b = b.to_str().ok_or("bundle path is not UTF-8")?;
        let heads = git::text(&dir, &["bundle", "list-heads", b]).map_err(ctx)?;
        // A bundle is read through the local file transport; nothing else is allowed.
        let mut args = vec!["-c", "protocol.file.allow=always", "fetch", "-q", "--no-tags", b, "+refs/heads/*:refs/devdy-sync/heads/*", "+refs/remotes/*:refs/devdy-sync/remotes/*"];
        if heads.lines().any(|l| l.ends_with(" HEAD")) {
            args.push("+HEAD:refs/devdy-sync/HEAD");
        }
        git::run(&dir, &args).map_err(ctx)?;
    }
    let mut wanted: Vec<String> = r.branches.values().chain(r.remote_refs.values()).chain(r.head.iter()).cloned().collect();
    wanted.sort();
    wanted.dedup();
    let present: HashSet<String> = git::existing_commits(&dir, &wanted).map_err(ctx)?.into_iter().collect();
    if let Some(miss) = wanted.iter().find(|s| !present.contains(*s)) {
        return Err(ctx(format!("commit {} missing after the bundle fetch", &miss[..miss.len().min(12)])));
    }

    let target = match &r.head {
        Some(h) => h.clone(),
        None => git::empty_tree(&dir).map_err(ctx)?,
    };
    let tree = git::tree_paths(&dir, &target).map_err(ctx)?;
    let gitlinks = git::tree_gitlinks(&dir, &target).map_err(ctx)?;
    let skip_repo = |reason: &str| {
        drop_tmp_refs(&dir);
        Ok(RepoOutcome { applied: false, skips: vec![SkipItem::new(r.path.clone(), reason)] })
    };
    // The new tree must not write into a blocked path (a repo only this
    // machine has, ...); a submodule's own gitlink sitting on its directory
    // is fine (git leaves the directory alone).
    let into_blocked = |p: &String| {
        let fp = full(p);
        cx.blocked.iter().any(|b| paths::is_under(&fp, b) && !(gitlinks.contains(p) && fp == *b))
    };
    if tree.iter().any(into_blocked) {
        return skip_repo("path_conflict");
    }

    // State to put back if the switch fails half-way.
    let old_sym = git::exec(&dir, &["symbolic-ref", "-q", "HEAD"], Opts::default())
        .ok()
        .filter(|o| o.ok)
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    let branches = git::branches(&dir).map_err(ctx)?;
    let old = Old {
        sym: old_sym,
        head: git::head(&dir),
        upstreams: branches.iter().filter_map(|(n, (_, up))| up.clone().map(|u| (n.clone(), u))).collect(),
        branches: branches.into_iter().map(|(n, (s, _))| (n, s)).collect(),
        remote_refs: git::remote_refs(&dir).map_err(ctx)?,
        remotes: git::remotes(&dir).map_err(ctx)?,
    };
    let elsewhere = git::branches_checked_out_elsewhere(&dir);
    // Refs that cannot be set here (`feature` vs `feature/x`, ...): found
    // before anything changes.
    if !refs_settable(&dir, r, &old, &elsewhere).map_err(ctx)? {
        return skip_repo("ref_conflict");
    }

    // Untracked / ignored / excluded paths here that the new tree replaces:
    // back them up first (git would overwrite them silently).
    let coll: Vec<String> = collisions(&dir, &tree, &gitlinks).map_err(ctx)?.iter().map(|p| full(p)).collect();
    if coll.iter().any(|c| cx.blocked.iter().any(|b| paths::is_under(b, c) || paths::is_under(c, b))) {
        return skip_repo("path_conflict");
    }
    backup::backup_files(root, &cx.ts, &coll).map_err(ctx)?;

    let mut skips = Vec::new();
    let lfs_keep = lfs_preserve(&dir, &target, &tree, &cx.staging, &full, &mut skips).map_err(ctx)?;

    if let Err(e) = switch(&dir, r, &target, &elsewhere, &old, &cx.ts, &mut skips) {
        let mut failed = restore(&dir, r, &old, cx.backup.as_ref());
        // `read-tree` removed what it had written over the colliding files.
        let lost = backup::restore_files(root, &cx.ts, &coll);
        if !lost.is_empty() {
            failed.push(format!("files {}", lost.join(", ")));
        }
        drop_tmp_refs(&dir);
        return Err(ctx(if failed.is_empty() {
            format!("{e}; the repo and the files it replaced were put back as they were")
        } else {
            format!("{e}; putting back failed for: {}", failed.join("; "))
        }));
    }
    for (p, staged) in lfs_keep {
        put_back(&dir, &p, &staged, &cx.staging).map_err(ctx)?;
    }
    Ok(RepoOutcome { applied: true, skips })
}

/// Every ref [`switch`] will write is valid and clashes with no existing
/// ref (refs are never deleted here, so `a` and `a/b` cannot both exist).
fn refs_settable(dir: &Path, r: &RepoManifest, old: &Old, elsewhere: &HashSet<String>) -> Result<bool, String> {
    let existing: BTreeSet<String> = git::ref_names(dir)?.into_iter().collect();
    let mut writes: Vec<String> = r
        .branches
        .iter()
        .filter(|(n, s)| old.branches.get(*n) != Some(*s) && !elsewhere.contains(*n))
        .map(|(n, _)| format!("refs/heads/{n}"))
        .collect();
    writes.extend(r.remote_refs.iter().filter(|(n, s)| old.remote_refs.get(*n) != Some(*s)).map(|(n, _)| n.clone()));
    if let Some(b) = &r.branch {
        writes.push(format!("refs/heads/{b}"));
    }
    for w in &writes {
        if !git::ref_format_ok(dir, w) {
            return Ok(false);
        }
        // An existing ref is a parent "directory" of `w`, or below it.
        let mut prefix = String::new();
        for part in w.split('/') {
            if !prefix.is_empty() {
                if existing.contains(&prefix) {
                    return Ok(false);
                }
                prefix.push('/');
            }
            prefix.push_str(part);
        }
        let below = format!("{w}/");
        if existing.range(below.clone()..).next().is_some_and(|e| e.starts_with(&below)) {
            return Ok(false);
        }
    }
    Ok(true)
}

struct Old {
    sym: Option<String>,
    head: Option<String>,
    branches: BTreeMap<String, String>,
    upstreams: BTreeMap<String, (String, String)>,
    remote_refs: BTreeMap<String, String>,
    remotes: BTreeMap<String, String>,
}

/// Index + worktree first, then refs and HEAD, then config and the staged
/// changes. Any failure leaves `restore` to undo it.
fn switch(dir: &Path, r: &RepoManifest, target: &str, elsewhere: &HashSet<String>, old: &Old, ts: &str, skips: &mut Vec<SkipItem>) -> Result<(), String> {
    // Flagged entries make `read-tree -u` refuse; their edits are in the backup.
    git::clear_hidden_flags(dir, vec![], false)?;
    git::run(dir, &["read-tree", "--reset", "-u", target])?;

    let label = |b: &str| if r.path.is_empty() { b.to_string() } else { format!("{}:{b}", r.path) };
    let mut input = String::new();
    for (name, sha) in &r.branches {
        let old_tip = old.branches.get(name);
        if old_tip == Some(sha) {
            continue;
        }
        if elsewhere.contains(name) {
            skips.push(SkipItem::new(label(name), "branch_in_worktree"));
            continue;
        }
        // Branch tips this sync rewinds stay on backup branches.
        if let Some(o) = old_tip {
            if !git::is_ancestor(dir, o, sha) {
                backup::backup_branch_tip(dir, ts, name, o)?;
            }
        }
        input.push_str(&format!("update refs/heads/{name} {sha}\n"));
    }
    for (name, sha) in &r.remote_refs {
        input.push_str(&format!("update {name} {sha}\n"));
    }
    if !input.is_empty() {
        git::run_with(dir, &["update-ref", "--stdin"], Opts { stdin: Some(input.as_bytes()), ..Default::default() })?;
    }

    match (&r.branch, &r.head) {
        // Checked out in another worktree: stay detached on the source's commit.
        (Some(b), Some(h)) if elsewhere.contains(b) => {
            git::run(dir, &["update-ref", "--no-deref", "HEAD", h])?;
        }
        (Some(b), _) => {
            git::run(dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{b}")])?;
        }
        (None, Some(h)) => {
            git::run(dir, &["update-ref", "--no-deref", "HEAD", h])?;
        }
        (None, None) => {}
    }

    let existing = git::remotes(dir)?;
    for (name, url) in &r.remotes {
        if !valid_ref_part(name) || url.contains(['\n', '\0']) {
            return Err(format!("invalid remote {name:?}"));
        }
        match existing.get(name) {
            Some(u) if u == url => {}
            Some(_) => {
                git::run(dir, &["remote", "set-url", name, url])?;
            }
            None => {
                git::run(dir, &["remote", "add", name, url])?;
            }
        }
    }
    for name in r.branches.keys() {
        let (kr, km) = (format!("branch.{name}.remote"), format!("branch.{name}.merge"));
        match r.upstreams.get(name) {
            Some(u) if valid_ref_part(&u.remote) && valid_ref_part(&u.merge) => {
                git::run(dir, &["config", &kr, &u.remote])?;
                git::run(dir, &["config", &km, &u.merge])?;
            }
            _ => {
                let _ = git::exec(dir, &["config", "--unset-all", &kr], Opts::default());
                let _ = git::exec(dir, &["config", "--unset-all", &km], Opts::default());
            }
        }
    }
    drop_tmp_refs(dir);

    let patch = r.patch_bytes()?;
    if !patch.is_empty() {
        let gdir = git::git_dir(dir)?;
        let pf = gdir.join(format!("devdy-sync-{}.patch", uuid::Uuid::new_v4().simple()));
        fs::write(&pf, &patch).map_err(|e| format!("index patch: {e}"))?;
        let res = git::run(dir, &["apply", "--cached", "--whitespace=nowarn", &pf.to_string_lossy()]);
        let _ = fs::remove_file(&pf);
        res.map_err(|e| format!("restoring staged changes: {e}"))?;
    }
    Ok(())
}

/// Undo a failed [`switch`], best effort step by step: HEAD and the
/// worktree + index first (from the backup commits), then refs, remotes and
/// upstreams. Returns what could not be put back.
fn restore(dir: &Path, r: &RepoManifest, old: &Old, backup: Option<&(String, Option<String>)>) -> Vec<String> {
    let mut failed: Vec<String> = Vec::new();
    let mut step = |what: &str, res: Result<(), String>| {
        if let Err(e) = res {
            failed.push(format!("{what} ({e})"));
        }
    };
    let ok = |r: Result<Vec<u8>, String>| r.map(|_| ());
    step(
        "HEAD",
        match (&old.sym, &old.head) {
            (Some(sym), _) => ok(git::run(dir, &["symbolic-ref", "HEAD", sym])),
            (None, Some(h)) => ok(git::run(dir, &["update-ref", "--no-deref", "HEAD", h])),
            (None, None) => Ok(()),
        },
    );
    step(
        "worktree",
        match (backup, &old.head) {
            (Some((wt, idx)), _) => ok(git::run(dir, &["read-tree", "--reset", "-u", wt]))
                .and_then(|_| idx.as_ref().map_or(Ok(()), |i| ok(git::run(dir, &["read-tree", i])))),
            (None, Some(h)) => ok(git::run(dir, &["read-tree", "--reset", "-u", h])),
            (None, None) => ok(git::run(dir, &["read-tree", "--empty"])),
        },
    );
    // One ref at a time, so one that cannot be set does not stop the others.
    let mut lines: Vec<String> = Vec::new();
    for name in r.branches.keys() {
        lines.push(match old.branches.get(name) {
            Some(s) => format!("update refs/heads/{name} {s}\n"),
            None => format!("delete refs/heads/{name}\n"),
        });
    }
    for name in r.remote_refs.keys() {
        lines.push(match old.remote_refs.get(name) {
            Some(s) => format!("update {name} {s}\n"),
            None => format!("delete {name}\n"),
        });
    }
    for l in lines {
        let res = git::run_with(dir, &["update-ref", "--stdin"], Opts { stdin: Some(l.as_bytes()), ..Default::default() });
        step(l.split_whitespace().nth(1).unwrap_or("ref"), ok(res));
    }
    for name in r.remotes.keys() {
        let res = match old.remotes.get(name) {
            Some(url) => ok(git::run(dir, &["config", &format!("remote.{name}.url"), url])),
            None => {
                let _ = git::exec(dir, &["config", "--remove-section", &format!("remote.{name}")], Opts::default());
                Ok(())
            }
        };
        step(&format!("remote {name}"), res);
    }
    for name in r.branches.keys() {
        let (kr, km) = (format!("branch.{name}.remote"), format!("branch.{name}.merge"));
        let res = match old.upstreams.get(name) {
            Some((remote, merge)) => ok(git::run(dir, &["config", &kr, remote])).and_then(|_| ok(git::run(dir, &["config", &km, merge]))),
            None => {
                let _ = git::exec(dir, &["config", "--unset-all", &kr], Opts::default());
                let _ = git::exec(dir, &["config", "--unset-all", &km], Opts::default());
                Ok(())
            }
        };
        step(&format!("upstream of {name}"), res);
    }
    failed
}

fn drop_tmp_refs(dir: &Path) {
    if let Ok(tmp) = git::run(dir, &["for-each-ref", "--format=delete %(refname)", "refs/devdy-sync/"]) {
        if !tmp.is_empty() {
            let _ = git::run_with(dir, &["update-ref", "--stdin"], Opts { stdin: Some(&tmp), ..Default::default() });
        }
    }
}

/// Repo-relative paths that exist here, untracked (or ignored / excluded),
/// where the new tree puts a file or needs a directory. Topmost only.
fn collisions(dir: &Path, tree: &[String], gitlinks: &HashSet<String>) -> Result<Vec<String>, String> {
    let tracked: HashSet<String> = git::ls_files(dir)?.into_iter().map(|(p, _)| p).collect();
    let mut dirs: BTreeSet<String> = BTreeSet::new();
    for p in tree {
        let mut cur = p.as_str();
        while let Some((parent, _)) = cur.rsplit_once('/') {
            dirs.insert(parent.to_string());
            cur = parent;
        }
    }
    let mut out: Vec<String> = Vec::new();
    // A file / symlink where a directory is needed (shallowest first).
    for d in &dirs {
        if out.iter().any(|o| paths::is_under(d, o)) || tracked.contains(d) {
            continue;
        }
        if fs::symlink_metadata(dir.join(d)).is_ok_and(|m| !m.is_dir()) {
            out.push(d.clone());
        }
    }
    for p in tree {
        if tracked.contains(p) || out.iter().any(|o| paths::is_under(p, o)) {
            continue;
        }
        match fs::symlink_metadata(dir.join(p)) {
            // A submodule's directory: git leaves it alone.
            Ok(m) if m.is_dir() && gitlinks.contains(p) => {}
            Ok(_) => out.push(p.clone()),
            Err(_) => {}
        }
    }
    Ok(out)
}

/// For LFS pointers in the new tree, keep (in staging) the real content this
/// worktree already has; the others are reported.
fn lfs_preserve(
    dir: &Path,
    target: &str,
    tree: &[String],
    staging: &Path,
    full: &dyn Fn(&str) -> String,
    skips: &mut Vec<SkipItem>,
) -> Result<Vec<(String, PathBuf)>, String> {
    let mut keep = Vec::new();
    for p in git::lfs_paths(dir, target, tree) {
        let Ok(blob) = git::run(dir, &["cat-file", "blob", &format!("{target}:{p}")]) else { continue };
        let text = String::from_utf8_lossy(&blob);
        let oid = text.lines().find_map(|l| l.strip_prefix("oid sha256:")).map(str::trim);
        let size = text.lines().find_map(|l| l.strip_prefix("size ")).and_then(|s| s.trim().parse::<u64>().ok());
        let (Some(oid), Some(size)) = (oid, size) else { continue };
        if !is_sha256(oid) {
            continue;
        }
        let abs = dir.join(&p);
        let real = fs::symlink_metadata(&abs).is_ok_and(|m| m.is_file() && m.len() == size)
            && paths::check_parents(dir, &p).is_ok()
            && scan::sha256_file(&abs).ok().as_deref() == Some(oid);
        if real {
            let staged = staging.join(format!("lfs-{oid}"));
            if !staged.exists() {
                fs::copy(&abs, &staged).map_err(|e| format!("staging: {e}"))?;
            }
            keep.push((p, staged));
        } else {
            skips.push(SkipItem::new(full(&p), "lfs"));
        }
    }
    Ok(keep)
}

/// Atomically put `staged` at `dir/rel`, keeping the mode git gave it.
fn put_back(dir: &Path, rel: &str, staged: &Path, staging: &Path) -> Result<(), String> {
    paths::check_parents(dir, rel)?;
    let dst = dir.join(rel);
    let mode = fs::symlink_metadata(&dst).ok().filter(|m| m.is_file()).map(|m| paths::mode_of(&m)).unwrap_or(0o644);
    let tmp = staging.join(format!("w-{}", uuid::Uuid::new_v4().simple()));
    fs::copy(staged, &tmp).map_err(|e| format!("write {rel}: {e}"))?;
    paths::set_mode(&tmp, mode)?;
    fs::rename(&tmp, &dst).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("write {rel}: {e}")
    })
}

fn preserve_dirty(root: &Path, m: &Manifest, plan: &Plan, staging: &Path) -> Result<(), String> {
    for &i in &plan.repos {
        for d in &m.repos[i].dirty {
            if d.deleted || d.link.is_some() || d.size == 0 || plan.is_blocked(&d.path) || paths::check_parents(root, &d.path).is_err() {
                continue;
            }
            let kept = staging.join(&d.sha256);
            if kept.parent() == Some(staging) && !kept.exists() && plan::identical(root, &d.path, d, false) {
                fs::copy(root.join(&d.path), &kept).map_err(|e| format!("staging: {e}"))?;
            }
        }
    }
    Ok(())
}

/// What the files phase does, computed after the git phase.
#[derive(Default)]
struct FilesTodo {
    /// Written without download: symlinks, empty files, mode-only fixes.
    local: Vec<(String, LocalWrite)>,
    /// sha → destinations (path, mode).
    dests: HashMap<String, Vec<(String, u32)>>,
    wanted: Vec<(String, u64)>,
    /// The source's deleted tracked files.
    deletes: Vec<String>,
    skipped: Vec<SkipItem>,
    /// Symlinks of the manifest (for chain checks).
    links: HashSet<String>,
}

enum LocalWrite {
    Link(String),
    Empty(u32),
    Mode(u32),
}

fn files_todo(root: &Path, m: &Manifest, plan: &Plan, ex: &Excludes, speller: &mut Speller) -> Result<FilesTodo, String> {
    let mut t = FilesTodo::default();
    let mut entries: Vec<&FileEntry> = Vec::new();
    // Excluded paths are never touched, tracked or not.
    for &i in &plan.repos {
        for d in &m.repos[i].dirty {
            if plan.is_blocked(&d.path) || ex.is_excluded(&d.path, false) {
                continue;
            }
            if d.deleted {
                t.deletes.push(d.path.clone());
            } else {
                entries.push(d);
            }
        }
    }
    for f in &m.files {
        if !plan.is_blocked(&f.path) && !ex.is_excluded(&f.path, false) {
            entries.push(f);
        }
    }
    t.links = entries.iter().filter(|e| e.link.is_some()).map(|e| e.path.clone()).collect();
    let may_rename = |a: &str| !plan.is_blocked(a) && !ex.is_excluded(a, false) && !ex.is_excluded(a, true);
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries {
        // Same name in another case / normalization here: take the source's.
        if speller.respell(root, &e.path, &may_rename).is_err() {
            t.skipped.push(SkipItem::new(e.path.clone(), "path_conflict"));
            continue;
        }
        if plan::identical(root, &e.path, e, true) {
            continue;
        }
        if let Some(target) = &e.link {
            t.local.push((e.path.clone(), LocalWrite::Link(target.clone())));
        } else if e.size == 0 && e.sha256 == EMPTY_SHA {
            t.local.push((e.path.clone(), LocalWrite::Empty(e.mode)));
        } else if plan::identical(root, &e.path, e, false) {
            t.local.push((e.path.clone(), LocalWrite::Mode(e.mode)));
        } else if e.size == 0 {
            t.skipped.push(SkipItem::new(e.path.clone(), "unsafe_path"));
        } else {
            t.dests.entry(e.sha256.clone()).or_default().push((e.path.clone(), e.mode));
            if seen.insert(e.sha256.clone()) {
                t.wanted.push((e.sha256.clone(), e.size));
            }
        }
    }
    Ok(t)
}

enum Conflict {
    Skip(SkipItem),
    Fail(String),
}

/// Checks before writing `rel`: safe path, parents are real dirs (creating
/// them; a receiver-only file in the way goes only if still as planned),
/// the destination is not a directory.
fn prepare_dest(root: &Path, rel: &str, plan: &Plan) -> Result<PathBuf, Conflict> {
    paths::check_rel(rel).map_err(|_| Conflict::Skip(SkipItem::new(rel, "unsafe_path")))?;
    paths::ensure_parents(root, rel, &|a| plan.deletable(root, a)).map_err(|_| Conflict::Skip(SkipItem::new(rel, "path_conflict")))?;
    let dst = root.join(rel);
    if fs::symlink_metadata(&dst).is_ok_and(|m| m.is_dir()) {
        return Err(Conflict::Skip(SkipItem::new(rel, "path_conflict")));
    }
    Ok(dst)
}

fn tmp_path(root: &Path) -> Result<PathBuf, Conflict> {
    let dir = root.join(STAGING);
    fs::create_dir_all(&dir).map_err(|e| Conflict::Fail(format!("staging: {e}")))?;
    Ok(dir.join(format!("w-{}", uuid::Uuid::new_v4().simple())))
}

/// Rename `tmp` onto `dst` (atomic; replaces a file or symlink, never
/// follows it).
fn commit_tmp(tmp: &Path, dst: &Path, rel: &str) -> Result<(), Conflict> {
    fs::rename(tmp, dst).map_err(|e| {
        let _ = fs::remove_file(tmp);
        Conflict::Fail(format!("write {rel}: {e}"))
    })
}

fn place_file(root: &Path, rel: &str, staged: &Path, mode: u32, plan: &Plan) -> Result<(), Conflict> {
    let dst = prepare_dest(root, rel, plan)?;
    let tmp = tmp_path(root)?;
    let fail = |e: String| Conflict::Fail(format!("write {rel}: {e}"));
    fs::copy(staged, &tmp).map_err(|e| fail(e.to_string()))?;
    paths::set_mode(&tmp, mode).map_err(fail)?;
    commit_tmp(&tmp, &dst, rel)
}

fn place_local(root: &Path, rel: &str, w: &LocalWrite, plan: &Plan, links: &HashSet<String>) -> Result<(), Conflict> {
    let dst = prepare_dest(root, rel, plan)?;
    let fail = |e: String| Conflict::Fail(format!("write {rel}: {e}"));
    match w {
        LocalWrite::Mode(mode) => paths::set_mode(&dst, *mode).map_err(fail),
        LocalWrite::Empty(mode) => {
            let tmp = tmp_path(root)?;
            fs::File::create(&tmp).map_err(|e| fail(e.to_string()))?;
            paths::set_mode(&tmp, *mode).map_err(fail)?;
            commit_tmp(&tmp, &dst, rel)
        }
        LocalWrite::Link(target) => {
            let is_link = |p: &str| links.contains(p) || paths::is_symlink_at(root, p);
            if !paths::symlink_target_ok_with(rel, target, &is_link) {
                return Err(Conflict::Skip(SkipItem::new(rel, "symlink_outside")));
            }
            let tmp = tmp_path(root)?;
            paths::make_symlink(target, &tmp).map_err(fail)?;
            commit_tmp(&tmp, &dst, rel)
        }
    }
}

/// Delete the receiver-only files planned (and backed up), only if still as
/// planned and not tracked now; then the directories that became empty.
/// Anything created since the plan is left alone.
fn cleanup(root: &Path, m: &Manifest, plan: &Plan, ex: &Excludes) -> Result<(u64, Vec<SkipItem>), String> {
    let fold = |s: &str| paths::fold(s, plan.ci);
    let mut tracked: HashSet<String> = HashSet::new();
    for &i in &plan.repos {
        let r = &m.repos[i];
        let dir = paths::join(root, &r.path);
        if git::is_repo_dir(&dir) {
            for (p, _) in git::ls_files(&dir)? {
                tracked.insert(fold(&if r.path.is_empty() { p } else { format!("{}/{p}", r.path) }));
            }
        }
    }
    let mut rels: Vec<&String> = plan.deletes.keys().collect();
    rels.sort();
    let mut n = 0;
    let mut skips = Vec::new();
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for rel in rels {
        if plan.keep.contains(&fold(rel)) || tracked.contains(&fold(rel)) || plan.is_blocked(rel) || ex.is_excluded(rel, false) {
            continue;
        }
        if paths::check_rel(rel).is_err() || paths::check_parents(root, rel).is_err() {
            skips.push(SkipItem::new(rel.clone(), "path_conflict"));
            continue;
        }
        let abs = root.join(rel);
        if fs::symlink_metadata(&abs).is_err() {
            continue;
        }
        if !plan.deletable(root, rel) {
            skips.push(SkipItem::new(rel.clone(), "changed_since_preview"));
            continue;
        }
        fs::remove_file(&abs).map_err(|e| format!("delete {rel}: {e}"))?;
        n += 1;
        let mut d = abs.parent().map(Path::to_path_buf);
        while let Some(p) = d {
            if p == root {
                break;
            }
            dirs.insert(p.clone());
            d = p.parent().map(Path::to_path_buf);
        }
    }
    // Deepest first; `remove_dir` only succeeds on empty directories.
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in dirs {
        let _ = fs::remove_dir(&d);
    }
    Ok((n, skips))
}
