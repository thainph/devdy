//! End-to-end folder sync over `peer::testing::pair` with real `git`.

use super::apply::{self, ApplyReport};
use super::handler::{FolderSource, ManifestReady, RootResolver};
use super::manifest::{FileEntry, Manifest, MANIFEST_VERSION};
use super::{backup, Cancel, FolderOptions, FolderProgress, ProgressSink, KIND_MANIFEST, REPLY_MANIFEST};
use crate::sync::files::tests::{tempdir, TempDir};
use crate::sync::peer::testing::{pair, MemLink};
use crate::sync::peer::{decode_chunk, encode_chunk, PeerHandler, PeerLink, Reply, CHUNK_SIZE};
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
    apply::apply(l, "src", o, s, Cancel::default(), Default::default()).await
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
    assert_eq!(recv.blob_count(), 0, "no relay blobs");

    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_same(&src, &dst);
    assert_eq!(read(&dst, "feat.txt"), b"feature staged then edited\n");
    assert!(!dst.join("node_modules").exists());
    assert!(!dst.join("escape-link").exists());
    assert!(!dst.join(apply::STAGING).exists());
    assert!(rep.bytes_downloaded > 0);
    assert_eq!(recv.blob_count(), 0, "no relay blobs");

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
    async fn handle(&self, _link: Arc<dyn PeerLink>, _from: String, _kind: String, _body: Value, reply: Arc<dyn Reply>) {
        let _ = reply.send(REPLY_MANIFEST, ready("evil", &self.0), true).await;
    }
}

/// A `folder_manifest_ready` reply carrying all of `m` inline.
fn ready(transfer: &str, m: &Manifest) -> Value {
    let json = serde_json::to_vec(m).unwrap();
    json!({ "transfer": transfer, "manifest": { "sha256": sha_hex(&json), "size": json.len() }, "data": encode_chunk(&json) })
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
        let err = apply::apply(l, "src", &opts(&dst), s, Cancel::default(), Default::default()).await.unwrap_err();
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
        write(&src, name, &vec![i as u8 + 1; 5 * MB]); // > one chunk each
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
    let err = apply::apply(l.clone(), "src", &opts(&dst), s, cancel, Default::default()).await.unwrap_err();
    assert!(err.contains("cancelled") && err.contains("backup:"), "{err}");
    assert_eq!(read(&dst, "a.bin"), vec![1u8; 5 * MB]);
    assert!(!dst.join("c.bin").exists());
    assert!(!dst.join(apply::STAGING).exists());

    let (s, ev) = sink();
    let rep = apply::apply(l, "src", &opts(&dst), s, Cancel::default(), Default::default()).await.unwrap();
    // Chunks of b.bin already in flight may have been kept too.
    assert!(rep.bytes_downloaded <= (10 * MB) as u64, "the finished file is not fetched again");
    assert!(!dst.join(apply::PARTIAL).exists());
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
fn keeps_five_backups_that_hold_data() {
    let t = tempdir();
    let d = t.path();
    git(d, &["init", "-q"]);
    commit(d, "a.txt", "1\n", "c");
    let head = git(d, &["rev-parse", "HEAD"]);
    for i in 0..7 {
        let ts = format!("20260101-00000{i}-000");
        write(d, "a.txt", format!("dirty {i}\n").as_bytes());
        assert!(backup::backup_repo(d, &ts).unwrap().is_some());
        // A rewound tip only this backup still reaches.
        let lost = git(d, &["commit-tree", &format!("{head}^{{tree}}"), "-p", &head, "-m", &format!("lost {i}")]);
        backup::backup_branch_tip(d, &ts, "main", &lost).unwrap();
        backup::backup_files(d, &ts, &["a.txt".into()]).unwrap();
    }
    // Newer backups holding nothing (as older builds made them): they must
    // not push a useful one out, and go themselves.
    for i in 0..3 {
        backup::backup_branch_tip(d, &format!("20260102-00000{i}-000"), "main", &head).unwrap();
    }
    let before = git(d, &["status", "--porcelain"]);
    backup::prune_repo(d).unwrap();
    backup::prune_folder(d).unwrap();
    let refs = git(d, &["for-each-ref", "--format=%(refname:short)", "refs/heads/devdy-sync-backup/"]);
    assert_eq!(refs.lines().count(), 10, "{refs}");
    for i in 2..7 {
        assert!(refs.contains(&format!("20260101-00000{i}-000")), "{refs}");
    }
    assert!(!refs.contains("20260102") && !refs.contains("20260101-000000") && !refs.contains("20260101-000001"));
    assert_eq!(fs::read_dir(d.join(backup::FOLDER)).unwrap().count(), 5);
    // The checked-out branch, index and worktree are untouched.
    assert_eq!(git(d, &["status", "--porcelain"]), before);
    assert_eq!(git(d, &["rev-parse", "HEAD"]), head);
}

#[test]
fn no_backup_when_nothing_would_be_lost() {
    let t = tempdir();
    let d = t.path();
    git(d, &["init", "-q"]);
    let c1 = commit(d, "a.txt", "1\n", "c1");
    assert!(backup::backup_repo(d, "20260101-000000-000").unwrap().is_none(), "clean");
    // Staged only.
    write(d, "a.txt", b"2\n");
    git(d, &["add", "a.txt"]);
    assert!(backup::backup_repo(d, "20260101-000001-000").unwrap().is_some(), "staged change");
    git(d, &["reset", "-q", "--hard"]);
    // Clean, but HEAD is a detached commit no ref reaches.
    git(d, &["checkout", "-q", "--detach"]);
    commit(d, "b.txt", "b\n", "detached");
    assert!(backup::backup_repo(d, "20260101-000002-000").unwrap().is_some(), "unreachable HEAD");
    git(d, &["checkout", "-q", &c1]);
    assert!(backup::backup_repo(d, "20260101-000003-000").unwrap().is_none(), "detached on a reachable commit");
    let refs = git(d, &["for-each-ref", "--format=%(refname:short)", "refs/heads/devdy-sync-backup/"]);
    assert_eq!(refs.lines().count(), 2, "{refs}");
}

#[tokio::test]
async fn a_useful_backup_survives_later_noop_syncs() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "a.txt", "a\n", "c1");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    write(&dst, "a.txt", b"local edit\n");
    let (recv, _) = link_to(&src);
    let first = run(&recv, &opts(&dst)).await.unwrap();
    let bk = format!("devdy-sync-backup/{}", first.backup.ts);
    for _ in 0..7 {
        let rep = run(&recv, &opts(&dst)).await.unwrap();
        assert!(rep.backup.repos.is_empty() && rep.backup.folder.is_none(), "nothing to back up: {:?}", rep.backup);
    }
    let refs = git(&dst, &["for-each-ref", "--format=%(refname:short)", "refs/heads/devdy-sync-backup/"]);
    assert_eq!(refs, bk);
    assert_eq!(git(&dst, &["show", &format!("{bk}:a.txt")]), "local edit");
}

