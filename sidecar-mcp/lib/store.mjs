// Thin data-access layer over Devdy's SQLite store (`data.db`).
//
// Opened read-write because the notes tools mutate. Reads (runs/sessions) are
// scoped to the current project by default via DEVDY_PROJECT_ID. We open lazily
// and reuse a single handle for the process lifetime.

import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { mkdirSync, writeFileSync, readFileSync, rmSync, renameSync, readdirSync, statSync, existsSync } from 'node:fs';
import { dirname, join, resolve, relative, sep } from 'node:path';

let handle = null;

function db() {
  if (handle) return handle;
  const dbPath = process.env.DEVDY_DB_PATH;
  if (!dbPath) throw new Error('DEVDY_DB_PATH is not set — cannot open Devdy store');
  handle = new DatabaseSync(dbPath);
  return handle;
}

const nowIso = () => new Date().toISOString();

/** Run `fn` inside a transaction so a failure mid-batch rolls the whole thing back. */
function tx(fn) {
  const d = db();
  d.exec('BEGIN');
  try {
    const out = fn(d);
    d.exec('COMMIT');
    return out;
  } catch (err) {
    try {
      d.exec('ROLLBACK');
    } catch {
      /* the transaction is already gone — surface the original error */
    }
    throw err;
  }
}

/** Accepts a single id or an array; returns a deduped, non-empty list. */
function normIds(ids) {
  const list = (Array.isArray(ids) ? ids : [ids])
    .map((x) => String(x ?? '').trim())
    .filter(Boolean);
  if (!list.length) throw new Error('at least one id is required');
  return [...new Set(list)];
}

/** The current project id, or throw — for tools that can't fall back to global. */
function requireProject() {
  const id = process.env.DEVDY_PROJECT_ID || null;
  if (!id) throw new Error('no current project (DEVDY_PROJECT_ID is not set)');
  return id;
}

/** scope="global" detaches; anything else links to the current project (when any). */
function scopedProjectId(scope) {
  return scope === 'global' ? null : process.env.DEVDY_PROJECT_ID || null;
}

/**
 * Rewrite `position` so the given ids sit at the top in the given order, with
 * every other row keeping its relative order below. Passing the full list is
 * therefore a plain reorder (same as the app's drag-and-drop), while passing a
 * few ids is a safe "move these to the top" — neither can corrupt the ordering.
 */
function reorderRows(table, ids, orderBy) {
  const all = db()
    .prepare(`SELECT id FROM ${table} ORDER BY ${orderBy}`)
    .all()
    .map((r) => r.id);
  const known = new Set(all);
  const missing = ids.filter((id) => !known.has(id));
  if (missing.length) throw new Error(`unknown ${table} id(s): ${missing.join(', ')}`);
  const moved = new Set(ids);
  const order = [...ids, ...all.filter((id) => !moved.has(id))];
  return tx((d) => {
    const stmt = d.prepare(`UPDATE ${table} SET position = ? WHERE id = ?`);
    order.forEach((id, index) => stmt.run(index, id));
    return { ordered: order.length };
  });
}

// ---- Notes -----------------------------------------------------------------

export function listNotes({ scope = 'project', limit = 50 } = {}) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  const lim = Math.max(1, Math.min(200, Number(limit) || 50));
  if (scope === 'project' && projectId) {
    return db()
      .prepare(
        `SELECT id, title, content, project_id, updated_at
         FROM notes WHERE project_id = ?
         ORDER BY position ASC, updated_at DESC LIMIT ?`,
      )
      .all(projectId, lim);
  }
  return db()
    .prepare(
      `SELECT id, title, content, project_id, updated_at
       FROM notes ORDER BY position ASC, updated_at DESC LIMIT ?`,
    )
    .all(lim);
}

export function readNote(id) {
  if (!id) throw new Error('note id is required');
  return db()
    .prepare(`SELECT id, title, content, project_id, created_at, updated_at FROM notes WHERE id = ?`)
    .get(id);
}

