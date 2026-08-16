// Thin data-access layer over Devdy's SQLite store (`data.db`).
//
// Opened read-write because the notes tools mutate. Reads (runs/sessions) are
// scoped to the current project by default via DEVDY_PROJECT_ID. We open lazily
// and reuse a single handle for the process lifetime.

import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import { mkdirSync, writeFileSync, readFileSync, rmSync, renameSync } from 'node:fs';
import { dirname, join } from 'node:path';

let handle = null;

function db() {
  if (handle) return handle;
  const dbPath = process.env.DEVDY_DB_PATH;
  if (!dbPath) throw new Error('DEVDY_DB_PATH is not set — cannot open Devdy store');
  handle = new DatabaseSync(dbPath);
  return handle;
}

const nowIso = () => new Date().toISOString();

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
  const projectId = scope === 'global' ? null : process.env.DEVDY_PROJECT_ID || null;
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

export function listTodos({ includeDone = true } = {}) {
  const where = includeDone ? '' : 'WHERE done = 0';
  return db()
    .prepare(`SELECT id, text, done, position, created_at FROM todos ${where} ORDER BY position ASC`)
    .all();
}

export function addTodo({ text } = {}) {
  const t = String(text || '').trim();
  if (!t) throw new Error('text is required');
  const min = db().prepare('SELECT MIN(position) AS m FROM todos').get();
  const position = (min && min.m != null ? min.m : 0) - 1;
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare('INSERT INTO todos (id, text, done, position, created_at) VALUES (?, ?, 0, ?, ?)')
    .run(id, t, position, now);
  return { id, text: t };
}

export function setTodoDone({ id, done = true } = {}) {
  if (!id) throw new Error('id is required');
  const row = db().prepare('SELECT id FROM todos WHERE id = ?').get(id);
  if (!row) throw new Error(`todo not found: ${id}`);
  db().prepare('UPDATE todos SET done = ? WHERE id = ?').run(done ? 1 : 0, id);
  return { id, done: !!done };
}

// ---- Project info -----------------------------------------------------------

export function currentProject() {
  const id = process.env.DEVDY_PROJECT_ID || null;
  if (!id) return null;
  return db()
    .prepare('SELECT id, name, path, github_owner, github_repo, default_engine FROM projects WHERE id = ?')
    .get(id);
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
