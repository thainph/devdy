#!/usr/bin/env node
// projects_create / repos_add of the built-in `devdy` MCP server: the rows match
// what the app's add_project / add_repo write (position, provider coordinates
// read off the origin remote), duplicates and bad paths are rejected.
//
// Builds a throwaway DB from every real migration and a few local git repos,
// then drives `index.mjs` over stdio. No network.
//
// Usage: node sidecar-mcp/test-projects.mjs

import { spawn, execFileSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, realpathSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { parseRemoteUrl } from './lib/projects-store.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const PROJECT = 'proj-1';
const T0 = '2026-01-01T00:00:00Z';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

// ---- Pure helpers ----------------------------------------------------------------

const gh = { provider: 'github', github_owner: 'Papay', github_repo: 'devdy', gitlab_project_path: null };
for (const url of ['https://github.com/Papay/devdy.git', 'git@github.com:Papay/devdy.git', 'ssh://git@github.com/Papay/devdy/']) {
  check(JSON.stringify(parseRemoteUrl(url)) === JSON.stringify(gh), `remote: ${url}`);
}
check(parseRemoteUrl('git@gitlab.com:grp/sub/app.git')?.gitlab_project_path === 'grp/sub/app', 'remote: gitlab nested group');
check(parseRemoteUrl('https://bitbucket.org/a/b') === null && parseRemoteUrl('https://github.com/a') === null, 'remote: unknown host / short path');

// ---- Fixture -------------------------------------------------------------------

const tmp = realpathSync(mkdtempSync(join(tmpdir(), 'devdy-projects-')));
const dbPath = join(tmp, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
db.prepare("INSERT INTO projects (id, name, path, created_at, position) VALUES (?, 'proj', '/tmp/proj-1', ?, 4)").run(PROJECT, T0);

const gitRepo = (dir, origin) => {
  mkdirSync(dir, { recursive: true });
  execFileSync('git', ['init', '-q', dir]);
  if (origin) execFileSync('git', ['-C', dir, 'remote', 'add', 'origin', origin]);
  return dir;
};
const ws = join(tmp, 'workspace');
gitRepo(join(ws, 'api'), 'git@github.com:acme/api.git');
gitRepo(join(ws, 'web'), 'https://gitlab.com/acme/front/web.git');
mkdirSync(join(ws, 'docs'));
const lone = gitRepo(join(tmp, 'lone'), 'https://github.com/acme/lone');
const plain = join(tmp, 'plain');
mkdirSync(plain);

// ---- Drive the server -------------------------------------------------------------

function drive(calls, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: PROJECT, ...env },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.on('error', reject);
    child.on('close', () => {
      const byId = new Map();
      for (const line of out.split('\n').filter((l) => l.trim())) {
        const m = JSON.parse(line); // only JSON-RPC frames may reach stdout
        byId.set(m.id, m);
      }
      resolve(Object.fromEntries(calls.map(([label], i) => [label, byId.get(i + 1)?.result?.content?.[0]?.text ?? ''])));
    });
    calls.forEach(([, name, args], i) =>
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: i + 1, method: 'tools/call', params: { name, arguments: args } })}\n`),
    );
    child.stdin.end();
  });
}

const reposOf = (projectId) => db.prepare('SELECT * FROM repos WHERE project_id = ? ORDER BY name').all(projectId);

let out = await drive([
  ['create', 'projects_create', { path: `${ws}/`, name: 'Acme' }],
  ['dup', 'projects_create', { path: ws }],
  ['bare', 'projects_create', { path: plain, detect_repos: false }],
  ['missing', 'projects_create', { path: join(tmp, 'nope') }],
  ['relative', 'projects_create', { path: 'workspace' }],
]);
const created = /Created project "Acme" \(([0-9a-f-]{36})\)/.exec(out.create)?.[1];
const p = created && db.prepare('SELECT * FROM projects WHERE id = ?').get(created);
check(p && p.path === ws && p.position === 5 && /\+00:00$/.test(p.created_at), 'projects_create: row at the end of the order, path normalised');
const repos = created ? reposOf(created) : [];
check(repos.length === 2 && out.create.includes('Linked 2 repo(s)'), 'projects_create: git subfolders linked, plain folder skipped');
check(
  repos[0]?.name === 'api' && repos[0].provider === 'github' && repos[0].github_owner === 'acme' && repos[0].github_repo === 'api',
  'projects_create: github coordinates from origin',
);
check(
  repos[1]?.provider === 'gitlab' && repos[1].gitlab_project_path === 'acme/front/web' && repos[1].github_owner === null,
  'projects_create: gitlab coordinates from origin',
);
check(out.dup.includes('already the project "Acme"'), 'projects_create: duplicate path rejected');
const bare = db.prepare('SELECT id, name FROM projects WHERE path = ?').get(plain);
check(bare?.name === 'plain' && reposOf(bare.id).length === 0 && out.bare.includes('No repos linked'), 'projects_create: default name, detect_repos=false');
check(out.missing.includes('does not exist') && out.relative.includes('must be absolute'), 'projects_create: bad paths rejected');

out = await drive([
  ['current', 'repos_add', { path: lone }],
  ['again', 'repos_add', { path: lone }],
  ['by-name', 'repos_add', { path: lone, project: 'acme', name: 'Lone svc', remote_url: 'git@gitlab.example.com:ops/lone.git' }],
  ['plain', 'repos_add', { path: plain, project: created }],
  ['bad-url', 'repos_add', { path: lone, project: 'plain', remote_url: 'not a url' }],
  ['ghost', 'repos_add', { path: lone, project: 'Ghost' }],
]);
const cur = reposOf(PROJECT);
check(cur.length === 1 && cur[0].github_repo === 'lone' && out.current.includes('acme/lone'), 'repos_add: current project, origin read');
check(out.again.includes('already linked'), 'repos_add: duplicate in the same project rejected');
const named = reposOf(created).find((r) => r.name === 'Lone svc');
check(named?.provider === 'gitlab' && named.gitlab_project_path === 'ops/lone', 'repos_add: project by name, remote_url overrides origin');
check(out.plain.includes('no .git') && reposOf(created).some((r) => r.path === plain), 'repos_add: non-git folder linked with a note');
check(out['bad-url'].includes('not a GitHub/GitLab'), 'repos_add: bad remote_url rejected');
check(out.ghost.includes('no project matches "Ghost"'), 'repos_add: unknown project rejected');

out = await drive([['no-current', 'repos_add', { path: lone }]], { DEVDY_PROJECT_ID: '' });
check(out['no-current'].includes('no current project'), 'repos_add: no current project and none given');

const [api, web] = reposOf(created);
db.prepare("INSERT INTO runs (id, project_id, type, engine, status, created_at, repo_id) VALUES ('run-1', ?, 'free', 'claude', 'done', ?, ?)").run(created, T0, api.id);
out = await drive([['rm-partial', 'repos_remove', { ids: [web.id, 'nope'] }]]);
check(out['rm-partial'].includes('repo not found: nope') && reposOf(created).some((r) => r.id === web.id), 'repos_remove: unknown id rolls the batch back');
out = await drive([
  ['rm', 'repos_remove', { ids: [api.id, web.id] }],
  ['rm-again', 'repos_remove', { id: api.id }],
]);
check(
  out.rm.includes('Unlinked 2 repo(s)') && out.rm.includes('(from "Acme")') && !reposOf(created).some((r) => r.id === api.id || r.id === web.id),
  'repos_remove: rows deleted',
);
check(db.prepare("SELECT repo_id FROM runs WHERE id = 'run-1'").get().repo_id === null, 'repos_remove: runs keep going, repo_id cleared');
check(execFileSync('git', ['-C', join(ws, 'api'), 'remote', 'get-url', 'origin'], { encoding: 'utf8' }).trim() === 'git@github.com:acme/api.git', 'repos_remove: folder on disk untouched');
check(out['rm-again'].includes('repo not found'), 'repos_remove: already removed');

db.close();
rmSync(tmp, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} check(s) failed.\n`);
  process.exit(1);
}
process.stdout.write('\nAll checks passed.\n');