export function createNote({ title = '', content = '', scope = 'project' } = {}) {
  const t = String(title || '').trim();
  const c = String(content || '').trim();
  if (!t && !c) throw new Error('a title or content is required');
  const projectId = scopedProjectId(scope);
  const min = db().prepare('SELECT MIN(position) AS m FROM notes').get();
  const position = (min && min.m != null ? min.m : 0) - 1;
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare(
      `INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, ?, ?)`,
    )
    .run(id, t, c, projectId, position, now, now);
  return { id, title: t, content: c, project_id: projectId, created_at: now, updated_at: now };
}

export function updateNote({ id, title, content } = {}) {
  const existing = readNote(id);
  if (!existing) throw new Error(`note not found: ${id}`);
  const t = title != null ? String(title).trim() : existing.title;
  const c = content != null ? String(content).trim() : existing.content;
  if (!t && !c) throw new Error('a title or content is required');
  const now = nowIso();
  db().prepare('UPDATE notes SET title = ?, content = ?, updated_at = ? WHERE id = ?').run(t, c, now, id);
  return { id, title: t, content: c, updated_at: now };
}

export function appendNote({ id, text } = {}) {
  const existing = readNote(id);
  if (!existing) throw new Error(`note not found: ${id}`);
  const add = String(text || '').trim();
  if (!add) throw new Error('text is required');
  const content = existing.content ? `${existing.content}\n\n${add}` : add;
  const now = nowIso();
  db().prepare('UPDATE notes SET content = ?, updated_at = ? WHERE id = ?').run(content, now, id);
  return { id, content, updated_at: now };
}

/** Substring search over title + content. `%`/`_`/`\` in the query are literal. */
export function searchNotes({ query, scope = 'project', limit = 20 } = {}) {
  const q = String(query || '').trim();
  if (!q) throw new Error('query is required');
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  const lim = Math.max(1, Math.min(200, Number(limit) || 20));
  const like = `%${q.replace(/[%_\\]/g, (c) => `\\${c}`)}%`;
  const cols = 'id, title, content, project_id, updated_at';
  const match = `(title LIKE ? ESCAPE '\\' OR content LIKE ? ESCAPE '\\')`;
  const order = 'ORDER BY position ASC, updated_at DESC LIMIT ?';
  if (scope === 'project' && projectId) {
    return db()
      .prepare(`SELECT ${cols} FROM notes WHERE project_id = ? AND ${match} ${order}`)
      .all(projectId, like, like, lim);
  }
  return db().prepare(`SELECT ${cols} FROM notes WHERE ${match} ${order}`).all(like, like, lim);
}

export function deleteNotes({ ids } = {}) {
  const list = normIds(ids);
  return tx((d) => {
    const stmt = d.prepare('DELETE FROM notes WHERE id = ?');
    const deleted = list.filter((id) => stmt.run(id).changes > 0);
    return { deleted: deleted.length, requested: list.length };
  });
}

/** Link a note to the current project, or detach it with scope="global". */
export function setNoteProject({ id, scope = 'project' } = {}) {
  if (!readNote(id)) throw new Error(`note not found: ${id}`);
  const now = nowIso();
  if (scope === 'global') {
    // The run backlink only means anything alongside its project.
    db()
      .prepare('UPDATE notes SET project_id = NULL, run_id = NULL, updated_at = ? WHERE id = ?')
      .run(now, id);
    return { id, project_id: null };
  }
  const projectId = requireProject();
  db().prepare('UPDATE notes SET project_id = ?, updated_at = ? WHERE id = ?').run(projectId, now, id);
  return { id, project_id: projectId };
}

export function reorderNotes({ ids } = {}) {
  return reorderRows('notes', normIds(ids), 'position ASC, updated_at DESC');
}

// ---- Sessions / runs --------------------------------------------------------

export function recentRuns({ scope = 'project', limit = 20 } = {}) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  const lim = Math.max(1, Math.min(200, Number(limit) || 20));
  const cols = `r.id, r.title, r.type, r.engine, r.status, r.session_id,
                r.transcript_path, r.created_at, p.name AS project_name, p.path AS project_path`;
  if (scope === 'project' && projectId) {
    return db()
      .prepare(
        `SELECT ${cols} FROM runs r JOIN projects p ON p.id = r.project_id
         WHERE r.project_id = ? ORDER BY r.created_at DESC LIMIT ?`,
      )
      .all(projectId, lim);
  }
  return db()
    .prepare(
      `SELECT ${cols} FROM runs r JOIN projects p ON p.id = r.project_id
       ORDER BY r.created_at DESC LIMIT ?`,
    )
    .all(lim);
}

