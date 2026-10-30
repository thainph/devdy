//! The `git` CLI, locked down: no hooks, no fsmonitor, no network transport,
//! no credential helpers, no LFS smudge downloads, no submodule recursion,
//! and environment overrides from the parent process removed.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

const LOCKDOWN: &[&str] = &[
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.allow=never",
    "-c",
    "credential.helper=",
    "-c",
    "gc.auto=0",
    "-c",
    "maintenance.auto=false",
    "-c",
    "submodule.recurse=false",
    "-c",
    "advice.detachedHead=false",
    "-c",
    "core.quotePath=false",
];

/// Inherited variables that would point git somewhere else.
const SCRUB_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_EXTERNAL_DIFF",
];

#[derive(Default)]
pub struct Opts<'a> {
    pub stdin: Option<&'a [u8]>,
    pub env: Vec<(&'a str, String)>,
}

pub struct Out {
    pub ok: bool,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

pub fn exec(dir: &Path, args: &[&str], opts: Opts) -> Result<Out, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(LOCKDOWN).args(args);
    for k in SCRUB_ENV {
        c.env_remove(k);
    }
    c.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        // `status` / `diff` must not refresh the index behind our back.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C")
        .env("GIT_AUTHOR_NAME", "Devdy")
        .env("GIT_AUTHOR_EMAIL", "devdy@localhost")
        .env("GIT_COMMITTER_NAME", "Devdy")
        .env("GIT_COMMITTER_EMAIL", "devdy@localhost");
    for (k, v) in &opts.env {
        c.env(k, v);
    }
    c.stdin(if opts.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().map_err(|e| format!("cannot run git: {e}"))?;
    if let Some(input) = opts.stdin {
        let mut sin = child.stdin.take().ok_or("git stdin")?;
        let data = input.to_vec();
        // Write on a thread so a large input cannot deadlock against stdout.
        let writer = std::thread::spawn(move || {
            let _ = sin.write_all(&data);
        });
        let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
        let _ = writer.join();
        return Ok(to_out(out));
    }
    let out = child.wait_with_output().map_err(|e| format!("git: {e}"))?;
    Ok(to_out(out))
}

fn to_out(o: std::process::Output) -> Out {
    Out {
        ok: o.status.success(),
        stdout: o.stdout,
        stderr: String::from_utf8_lossy(&o.stderr).trim().to_string(),
    }
}

fn fail(args: &[&str], o: &Out) -> String {
    let first = o.stderr.lines().next().unwrap_or("").trim();
    format!("git {} failed: {first}", args.first().copied().unwrap_or(""))
}

/// Run, fail on non-zero exit, return stdout.
pub fn run(dir: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    run_with(dir, args, Opts::default())
}

pub fn run_with(dir: &Path, args: &[&str], opts: Opts) -> Result<Vec<u8>, String> {
    let o = exec(dir, args, opts)?;
    if !o.ok {
        return Err(fail(args, &o));
    }
    Ok(o.stdout)
}

/// Trimmed stdout as text.
pub fn text(dir: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&run(dir, args)?).trim().to_string())
}

/// `dir` has a `.git` directory (a plain repo, not a submodule/worktree link).
pub fn is_repo_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir.join(".git")).map(|m| m.is_dir()).unwrap_or(false)
}