// ---- regressions (review findings)

async fn run_with(link: &Arc<MemLink>, o: &FolderOptions, s: ProgressSink, guard: apply::ApplyGuard) -> Result<ApplyReport, String> {
    let l: Arc<dyn PeerLink> = link.clone();
    apply::apply(l, "src", o, s, Cancel::default(), guard).await
}

fn evil(m: Manifest) -> Arc<MemLink> {
    let (recv, src) = pair("recv", "src");
    src.add_handler(Arc::new(EvilSource(m)));
    recv
}

fn sha_hex(data: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(data))
}

#[tokio::test]
async fn malformed_content_ids_are_rejected() {
    let t = tempdir();
    let dst = t.path().join("dst");
    write(&dst, "x.txt", b"x\n");
    let file = |sha: &str| FileEntry { path: "f.txt".into(), size: 5, mode: 0o644, sha256: sha.into(), ..Default::default() };
    let mut cases = vec![];
    for bad in ["../../x.txt", "/etc/passwd", "", &"A".repeat(64), &format!("{}/..", "a".repeat(61))] {
        cases.push(Manifest { v: MANIFEST_VERSION, files: vec![file(bad)], ..Default::default() });
        cases.push(Manifest {
            v: MANIFEST_VERSION,
            repos: vec![super::manifest::RepoManifest { path: "r".into(), dirty: vec![FileEntry { path: "r/f".into(), ..file(bad) }], ..Default::default() }],
            ..Default::default()
        });
        cases.push(Manifest {
            v: MANIFEST_VERSION,
            repos: vec![super::manifest::RepoManifest {
                path: "r".into(),
                bundle: Some(super::manifest::BundleRef { sha256: bad.into(), size: 1 }),
                ..Default::default()
            }],
            ..Default::default()
        });
    }
    for m in cases {
        let recv = evil(m);
        let l: Arc<dyn PeerLink> = recv.clone();
        let err = apply::preview(l.clone(), "src", &opts(&dst)).await.unwrap_err();
        assert!(err.contains("bad content id"), "{err}");
        let (s, _) = sink();
        assert!(apply::apply(l, "src", &opts(&dst), s, Cancel::default(), Default::default()).await.unwrap_err().contains("bad content id"));
    }
    assert_eq!(fs::read_dir(&dst).unwrap().count(), 1);
}