export function getRun(runId) {
  if (!runId) throw new Error('run_id is required');
  return db()
    .prepare(
      `SELECT r.id, r.title, r.type, r.engine, r.status, r.session_id,
              r.transcript_path, r.created_at, p.name AS project_name, p.path AS project_path
       FROM runs r JOIN projects p ON p.id = r.project_id WHERE r.id = ?`,
    )
    .get(runId);
}

// ---- Todos (global scratchpad) ---------------------------------------------

/** scope="project" narrows to the current project's todos; default spans all. */
export function listTodos({ includeDone = true, scope = 'all' } = {}) {
  const clauses = [];
  const params = [];
  if (!includeDone) clauses.push('t.done = 0');
  if (scope === 'project') {
    clauses.push('t.project_id = ?');
    params.push(requireProject());
  }
  const where = clauses.length ? `WHERE ${clauses.join(' AND ')}` : '';
  return db()
    .prepare(
      `SELECT t.id, t.text, t.done, t.position, t.created_at, t.project_id, t.run_id,
              p.name AS project_name
       FROM todos t LEFT JOIN projects p ON p.id = t.project_id
       ${where} ORDER BY t.position ASC, t.created_at DESC`,
    )
    .all(...params);
}

/** New todos go to the top (a position below the current minimum), like the app. */
export function addTodo({ text, scope = 'project' } = {}) {
  const t = String(text || '').trim();
  if (!t) throw new Error('text is required');
  const projectId = scopedProjectId(scope);
  const min = db().prepare('SELECT MIN(position) AS m FROM todos').get();
  const position = (min && min.m != null ? min.m : 0) - 1;
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare(
      'INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES (?, ?, 0, ?, ?, ?)',
    )
    .run(id, t, position, now, projectId);
  return { id, text: t, project_id: projectId };
}

export function readTodo(id) {
  if (!id) throw new Error('id is required');
  return db()
    .prepare(
      `SELECT t.id, t.text, t.done, t.position, t.created_at, t.project_id, t.run_id,
              p.name AS project_name
       FROM todos t LEFT JOIN projects p ON p.id = t.project_id WHERE t.id = ?`,
    )
    .get(id);
}

export function setTodoDone({ id, done = true } = {}) {
  if (!id) throw new Error('id is required');
  const row = db().prepare('SELECT id FROM todos WHERE id = ?').get(id);
  if (!row) throw new Error(`todo not found: ${id}`);
  db().prepare('UPDATE todos SET done = ? WHERE id = ?').run(done ? 1 : 0, id);
  return { id, done: !!done };
}

/** Flip done state without having to read it first. */
export function toggleTodo({ id } = {}) {
  if (!id) throw new Error('id is required');
  const row = db().prepare('SELECT id, done FROM todos WHERE id = ?').get(id);
  if (!row) throw new Error(`todo not found: ${id}`);
  const done = !row.done;
  db().prepare('UPDATE todos SET done = ? WHERE id = ?').run(done ? 1 : 0, id);
  return { id, done };
}

export function updateTodo({ id, text } = {}) {
  if (!id) throw new Error('id is required');
  const t = String(text || '').trim();
  if (!t) throw new Error('text is required');
  const row = db().prepare('SELECT id FROM todos WHERE id = ?').get(id);
  if (!row) throw new Error(`todo not found: ${id}`);
  db().prepare('UPDATE todos SET text = ? WHERE id = ?').run(t, id);
  return { id, text: t };
}

export function deleteTodos({ ids } = {}) {
  const list = normIds(ids);
  return tx((d) => {
    const stmt = d.prepare('DELETE FROM todos WHERE id = ?');
    const deleted = list.filter((id) => stmt.run(id).changes > 0);
    return { deleted: deleted.length, requested: list.length };
  });
}

