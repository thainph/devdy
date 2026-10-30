//! Receiver: preview (no writes) and apply (backup → git → files → cleanup).

use super::backup::{self, BackupInfo};
use super::git::{self, Opts};
use super::manifest::{FileEntry, Manifest, ManifestRequest, RepoManifest};
use super::paths::{self, Excludes};
use super::plan::{self, FolderPreview, Plan};
use super::scan::{self, SkipItem};
use super::transfer::{self, Downloader};
use super::{blocking, Cancel, FolderOptions, FolderProgress, ProgressSink};
use crate::sync::peer::PeerLink;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
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
    ex: Arc<Excludes>,
    scan: scan::Scan,
}

async fn local_state(opts: &FolderOptions) -> Result<Local, String> {
    let root = resolve_root(opts)?;
    let ex = Arc::new(Excludes::new(&opts.patterns())?);
    let (r2, e2) = (root.clone(), ex.clone());
    // Everything here counts, whatever its size (receiver-only files are deleted).
    let scan = blocking(move || scan::scan(&r2, &e2, u64::MAX)).await?;
    Ok(Local { root, ex, scan })
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
    };
    transfer::request_manifest(link, source, &req).await
}

async fn make_plan(opts: &FolderOptions, local: &Local, m: Manifest) -> Result<(Manifest, Plan), String> {
    let (root, ex, pid) = (local.root.clone(), local.ex.clone(), opts.project_id.clone());
    let sc = local.scan.clone();
    blocking(move || {
        let p = plan::plan(&root, &pid, &m, &sc, &ex)?;
        Ok((m, p))
    })
    .await
}

