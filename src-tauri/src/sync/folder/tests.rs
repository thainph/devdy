//! End-to-end folder sync over `peer::testing::pair` with real `git`.

use super::apply::{self, ApplyReport};
use super::handler::{FolderSource, RootResolver};
use super::manifest::{FileEntry, Manifest, MANIFEST_VERSION};
use super::{backup, Cancel, FolderOptions, FolderProgress, ProgressSink, KIND_MANIFEST, REPLY_MANIFEST};
use crate::sync::files::tests::{tempdir, TempDir};
use crate::sync::peer::testing::{pair, MemLink};
use crate::sync::peer::{PeerHandler, PeerLink, Reply};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    c.args(["-c", "user.name=T", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-c", "core.hooksPath=/dev/null"]);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"] {
        c.env_remove(k);
    }
    let o = c.args(args).output().unwrap();
    assert!(o.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
}

fn read(root: &Path, rel: &str) -> Vec<u8> {
    fs::read(root.join(rel)).unwrap()
}

#[cfg(unix)]
fn chmod(root: &Path, rel: &str, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root.join(rel), fs::Permissions::from_mode(mode)).unwrap();
}

fn commit(dir: &Path, rel: &str, content: &str, msg: &str) -> String {
    write(dir, rel, content.as_bytes());
    git(dir, &["add", "--", rel]);
    git(dir, &["commit", "-q", "-m", msg]);
    git(dir, &["rev-parse", "HEAD"])
}

struct MapRoots(PathBuf);

#[async_trait]
impl RootResolver for MapRoots {
    async fn root(&self, project_id: &str) -> Result<PathBuf, String> {
        if project_id == "p1" {
            Ok(self.0.clone())
        } else {
            Err("unknown project".into())
        }
    }
}

/// (receiver link, source link) with the folder handler on the source.
fn link_to(src_root: &Path) -> (Arc<MemLink>, Arc<MemLink>) {
    let (recv, src) = pair("recv", "src");
    src.add_handler(Arc::new(FolderSource::new(Arc::new(MapRoots(src_root.to_path_buf())))));
    (recv, src)
}

fn opts(root: &Path) -> FolderOptions {
    FolderOptions { project_id: "p1".into(), local_root: root.to_path_buf(), excludes: vec![], default_excludes: true, max_file_mb: 500 }
}

type Events = Arc<Mutex<Vec<FolderProgress>>>;

fn sink() -> (ProgressSink, Events) {
    let ev: Events = Default::default();
    let e2 = ev.clone();
    (Arc::new(move |p: &FolderProgress| e2.lock().unwrap().push(p.clone())), ev)
}

async fn run(link: &Arc<MemLink>, o: &FolderOptions) -> Result<ApplyReport, String> {
    let (s, _) = sink();
    let l: Arc<dyn PeerLink> = link.clone();
    apply::apply(l, "src", o, s, Cancel::default()).await
}

async fn preview(link: &Arc<MemLink>, o: &FolderOptions) -> super::plan::FolderPreview {
    let l: Arc<dyn PeerLink> = link.clone();
    apply::preview(l, "src", o).await.unwrap()
}