export function clearDoneTodos() {
  const r = db().prepare('DELETE FROM todos WHERE done = 1').run();
  return { deleted: Number(r.changes) };
}

/** Link a todo to the current project, or detach it with scope="global". */
export function setTodoProject({ id, scope = 'project' } = {}) {
  if (!id) throw new Error('id is required');
  if (!db().prepare('SELECT id FROM todos WHERE id = ?').get(id)) {
    throw new Error(`todo not found: ${id}`);
  }
  if (scope === 'global') {
    // The run backlink only means anything alongside its project.
    db().prepare('UPDATE todos SET project_id = NULL, run_id = NULL WHERE id = ?').run(id);
    return { id, project_id: null };
  }
  const projectId = requireProject();
  db().prepare('UPDATE todos SET project_id = ? WHERE id = ?').run(projectId, id);
  return { id, project_id: projectId };
}

export function reorderTodos({ ids } = {}) {
  return reorderRows('todos', normIds(ids), 'position ASC, created_at DESC');
}

// ---- Project info -----------------------------------------------------------

export function currentProject() {
  const id = process.env.DEVDY_PROJECT_ID || null;
  if (!id) return null;
  return db()
    .prepare('SELECT id, name, path, github_owner, github_repo, default_engine FROM projects WHERE id = ?')
    .get(id);
}

/** All Devdy projects (name, path, linked github + repo count). */
export function listProjects() {
  return db()
    .prepare(
      `SELECT p.id, p.name, p.path, p.github_owner, p.github_repo, p.default_engine,
              (SELECT COUNT(*) FROM repos r WHERE r.project_id = p.id) AS repo_count
       FROM projects p ORDER BY p.name`,
    )
    .all();
}

/**
 * List repos across projects. A `project` filter (id or exact name) targets one
 * project regardless of the current run. Otherwise scope="project" (default)
 * returns the current project's repos and scope="all" returns every repo.
 * Returns { project, rows } — `project` is the resolved project when a single
 * one is in scope (filter or current), else null (e.g. scope="all").
 */
export function listRepos({ scope = 'project', project = null } = {}) {
  const cols =
    'r.id, r.project_id, r.name, r.path, r.github_owner, r.github_repo, p.name AS project_name';
  const forProject = (proj) => ({
    project: proj,
    rows: proj
      ? db()
          .prepare(
            `SELECT ${cols} FROM repos r JOIN projects p ON p.id = r.project_id
             WHERE r.project_id = ? ORDER BY r.name`,
          )
          .all(proj.id)
      : [],
  });

  if (project != null && String(project).trim()) {
    const key = String(project).trim();
    const proj = db().prepare('SELECT id, name FROM projects WHERE id = ? OR name = ? LIMIT 1').get(key, key);
    return forProject(proj || null);
  }

  if (scope === 'all') {
    return {
      project: null,
      rows: db()
        .prepare(`SELECT ${cols} FROM repos r JOIN projects p ON p.id = r.project_id ORDER BY p.name, r.name`)
        .all(),
    };
  }

  const projectId = process.env.DEVDY_PROJECT_ID || null;
  if (!projectId) return { project: null, rows: [] };
  return forProject(db().prepare('SELECT id, name FROM projects WHERE id = ?').get(projectId));
}

// ---- VPS / managed servers --------------------------------------------------

/**
 * List managed VPS/servers. When scope="project" (default) and a current
 * project is set, only servers mapped to it (with their deployment role) are
 * returned; otherwise every managed server. Never exposes secrets.
 */
export function listServers({ scope = 'project' } = {}) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  const cols = 's.id, s.label, s.host, s.port, s.username, s.auth_method, s.tags, s.status, s.last_checked_at';
  if (scope === 'project' && projectId) {
    return db()
      .prepare(
        `SELECT ${cols}, ps.role AS role FROM servers s
         JOIN project_servers ps ON ps.server_id = s.id
         WHERE ps.project_id = ? ORDER BY ps.role, s.label`,
      )
      .all(projectId);
  }
  return db().prepare(`SELECT ${cols}, NULL AS role FROM servers s ORDER BY s.label`).all();
}

