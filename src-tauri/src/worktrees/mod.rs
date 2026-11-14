//! Per-task git worktrees. A session can run in its own checkout of the project
//! at `<project>/.devdy/worktrees/<slug>` on branch `devdy/<slug>`, so several
//! sessions edit one repo in parallel without stepping on each other.
//!
//! The checkout starts from the freshly fetched remote base branch (never the
//! user's uncommitted work). Files git doesn't carry — Devdy-installed rules and
//! skills (`.claude/`, `.agents/`, `AGENTS.md`…) and `.env*` — are brought over so
//! the session keeps the project's context, then the project's optional setup
//! command (e.g. `pnpm install`) runs.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use tauri::AppHandle;
use tokio::process::Command;

use crate::db::Db;

/// Where worktrees live, relative to the project root.
pub const WORKTREES_DIR: &str = ".devdy/worktrees";
/// Prefix of the branch each worktree is created on.
pub const BRANCH_PREFIX: &str = "devdy/";

/// Linked into the worktree when missing there (Devdy-managed, usually gitignored):
/// edits to rules/skills in the project apply to every worktree at once.
const LINK_ENTRIES: &[&str] = &[".claude", ".agents", "AGENTS.md", "CLAUDE.md", "CLAUDE.local.md"];
/// Top-level `.env` files are copied (not linked) so a worktree can change its own
/// ports without touching the project's.
fn is_copied_entry(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.")
}

const GIT_TIMEOUT: Duration = Duration::from_secs(120);
const SETUP_TIMEOUT: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub id: String,
    pub project_id: String,
    pub path: String,
    pub branch: String,
    pub base_ref: String,
    pub todo_id: Option<String>,
    pub created_at: String,
    pub removed_at: Option<String>,
}

impl Worktree {
    fn from_row(row: &sqlx::sqlite::SqliteRow) -> Self {
        use sqlx::Row;
        Self {
            id: row.get("id"),
            project_id: row.get("project_id"),
            path: row.get("path"),
            branch: row.get("branch"),
            base_ref: row.get("base_ref"),
            todo_id: row.get("todo_id"),
            created_at: row.get("created_at"),
            removed_at: row.get("removed_at"),
        }
    }
}