#[tokio::test]
async fn symlink_chains_and_paths_through_links_are_rejected() {
    let t = tempdir();
    let dst = t.path().join("dst");
    let link = |p: &str, to: &str| FileEntry { path: p.into(), link: Some(to.into()), ..Default::default() };
    let chain = Manifest { v: MANIFEST_VERSION, files: vec![link("a/l1", ".."), link("a/l2", "l1/..")], ..Default::default() };
    let l: Arc<dyn PeerLink> = evil(chain);
    assert!(apply::preview(l, "src", &opts(&dst)).await.unwrap_err().contains("unsafe symlink"));
    let through = Manifest {
        v: MANIFEST_VERSION,
        files: vec![link("a/l1", ".."), FileEntry { path: "a/l1/x".into(), size: 1, mode: 0o644, sha256: "a".repeat(64), ..Default::default() }],
        ..Default::default()
    };
    let l: Arc<dyn PeerLink> = evil(through);
    assert!(apply::preview(l, "src", &opts(&dst)).await.unwrap_err().contains("through a symlink"));
}

#[tokio::test]
async fn git_collisions_with_untracked_and_excluded_paths_are_backed_up() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "a.txt", "a\n", "c1");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    commit(&src, "conf/.env.local", "source env\n", "env");
    commit(&src, "build", "build file\n", "build");
    // Receiver: an excluded local file and a default-excluded dir in the way.
    write(&dst, "conf/.env.local", b"mine\n");
    write(&dst, "build/keep.txt", b"keep\n");
    let mut o = opts(&dst);
    o.excludes = vec!["conf/.env.local".into()];
    let (recv, _) = link_to(&src);
    let rep = run(&recv, &o).await.unwrap();
    assert_eq!(read(&dst, "conf/.env.local"), b"source env\n");
    assert_eq!(read(&dst, "build"), b"build file\n");
    let bk = dst.canonicalize().unwrap().join(backup::FOLDER).join(&rep.backup.ts);
    assert_eq!(read(&bk, "conf/.env.local"), b"mine\n");
    assert_eq!(read(&bk, "build/keep.txt"), b"keep\n");
    assert_eq!(rep.backup.folder.as_deref(), Some(bk.as_path()));
}

#[tokio::test]
async fn a_tree_writing_into_a_local_only_repo_skips_the_repo() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let c1 = commit(&src, "a.txt", "a\n", "c1");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    commit(&src, "inner/x.txt", "source\n", "c2");
    write(&src, "a.txt", b"dirty on source\n");
    write(&src, "notes.txt", b"untracked on source\n");
    let inner = dst.join("inner");
    fs::create_dir_all(&inner).unwrap();
    git(&inner, &["init", "-q"]);
    commit(&inner, "y.txt", "mine\n", "inner");
    let (recv, _) = link_to(&src);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert!(rep.skipped.iter().any(|s| s.path.is_empty() && s.reason == "path_conflict"), "{:?}", rep.skipped);
    assert!(!inner.join("x.txt").exists());
    assert_eq!(git(&inner, &["status", "--porcelain"]), "");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), c1, "repo left as it was");
    assert_eq!(read(&dst, "a.txt"), b"a\n", "its dirty files are not written either");
    assert!(!dst.join("notes.txt").exists());
}

#[tokio::test]
async fn cleanup_deletes_only_planned_unchanged_files() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    write(&src, "a.txt", b"a\n");
    write(&dst, "old.txt", b"old\n");
    write(&dst, "old2.txt", b"old2\n");
    let d2 = dst.clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let s: ProgressSink = Arc::new(move |p: &FolderProgress| {
        if p.phase == "files" && !done.swap(true, std::sync::atomic::Ordering::SeqCst) {
            // Created / edited after planning (an editor, a session, ...).
            write(&d2, "late.txt", b"late\n");
            write(&d2, "old2.txt", b"edited meanwhile\n");
        }
    });
    let (recv, _) = link_to(&src);
    let rep = run_with(&recv, &opts(&dst), s, Default::default()).await.unwrap();
    assert!(!dst.join("old.txt").exists());
    assert_eq!(read(&dst, "old2.txt"), b"edited meanwhile\n");
    assert_eq!(read(&dst, "late.txt"), b"late\n");
    assert!(rep.skipped.iter().any(|s| s.path == "old2.txt" && s.reason == "changed_since_preview"));
}

