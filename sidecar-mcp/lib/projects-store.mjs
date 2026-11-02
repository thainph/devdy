// Project / repo registration for the `devdy` MCP server: create a project and
// link git repos to one, with the same rows the app writes (`add_project` /
// `add_repo` in src-tauri/src/commands/projects.rs) — repo coordinates are read
// off `git remote get-url origin` like `detect_project_info`.

import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync, realpathSync, statSync } from 'node:fs';
import { homedir } from 'node:os';
import { basename, isAbsolute, join, relative, resolve, sep } from 'node:path';
import { db, tx, normIds, nowRfc3339, findProject } from './store.mjs';

// Credential stores under HOME (src-tauri/src/remote/guard.rs SECRET_HOME_DIRS):
// a registered project becomes a root the remote desktop may browse.
const SECRET_HOME_DIRS = [
  '.devdy', '.ssh', '.aws', '.azure', '.codex', '.gnupg', '.kube', '.docker',
  '.password-store', '.config/gh', '.config/glab-cli', '.config/gcloud', 'Library/Keychains',
];

const inside = (child, parent) => {
  const rel = relative(parent, child);
  return rel === '' || (!rel.startsWith('..') && !isAbsolute(rel));
};

/** An absolute, existing directory (symlinks resolved), outside credential stores. */
function normDir(path, label = 'path') {
  const raw = String(path ?? '').trim();
  if (!raw) throw new Error(`${label} is required`);
  const expanded = raw === '~' || raw.startsWith('~/') ? join(homedir(), raw.slice(1)) : raw;
  if (!isAbsolute(expanded)) throw new Error(`${label} must be absolute: ${raw}`);
  if (!existsSync(expanded)) throw new Error(`${label} does not exist: ${raw}`);
  if (!statSync(expanded).isDirectory()) throw new Error(`${label} is not a directory: ${raw}`);
  const real = realpathSync(resolve(expanded)).replace(/[\\/]+$/, '') || sep;
  const home = realpathSync(homedir());
  const secret = SECRET_HOME_DIRS.find((d) => inside(real, join(home, d)));
  if (secret) throw new Error(`${label} is inside ~/${secret} — not allowed`);
  return real;
}

/** Host + path of a remote URL: https/ssh/git schemes and scp-style `git@host:group/repo`. */
function splitRemote(raw) {
  const trimmed = String(raw).trim().replace(/\/+$/, '');
  const url = trimmed.endsWith('.git') ? trimmed.slice(0, -4) : trimmed;
  if (!url.includes('://')) {
    const i = url.indexOf(':');
    if (i >= 0) {
      const userhost = url.slice(0, i);
      return { host: userhost.split('@').pop().toLowerCase(), path: url.slice(i + 1) };
    }
  }
  const noScheme = url.includes('://') ? url.slice(url.indexOf('://') + 3) : url;
  const slash = noScheme.indexOf('/');
  if (slash < 0) return null;
  return { host: noScheme.slice(0, slash).split('@').pop().toLowerCase(), path: noScheme.slice(slash + 1) };
}