/// ASCII slug for paths and branch names: Vietnamese diacritics dropped,
/// anything else non-alphanumeric collapsed to `-`, at most `max` chars.
pub fn slugify(text: &str, max: usize) -> String {
    use unicode_normalization::UnicodeNormalization;
    let mut out = String::new();
    for c in text.nfd() {
        let c = match c {
            'đ' | 'Đ' => 'd',
            c if c.is_ascii_alphanumeric() => c.to_ascii_lowercase(),
            c if unicode_normalization::char::is_combining_mark(c) => continue,
            _ => '-',
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        if out.len() >= max {
            break;
        }
        out.push(c);
    }
    out.trim_end_matches('-').to_string()
}

/// Whether `cwd` is one of `project_path`'s worktrees (a transcript recorded there
/// belongs to the project).
pub fn is_project_worktree(project_path: &str, cwd: &str) -> bool {
    let root = Path::new(project_path).join(WORKTREES_DIR);
    Path::new(cwd).parent() == Some(root.as_path())
}

/// Whether a transcript's recorded `cwd` belongs to the project: its root or one of its worktrees.
pub fn cwd_in_project(project_path: &str, cwd: &str) -> bool {
    cwd == project_path || is_project_worktree(project_path, cwd)
}

/// `<slug>-<n>` free in both the worktrees folder and the branch namespace. A
/// hint with nothing to slug (e.g. Japanese text) becomes `task-<random>`.
fn unique_slug(root: &Path, base: &str, branch_taken: impl Fn(&str) -> bool) -> String {
    let random;
    let base = if base.is_empty() {
        random = format!("task-{}", &uuid::Uuid::new_v4().simple().to_string()[..6]);
        random.as_str()
    } else {
        base
    };
    let mut n = 1;
    loop {
        let slug = if n == 1 { base.to_string() } else { format!("{base}-{n}") };
        if !root.join(&slug).exists() && !branch_taken(&format!("{BRANCH_PREFIX}{slug}")) {
            return slug;
        }
        n += 1;
    }
}

/// Run `git` in `cwd`; stdout on success, a readable error (stderr) otherwise.
async fn git(cwd: &Path, args: &[&str], mut cmd: Command) -> Result<String, String> {
    cmd.current_dir(cwd).args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.kill_on_drop(true);
    let out = tokio::time::timeout(GIT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("git {} timed out", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("git: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        Err(format!("git {}: {}", args.join(" "), err))
    }
}

/// A plain local `git` (no network). The inherited credential helpers are reset
/// so nothing it triggers can authenticate with the user's own credentials.
fn local_git() -> Command {
    let mut cmd = Command::new("git");
    crate::runs::sidecar::augment_command_path(&mut cmd);
    cmd.env("GIT_CONFIG_COUNT", "1");
    cmd.env("GIT_CONFIG_KEY_0", "credential.helper");
    cmd.env("GIT_CONFIG_VALUE_0", "");
    cmd.env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Serialises fetch → name pick → `worktree add` per project: parallel spawns
/// would otherwise pick the same branch name or collide on fetch's ref locks.
async fn project_lock(project_id: &str) -> tokio::sync::OwnedMutexGuard<()> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let lock = {
        let mut map = LOCKS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner());
        map.entry(project_id.to_string()).or_default().clone()
    };
    lock.lock_owned().await
}

/// A fresh broker-wired command (see `wire_backend_command`) for `program`.
async fn backend_command(
    app: &AppHandle,
    db: &Db,
    program: &str,
    project_id: &str,
    cwd: &str,
) -> Result<(Command, crate::runs::BrokerRunGuard), String> {
    let mut cmd = Command::new(program);
    let call_id = format!("worktree-{}", uuid::Uuid::new_v4());
    let guard = crate::commands::runs::wire_backend_command(app, db, &mut cmd, project_id, &call_id, cwd).await?;
    Ok((cmd, guard))
}

/// The project's repo root must be the project folder itself.
async fn require_repo_root(project: &Path) -> Result<(), String> {
    let top = git(project, &["rev-parse", "--show-toplevel"], local_git())
        .await
        .map_err(|_| "not_git_repo: this project folder is not a git repository".to_string())?;
    let same = fs::canonicalize(&top).ok() == fs::canonicalize(project).ok();
    if same {
        Ok(())
    } else {
        Err(format!(
            "not_git_repo: the project folder is inside the repository {top}; worktrees need the project to be the repository root"
        ))
    }
}

/// Branch new worktrees start from: the project setting, else the remote's default
/// branch, else the branch currently checked out.
async fn resolve_base_branch(project: &Path, configured: Option<String>) -> Result<String, String> {
    if let Some(b) = configured.map(|b| b.trim().to_string()).filter(|b| !b.is_empty()) {
        return Ok(b.trim_start_matches("origin/").to_string());
    }
    if let Ok(head) = git(project, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], local_git()).await {
        if let Some(b) = head.strip_prefix("origin/") {
            return Ok(b.to_string());
        }
    }
    git(project, &["rev-parse", "--abbrev-ref", "HEAD"], local_git()).await
}

/// Keep `.devdy/` out of the project's `git status` (worktrees would otherwise show
/// up as untracked nested repos). Uses the repo-local exclude file, never .gitignore.
async fn ensure_devdy_excluded(project: &Path) {
    if git(project, &["check-ignore", "-q", WORKTREES_DIR], local_git()).await.is_ok() {
        return;
    }
    let Ok(common) = git(project, &["rev-parse", "--git-common-dir"], local_git()).await else {
        return;
    };
    let common = project.join(common);
    let exclude = common.join("info").join("exclude");
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    let mut body = existing.clone();
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("# Devdy: per-task worktrees and run logs\n/.devdy/\n");
    if let Some(dir) = exclude.parent() {
        let _ = fs::create_dir_all(dir);
    }
    if let Err(e) = fs::write(&exclude, body) {
        tracing::warn!(event = "worktree_exclude_failed", error = %e);
    }
}

/// Link `src` at `dst` where nothing exists yet; where both are real folders (part
/// of it is tracked), recurse so only the missing pieces get linked. `rel` is
/// `dst` relative to the worktree; every link made is pushed to `made`.
fn link_missing(src: &Path, dst: &Path, rel: &str, made: &mut Vec<String>) -> std::io::Result<()> {
    match fs::symlink_metadata(dst) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            #[cfg(unix)]
            std::os::unix::fs::symlink(src, dst)?;
            made.push(rel.to_string());
            Ok(())
        }
        Err(e) => Err(e),
        Ok(meta) if meta.is_dir() && src.is_dir() => {
            for entry in fs::read_dir(src)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                link_missing(&entry.path(), &dst.join(&name), &format!("{rel}/{name}"), made)?;
            }
            Ok(())
        }
        Ok(_) => Ok(()),
    }
}