#[tokio::test]
async fn precheck_and_plan_growth_stop_before_any_change() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    write(&src, "a.txt", b"a\n");
    write(&dst, "old.txt", b"old\n");
    let (recv, _) = link_to(&src);

    // A session started meanwhile.
    let pc: apply::Precheck = Arc::new(|| Box::pin(async { Err("running_session: started meanwhile".to_string()) }));
    let (s, _) = sink();
    let err = run_with(&recv, &opts(&dst), s, apply::ApplyGuard { precheck: Some(pc), ..Default::default() }).await.unwrap_err();
    assert!(err.contains("running_session"), "{err}");
    assert!(dst.join("old.txt").exists() && !dst.join("a.txt").exists());

    // More would be deleted than the preview showed.
    let l: Arc<dyn PeerLink> = recv.clone();
    let (_, expected) = apply::preview_expected(l, "src", &opts(&dst)).await.unwrap();
    write(&dst, "new-here.txt", b"created after the preview\n");
    let (s, _) = sink();
    let guard = apply::ApplyGuard { expected: Some(expected.clone()), ..Default::default() };
    let err = run_with(&recv, &opts(&dst), s, guard).await.unwrap_err();
    assert!(err.starts_with("plan_changed:"), "{err}");
    assert!(dst.join("new-here.txt").exists() && !dst.join("a.txt").exists());

    // As confirmed: goes through.
    fs::remove_file(dst.join("new-here.txt")).unwrap();
    let (s, _) = sink();
    run_with(&recv, &opts(&dst), s, apply::ApplyGuard { expected: Some(expected), ..Default::default() }).await.unwrap();
    assert!(!dst.join("old.txt").exists() && dst.join("a.txt").exists());
}

#[tokio::test]
async fn case_only_differences_take_the_source_spelling() {
    let t = tempdir();
    if !super::paths::case_insensitive(t.path()) {
        return; // case-sensitive volume: nothing to test
    }
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    write(&src, "foo.txt", b"new\n");
    write(&src, "docs/a.md", b"new doc\n");
    write(&dst, "Foo.txt", b"old\n");
    write(&dst, "Docs/a.md", b"old doc\n");
    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &opts(&dst)).await;
    assert!(pv.delete.is_empty(), "{:?}", pv.delete);
    run(&recv, &opts(&dst)).await.unwrap();
    let mut names: Vec<String> = fs::read_dir(&dst).unwrap().filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n != ".devdy").collect();
    names.sort();
    assert_eq!(names, vec!["docs", "foo.txt"]);
    assert_eq!(read(&dst, "foo.txt"), b"new\n");
    assert_eq!(read(&dst, "docs/a.md"), b"new doc\n");
}

#[tokio::test]
async fn hidden_edits_are_backed_up_and_the_switch_succeeds() {
    for flag in ["--assume-unchanged", "--skip-worktree"] {
        let t = tempdir();
        let src = t.path().join("src");
        fs::create_dir_all(&src).unwrap();
        git(&src, &["init", "-q"]);
        commit(&src, "cfg", "base\n", "c1");
        git(&src, &["checkout", "-q", "-b", "other"]);
        let other = commit(&src, "cfg", "other\n", "c2");
        let dst = t.path().join("dst");
        git(t.path(), &["clone", "-q", "-b", "main", src.to_str().unwrap(), dst.to_str().unwrap()]);
        git(&dst, &["update-index", flag, "cfg"]);
        write(&dst, "cfg", b"LOCAL\n");
        let (recv, _) = link_to(&src);
        let rep = run(&recv, &opts(&dst)).await.unwrap();
        assert_eq!(git(&dst, &["rev-parse", "HEAD"]), other);
        assert_eq!(read(&dst, "cfg"), b"other\n", "{flag}");
        assert_eq!(git(&dst, &["show", &format!("devdy-sync-backup/{}:cfg", rep.backup.ts)]), "LOCAL", "{flag}");
    }
}

/// Wraps the real source and edits the manifest it serves.
struct Tamper {
    inner: Arc<FolderSource>,
    edit: fn(&mut Manifest),
}

struct Capture(Mutex<Vec<(String, Value)>>);

#[async_trait]
impl Reply for Capture {
    async fn send(&self, kind: &str, body: Value, _last: bool) -> Result<(), String> {
        self.0.lock().unwrap().push((kind.to_string(), body));
        Ok(())
    }
}