/// `git status --porcelain=v2` without Devdy's own `.devdy/` dir.
fn status(dir: &Path) -> String {
    git(dir, &["status", "--porcelain=v2", "--branch", "--untracked-files=all"])
        .lines()
        .filter(|l| !l.contains(".devdy/"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file / symlink below `root` (not `.git`, `.devdy`, or `skip` dirs):
/// path → mode + content hash, or link target.
fn tree(root: &Path, skip: &[&str]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in walkdir::WalkDir::new(root).follow_links(false).into_iter().filter_entry(|e| {
        let n = e.file_name().to_string_lossy();
        n != ".git" && n != ".devdy" && !skip.contains(&n.as_ref())
    }) {
        let e = e.unwrap();
        let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().to_string();
        let m = fs::symlink_metadata(e.path()).unwrap();
        if m.file_type().is_symlink() {
            out.insert(rel, format!("link {}", fs::read_link(e.path()).unwrap().display()));
        } else if m.is_file() {
            let mode = super::paths::mode_of(&m);
            out.insert(rel, format!("{mode:o} {}", super::scan::sha256_file(e.path()).unwrap()));
        }
    }
    out
}

fn branches(dir: &Path) -> String {
    git(dir, &["for-each-ref", "--format=%(refname) %(objectname) %(upstream)", "refs/heads", "refs/remotes"])
        .lines()
        .filter(|l| !l.contains("devdy-sync-backup"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A source project: root repo (two branches, upstream + remote-tracking
/// ref, unpushed commits, dirty/staged/deleted tracked files, untracked,
/// ignored, excluded, executable, symlink, empty files) + a nested repo.
fn make_source(t: &TempDir) -> PathBuf {
    let s = t.path().join("src");
    fs::create_dir_all(&s).unwrap();
    git(&s, &["init", "-q"]);
    write(&s, ".gitignore", b"*.log\nnode_modules/\nlibs/inner/\nescape-link\n");
    git(&s, &["add", ".gitignore"]);
    let c1 = commit(&s, "a.txt", "one\n", "c1");
    commit(&s, "gone.txt", "bye\n", "c2");
    commit(&s, "bin/run.sh", "#!/bin/sh\necho hi\n", "c3");
    git(&s, &["remote", "add", "origin", "/nonexistent/remote.git"]);
    git(&s, &["update-ref", "refs/remotes/origin/main", &c1]);
    git(&s, &["config", "branch.main.remote", "origin"]);
    git(&s, &["config", "branch.main.merge", "refs/heads/main"]);
    git(&s, &["branch", "feature"]);
    git(&s, &["checkout", "-q", "feature"]);
    commit(&s, "feat.txt", "feature\n", "f1"); // unpushed
    // Worktree state.
    write(&s, "a.txt", b"one\nmodified\n");
    write(&s, "staged.txt", b"new staged\n");
    git(&s, &["add", "staged.txt"]);
    write(&s, "feat.txt", b"feature staged\n");
    git(&s, &["add", "feat.txt"]);
    write(&s, "feat.txt", b"feature staged then edited\n"); // partially staged
    fs::remove_file(s.join("gone.txt")).unwrap();
    write(&s, "notes/todo.md", b"untracked\n");
    write(&s, "debug.log", b"ignored log\n");
    write(&s, "node_modules/pkg/index.js", b"excluded\n");
    write(&s, "empty.txt", b"");
    write(&s, "tool.sh", b"#!/bin/sh\n");
    #[cfg(unix)]
    {
        chmod(&s, "tool.sh", 0o755);
        std::os::unix::fs::symlink("notes/todo.md", s.join("todo-link")).unwrap();
        std::os::unix::fs::symlink("/etc/hosts", s.join("escape-link")).unwrap();
    }
    // Nested repo (ignored by the outer one).
    let inner = s.join("libs/inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-q"]);
    commit(&inner, "lib.rs", "fn main() {}\n", "inner");
    write(&inner, "lib.rs", b"fn main() { dirty }\n");
    write(&inner, "scratch.txt", b"inner untracked\n");
    s
}

fn assert_same(src: &Path, dst: &Path) {
    assert_eq!(status(dst), status(src), "root status");
    assert_eq!(status(&dst.join("libs/inner")), status(&src.join("libs/inner")), "inner status");
    assert_eq!(branches(dst), branches(src));
    assert_eq!(branches(&dst.join("libs/inner")), branches(&src.join("libs/inner")));
    let mut want = tree(src, &["node_modules"]);
    want.remove("escape-link");
    assert_eq!(tree(dst, &[]), want);
    assert_eq!(git(dst, &["remote", "get-url", "origin"]), "/nonexistent/remote.git");
}

#[tokio::test]
async fn fresh_clone_from_bundle() {
    let t = tempdir();
    let src = make_source(&t);
    let dst = t.path().join("dst");
    let (recv, _) = link_to(&src);

    let pv = preview(&recv, &opts(&dst)).await;
    assert!(pv.create);
    assert_eq!(pv.repos.len(), 2);
    let root = pv.repos.iter().find(|r| r.path.is_empty()).unwrap();
    assert!(root.clone);
    assert_eq!(root.branch_to.as_deref(), Some("feature"));
    assert_eq!(root.commits, 4);
    assert_eq!(root.staged, 2);
    assert_eq!(root.dirty, 4);
    assert!(root.bundle_bytes > 0);
    assert!(pv.add.iter().any(|f| f.path == "notes/todo.md"));
    assert!(pv.add.iter().all(|f| !f.path.starts_with("node_modules")));
    assert!(pv.skipped_items.iter().any(|s| s.path == "escape-link" && s.reason == "symlink_outside"));
    assert!(!dst.exists(), "preview must not write");
    assert_eq!(recv.blob_count(), 0, "preview releases its blobs");

    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_same(&src, &dst);
    assert_eq!(read(&dst, "feat.txt"), b"feature staged then edited\n");
    assert!(!dst.join("node_modules").exists());
    assert!(!dst.join("escape-link").exists());
    assert!(!dst.join(apply::STAGING).exists());
    assert!(rep.bytes_downloaded > 0);
    assert_eq!(recv.blob_count(), 0, "every blob released");

    // A second sync has nothing to download.
    let pv = preview(&recv, &opts(&dst)).await;
    assert_eq!(pv.download_bytes, 0, "{pv:#?}");
    assert_eq!(pv.add_count + pv.overwrite_count + pv.delete_count, 0);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(rep.bytes_downloaded, 0);
    assert_same(&src, &dst);
}

#[tokio::test]
async fn existing_repo_behind_ahead_diverged_and_branch_switch() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let base = commit(&src, "a.txt", "base\n", "base");
    git(&src, &["branch", "feature"]);
    git(&src, &["branch", "old"]);
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    git(&dst, &["branch", "feature", "origin/feature"]);
    git(&dst, &["branch", "old", "origin/old"]);
    git(&dst, &["branch", "mine"]);

    // Source: main ahead, feature diverged, checked out on feature.
    commit(&src, "a.txt", "main 2\n", "main 2");
    git(&src, &["checkout", "-q", "feature"]);
    let src_feat = commit(&src, "f.txt", "source feature\n", "src feature");
    // Receiver: own commit on feature, sits on `mine` with a dirty file.
    git(&dst, &["checkout", "-q", "feature"]);
    let dst_feat = commit(&dst, "f.txt", "receiver feature\n", "dst feature");
    git(&dst, &["checkout", "-q", "mine"]);
    write(&dst, "a.txt", b"receiver dirty\n");
    write(&dst, "untracked-here.txt", b"only here\n");

    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &opts(&dst)).await;
    let r = &pv.repos[0];
    assert!(!r.clone);
    assert_eq!(r.branch_from.as_deref(), Some("mine"));
    assert_eq!(r.branch_to.as_deref(), Some("feature"));
    assert_eq!(r.commits, 2);
    assert_eq!(r.branches_rewound, vec!["feature".to_string()]);
    assert_eq!(pv.local_changes, vec!["a.txt".to_string()]);
    assert!(pv.delete.iter().any(|f| f.path == "untracked-here.txt"));
    assert_eq!(read(&dst, "a.txt"), b"receiver dirty\n", "preview must not write");

    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(git(&dst, &["symbolic-ref", "--short", "HEAD"]), "feature");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), src_feat);
    for b in ["main", "feature", "old"] {
        assert_eq!(git(&dst, &["rev-parse", b]), git(&src, &["rev-parse", b]), "{b}");
    }
    assert_eq!(git(&dst, &["rev-parse", "mine"]), base, "receiver-only branch kept");
    assert_eq!(status(&dst), status(&src));
    assert!(!dst.join("untracked-here.txt").exists());

    // Backups: worktree state, rewound branch tip, deleted file.
    let ts = &rep.backup.ts;
    let bk = format!("devdy-sync-backup/{ts}");
    assert_eq!(git(&dst, &["show", &format!("{bk}:a.txt")]), "receiver dirty");
    assert_eq!(git(&dst, &["rev-parse", &format!("{bk}^1")]), base, "first parent = old HEAD");
    assert_eq!(git(&dst, &["rev-parse", &format!("devdy-sync-backup/{ts}-feature")]), dst_feat);
    assert_eq!(read(&dst.join(".devdy/sync-backup").join(ts), "untracked-here.txt"), b"only here\n");
    // Restorable.
    git(&dst, &["checkout", "-q", &bk, "--", "a.txt"]);
    assert_eq!(read(&dst, "a.txt"), b"receiver dirty\n");
}

#[tokio::test]
async fn dirty_and_staged_state_reproduced_exactly() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "m.txt", "1\n2\n3\n", "c");
    commit(&src, "rm-cached.txt", "keep me\n", "c2");
    commit(&src, "mode.sh", "echo\n", "c3");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    // Partially staged, staged-new, staged-deleted kept as untracked, mode change, binary.
    write(&src, "m.txt", b"1\n2 staged\n3\n");
    git(&src, &["add", "m.txt"]);
    write(&src, "m.txt", b"1\n2 staged\n3 worktree\n");
    write(&src, "bin.dat", &[0u8, 159, 146, 150, 0, 1, 2]);
    git(&src, &["add", "bin.dat"]);
    git(&src, &["rm", "-q", "--cached", "rm-cached.txt"]);
    #[cfg(unix)]
    chmod(&src, "mode.sh", 0o755);
    // Receiver has its own staged change that must go away.
    write(&dst, "m.txt", b"receiver\n");
    git(&dst, &["add", "m.txt"]);

    let (recv, _) = link_to(&src);
    run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(status(&dst), status(&src));
    assert_eq!(git(&dst, &["diff", "--cached"]), git(&src, &["diff", "--cached"]));
    assert_eq!(git(&dst, &["diff"]), git(&src, &["diff"]));
    assert_eq!(tree(&dst, &[]), tree(&src, &[]));
    assert!(git(&dst, &["remote"]).contains("origin"), "receiver-only remote kept");
}