export function getServer(id) {
  if (!id) throw new Error('server id is required');
  return db()
    .prepare(
      `SELECT id, label, host, port, username, auth_method, private_key_path, tags, status, last_checked_at
       FROM servers WHERE id = ?`,
    )
    .get(id);
}

// ---- Skills & Rules library -------------------------------------------------
//
// Source-of-truth mirrors Devdy's own screens (commands/skills.rs, rules.rs):
//   • skills live in  <app_data>/skills/<name>/SKILL.md  (row in `skills`)
//   • rules  live in  <app_data>/rules/<name>.md          (row in `rules`)
// `<app_data>` is derived from DEVDY_DB_PATH (data.db sits at its root). These
// tools manage the SOURCE only — they never touch per-project artifacts
// (.claude/.codex/AGENTS.md). When a skill/rule is already applied to projects,
// callers get a warning to re-apply via the Devdy app so the sync/hash tracking
// stays correct.

function appDataDir() {
  const dbPath = process.env.DEVDY_DB_PATH;
  if (!dbPath) throw new Error('DEVDY_DB_PATH is not set — cannot locate app data dir');
  return dirname(dbPath);
}
const skillsDir = () => join(appDataDir(), 'skills');
const rulesDir = () => join(appDataDir(), 'rules');

function validateLibName(name, kind) {
  const n = String(name || '').trim();
  if (!n) throw new Error(`${kind} name is required`);
  if (!/^[A-Za-z0-9_-]+$/.test(n)) {
    throw new Error(`${kind} name must only contain letters, numbers, hyphens and underscores`);
  }
  return n;
}
function validateTarget(target) {
  const t = String(target || '').trim();
  if (!['claude', 'codex', 'both'].includes(t)) {
    throw new Error('target must be one of: claude, codex, both');
  }
  return t;
}

/** Project names a skill/rule is currently applied to (for stale-artifact warnings). */
function appliedProjects(kind, id) {
  const sql =
    kind === 'skill'
      ? `SELECT p.name AS name FROM project_skills ps JOIN projects p ON p.id = ps.project_id
         WHERE ps.skill_id = ? ORDER BY p.name`
      : `SELECT p.name AS name FROM project_rules pr JOIN projects p ON p.id = pr.project_id
         WHERE pr.rule_id = ? ORDER BY p.name`;
  return db()
    .prepare(sql)
    .all(id)
    .map((r) => r.name);
}

// ---- Skills ----------------------------------------------------------------

export function listSkills() {
  return db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM skills ORDER BY name')
    .all();
}

export function readSkill(id) {
  if (!id) throw new Error('skill id is required');
  const row = db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM skills WHERE id = ?')
    .get(id);
  if (!row) return null;
  let content = '';
  try {
    content = readFileSync(join(row.source_path, 'SKILL.md'), 'utf8');
  } catch {
    content = '';
  }
  return { ...row, content };
}

export function createSkill({ name, description = '', target = 'claude', content = '' } = {}) {
  const n = validateLibName(name, 'skill');
  const t = validateTarget(target);
  const desc = String(description || '').trim();
  if (!desc) throw new Error('description is required');
  if (db().prepare('SELECT id FROM skills WHERE name = ?').get(n)) {
    throw new Error(`skill '${n}' already exists`);
  }
  const dir = join(skillsDir(), n);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, 'SKILL.md'), String(content || ''));
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare('INSERT INTO skills (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)')
    .run(id, n, desc, t, dir, now);
  return { id, name: n, description: desc, target: t, source_path: dir, updated_at: now };
}