/// Bring the files git doesn't carry from the project root into a new worktree.
/// Returns what was linked or copied, relative to the worktree.
pub fn carry_untracked(project: &Path, worktree: &Path) -> Result<Vec<String>, String> {
    let mut made = Vec::new();
    for name in LINK_ENTRIES {
        let src = project.join(name);
        if fs::symlink_metadata(&src).is_ok() {
            link_missing(&src, &worktree.join(name), name, &mut made).map_err(|e| format!("{name}: {e}"))?;
        }
    }
    for entry in fs::read_dir(project).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let dst = worktree.join(&name);
        if is_copied_entry(&name) && entry.path().is_file() && !dst.exists() {
            fs::copy(entry.path(), &dst).map_err(|e| format!("{name}: {e}"))?;
            made.push(name);
        }
    }
    Ok(made)
}

/// Keep carried files out of the worktree's commits: whatever git would pick up
/// (not ignored) is added to the repo's exclude file. That file is shared by all
/// worktrees, so these paths are also hidden from the project's own status —
/// they are the same Devdy-managed files there.
async fn exclude_carried(worktree: &Path, carried: &[String]) -> Result<(), String> {
    let mut lines = Vec::new();
    for rel in carried {
        if git(worktree, &["check-ignore", "-q", "--no-index", rel], local_git()).await.is_err() {
            lines.push(format!("/{rel}"));
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    let common = git(worktree, &["rev-parse", "--git-common-dir"], local_git()).await?;
    let exclude = worktree.join(common).join("info").join("exclude");
    let existing = fs::read_to_string(&exclude).unwrap_or_default();
    let mut body = existing.clone();
    for line in lines {
        if !existing.lines().any(|l| l == line) {
            if !body.is_empty() && !body.ends_with('\n') {
                body.push('\n');
            }
            body.push_str(&line);
            body.push('\n');
        }
    }
    // Failing here is fatal: a later "commit all" would commit the copied .env.
    if body != existing {
        if let Some(dir) = exclude.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("git exclude: {e}"))?;
        }
        fs::write(&exclude, body).map_err(|e| format!("git exclude: {e}"))?;
    }
    Ok(())
}

/// Run the project's setup command in the worktree, broker-wired like a run (a
/// git dependency fetched by the package manager uses the project's account).
/// The error carries the tail of its output so the user can see why it failed.
async fn run_setup(mut cmd: Command, worktree: &Path, setup: &str) -> Result<(), String> {
    cmd.arg("-c").arg(setup).current_dir(worktree);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // Its own process group, so a timeout stops the package manager too, not just sh.
    crate::runs::sidecar::detach_process_group(&mut cmd);
    cmd.kill_on_drop(true);
    let child = cmd.spawn().map_err(|e| format!("setup_failed: {e}"))?;
    let pgid = child.id();
    let out = match tokio::time::timeout(SETUP_TIMEOUT, child.wait_with_output()).await {
        Ok(out) => out.map_err(|e| format!("setup_failed: {e}"))?,
        Err(_) => {
            #[cfg(unix)]
            if let Some(pgid) = pgid {
                unsafe {
                    libc::killpg(pgid as libc::pid_t, libc::SIGKILL);
                }
            }
            let _ = pgid;
            return Err(format!("setup_failed: `{setup}` took longer than 15 minutes"));
        }
    };
    if out.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    let tail: Vec<&str> = text.lines().rev().take(15).collect();
    let tail: Vec<&str> = tail.into_iter().rev().collect();
    Err(format!("setup_failed: `{setup}` exited with an error:\n{}", tail.join("\n")))
}

struct ProjectRow {
    path: String,
    base_branch: Option<String>,
    setup_cmd: Option<String>,
}