#[tokio::test]
async fn untracked_ignored_excludes_deletes_and_backups() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    // Plain folders, no git.
    write(&src, "keep.txt", b"source\n");
    write(&src, "same.txt", b"same\n");
    write(&src, "sub/new.txt", b"new\n");
    write(&src, "secret/s.txt", b"excluded by extra pattern\n");
    write(&src, "node_modules/x.js", b"excluded by default\n");
    write(&dst, "keep.txt", b"receiver version\n");
    write(&dst, "same.txt", b"same\n");
    write(&dst, "only-here/a.txt", b"receiver only\n");
    write(&dst, "secret/mine.txt", b"never touched\n");
    write(&dst, "node_modules/y.js", b"never touched\n");
    write(&dst, ".devdy/runs/r.log", b"devdy data\n");

    let mut o = opts(&dst);
    o.excludes = vec!["secret/".into()];
    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &o).await;
    assert_eq!(pv.overwrite.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["keep.txt"]);
    assert_eq!(pv.add.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["sub/new.txt"]);
    assert_eq!(pv.delete.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), vec!["only-here/a.txt"]);
    assert_eq!(pv.download_bytes, 11);

    let rep = run(&recv, &o).await.unwrap();
    assert_eq!(read(&dst, "keep.txt"), b"source\n");
    assert_eq!(read(&dst, "sub/new.txt"), b"new\n");
    assert!(!dst.join("only-here").exists(), "receiver-only file and its empty dir deleted");
    assert!(!dst.join("secret/s.txt").exists());
    assert_eq!(read(&dst, "secret/mine.txt"), b"never touched\n");
    assert_eq!(read(&dst, "node_modules/y.js"), b"never touched\n");
    assert!(!dst.join("node_modules/x.js").exists());
    assert_eq!(read(&dst, ".devdy/runs/r.log"), b"devdy data\n");
    let bk = rep.backup.folder.clone().unwrap();
    assert_eq!(read(&bk, "keep.txt"), b"receiver version\n");
    assert_eq!(read(&bk, "only-here/a.txt"), b"receiver only\n");
    assert!(!bk.join("same.txt").exists());
    assert!(rep.backup.describe().contains(".devdy/sync-backup"));
}