#[async_trait]
impl PeerHandler for Tamper {
    fn kinds(&self) -> &'static [&'static str] {
        &[KIND_MANIFEST, super::KIND_FETCH, super::KIND_DONE]
    }
    async fn handle(&self, link: Arc<dyn PeerLink>, from: String, kind: String, body: Value, reply: Arc<dyn Reply>) {
        if kind != KIND_MANIFEST {
            return self.inner.handle(link, from, kind, body, reply).await;
        }
        let cap = Arc::new(Capture(Mutex::new(vec![])));
        self.inner.handle(link.clone(), from, kind, body, cap.clone()).await;
        let (_, b) = cap.0.lock().unwrap()[0].clone();
        let r: ManifestReady = serde_json::from_value(b).unwrap();
        let mut m: Manifest = serde_json::from_slice(&decode_chunk(&r.data).unwrap()).unwrap();
        (self.edit)(&mut m);
        let _ = reply.send(REPLY_MANIFEST, ready(&r.transfer, &m), true).await;
    }
}

#[tokio::test]
async fn a_failed_switch_puts_the_repo_back() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let c1 = commit(&src, "a.txt", "a\n", "c1");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    git(&src, &["checkout", "-q", "-b", "feature"]);
    commit(&src, "a.txt", "feature\n", "c2");
    write(&dst, "a.txt", b"local edit\n");
    write(&dst, "b.txt", b"staged here\n");
    git(&dst, &["add", "b.txt"]);
    let before = status(&dst);

    let (recv, srcl) = pair("recv", "src");
    let inner = Arc::new(FolderSource::new(Arc::new(MapRoots(src.clone()))));
    use base64::Engine;
    srcl.add_handler(Arc::new(Tamper {
        inner,
        edit: |m| m.repos[0].index_patch = base64::engine::general_purpose::STANDARD.encode(b"diff --git a/zz b/zz\nthis is not a patch\n"),
    }));
    let err = run(&recv, &opts(&dst)).await.unwrap_err();
    assert!(err.contains("were put back as they were") && err.contains("backup:"), "{err}");
    assert_eq!(git(&dst, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), c1);
    assert!(git(&dst, &["branch", "--list", "feature"]).is_empty());
    assert_eq!(read(&dst, "a.txt"), b"local edit\n");
    assert_eq!(status(&dst), before);
}

#[tokio::test]
async fn excluded_tracked_paths_keep_their_committed_content() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "secret/x", "base\n", "c1");
    commit(&src, "secret/y", "y\n", "c2");
    write(&src, "secret/x", b"source dirty\n");
    fs::remove_file(src.join("secret/y")).unwrap();
    let dst = t.path().join("dst");
    let mut o = opts(&dst);
    o.excludes = vec!["secret/".into()];
    let (recv, _) = link_to(&src);
    run(&recv, &o).await.unwrap();
    assert_eq!(read(&dst, "secret/x"), b"base\n");
    assert_eq!(read(&dst, "secret/y"), b"y\n");
}

#[tokio::test]
async fn branches_checked_out_in_linked_worktrees_are_left_alone() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "a.txt", "a\n", "c1");
    git(&src, &["branch", "feature"]);
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    git(&dst, &["branch", "feature", "origin/feature"]);
    let wt = t.path().join("wt");
    git(&dst, &["worktree", "add", "-q", wt.to_str().unwrap(), "feature"]);
    let old_feature = git(&dst, &["rev-parse", "feature"]);
    git(&src, &["checkout", "-q", "feature"]);
    let new_feature = commit(&src, "f.txt", "f\n", "f1");

    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &opts(&dst)).await;
    assert!(pv.skipped_items.iter().any(|s| s.path == "feature" && s.reason == "branch_in_worktree"), "{:?}", pv.skipped_items);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert!(rep.skipped.iter().any(|s| s.path == "feature" && s.reason == "branch_in_worktree"));
    assert_eq!(git(&dst, &["rev-parse", "feature"]), old_feature);
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), new_feature, "detached on the source's commit");
    assert_eq!(git(&wt, &["symbolic-ref", "--short", "HEAD"]), "feature");
    assert_eq!(git(&wt, &["status", "--porcelain"]), "");
}

#[tokio::test]
async fn lfs_pointers_keep_local_real_content() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let real = b"real large file content\n".to_vec();
    let pointer = |data: &[u8]| format!("version https://git-lfs.github.com/spec/v1\noid sha256:{}\nsize {}\n", sha_hex(data), data.len());
    write(&src, ".gitattributes", b"*.bin filter=lfs diff=lfs merge=lfs -text\n");
    write(&src, "big.bin", pointer(&real).as_bytes());
    write(&src, "other.bin", pointer(b"not here").as_bytes());
    // Commit the pointer texts as they are (no LFS filter involved).
    git(&src, &["-c", "filter.lfs.clean=cat", "-c", "filter.lfs.smudge=cat", "-c", "filter.lfs.process=", "-c", "filter.lfs.required=false", "add", "."]);
    git(&src, &["-c", "filter.lfs.clean=cat", "-c", "filter.lfs.process=", "-c", "filter.lfs.required=false", "commit", "-q", "-m", "lfs"]);
    let dst = t.path().join("dst");
    write(&dst, "big.bin", &real);
    let (recv, _) = link_to(&src);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(read(&dst, "big.bin"), real);
    assert_eq!(read(&dst, "other.bin"), pointer(b"not here").into_bytes());
    assert!(rep.skipped.iter().any(|s| s.path == "other.bin" && s.reason == "lfs"), "{:?}", rep.skipped);
    assert!(!rep.skipped.iter().any(|s| s.path == "big.bin"));
}