async fn load_project(db: &Db, project_id: &str) -> Result<ProjectRow, String> {
    use sqlx::Row;
    let row = sqlx::query("SELECT path, worktree_base_branch, worktree_setup_cmd FROM projects WHERE id = ?")
        .bind(project_id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("project not found")?;
    Ok(ProjectRow {
        path: row.get("path"),
        base_branch: row.get("worktree_base_branch"),
        setup_cmd: row.get("worktree_setup_cmd"),
    })
}

/// Create a worktree for `hint` (usually the todo text): fetch the base branch,
/// check out a new `devdy/<slug>` branch from it, carry untracked config, run setup.
pub async fn create(
    app: &AppHandle,
    db: &Db,
    project_id: &str,
    hint: &str,
    todo_id: Option<&str>,
) -> Result<Worktree, String> {
    let project = load_project(db, project_id).await?;
    let root = PathBuf::from(&project.path);
    require_repo_root(&root).await?;
    let base = resolve_base_branch(&root, project.base_branch).await?;

    // Fetch, pick a free name and add the worktree under one per-project lock.
    let lock = project_lock(project_id).await;
    // Always start from the latest remote state, through the project's git account.
    let has_origin = git(&root, &["remote", "get-url", "origin"], local_git()).await.is_ok();
    let base_ref = if has_origin {
        let (cmd, _guard) = backend_command(app, db, "git", project_id, &project.path).await?;
        git(&root, &["fetch", "origin", &base], cmd)
            .await
            .map_err(|e| format!("fetch_failed: {e}"))?;
        format!("origin/{base}")
    } else {
        base.clone()
    };

    ensure_devdy_excluded(&root).await;
    let dir = root.join(WORKTREES_DIR);
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let branches = git(&root, &["branch", "--list", "--format=%(refname:short)", &format!("{BRANCH_PREFIX}*")], local_git())
        .await
        .unwrap_or_default();
    let taken: Vec<&str> = branches.lines().collect();
    let slug = unique_slug(&dir, &slugify(hint, 40), |b| taken.contains(&b));
    let path = dir.join(&slug);
    let path_str = path.to_string_lossy().into_owned();
    let branch = format!("{BRANCH_PREFIX}{slug}");

    // A half-made checkout is worse than none: every failure below removes it.
    let cleanup = |root: PathBuf, path_str: String, branch: String| async move {
        let _ = git(&root, &["worktree", "remove", "--force", &path_str], local_git()).await;
        let _ = fs::remove_dir_all(&path_str);
        let _ = git(&root, &["worktree", "prune"], local_git()).await;
        let _ = git(&root, &["branch", "-D", &branch], local_git()).await;
    };

    // The checkout may run LFS / hooks that authenticate: broker-wired too.
    let (cmd, _guard) = backend_command(app, db, "git", project_id, &project.path).await?;
    if let Err(e) = git(&root, &["worktree", "add", "--no-track", "-b", &branch, &path_str, &base_ref], cmd).await {
        cleanup(root.clone(), path_str.clone(), branch.clone()).await;
        return Err(e);
    }
    drop(lock);

    let prepared = async {
        let carried = carry_untracked(&root, &path)?;
        exclude_carried(&path, &carried).await?;
        if let Some(setup) = project.setup_cmd.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            let (cmd, _guard) = backend_command(app, db, "/bin/sh", project_id, &path_str).await?;
            run_setup(cmd, &path, setup).await?;
        }
        Ok::<(), String>(())
    }
    .await;
    if let Err(e) = prepared {
        cleanup(root.clone(), path_str.clone(), branch.clone()).await;
        return Err(e);
    }

    let wt = Worktree {
        id: uuid::Uuid::new_v4().to_string(),
        project_id: project_id.to_string(),
        path: path_str.clone(),
        branch: branch.clone(),
        base_ref,
        todo_id: todo_id.map(str::to_string),
        created_at: chrono::Utc::now().to_rfc3339(),
        removed_at: None,
    };
    let inserted = sqlx::query(
        "INSERT INTO worktrees (id, project_id, path, branch, base_ref, todo_id, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&wt.id)
    .bind(&wt.project_id)
    .bind(&wt.path)
    .bind(&wt.branch)
    .bind(&wt.base_ref)
    .bind(&wt.todo_id)
    .bind(&wt.created_at)
    .execute(db)
    .await;
    if let Err(e) = inserted {
        cleanup(root, path_str, branch).await;
        return Err(e.to_string());
    }
    Ok(wt)
}

pub async fn get(db: &Db, id: &str) -> Result<Worktree, String> {
    sqlx::query("SELECT * FROM worktrees WHERE id = ?")
        .bind(id)
        .fetch_optional(db)
        .await
        .map_err(|e| e.to_string())?
        .map(|r| Worktree::from_row(&r))
        .ok_or_else(|| "worktree not found".to_string())
}

/// Active worktrees of a project, newest first.
pub async fn list(db: &Db, project_id: &str) -> Result<Vec<Worktree>, String> {
    let rows = sqlx::query(
        "SELECT * FROM worktrees WHERE project_id = ? AND removed_at IS NULL ORDER BY created_at DESC",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(Worktree::from_row).collect())
}

/// Delete the checkout (uncommitted changes in it are lost) and, when asked, its
/// branch. Refuses while a session is still running in it.
pub async fn remove(db: &Db, id: &str, delete_branch: bool) -> Result<(), String> {
    let wt = get(db, id).await?;
    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs WHERE worktree_path = ? AND status = 'running'")
        .bind(&wt.path)
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?;
    if running > 0 {
        return Err("worktree_busy: a session is still running in this worktree".into());
    }
    let project = load_project(db, &wt.project_id).await?;
    let root = PathBuf::from(&project.path);
    if Path::new(&wt.path).exists() {
        git(&root, &["worktree", "remove", "--force", &wt.path], local_git()).await?;
    } else {
        let _ = git(&root, &["worktree", "prune"], local_git()).await;
    }
    if delete_branch {
        let _ = git(&root, &["branch", "-D", &wt.branch], local_git()).await;
    }
    sqlx::query("UPDATE worktrees SET removed_at = ? WHERE id = ?")
        .bind(chrono::Utc::now().to_rfc3339())
        .bind(id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// A local `git` call in `dir` (no network); stdout trimmed.
pub async fn git_local(dir: &Path, args: &[&str]) -> Result<String, String> {
    git(dir, args, local_git()).await
}

/// An active worktree whose checkout still exists.
async fn active(db: &Db, id: &str) -> Result<Worktree, String> {
    let wt = get(db, id).await?;
    if wt.removed_at.is_some() || !Path::new(&wt.path).is_dir() {
        return Err("worktree_gone: this worktree was already removed".into());
    }
    Ok(wt)
}

/// Stage and commit everything in the worktree, as the project's git identity.
pub async fn commit_all(app: &AppHandle, db: &Db, id: &str, message: &str) -> Result<(), String> {
    let wt = active(db, id).await?;
    let dir = PathBuf::from(&wt.path);
    let message = message.trim();
    if message.is_empty() {
        return Err("a commit message is required".into());
    }
    git(&dir, &["add", "-A"], local_git()).await?;
    let (cmd, _guard) = backend_command(app, db, "git", &wt.project_id, &wt.path).await?;
    git(&dir, &["commit", "-m", message], cmd).await?;
    Ok(())
}

/// Push the worktree's branch to `origin` (project's git account) and return the
/// web page that opens a pull / merge request for it against its base.
pub async fn push(app: &AppHandle, db: &Db, id: &str) -> Result<String, String> {
    let wt = active(db, id).await?;
    let dir = PathBuf::from(&wt.path);
    let remote = git(&dir, &["remote", "get-url", "origin"], local_git())
        .await
        .map_err(|_| "no_remote: this repository has no `origin` remote".to_string())?;
    let (cmd, _guard) = backend_command(app, db, "git", &wt.project_id, &wt.path).await?;
    git(&dir, &["push", "-u", "origin", &wt.branch], cmd)
        .await
        .map_err(|e| format!("push_failed: {e}"))?;
    let base = wt.base_ref.strip_prefix("origin/").unwrap_or(&wt.base_ref);
    review_url(&remote, base, &wt.branch).ok_or_else(|| format!("pushed {}; open a PR for it on the remote", wt.branch))
}

/// `https://host/owner/repo` for an `origin` URL (https, ssh or scp-like).
fn web_base(remote: &str) -> Option<String> {
    let r = remote.trim().trim_end_matches('/').trim_end_matches(".git");
    let (host, path) = if let Some(rest) = r.strip_prefix("https://").or_else(|| r.strip_prefix("http://")) {
        let rest = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        rest.split_once('/')?
    } else if let Some(rest) = r.strip_prefix("ssh://") {
        let rest = rest.rsplit_once('@').map(|(_, h)| h).unwrap_or(rest);
        let (host, path) = rest.split_once('/')?;
        (host.split(':').next()?, path)
    } else {
        let rest = r.rsplit_once('@').map(|(_, h)| h).unwrap_or(r);
        rest.split_once(':')?
    };
    (!host.is_empty() && !path.is_empty()).then(|| format!("https://{host}/{path}"))
}

/// Page that opens a PR (GitHub) or MR (GitLab and anything else) for `branch` into `base`.
pub fn review_url(remote: &str, base: &str, branch: &str) -> Option<String> {
    let web = web_base(remote)?;
    if web.starts_with("https://github.com/") {
        Some(format!("{web}/compare/{base}...{branch}?expand=1"))
    } else {
        Some(format!(
            "{web}/-/merge_requests/new?merge_request%5Bsource_branch%5D={branch}&merge_request%5Btarget_branch%5D={base}"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_url_for_github_and_gitlab_remotes() {
        assert_eq!(
            review_url("git@github.com:acme/shop.git", "main", "devdy/fix").as_deref(),
            Some("https://github.com/acme/shop/compare/main...devdy/fix?expand=1")
        );
        assert_eq!(
            review_url("https://x-access-token@github.com/acme/shop", "dev", "devdy/a").as_deref(),
            Some("https://github.com/acme/shop/compare/dev...devdy/a?expand=1")
        );
        assert_eq!(
            review_url("ssh://git@gitlab.corp.io:2222/team/app.git", "main", "devdy/b").as_deref(),
            Some("https://gitlab.corp.io/team/app/-/merge_requests/new?merge_request%5Bsource_branch%5D=devdy/b&merge_request%5Btarget_branch%5D=main")
        );
        assert_eq!(review_url("/local/path/repo", "main", "x"), None);
    }

    #[test]
    fn slugify_drops_diacritics_and_punctuation() {
        assert_eq!(slugify("Sửa lỗi đăng nhập (OAuth)!", 40), "sua-loi-dang-nhap-oauth");
        assert_eq!(slugify("  --Fix  CSV export-- ", 40), "fix-csv-export");
        assert_eq!(slugify("日本語", 40), "");
        assert_eq!(slugify("abcdef-ghij", 7), "abcdef");
    }

    #[test]
    fn worktree_cwd_belongs_to_project() {
        assert!(cwd_in_project("/p/shop", "/p/shop"));
        assert!(cwd_in_project("/p/shop", "/p/shop/.devdy/worktrees/fix-login"));
        assert!(!cwd_in_project("/p/shop", "/p/shop/.devdy/worktrees/a/b"));
        assert!(!cwd_in_project("/p/shop", "/p/shop-2"));
    }

    #[test]
    fn unique_slug_skips_taken_names() {
        let tmp = std::env::temp_dir().join(format!("devdy-wt-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(tmp.join("fix")).unwrap();
        assert_eq!(unique_slug(&tmp, "fix", |_| false), "fix-2");
        assert_eq!(unique_slug(&tmp, "fix", |b| b == "devdy/fix-2"), "fix-3");
        let random = unique_slug(&tmp, "", |_| false);
        assert!(random.starts_with("task-") && random.len() == 11, "{random}");
        fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn carry_links_config_and_copies_env() {
        let tmp = std::env::temp_dir().join(format!("devdy-carry-{}", uuid::Uuid::new_v4()));
        let (project, wt) = (tmp.join("p"), tmp.join("w"));
        fs::create_dir_all(project.join(".claude/rules")).unwrap();
        fs::write(project.join(".claude/rules/style.md"), "x").unwrap();
        fs::write(project.join(".claude/settings.json"), "{}").unwrap();
        fs::write(project.join("AGENTS.md"), "a").unwrap();
        fs::write(project.join(".env"), "PORT=1").unwrap();
        fs::write(project.join(".env.local"), "K=v").unwrap();
        // `.claude/settings.json` is tracked: the worktree already has a real `.claude/`.
        fs::create_dir_all(wt.join(".claude")).unwrap();
        fs::write(wt.join(".claude/settings.json"), "{\"tracked\":1}").unwrap();

        let mut made = carry_untracked(&project, &wt).unwrap();
        made.sort();
        assert_eq!(made, vec![".claude/rules", ".env", ".env.local", "AGENTS.md"]);

        assert!(fs::symlink_metadata(wt.join("AGENTS.md")).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(wt.join(".claude/rules")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(wt.join(".claude/settings.json")).unwrap(), "{\"tracked\":1}");
        assert!(!fs::symlink_metadata(wt.join(".env")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(wt.join(".env.local")).unwrap(), "K=v");
        fs::remove_dir_all(&tmp).unwrap();
    }
}