#[tokio::test]
async fn oversize_files_are_skipped_and_kept() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    write(&src, "big.bin", &vec![7u8; 2 * 1024 * 1024 + 1]);
    write(&src, "small.txt", b"s\n");
    write(&dst, "big.bin", b"receiver's own big file\n");
    let mut o = opts(&dst);
    o.max_file_mb = 2;
    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &o).await;
    assert_eq!(pv.too_large.len(), 1);
    assert_eq!(pv.too_large[0].path, "big.bin");
    assert_eq!(pv.too_large[0].size, 2 * 1024 * 1024 + 1);
    assert!(pv.delete.is_empty() && pv.overwrite.is_empty());
    run(&recv, &o).await.unwrap();
    assert_eq!(read(&dst, "small.txt"), b"s\n");
    assert_eq!(read(&dst, "big.bin"), b"receiver's own big file\n");
}

/// Serves a crafted manifest (a hostile or buggy source).
struct EvilSource(Manifest);

#[async_trait]
impl PeerHandler for EvilSource {
    fn kinds(&self) -> &'static [&'static str] {
        &[KIND_MANIFEST]
    }
    async fn handle(&self, link: Arc<dyn PeerLink>, _from: String, _kind: String, _body: Value, reply: Arc<dyn Reply>) {
        let sha = link.blob_put(serde_json::to_vec(&self.0).unwrap()).await.unwrap();
        let _ = reply.send(REPLY_MANIFEST, json!({ "manifest_blobs": [sha], "bundles": {} }), true).await;
    }
}