export function updateSkill({ id, name, description, target, content } = {}) {
  if (!id) throw new Error('skill id is required');
  const row = db()
    .prepare('SELECT id, name, description, target, source_path FROM skills WHERE id = ?')
    .get(id);
  if (!row) throw new Error(`skill not found: ${id}`);
  const newName = name != null ? validateLibName(name, 'skill') : row.name;
  const newTarget = target != null ? validateTarget(target) : row.target;
  const newDesc = description != null ? String(description).trim() : row.description;
  if (!newDesc) throw new Error('description cannot be empty');
  if (newName !== row.name && db().prepare('SELECT id FROM skills WHERE name = ? AND id != ?').get(newName, id)) {
    throw new Error(`skill '${newName}' already exists`);
  }
  let sourcePath = row.source_path;
  if (newName !== row.name) {
    sourcePath = join(skillsDir(), newName);
    renameSync(row.source_path, sourcePath);
  }
  if (content != null) writeFileSync(join(sourcePath, 'SKILL.md'), String(content));
  const now = nowIso();
  db()
    .prepare('UPDATE skills SET name = ?, description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?')
    .run(newName, newDesc, newTarget, sourcePath, now, id);
  return {
    skill: { id, name: newName, description: newDesc, target: newTarget, source_path: sourcePath, updated_at: now },
    renamed: newName !== row.name,
    applied: appliedProjects('skill', id),
  };
}

export function deleteSkill(id) {
  if (!id) throw new Error('skill id is required');
  const row = db().prepare('SELECT name, source_path FROM skills WHERE id = ?').get(id);
  if (!row) throw new Error(`skill not found: ${id}`);
  const applied = appliedProjects('skill', id);
  try {
    rmSync(row.source_path, { recursive: true, force: true });
  } catch {
    /* best effort */
  }
  db().prepare('DELETE FROM project_skills WHERE skill_id = ?').run(id);
  db().prepare('DELETE FROM skills WHERE id = ?').run(id);
  return { name: row.name, applied };
}

// ---- Skill reference files -------------------------------------------------
//
// A skill folder (<app_data>/skills/<name>/) may hold more than SKILL.md:
// references/, scripts/, assets/, templates… These helpers let MCP list/read/
// write/delete those files. Every path is resolved and confined to the skill
// folder — traversal outside it (../, absolute paths) is rejected. Same
// source-only caveat as skills_update: applied projects must re-apply to sync.

function skillRow(id) {
  if (!id) throw new Error('skill id is required');
  const row = db().prepare('SELECT id, name, source_path FROM skills WHERE id = ?').get(id);
  if (!row) throw new Error(`skill not found: ${id}`);
  return row;
}

/** Resolve a relative reference path safely inside the skill folder. */
function resolveSkillFile(sourcePath, relPath) {
  const rel = String(relPath || '').trim();
  if (!rel) throw new Error('file path is required');
  if (rel.startsWith('/') || /^[A-Za-z]:[\\/]/.test(rel)) {
    throw new Error('file path must be relative to the skill folder');
  }
  const base = resolve(sourcePath);
  const target = resolve(base, rel);
  if (target !== base && !target.startsWith(base + sep)) {
    throw new Error('file path escapes the skill folder');
  }
  return { base, target };
}

export function listSkillFiles(id) {
  const row = skillRow(id);
  const base = resolve(row.source_path);
  const files = [];
  const walk = (dir) => {
    let entries = [];
    try {
      entries = readdirSync(dir, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const full = join(dir, entry.name);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile()) {
        let size = 0;
        try {
          size = statSync(full).size;
        } catch {
          /* ignore */
        }
        files.push({ path: relative(base, full), size });
      }
    }
  };
  walk(base);
  files.sort((a, b) => a.path.localeCompare(b.path));
  return { skill: { id: row.id, name: row.name, source_path: row.source_path }, files };
}

export function readSkillFile({ id, path: relPath } = {}) {
  const row = skillRow(id);
  const { target } = resolveSkillFile(row.source_path, relPath);
  if (!existsSync(target) || !statSync(target).isFile()) {
    throw new Error(`file not found in skill '${row.name}': ${relPath}`);
  }
  return { skill: row.name, path: relPath, content: readFileSync(target, 'utf8') };
}

export function writeSkillFile({ id, path: relPath, content = '' } = {}) {
  const row = skillRow(id);
  const { target } = resolveSkillFile(row.source_path, relPath);
  if (existsSync(target) && statSync(target).isDirectory()) {
    throw new Error(`path is a directory, not a file: ${relPath}`);
  }
  const created = !existsSync(target);
  mkdirSync(dirname(target), { recursive: true });
  writeFileSync(target, String(content));
  db().prepare('UPDATE skills SET updated_at = ? WHERE id = ?').run(nowIso(), id);
  return { skill: row.name, path: relPath, created, applied: appliedProjects('skill', id) };
}