/// What a sync from `source` would do to this project folder. No writes.
pub async fn preview(link: Arc<dyn PeerLink>, source: &str, opts: &FolderOptions) -> Result<FolderPreview, String> {
    let local = local_state(opts).await?;
    let m = fetch_manifest(&link, source, opts, &local).await?;
    // Bundles were built for their size only; apply asks for a fresh manifest.
    let (_, p) = make_plan(opts, &local, m).await?;
    Ok(p.preview)
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
pub async fn apply(link: Arc<dyn PeerLink>, source: &str, opts: &FolderOptions, sink: ProgressSink, cancel: Cancel) -> Result<ApplyReport, String> {
    let mut em = Emitter { sink, base: FolderProgress { project_id: opts.project_id.clone(), ..Default::default() } };
    em.phase("manifest");
    let local = local_state(opts).await?;
    let m = fetch_manifest(&link, source, opts, &local).await?;
    let (m, plan) = make_plan(opts, &local, m).await?;
    let (m, plan) = (Arc::new(m), Arc::new(plan));
    let root = local.root.clone();
    cancel.check()?;

    em.phase("backup");
    let info = {
        let (root, m, plan) = (root.clone(), m.clone(), plan.clone());
        blocking(move || take_backup(&root, &m, &plan)).await?
    };
    let mut report = ApplyReport { backup: info.clone(), ..Default::default() };
    let with_backup = |e: String| format!("{e} (backup: {})", info.describe());

    let res = run_after_backup(&link, source, opts, &root, m, plan, &mut em, &cancel, &mut report).await;
    let _ = fs::remove_dir_all(root.join(STAGING));
    res.map_err(with_backup)?;
    Ok(report)
}

fn take_backup(root: &Path, m: &Manifest, plan: &Plan) -> Result<BackupInfo, String> {
    let ts = backup::new_ts();
    let mut info = BackupInfo { ts: ts.clone(), ..Default::default() };
    fs::create_dir_all(root).map_err(|e| format!("create project folder: {e}"))?;
    for &i in &plan.repos {
        let rel = &m.repos[i].path;
        let dir = paths::join(root, rel);
        if git::is_repo_dir(&dir) && backup::backup_repo(&dir, &ts)? {
            info.repos.push(rel.clone());
            backup::prune_repo(&dir)?;
        }
    }
    info.folder = backup::backup_files(root, &ts, &plan.backup_files)?;
    backup::prune_folder(root)?;
    Ok(info)
}

#[allow(clippy::too_many_arguments)]
async fn run_after_backup(
    link: &Arc<dyn PeerLink>,
    source: &str,
    opts: &FolderOptions,
    root: &Path,
    m: Arc<Manifest>,
    plan: Arc<Plan>,
    em: &mut Emitter,
    cancel: &Cancel,
    report: &mut ApplyReport,
) -> Result<(), String> {
    cancel.check()?;
    let staging = root.join(STAGING);
    let _ = fs::remove_dir_all(&staging);
    let mut dl = Downloader::new(&staging)?;

    // `reset --hard` reverts dirty files: keep the ones that already have
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
        dl.fetch(link, source, &opts.project_id, &bundles, cancel, &mut on_progress, &mut |_, _| Ok(())).await?;
        report.bytes_downloaded += base.lock().unwrap().done_bytes;
        em.base.done_bytes = em.base.total_bytes;
    }
    for &i in &plan.repos {
        cancel.check()?;
        let r = m.repos[i].clone();
        let bundle = r.bundle.as_ref().map(|b| dl.done_path(&b.sha256));
        let (root2, ts) = (root.to_path_buf(), report.backup.ts.clone());
        blocking(move || apply_repo(&root2, &r, bundle.as_deref(), &ts)).await?;
        em.base.files_done += 1;
        em.emit();
    }

    // ---- files (non-git + the source's dirty tracked files)
    em.phase("files");
    cancel.check()?;
    let (root2, m2, plan2) = (root.to_path_buf(), m.clone(), plan.clone());
    let ex = Excludes::new(&opts.patterns())?;
    let todo = blocking(move || files_todo(&root2, &m2, &plan2, &ex)).await?;
    report.skipped.extend(todo.skipped.iter().cloned());
    em.base.files_total = todo.local.len() as u64 + todo.dests.values().map(|v| v.len() as u64).sum::<u64>() + todo.deletes.len() as u64;
    em.base.total_bytes = todo.wanted.iter().map(|(_, n)| n).sum();
    em.emit();
    let deletable = plan.deletes.clone();
    let removable = move |rel: &str| deletable.contains(rel);
    for (rel, kind) in &todo.local {
        cancel.check()?;
        match place_local(root, rel, kind, &removable) {
            Ok(()) => report.files_written += 1,
            Err(Conflict::Skip(s)) => report.skipped.push(s),
            Err(Conflict::Fail(e)) => return Err(e),
        }
        em.base.files_done += 1;
    }
    for rel in &todo.deletes {
        cancel.check()?;
        paths::check_parents(root, rel)?;
        let abs = root.join(rel);
        if fs::symlink_metadata(&abs).is_ok_and(|md| !md.is_dir()) {
            fs::remove_file(&abs).map_err(|e| format!("delete {rel}: {e}"))?;
            report.files_deleted += 1;
        }
        em.base.files_done += 1;
    }
    em.emit();
    {
        let state = Arc::new(Mutex::new((em.base.clone(), 0u64, 0u64, Vec::<SkipItem>::new())));
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
        let deletable = plan.deletes.clone();
        let mut on_done = move |sha: &str, staged: &Path| -> Result<(), String> {
            let mut st = s2.lock().unwrap();
            for (rel, mode) in dests.get(sha).into_iter().flatten() {
                match place_file(&root2, rel, staged, *mode, &|a: &str| deletable.contains(a)) {
                    Ok(()) => st.1 += 1,
                    Err(Conflict::Skip(s)) => st.3.push(s),
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
            let kept = dl.done_path(sha);
            if kept.is_file() {
                cancel.check()?;
                on_done(sha, &kept)?;
            } else {
                wanted.push((sha.clone(), *size));
            }
        }
        let res = dl.fetch(link, source, &opts.project_id, &wanted, cancel, &mut on_progress, &mut on_done).await;
        let st = state.lock().unwrap();
        report.files_written += st.1;
        report.bytes_downloaded += st.0.done_bytes;
        report.skipped.extend(st.3.iter().cloned());
        em.base = st.0.clone();
        res?;
    }

    // ---- cleanup: receiver-only non-git files
    em.phase("cleanup");
    cancel.check()?;
    let (root2, plan2) = (root.to_path_buf(), plan.clone());
    let ex = Excludes::new(&opts.patterns())?;
    report.files_deleted += blocking(move || cleanup(&root2, &plan2, &ex)).await?;
    Ok(())
}

fn valid_ref_part(s: &str) -> bool {
    !s.is_empty() && !s.chars().any(|c| c.is_whitespace() || c.is_control()) && !s.starts_with('-')
}

/// Make one repo match the source (objects come from `bundle`).
pub fn apply_repo(root: &Path, r: &RepoManifest, bundle: Option<&Path>, ts: &str) -> Result<(), String> {
    let label = if r.path.is_empty() { "." } else { r.path.as_str() };
    let ctx = |e: String| format!("{label}: {e}");
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

    // Branch tips this sync rewinds stay on backup branches.
    let local: BTreeMap<String, String> = git::branches(&dir).map_err(ctx)?.into_iter().map(|(n, (s, _))| (n, s)).collect();
    for (name, sha) in &r.branches {
        if let Some(old) = local.get(name) {
            if old != sha && !git::is_ancestor(&dir, old, sha) {
                backup::backup_branch_tip(&dir, ts, name, old).map_err(ctx)?;
            }
        }
    }

    let mut input = String::new();
    for (name, sha) in &r.branches {
        input.push_str(&format!("update refs/heads/{name} {sha}\n"));
    }
    for (name, sha) in &r.remote_refs {
        input.push_str(&format!("update {name} {sha}\n"));
    }
    if !input.is_empty() {
        git::run_with(&dir, &["update-ref", "--stdin"], Opts { stdin: Some(input.as_bytes()), ..Default::default() }).map_err(ctx)?;
    }

    match (&r.branch, &r.head) {
        (Some(b), _) => {
            if !valid_ref_part(b) {
                return Err(ctx(format!("invalid branch {b:?}")));
            }
            git::run(&dir, &["symbolic-ref", "HEAD", &format!("refs/heads/{b}")]).map_err(ctx)?;
        }
        (None, Some(h)) => {
            git::run(&dir, &["update-ref", "--no-deref", "HEAD", h]).map_err(ctx)?;
        }
        (None, None) => {}
    }
    if r.head.is_some() {
        git::run(&dir, &["reset", "-q", "--hard", "HEAD"]).map_err(ctx)?;
    } else {
        git::run(&dir, &["read-tree", "--empty"]).map_err(ctx)?;
    }

    let existing = git::remotes(&dir).map_err(ctx)?;
    for (name, url) in &r.remotes {
        if !valid_ref_part(name) || url.contains(['\n', '\0']) {
            return Err(ctx(format!("invalid remote {name:?}")));
        }
        match existing.get(name) {
            Some(u) if u == url => {}
            Some(_) => {
                git::run(&dir, &["remote", "set-url", name, url]).map_err(ctx)?;
            }
            None => {
                git::run(&dir, &["remote", "add", name, url]).map_err(ctx)?;
            }
        }
    }
    for name in r.branches.keys() {
        let (kr, km) = (format!("branch.{name}.remote"), format!("branch.{name}.merge"));
        match r.upstreams.get(name) {
            Some(u) if valid_ref_part(&u.remote) && valid_ref_part(&u.merge) => {
                git::run(&dir, &["config", &kr, &u.remote]).map_err(ctx)?;
                git::run(&dir, &["config", &km, &u.merge]).map_err(ctx)?;
            }
            _ => {
                let _ = git::exec(&dir, &["config", "--unset-all", &kr], Opts::default());
                let _ = git::exec(&dir, &["config", "--unset-all", &km], Opts::default());
            }
        }
    }

    // Temporary fetch refs.
    let tmp = git::run(&dir, &["for-each-ref", "--format=delete %(refname)", "refs/devdy-sync/"]).map_err(ctx)?;
    if !tmp.is_empty() {
        git::run_with(&dir, &["update-ref", "--stdin"], Opts { stdin: Some(&tmp), ..Default::default() }).map_err(ctx)?;
    }

    let patch = r.patch_bytes().map_err(ctx)?;
    if !patch.is_empty() {
        let gdir = git::git_dir(&dir).map_err(ctx)?;
        let pf = gdir.join(format!("devdy-sync-{}.patch", uuid::Uuid::new_v4().simple()));
        fs::write(&pf, &patch).map_err(|e| ctx(format!("index patch: {e}")))?;
        let res = git::run(&dir, &["apply", "--cached", "--whitespace=nowarn", &pf.to_string_lossy()]);
        let _ = fs::remove_file(&pf);
        res.map_err(|e| ctx(format!("restoring staged changes: {e}")))?;
    }
    Ok(())
}

fn preserve_dirty(root: &Path, m: &Manifest, plan: &Plan, staging: &Path) -> Result<(), String> {
    for &i in &plan.repos {
        for d in &m.repos[i].dirty {
            if d.deleted || d.link.is_some() || d.size == 0 || plan.is_blocked(&d.path) || paths::check_parents(root, &d.path).is_err() {
                continue;
            }
            let kept = staging.join(&d.sha256);
            if !kept.exists() && plan::identical(root, &d.path, d, false) {
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
}

enum LocalWrite {
    Link(String),
    Empty(u32),
    Mode(u32),
}

fn files_todo(root: &Path, m: &Manifest, plan: &Plan, ex: &Excludes) -> Result<FilesTodo, String> {
    let mut t = FilesTodo::default();
    let mut entries: Vec<&FileEntry> = Vec::new();
    for &i in &plan.repos {
        for d in &m.repos[i].dirty {
            if plan.is_blocked(&d.path) {
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
    let mut seen: HashSet<String> = HashSet::new();
    for e in entries {
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
/// them), the destination is not a directory.
fn prepare_dest(root: &Path, rel: &str, removable: &dyn Fn(&str) -> bool) -> Result<PathBuf, Conflict> {
    paths::check_rel(rel).map_err(|_| Conflict::Skip(SkipItem::new(rel, "unsafe_path")))?;
    paths::ensure_parents(root, rel, removable).map_err(|_| Conflict::Skip(SkipItem::new(rel, "path_conflict")))?;
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

fn place_file(root: &Path, rel: &str, staged: &Path, mode: u32, removable: &dyn Fn(&str) -> bool) -> Result<(), Conflict> {
    let dst = prepare_dest(root, rel, removable)?;
    let tmp = tmp_path(root)?;
    let fail = |e: String| Conflict::Fail(format!("write {rel}: {e}"));
    fs::copy(staged, &tmp).map_err(|e| fail(e.to_string()))?;
    paths::set_mode(&tmp, mode).map_err(fail)?;
    commit_tmp(&tmp, &dst, rel)
}

fn place_local(root: &Path, rel: &str, w: &LocalWrite, removable: &dyn Fn(&str) -> bool) -> Result<(), Conflict> {
    let dst = prepare_dest(root, rel, removable)?;
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
            if !paths::symlink_target_ok(rel, target) {
                return Err(Conflict::Skip(SkipItem::new(rel, "symlink_outside")));
            }
            let tmp = tmp_path(root)?;
            paths::make_symlink(target, &tmp).map_err(fail)?;
            commit_tmp(&tmp, &dst, rel)
        }
    }
}

/// Delete this machine's non-git files the source does not have (never
/// excluded or blocked ones), then the directories that became empty.
fn cleanup(root: &Path, plan: &Plan, ex: &Excludes) -> Result<u64, String> {
    let sc = scan::scan(root, ex, u64::MAX)?;
    let mut n = 0;
    let mut dirs: HashSet<PathBuf> = HashSet::new();
    for f in &sc.files {
        if plan.keep.contains(&f.rel) || plan.is_blocked(&f.rel) || sc.is_blocked(&f.rel) {
            continue;
        }
        paths::check_rel(&f.rel)?;
        paths::check_parents(root, &f.rel)?;
        let abs = root.join(&f.rel);
        fs::remove_file(&abs).map_err(|e| format!("delete {}: {e}", f.rel))?;
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
    Ok(n)
}