#[tokio::test]
async fn path_traversal_is_rejected() {
    let t = tempdir();
    let dst = t.path().join("dst");
    write(&dst, "x.txt", b"x\n");
    for bad in ["../outside.txt", "/etc/evil", "a/../../b", ".git/hooks/pre-commit", "sub/.git/config"] {
        let m = Manifest {
            v: MANIFEST_VERSION,
            files: vec![FileEntry { path: bad.into(), size: 1, mode: 0o644, sha256: "00".repeat(32), ..Default::default() }],
            ..Default::default()
        };
        let (recv, src) = pair("recv", "src");
        src.add_handler(Arc::new(EvilSource(m)));
        let l: Arc<dyn PeerLink> = recv.clone();
        let err = apply::preview(l.clone(), "src", &opts(&dst)).await.unwrap_err();
        assert!(err.contains("unsafe path"), "{bad}: {err}");
        let (s, _) = sink();
        let err = apply::apply(l, "src", &opts(&dst), s, Cancel::default()).await.unwrap_err();
        assert!(err.contains("unsafe path"), "{bad}: {err}");
    }
    // Escaping symlink targets are rejected too.
    let m = Manifest {
        v: MANIFEST_VERSION,
        files: vec![FileEntry { path: "l".into(), link: Some("../../etc".into()), ..Default::default() }],
        ..Default::default()
    };
    let (recv, src) = pair("recv", "src");
    src.add_handler(Arc::new(EvilSource(m)));
    let l: Arc<dyn PeerLink> = recv.clone();
    assert!(apply::preview(l, "src", &opts(&dst)).await.unwrap_err().contains("unsafe symlink"));
    assert!(!t.path().join("outside.txt").exists());
    assert_eq!(fs::read_dir(&dst).unwrap().count(), 1, "nothing written");
}