export function deleteSkillFile({ id, path: relPath } = {}) {
  const row = skillRow(id);
  const { base, target } = resolveSkillFile(row.source_path, relPath);
  if (target === base) throw new Error('cannot delete the skill root folder');
  if (target === join(base, 'SKILL.md')) {
    throw new Error('cannot delete SKILL.md — a skill must keep its definition (use skills_update to edit it)');
  }
  if (!existsSync(target)) throw new Error(`file not found in skill '${row.name}': ${relPath}`);
  rmSync(target, { recursive: true, force: true });
  db().prepare('UPDATE skills SET updated_at = ? WHERE id = ?').run(nowIso(), id);
  return { skill: row.name, path: relPath, applied: appliedProjects('skill', id) };
}

// ---- Rules -----------------------------------------------------------------

export function listRules() {
  return db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM rules ORDER BY name')
    .all();
}

export function readRule(id) {
  if (!id) throw new Error('rule id is required');
  const row = db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM rules WHERE id = ?')
    .get(id);
  if (!row) return null;
  let content = '';
  try {
    content = readFileSync(row.source_path, 'utf8');
  } catch {
    content = '';
  }
  return { ...row, content };
}

export function createRule({ name, description = '', target = 'both', content = '' } = {}) {
  const n = validateLibName(name, 'rule');
  const t = validateTarget(target);
  const desc = String(description || '').trim();
  if (!desc) throw new Error('description is required');
  if (db().prepare('SELECT id FROM rules WHERE name = ?').get(n)) {
    throw new Error(`rule '${n}' already exists`);
  }
  mkdirSync(rulesDir(), { recursive: true });
  const sourcePath = join(rulesDir(), `${n}.md`);
  writeFileSync(sourcePath, String(content || ''));
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare('INSERT INTO rules (id, name, description, target, source_path, updated_at) VALUES (?, ?, ?, ?, ?, ?)')
    .run(id, n, desc, t, sourcePath, now);
  return { id, name: n, description: desc, target: t, source_path: sourcePath, updated_at: now };
}

export function updateRule({ id, name, description, target, content } = {}) {
  if (!id) throw new Error('rule id is required');
  const row = db()
    .prepare('SELECT id, name, description, target, source_path FROM rules WHERE id = ?')
    .get(id);
  if (!row) throw new Error(`rule not found: ${id}`);
  const newName = name != null ? validateLibName(name, 'rule') : row.name;
  const newTarget = target != null ? validateTarget(target) : row.target;
  const newDesc = description != null ? String(description).trim() : row.description;
  if (!newDesc) throw new Error('description cannot be empty');
  if (newName !== row.name && db().prepare('SELECT id FROM rules WHERE name = ? AND id != ?').get(newName, id)) {
    throw new Error(`rule '${newName}' already exists`);
  }
  let sourcePath = row.source_path;
  if (newName !== row.name) {
    sourcePath = join(rulesDir(), `${newName}.md`);
    renameSync(row.source_path, sourcePath);
  }
  if (content != null) writeFileSync(sourcePath, String(content));
  const now = nowIso();
  db()
    .prepare('UPDATE rules SET name = ?, description = ?, target = ?, source_path = ?, updated_at = ? WHERE id = ?')
    .run(newName, newDesc, newTarget, sourcePath, now, id);
  return {
    rule: { id, name: newName, description: newDesc, target: newTarget, source_path: sourcePath, updated_at: now },
    renamed: newName !== row.name,
    applied: appliedProjects('rule', id),
  };
}

export function deleteRule(id) {
  if (!id) throw new Error('rule id is required');
  const row = db().prepare('SELECT name, source_path FROM rules WHERE id = ?').get(id);
  if (!row) throw new Error(`rule not found: ${id}`);
  const applied = appliedProjects('rule', id);
  try {
    rmSync(row.source_path, { force: true });
  } catch {
    /* best effort */
  }
  db().prepare('DELETE FROM project_rules WHERE rule_id = ?').run(id);
  db().prepare('DELETE FROM rules WHERE id = ?').run(id);
  return { name: row.name, applied };
}