#[tokio::test]
async fn a_ref_clash_leaves_the_repo_untouched() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let c1 = commit(&src, "a.txt", "one\n", "c1");
    let dst = t.path().join("dst");
    let mut o = opts(&dst);
    o.excludes = vec!["conf/x.local".into()];
    let (recv, _) = link_to(&src);
    run(&recv, &o).await.unwrap();
    // Here: branch `feature` and an excluded local file; the source tracks
    // that file and has `feature/x` (cannot coexist with `feature`).
    git(&dst, &["branch", "feature"]);
    write(&dst, "conf/x.local", b"MY SECRET\n");
    git(&src, &["checkout", "-q", "-b", "other"]);
    commit(&src, "conf/x.local", "committed\n", "c2");
    git(&src, &["branch", "feature/x"]);
    let rep = run(&recv, &o).await.unwrap();
    assert!(rep.skipped.iter().any(|s| s.path.is_empty() && s.reason == "ref_conflict"), "{:?}", rep.skipped);
    assert_eq!(read(&dst, "conf/x.local"), b"MY SECRET\n");
    assert_eq!(git(&dst, &["symbolic-ref", "--short", "HEAD"]), "main");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), c1);
    assert!(git(&dst, &["branch", "--list", "other"]).is_empty());
}

#[tokio::test]
async fn a_failed_switch_returns_the_files_it_replaced() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    let c1 = commit(&src, "a.txt", "a\n", "c1");
    let dst = t.path().join("dst");
    git(t.path(), &["clone", "-q", src.to_str().unwrap(), dst.to_str().unwrap()]);
    commit(&src, "conf/x.local", "committed\n", "c2");
    write(&dst, "conf/x.local", b"MY SECRET\n");
    let mut o = opts(&dst);
    o.excludes = vec!["conf/x.local".into()];
    let (recv, srcl) = pair("recv", "src");
    let inner = Arc::new(FolderSource::new(Arc::new(MapRoots(src.clone()))));
    use base64::Engine;
    srcl.add_handler(Arc::new(Tamper {
        inner,
        edit: |m| m.repos[0].index_patch = base64::engine::general_purpose::STANDARD.encode(b"diff --git a/zz b/zz\nnot a patch\n"),
    }));
    let err = run(&recv, &o).await.unwrap_err();
    assert!(err.contains("were put back as they were"), "{err}");
    assert!(err.contains(".devdy/sync-backup"), "names the folder backup: {err}");
    assert_eq!(read(&dst, "conf/x.local"), b"MY SECRET\n");
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), c1);
    assert_eq!(git(&dst, &["status", "--porcelain"]), "?? .devdy/\n?? conf/", "as before the sync");
}

#[tokio::test]
async fn repos_with_submodules_are_synced() {
    let t = tempdir();
    let s = t.path().join("src");
    fs::create_dir_all(&s).unwrap();
    git(&s, &["init", "-q"]);
    let c1 = commit(&s, "a.txt", "one\n", "c1");
    git(&s, &["update-index", "--add", "--cacheinfo", &format!("160000,{c1},vendor/lib")]);
    git(&s, &["commit", "-q", "-m", "sub"]);
    let dst = t.path().join("dst");
    let (recv, _) = link_to(&s);
    run(&recv, &opts(&dst)).await.unwrap();
    assert!(dst.join("vendor/lib").is_dir(), "gitlink dir");
    commit(&s, "b.txt", "two\n", "c3");
    write(&s, "a.txt", b"dirty on source\n");
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert!(!rep.skipped.iter().any(|x| x.reason == "path_conflict"), "{:?}", rep.skipped);
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), git(&s, &["rev-parse", "HEAD"]));
    assert_eq!(read(&dst, "a.txt"), b"dirty on source\n");
}