pub fn head(dir: &Path) -> Option<String> {
    let o = exec(dir, &["rev-parse", "-q", "--verify", "HEAD^{commit}"], Opts::default()).ok()?;
    o.ok.then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Checked-out branch name; `None` when HEAD is detached.
pub fn current_branch(dir: &Path) -> Option<String> {
    let o = exec(dir, &["symbolic-ref", "-q", "HEAD"], Opts::default()).ok()?;
    if !o.ok {
        return None;
    }
    let r = String::from_utf8_lossy(&o.stdout).trim().to_string();
    r.strip_prefix("refs/heads/").map(|s| s.to_string())
}

pub fn object_format(dir: &Path) -> String {
    text(dir, &["rev-parse", "--show-object-format"]).unwrap_or_else(|_| "sha1".into())
}

pub fn git_dir(dir: &Path) -> Result<std::path::PathBuf, String> {
    Ok(text(dir, &["rev-parse", "--absolute-git-dir"])?.into())
}

/// Local branches: name → (sha, upstream (remote, merge ref)).
pub type Branches = BTreeMap<String, (String, Option<(String, String)>)>;

pub fn branches(dir: &Path) -> Result<Branches, String> {
    let out = run(
        dir,
        &[
            "for-each-ref",
            "--format=%(objectname)%00%(refname)%00%(upstream:remotename)%00%(upstream:remoteref)",
            "refs/heads",
        ],
    )?;
    let mut m = Branches::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 4 {
            continue;
        }
        let Some(name) = f[1].strip_prefix("refs/heads/") else { continue };
        let up = (!f[2].is_empty() && !f[3].is_empty()).then(|| (f[2].to_string(), f[3].to_string()));
        m.insert(name.to_string(), (f[0].to_string(), up));
    }
    Ok(m)
}

/// Remote-tracking refs (full names) → sha, without symbolic `*/HEAD`.
pub fn remote_refs(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let out = run(dir, &["for-each-ref", "--format=%(objectname)%00%(refname)%00%(symref)", "refs/remotes"])?;
    let mut m = BTreeMap::new();
    for line in String::from_utf8_lossy(&out).lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() >= 3 && f[2].is_empty() && !f[1].ends_with("/HEAD") {
            m.insert(f[1].to_string(), f[0].to_string());
        }
    }
    Ok(m)
}

/// Remote name → fetch url.
pub fn remotes(dir: &Path) -> Result<BTreeMap<String, String>, String> {
    let o = exec(dir, &["config", "-z", "--get-regexp", r"^remote\..*\.url$"], Opts::default())?;
    let mut m = BTreeMap::new();
    if !o.ok {
        return Ok(m); // exit 1: none
    }
    for item in String::from_utf8_lossy(&o.stdout).split('\0') {
        let Some((key, val)) = item.split_once('\n') else { continue };
        if let Some(name) = key.strip_prefix("remote.").and_then(|k| k.strip_suffix(".url")) {
            m.entry(name.to_string()).or_insert_with(|| val.to_string());
        }
    }
    Ok(m)
}

/// Commit shas from `shas` that exist in the repo.
pub fn existing_commits(dir: &Path, shas: &[String]) -> Result<Vec<String>, String> {
    if shas.is_empty() {
        return Ok(vec![]);
    }
    let input: String = shas.iter().map(|s| format!("{s}\n")).collect();
    let out = run_with(dir, &["cat-file", "--batch-check=%(objectname) %(objecttype)"], Opts { stdin: Some(input.as_bytes()), ..Default::default() })?;
    Ok(String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| l.strip_suffix(" commit").map(|s| s.to_string()))
        .collect())
}

pub fn is_ancestor(dir: &Path, old: &str, new: &str) -> bool {
    exec(dir, &["merge-base", "--is-ancestor", old, new], Opts::default()).map(|o| o.ok).unwrap_or(false)
}

/// Tracked paths of the index (repo-relative) and whether each is a gitlink.
pub fn ls_files(dir: &Path) -> Result<Vec<(String, bool)>, String> {
    let out = run(dir, &["ls-files", "-z", "-s"])?;
    let mut v = Vec::new();
    for rec in out.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        let rec = String::from_utf8_lossy(rec);
        // "<mode> <sha> <stage>\t<path>"
        let Some((meta, path)) = rec.split_once('\t') else { continue };
        v.push((path.to_string(), meta.starts_with("160000")));
    }
    v.dedup_by(|a, b| a.0 == b.0); // conflict stages
    Ok(v)
}

/// Empty tree id for this repo's object format.
pub fn empty_tree(dir: &Path) -> Result<String, String> {
    let out = run_with(dir, &["hash-object", "-t", "tree", "--stdin"], Opts { stdin: Some(b""), ..Default::default() })?;
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

/// Branches checked out in the repo's *other* linked worktrees (moving them
/// would change those worktrees' HEAD under them).
pub fn branches_checked_out_elsewhere(dir: &Path) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    let Ok(raw) = run(dir, &["worktree", "list", "--porcelain"]) else { return out };
    let me = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    for block in String::from_utf8_lossy(&raw).split("\n\n") {
        let mut wt = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                wt = Some(std::path::PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_string());
            }
        }
        if let (Some(wt), Some(b)) = (wt, branch) {
            if wt.canonicalize().unwrap_or(wt) != me {
                out.insert(b);
            }
        }
    }
    out
}