#[cfg(unix)]
#[tokio::test]
async fn never_writes_through_a_receiver_symlink() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    let outside = t.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    write(&src, "d/file.txt", b"payload\n");
    fs::create_dir_all(&dst).unwrap();
    std::os::unix::fs::symlink(&outside, dst.join("d")).unwrap();
    let (recv, _) = link_to(&src);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert!(rep.skipped.iter().any(|s| s.path == "d/file.txt" && s.reason == "path_conflict"));
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert!(fs::symlink_metadata(dst.join("d")).unwrap().file_type().is_symlink());
}

#[tokio::test]
async fn resumes_after_a_partial_transfer() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    const MB: usize = 1024 * 1024;
    for (i, name) in ["a.bin", "b.bin", "c.bin"].iter().enumerate() {
        write(&src, name, &vec![i as u8 + 1; 5 * MB]); // > one blob each
    }
    let (recv, _) = link_to(&src);
    let l: Arc<dyn PeerLink> = recv.clone();

    // Stop once the first file is written.
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    let s: ProgressSink = Arc::new(move |p: &FolderProgress| {
        if p.phase == "files" && p.files_done >= 1 {
            c2.cancel();
        }
    });
    let err = apply::apply(l.clone(), "src", &opts(&dst), s, cancel).await.unwrap_err();
    assert!(err.contains("cancelled") && err.contains("backup:"), "{err}");
    assert_eq!(read(&dst, "a.bin"), vec![1u8; 5 * MB]);
    assert!(!dst.join("c.bin").exists());
    assert!(!dst.join(apply::STAGING).exists());

    let (s, ev) = sink();
    let rep = apply::apply(l, "src", &opts(&dst), s, Cancel::default()).await.unwrap();
    assert_eq!(rep.bytes_downloaded, (10 * MB) as u64, "the finished file is not fetched again");
    assert_eq!(tree(&dst, &[]), tree(&src, &[]));
    let phases: Vec<String> = ev.lock().unwrap().iter().map(|p| p.phase.clone()).collect::<Vec<_>>();
    let mut uniq = phases.clone();
    uniq.dedup();
    assert_eq!(uniq, vec!["manifest", "backup", "git", "files", "cleanup"]);
}

#[tokio::test]
async fn detached_head_and_offline_source() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let c1 = commit(&src, "a.txt", "1\n", "c1");
    commit(&src, "a.txt", "2\n", "c2");
    git(&src, &["checkout", "-q", "--detach", &c1]);
    let dst = t.path().join("dst");
    let (recv, _) = link_to(&src);
    run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), c1);
    assert_eq!(status(&dst), status(&src));

    recv.online.store(false, std::sync::atomic::Ordering::SeqCst);
    let l: Arc<dyn PeerLink> = recv.clone();
    assert!(apply::preview(l, "src", &opts(&dst)).await.unwrap_err().contains("peer_offline"));
}

#[test]
fn keeps_five_backups() {
    let t = tempdir();
    let d = t.path();
    git(d, &["init", "-q"]);
    commit(d, "a.txt", "1\n", "c");
    for i in 0..7 {
        let ts = format!("20260101-00000{i}-000");
        assert!(backup::backup_repo(d, &ts).unwrap());
        backup::backup_branch_tip(d, &ts, "main", &git(d, &["rev-parse", "HEAD"])).unwrap();
        backup::backup_files(d, &ts, &["a.txt".into()]).unwrap();
    }
    backup::prune_repo(d).unwrap();
    backup::prune_folder(d).unwrap();
    let refs = git(d, &["for-each-ref", "--format=%(refname:short)", "refs/heads/devdy-sync-backup/"]);
    assert_eq!(refs.lines().count(), 10, "{refs}");
    assert!(!refs.contains("20260101-000000") && !refs.contains("20260101-000001"));
    assert_eq!(fs::read_dir(d.join(backup::FOLDER)).unwrap().count(), 5);
    // The checked-out branch, index and worktree are untouched.
    assert_eq!(git(d, &["status", "--porcelain"]), "?? .devdy/");
}