#[cfg(unix)]
#[tokio::test]
async fn a_path_backed_up_twice_does_not_fail() {
    let t = tempdir();
    let s = t.path().join("src");
    fs::create_dir_all(&s).unwrap();
    git(&s, &["init", "-q"]);
    commit(&s, "a.txt", "one\n", "c1");
    std::os::unix::fs::symlink("a.txt", s.join("cur")).unwrap();
    git(&s, &["add", "cur"]);
    git(&s, &["commit", "-q", "-m", "link"]);
    let dst = t.path().join("dst");
    write(&dst, "a.txt", b"old copy\n");
    std::os::unix::fs::symlink("a.txt", dst.join("cur")).unwrap();
    let (recv, _) = link_to(&s);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    let bk = dst.canonicalize().unwrap().join(backup::FOLDER).join(&rep.backup.ts);
    assert_eq!(fs::read_link(bk.join("cur")).unwrap(), PathBuf::from("a.txt"));
    assert_eq!(read(&bk, "a.txt"), b"old copy\n");
    assert_eq!(read(&dst, "a.txt"), b"one\n");
}

// ---- inline chunks (the relay stores nothing)

/// Incompressible bytes (xorshift).
fn noise(n: usize, seed: u64) -> Vec<u8> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

async fn until(what: &str, f: impl Fn() -> bool) {
    for _ in 0..500 {
        if f() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

#[tokio::test]
async fn a_large_file_resumes_mid_file_after_cancel() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    let big = noise(5 * CHUNK_SIZE + 12345, 7); // > 3 chunks
    write(&src, "big.bin", &big);
    let (recv, _) = link_to(&src);
    let l: Arc<dyn PeerLink> = recv.clone();

    // Stop after the first chunk of the file.
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    let s: ProgressSink = Arc::new(move |p: &FolderProgress| {
        if p.phase == "files" && p.done_bytes >= CHUNK_SIZE as u64 {
            c2.cancel();
        }
    });
    let err = apply::apply(l.clone(), "src", &opts(&dst), s, cancel, Default::default()).await.unwrap_err();
    assert!(err.contains("cancelled"), "{err}");
    assert!(!dst.join("big.bin").exists());
    let part = dst.join(apply::PARTIAL).join(format!("{}.part", sha_hex(&big)));
    let kept = fs::metadata(&part).unwrap().len();
    assert!(kept >= CHUNK_SIZE as u64 && kept < big.len() as u64, "{kept}");

    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(rep.bytes_downloaded, big.len() as u64 - kept, "resumed where it stopped");
    assert_eq!(read(&dst, "big.bin"), big);
    assert!(!dst.join(apply::PARTIAL).exists());
    assert_eq!(recv.blob_count(), 0, "no relay blobs");
}

#[tokio::test]
async fn a_corrupt_partial_is_not_trusted() {
    let t = tempdir();
    let src = t.path().join("src");
    let dst = t.path().join("dst");
    let big = noise(2 * CHUNK_SIZE + 1, 3);
    write(&src, "big.bin", &big);
    write(&dst, &format!("{}/{}.part", apply::PARTIAL, sha_hex(&big)), &vec![0u8; CHUNK_SIZE]);
    let (recv, _) = link_to(&src);
    let err = run(&recv, &opts(&dst)).await.unwrap_err();
    assert!(err.contains("failed verification"), "{err}");
    // The bad prefix is gone: the next sync fetches it all.
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert_eq!(rep.bytes_downloaded, big.len() as u64);
    assert_eq!(read(&dst, "big.bin"), big);
}

#[tokio::test]
async fn a_bundle_larger_than_a_chunk() {
    let t = tempdir();
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    write(&src, "blob.dat", &noise(3 * CHUNK_SIZE, 11));
    git(&src, &["add", "blob.dat"]);
    git(&src, &["commit", "-q", "-m", "big"]);
    let dst = t.path().join("dst");
    let (recv, _) = link_to(&src);
    let pv = preview(&recv, &opts(&dst)).await;
    assert!(pv.repos[0].bundle_bytes > 2 * CHUNK_SIZE as u64, "{}", pv.repos[0].bundle_bytes);
    let rep = run(&recv, &opts(&dst)).await.unwrap();
    assert!(rep.bytes_downloaded > 2 * CHUNK_SIZE as u64);
    assert_eq!(git(&dst, &["rev-parse", "HEAD"]), git(&src, &["rev-parse", "HEAD"]));
    assert_eq!(read(&dst, "blob.dat"), read(&src, "blob.dat"));
    assert_eq!(status(&dst), status(&src));
    assert_eq!(recv.blob_count(), 0, "no relay blobs");
}

/// A source repo with one commit to bundle; (receiver link, source handler, bundle dir).
fn bundling_source(t: &TempDir, ttl: std::time::Duration) -> (Arc<MemLink>, Arc<FolderSource>, PathBuf, PathBuf) {
    let src = t.path().join("src");
    fs::create_dir_all(&src).unwrap();
    git(&src, &["init", "-q"]);
    commit(&src, "a.txt", "a\n", "c1");
    let tmp = t.path().join("bundles");
    let (recv, srcl) = pair("recv", "src");
    let fs_ = Arc::new(FolderSource::with(Arc::new(MapRoots(src.clone())), tmp.clone(), ttl));
    srcl.add_handler(fs_.clone());
    (recv, fs_, src, tmp)
}

fn count(dir: &Path) -> usize {
    fs::read_dir(dir).map(|d| d.count()).unwrap_or(0)
}

#[tokio::test]
async fn bundles_live_only_while_their_transfer_runs() {
    let long = std::time::Duration::from_secs(3600);
    // Done after a successful apply.
    let t = tempdir();
    let (recv, source, _, tmp) = bundling_source(&t, long);
    let dst = t.path().join("dst");
    run(&recv, &opts(&dst)).await.unwrap();
    until("transfer ended", || source.transfers() == 0 && count(&tmp) == 0).await;

    // Done after a cancelled apply.
    let t = tempdir();
    let (recv, source, _, tmp) = bundling_source(&t, long);
    let dst = t.path().join("dst");
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    let seen = Arc::new(Mutex::new(0usize));
    let (seen2, tmp2) = (seen.clone(), tmp.clone());
    let s: ProgressSink = Arc::new(move |p: &FolderProgress| {
        if p.phase == "backup" {
            *seen2.lock().unwrap() = count(&tmp2);
            c2.cancel();
        }
    });
    let l: Arc<dyn PeerLink> = recv.clone();
    let err = apply::apply(l, "src", &opts(&dst), s, cancel, Default::default()).await.unwrap_err();
    assert!(err.contains("cancelled"), "{err}");
    assert_eq!(*seen.lock().unwrap(), 1, "the bundle exists during the transfer");
    until("transfer ended", || source.transfers() == 0 && count(&tmp) == 0).await;

    // Dropped when idle (the receiver went away without a word).
    let t = tempdir();
    let (recv, source, _, tmp) = bundling_source(&t, std::time::Duration::from_millis(200));
    let l: Arc<dyn PeerLink> = recv.clone();
    let req = super::manifest::ManifestRequest { project_id: "p1".into(), max_file_bytes: u64::MAX, with_bundles: true, ..Default::default() };
    let (m, transfer) = super::transfer::request_manifest(&l, "src", &req, &Cancel::default()).await.unwrap();
    assert!(m.repos[0].bundle.is_some());
    std::mem::forget(transfer);
    assert_eq!((source.transfers(), count(&tmp)), (1, 1));
    until("idle transfer dropped", || source.transfers() == 0 && count(&tmp) == 0).await;

    // A fetch for an ended transfer fails cleanly.
    let item = super::handler::FetchItem { sha: m.repos[0].bundle.clone().unwrap().sha256, offset: 0, len: 1 };
    let mut rx = l.request("src", super::KIND_FETCH, json!({ "transfer": "gone", "items": [item] })).await.unwrap();
    assert!(rx.recv().await.unwrap().unwrap_err().contains("start the sync again"));
}

#[tokio::test]
async fn oversized_fetches_are_refused() {
    let t = tempdir();
    let (recv, _, _, _) = bundling_source(&t, std::time::Duration::from_secs(3600));
    let l: Arc<dyn PeerLink> = recv.clone();
    let req = super::manifest::ManifestRequest { project_id: "p1".into(), max_file_bytes: u64::MAX, with_bundles: true, ..Default::default() };
    let (m, transfer) = super::transfer::request_manifest(&l, "src", &req, &Cancel::default()).await.unwrap();
    let b = m.repos[0].bundle.clone().unwrap();
    let ask = |items: Value| {
        let l = l.clone();
        let id = transfer.id.clone();
        async move { l.request("src", super::KIND_FETCH, json!({ "transfer": id, "items": items })).await.unwrap().recv().await.unwrap() }
    };
    let too_much = json!([{ "sha": b.sha256, "offset": 0, "len": CHUNK_SIZE + 1 }]);
    assert!(ask(too_much).await.unwrap_err().contains("too much"));
    let past_end = json!([{ "sha": b.sha256, "offset": b.size, "len": 1 }]);
    assert!(ask(past_end).await.unwrap_err().contains("bad range"));
    let (kind, body) = ask(json!([{ "sha": b.sha256, "offset": 0, "len": b.size }])).await.unwrap();
    assert_eq!(kind, super::REPLY_CHUNK);
    assert_eq!(decode_chunk(body["chunks"][0]["data"].as_str().unwrap()).unwrap().len() as u64, b.size);
}