/// Paths (files, symlinks, gitlinks) of a tree, repo-relative.
pub fn tree_paths(dir: &Path, tree: &str) -> Result<Vec<String>, String> {
    let out = run(dir, &["ls-tree", "-r", "-z", "--name-only", "--full-tree", tree])?;
    Ok(out.split(|b| *b == 0).filter(|r| !r.is_empty()).map(|r| String::from_utf8_lossy(r).to_string()).collect())
}

/// Paths of `paths` whose `filter` attribute (as of `tree`) is `lfs`.
/// Empty when git cannot tell (`check-attr --source` needs git ≥ 2.40).
pub fn lfs_paths(dir: &Path, tree: &str, paths: &[String]) -> Vec<String> {
    if paths.is_empty() {
        return vec![];
    }
    let input: Vec<u8> = paths.iter().flat_map(|p| p.bytes().chain(std::iter::once(0))).collect();
    let Ok(out) = run_with(dir, &["check-attr", &format!("--source={tree}"), "-z", "--stdin", "filter"], Opts { stdin: Some(&input), ..Default::default() })
    else {
        return vec![];
    };
    let f: Vec<&[u8]> = out.split(|b| *b == 0).collect();
    f.chunks(3).filter(|c| c.len() == 3 && c[2] == b"lfs").map(|c| String::from_utf8_lossy(c[0]).to_string()).collect()
}

/// Clear assume-unchanged / skip-worktree flags (they hide worktree edits
/// from `add -u` and make `read-tree -u` refuse). `only_present`: only for
/// paths that exist in the worktree.
pub fn clear_hidden_flags(dir: &Path, env: Vec<(&str, String)>, only_present: bool) -> Result<(), String> {
    let listed = run_with(dir, &["ls-files", "-v", "-z"], Opts { env: env.clone(), ..Default::default() })?;
    let mut hidden = Vec::new();
    for rec in listed.split(|b| *b == 0).filter(|r| r.len() > 2) {
        let (tag, path) = (rec[0], &rec[2..]);
        let flagged = tag.is_ascii_lowercase() || tag == b'S';
        if flagged && (!only_present || std::fs::symlink_metadata(dir.join(String::from_utf8_lossy(path).as_ref())).is_ok()) {
            hidden.extend_from_slice(path);
            hidden.push(0);
        }
    }
    if !hidden.is_empty() {
        // With --stdin only the last flag option applies: one call each.
        for flag in ["--no-assume-unchanged", "--no-skip-worktree"] {
            run_with(dir, &["update-index", flag, "-z", "--stdin"], Opts { env: env.clone(), stdin: Some(&hidden) })?;
        }
    }
    Ok(())
}

/// Gitlink (submodule) paths of a tree, repo-relative.
pub fn tree_gitlinks(dir: &Path, tree: &str) -> Result<std::collections::HashSet<String>, String> {
    let out = run(dir, &["ls-tree", "-r", "-z", "--full-tree", tree])?;
    Ok(out
        .split(|b| *b == 0)
        .filter_map(|r| {
            let r = String::from_utf8_lossy(r);
            let (meta, path) = r.split_once('\t')?;
            meta.starts_with("160000 ").then(|| path.to_string())
        })
        .collect())
}

/// Full names of all refs under `refs/heads` and `refs/remotes`.
pub fn ref_names(dir: &Path) -> Result<Vec<String>, String> {
    let out = run(dir, &["for-each-ref", "--format=%(refname)", "refs/heads", "refs/remotes"])?;
    Ok(String::from_utf8_lossy(&out).lines().map(|s| s.to_string()).collect())
}

/// `name` is a valid full ref name.
pub fn ref_format_ok(dir: &Path, name: &str) -> bool {
    exec(dir, &["check-ref-format", name], Opts::default()).map(|o| o.ok).unwrap_or(false)
}