/** Provider coordinates of a remote URL (`parse_remote_url` in projects.rs), or null. */
export function parseRemoteUrl(raw) {
  const split = splitRemote(raw);
  if (!split) return null;
  const segments = split.path.split(/[?#]/)[0].split('/').filter(Boolean);
  if (segments.length < 2) return null;
  if (split.host.includes('gitlab')) {
    return { provider: 'gitlab', github_owner: null, github_repo: null, gitlab_project_path: segments.join('/') };
  }
  if (split.host.includes('github')) {
    return { provider: 'github', github_owner: segments[0], github_repo: segments.at(-1), gitlab_project_path: null };
  }
  return null;
}

function originUrl(dir) {
  try {
    return execFileSync('git', ['remote', 'get-url', 'origin'], {
      cwd: dir,
      encoding: 'utf8',
      timeout: 5000,
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return null;
  }
}

const isGitRepo = (dir) => existsSync(join(dir, '.git'));

/** Repo row fields for `dir`: name + provider coordinates (explicit remote_url wins over origin). */
function repoFields(dir, { name, remote_url } = {}) {
  const url = remote_url != null && String(remote_url).trim() ? String(remote_url).trim() : originUrl(dir);
  const remote = url ? parseRemoteUrl(url) : null;
  if (remote_url != null && String(remote_url).trim() && !remote) {
    throw new Error(`remote_url is not a GitHub/GitLab repo URL: ${remote_url}`);
  }
  return {
    name: String(name ?? '').trim() || basename(dir),
    path: dir,
    provider: remote?.provider ?? 'github',
    github_owner: remote?.github_owner ?? null,
    github_repo: remote?.github_repo ?? null,
    gitlab_project_path: remote?.gitlab_project_path ?? null,
  };
}

/** The project root itself and its immediate subdirectories that are git repos (`detect_project_info`). */
function detectRepos(root) {
  const found = isGitRepo(root) ? [root] : [];
  let entries = [];
  try {
    entries = readdirSync(root, { withFileTypes: true });
  } catch {
    /* unreadable root: just the root itself */
  }
  const subdirs = entries
    .filter((e) => e.isDirectory())
    .map((e) => join(root, e.name))
    .sort();
  return found.concat(subdirs.filter(isGitRepo));
}

function insertRepo(d, projectId, f, now) {
  const id = randomUUID();
  d.prepare(
    `INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at, provider, gitlab_project_path, gitlab_project_id)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)`,
  ).run(id, projectId, f.name, f.path, f.github_owner, f.github_repo, now, f.provider, f.gitlab_project_path);
  return { id, ...f };
}

/**
 * Register a folder as a Devdy project (appended to the manual order). With
 * `detect_repos` (default) the root and its immediate git subfolders are linked
 * as repos, like the app's "Add project" dialog.
 */
export function createProject({ path, name, detect_repos = true } = {}) {
  const dir = normDir(path);
  const projectName = String(name ?? '').trim() || basename(dir) || 'Project';
  const d = db();
  const dup = d.prepare('SELECT id, name FROM projects WHERE path = ?').get(dir);
  if (dup) throw new Error(`"${dir}" is already the project "${dup.name}" (${dup.id})`);
  const repos = detect_repos === false ? [] : detectRepos(dir).map((r) => repoFields(r));
  const now = nowRfc3339();
  return tx((t) => {
    const id = randomUUID();
    const { next } = t.prepare('SELECT COALESCE(MAX(position), -1) + 1 AS next FROM projects').get();
    t.prepare('INSERT INTO projects (id, name, path, created_at, position) VALUES (?, ?, ?, ?, ?)').run(
      id,
      projectName,
      dir,
      now,
      next,
    );
    return { id, name: projectName, path: dir, repos: repos.map((f) => insertRepo(t, id, f, now)) };
  });
}

/**
 * Link a git repo folder to a project (`project` id or name, else the current
 * one). Provider coordinates come from `remote_url` or the repo's origin.
 */
export function addRepo({ path, project, name, remote_url } = {}) {
  const dir = normDir(path);
  const target =
    project != null && String(project).trim()
      ? findProject(project)
      : (() => {
          const id = process.env.DEVDY_PROJECT_ID || null;
          if (!id) throw new Error('no current project — pass project="<id or name>" (see projects_list)');
          return findProject(id);
        })();
  const d = db();
  const dup = d.prepare('SELECT id FROM repos WHERE project_id = ? AND path = ?').get(target.id, dir);
  if (dup) throw new Error(`"${dir}" is already linked to project "${target.name}" (repo ${dup.id})`);
  const fields = repoFields(dir, { name, remote_url });
  const repo = tx((t) => insertRepo(t, target.id, fields, nowRfc3339()));
  return { project: target, repo, git: isGitRepo(dir) };
}

/**
 * Unlink repos (`id` or `ids`) from their project (`remove_repo`), all or
 * nothing. Only the Devdy row goes — the folder on disk is untouched.
 */
export function removeRepos({ ids } = {}) {
  const list = normIds(ids);
  return tx((d) => {
    const get = d.prepare(
      'SELECT r.id, r.name, r.path, p.name AS project_name FROM repos r JOIN projects p ON p.id = r.project_id WHERE r.id = ?',
    );
    const rows = list.map((id) => get.get(id));
    const missing = list.filter((_, i) => !rows[i]);
    if (missing.length) throw new Error(`repo not found: ${missing.join(', ')} (see repos_list)`);
    // No `foreign_keys` on this connection: do the `ON DELETE SET NULL` of runs.repo_id by hand.
    const runs = d.prepare('UPDATE runs SET repo_id = NULL WHERE repo_id = ?');
    const del = d.prepare('DELETE FROM repos WHERE id = ?');
    for (const id of list) {
      runs.run(id);
      del.run(id);
    }
    return rows;
  });
}
