// Thin data-access layer over Devdy's SQLite store (`data.db`).
//
// Opened read-write because the notes tools mutate. Reads (runs/sessions) are
// scoped to the current project by default via DEVDY_PROJECT_ID. We open lazily
// and reuse a single handle for the process lifetime.

import { DatabaseSync } from 'node:sqlite';
import { randomUUID } from 'node:crypto';
import {
  mkdirSync,
  writeFileSync,
  readFileSync,
  rmSync,
  renameSync,
  readdirSync,
  statSync,
  existsSync,
  lstatSync,
  readlinkSync,
  unlinkSync,
  realpathSync,
} from 'node:fs';
import { dirname, join, resolve, relative, sep } from 'node:path';

let handle = null;

function db() {
  if (handle) return handle;
  const dbPath = process.env.DEVDY_DB_PATH;
  if (!dbPath) throw new Error('DEVDY_DB_PATH is not set — cannot open Devdy store');
  handle = new DatabaseSync(dbPath);
  // The conductor runs several sidecar processes at once, each with its own
  // connection into the same data.db. Without a busy timeout, concurrent writers
  // (e.g. two workers appending to the shared master note) would fail with
  // SQLITE_BUSY instead of serializing; wait for the lock instead.
  handle.exec('PRAGMA busy_timeout = 5000');
  return handle;
}

const nowIso = () => new Date().toISOString();

// Shared with slack-store.mjs (same handle, same transaction / id / scope rules).
export { db, tx, normIds, requireProject, nowRfc3339 };

/**
 * Current UTC time in the exact shape of Rust's `chrono::Utc::now().to_rfc3339()`
 * (`2026-10-08T09:00:00.123456+00:00`): `+00:00` offset instead of `Z`, and the
 * fraction printed like chrono's AutoSi — omitted when zero, else 3 or 6 digits
 * (JS has no nanosecond clock, so we stop at microseconds). Used for tables the
 * Rust backend also writes, so `ORDER BY updated_at` compares like with like.
 */
function nowRfc3339() {
  const micros = Math.floor((performance.timeOrigin + performance.now()) * 1000);
  const secs = Math.floor(micros / 1e6);
  const frac = micros - secs * 1e6;
  const base = new Date(secs * 1000).toISOString().slice(0, 19);
  let f = '';
  if (frac) f = frac % 1000 === 0 ? `.${String(frac / 1000).padStart(3, '0')}` : `.${String(frac).padStart(6, '0')}`;
  return `${base}${f}+00:00`;
}

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
function reorderRows(table, ids, orderBy, { where = '', params = [], label = table } = {}) {
  // `where`/`params` narrow the set being renumbered.
  const filter = where ? `WHERE ${where}` : '';
  const all = db()
    .prepare(`SELECT id FROM ${table} ${filter} ORDER BY ${orderBy}`)
    .all(...params)
    .map((r) => r.id);
  const known = new Set(all);
  const missing = ids.filter((id) => !known.has(id));
  if (missing.length) throw new Error(`unknown ${label} id(s): ${missing.join(', ')}`);
  const moved = new Set(ids);
  const order = [...ids, ...all.filter((id) => !moved.has(id))];
  return tx((d) => {
    const stmt = d.prepare(`UPDATE ${table} SET position = ? WHERE id = ?`);
    order.forEach((id, index) => stmt.run(index, id));
    return { ordered: order.length };
  });
}

// ---- Notes -----------------------------------------------------------------

/**
 * Session (run) ids linked to a note / todo (`note_runs` / `todo_runs`,
 * migration 0044) whose run still exists, comma-separated in link order, as
 * column `linked_runs`. `alias` is the items table alias.
 */
function linkedRunsCol(links, col, alias) {
  return `(SELECT group_concat(run_id, ', ') FROM (SELECT l.run_id FROM ${links} l
    JOIN runs r ON r.id = l.run_id WHERE l.${col} = ${alias}.id ORDER BY l.linked_at, l.run_id)) AS linked_runs`;
}
const NOTE_LINKS = linkedRunsCol('note_runs', 'note_id', 'n');
const TODO_LINKS = linkedRunsCol('todo_runs', 'todo_id', 't');

export function listNotes({ scope = 'project', limit = 50 } = {}) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  const lim = Math.max(1, Math.min(200, Number(limit) || 50));
  if (scope === 'project' && projectId) {
    return db()
      .prepare(
        `SELECT n.id, n.title, n.content, n.project_id, n.updated_at, ${NOTE_LINKS}
         FROM notes n WHERE n.project_id = ?
         ORDER BY n.position ASC, n.updated_at DESC LIMIT ?`,
      )
      .all(projectId, lim);
  }
  return db()
    .prepare(
      `SELECT n.id, n.title, n.content, n.project_id, n.updated_at, ${NOTE_LINKS}
       FROM notes n ORDER BY n.position ASC, n.updated_at DESC LIMIT ?`,
    )
    .all(lim);
}

export function readNote(id) {
  if (!id) throw new Error('note id is required');
  return db()
    .prepare(
      `SELECT n.id, n.title, n.content, n.project_id, n.created_at, n.updated_at, ${NOTE_LINKS}
       FROM notes n WHERE n.id = ?`,
    )
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
  const t = title != null ? String(title).trim() : null;
  const c = content != null ? String(content).trim() : null;
  if (t == null && c == null) throw new Error('a title or content is required');
  // Don't let a partial update empty the note entirely.
  if ((t != null ? t : existing.title) === '' && (c != null ? c : existing.content) === '') {
    throw new Error('a title or content is required');
  }
  const now = nowIso();
  // COALESCE so an omitted field keeps its current DB value atomically. Resolving
  // omitted fields from a prior read and writing them all back is a read-modify-
  // write: a partial update (e.g. title only) would rewrite `content` with the
  // value it read, silently clobbering a content change a concurrent writer just
  // committed. Passing NULL for omitted fields leaves that column untouched.
  db()
    .prepare(
      'UPDATE notes SET title = COALESCE(?, title), content = COALESCE(?, content), updated_at = ? WHERE id = ?',
    )
    .run(t, c, now, id);
  const row = readNote(id);
  return { id, title: row.title, content: row.content, updated_at: now };
}

export function appendNote({ id, text } = {}) {
  if (!id) throw new Error('note id is required');
  const add = String(text || '').trim();
  if (!add) throw new Error('text is required');
  const now = nowIso();
  // Concatenate inside the UPDATE so the read and write are one atomic statement.
  // A read-modify-write in JS (read content, append in memory, write back) lets a
  // concurrent writer clobber us: two workers both read the old content and the
  // second write wins, silently dropping the first append. Doing `content || ...`
  // in SQL means SQLite holds the write lock for the whole read-append-write.
  const res = db()
    .prepare(
      `UPDATE notes
          SET content = CASE
                WHEN content IS NULL OR content = '' THEN ?
                ELSE content || char(10) || char(10) || ?
              END,
              updated_at = ?
        WHERE id = ?`,
    )
    .run(add, add, now, id);
  if (res.changes === 0) throw new Error(`note not found: ${id}`);
  const row = db().prepare('SELECT content FROM notes WHERE id = ?').get(id);
  return { id, content: row.content, updated_at: now };
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
    // No `foreign_keys` on this connection: drop the session links explicitly.
    const links = d.prepare('DELETE FROM note_runs WHERE note_id = ?');
    const stmt = d.prepare('DELETE FROM notes WHERE id = ?');
    const deleted = list.filter((id) => {
      links.run(id);
      return stmt.run(id).changes > 0;
    });
    return { deleted: deleted.length, requested: list.length };
  });
}

/** Link a note to the current project, or detach it with scope="global". */
export function setNoteProject({ id, scope = 'project' } = {}) {
  if (!readNote(id)) throw new Error(`note not found: ${id}`);
  const now = nowIso();
  if (scope === 'global') {
    db().prepare('UPDATE notes SET project_id = NULL, updated_at = ? WHERE id = ?').run(now, id);
    return { id, project_id: null };
  }
  const projectId = requireProject();
  db().prepare('UPDATE notes SET project_id = ?, updated_at = ? WHERE id = ?').run(projectId, now, id);
  return { id, project_id: projectId };
}

export function reorderNotes({ ids } = {}) {
  return reorderRows('notes', normIds(ids), 'position ASC, updated_at DESC');
}

// ---- App data files -----------------------------------------------------------

/** Resolve a stored (app_data-relative) file path to an absolute one inside app_data. */
export function attachmentAbsPath(filePath) {
  const base = resolve(appDataDir());
  const target = resolve(base, String(filePath || ''));
  if (target === base || !target.startsWith(base + sep)) {
    throw new Error(`attachment path escapes the app data dir: ${filePath}`);
  }
  return target;
}

// ---- Sessions / runs --------------------------------------------------------

const YMD_RE = /^\d{4}-\d{2}-\d{2}$/;

/** Local `YYYY-MM-DD` → UTC ISO instant of that local day's start (+`dayOffset` days). */
function localDayStartIso(ymd, dayOffset = 0) {
  const [y, m, d] = ymd.split('-').map(Number);
  return new Date(y, m - 1, d + dayOffset).toISOString();
}

/**
 * Recent runs, newest first, with tokens + cost. Optional `from` / `to` are
 * inclusive LOCAL dates (`YYYY-MM-DD`). With a range, a run is included when it
 * started in the range OR has usage (a turn) in it — long-lived sessions reset
 * `started_at` on every turn, so start time alone misses earlier days — and
 * tokens / cost / `agent_ms` count only the usage that happened in the range.
 */
export function recentRuns({ scope = 'project', limit, from, to } = {}) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  for (const [name, v] of [['from', from], ['to', to]]) {
    if (v && !YMD_RE.test(String(v))) throw new Error(`${name} must be YYYY-MM-DD`);
  }
  if (from && to && from > to) throw new Error('from must be on or before to');
  const ranged = Boolean(from || to);
  // A date range is the real bound, so allow the full cap by default there.
  const lim = Math.max(1, Math.min(200, Number(limit) || (ranged ? 200 : 20)));
  // Stored timestamps are UTC RFC3339; julianday() compares them as instants.
  const lo = from ? localDayStartIso(from) : '0001-01-01T00:00:00Z';
  const hi = to ? localDayStartIso(to, 1) : '9999-12-31T00:00:00Z';
  const inRange = (col) => `julianday(${col}) >= julianday(?) AND julianday(${col}) < julianday(?)`;

  const where = [];
  const params = [];
  if (ranged) params.push(lo, hi); // usage subquery
  if (scope === 'project' && projectId) {
    where.push('r.project_id = ?');
    params.push(projectId);
  }
  if (ranged) {
    where.push(`((${inRange('COALESCE(r.started_at, r.created_at)')}) OR u.run_id IS NOT NULL)`);
    params.push(lo, hi);
  }
  const sql = `SELECT r.id, r.title, r.type, r.ref_number, r.engine, r.status, r.session_id,
                      r.transcript_path, r.created_at, r.started_at, r.finished_at,
                      p.name AS project_name, p.path AS project_path,
                      COALESCE(u.tokens, 0) AS tokens, COALESCE(u.cost, 0) AS cost,
                      COALESCE(u.agent_ms, 0) AS agent_ms
               FROM runs r JOIN projects p ON p.id = r.project_id
               LEFT JOIN (
                 SELECT run_id, SUM(total_tokens) AS tokens, SUM(cost_usd) AS cost,
                        SUM(duration_ms) AS agent_ms
                 FROM run_usage WHERE run_id IS NOT NULL ${ranged ? `AND ${inRange('created_at')}` : ''}
                 GROUP BY run_id
               ) u ON u.run_id = r.id
               ${where.length ? `WHERE ${where.join(' AND ')}` : ''}
               ORDER BY r.created_at DESC
               LIMIT ?`;
  return db().prepare(sql).all(...params, lim);
}

/** Wall-clock seconds of a run (running runs count up to now); null if unknown. */
export function runWallSecs(run, now = Date.now()) {
  const start = Date.parse(run.started_at || '');
  if (Number.isNaN(start)) return null;
  const end = run.finished_at ? Date.parse(run.finished_at) : run.status === 'running' ? now : NaN;
  if (Number.isNaN(end)) return null;
  return Math.max(0, Math.round((end - start) / 1000));
}

/** Total wall-clock of several runs, union-merging overlaps so parallel runs aren't double counted. */
export function mergedWallSecs(runs, now = Date.now()) {
  const spans = runs
    .map((r) => {
      const secs = runWallSecs(r, now);
      if (!secs) return null;
      const s = Date.parse(r.started_at);
      return [s, s + secs * 1000];
    })
    .filter(Boolean)
    .sort((a, b) => a[0] - b[0]);
  let total = 0;
  let cur = null;
  for (const [s, e] of spans) {
    if (cur && s <= cur[1]) cur[1] = Math.max(cur[1], e);
    else {
      if (cur) total += cur[1] - cur[0];
      cur = [s, e];
    }
  }
  if (cur) total += cur[1] - cur[0];
  return Math.round(total / 1000);
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

/** A deadline as stored: `YYYY-MM-DD` (a real calendar date), or null to clear. */
function normDue(due) {
  if (due == null || String(due).trim() === '') return null;
  const d = String(due).trim();
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(d);
  const date = m && new Date(Date.UTC(+m[1], +m[2] - 1, +m[3]));
  if (!date || date.toISOString().slice(0, 10) !== d) {
    throw new Error(`invalid due date (want YYYY-MM-DD): ${d}`);
  }
  return d;
}

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
      `SELECT t.id, t.text, t.done, t.position, t.created_at, t.project_id, t.due_date, ${TODO_LINKS},
              p.name AS project_name
       FROM todos t LEFT JOIN projects p ON p.id = t.project_id
       ${where} ORDER BY t.position ASC, t.created_at DESC`,
    )
    .all(...params);
}

/** New todos go to the top (a position below the current minimum), like the app. */
export function addTodo({ text, scope = 'project', due } = {}) {
  const t = String(text || '').trim();
  if (!t) throw new Error('text is required');
  const dueDate = normDue(due);
  const projectId = scopedProjectId(scope);
  const min = db().prepare('SELECT MIN(position) AS m FROM todos').get();
  const position = (min && min.m != null ? min.m : 0) - 1;
  const id = randomUUID();
  const now = nowIso();
  db()
    .prepare(
      'INSERT INTO todos (id, text, done, position, created_at, project_id, due_date) VALUES (?, ?, 0, ?, ?, ?, ?)',
    )
    .run(id, t, position, now, projectId, dueDate);
  return { id, text: t, project_id: projectId, due_date: dueDate };
}

export function readTodo(id) {
  if (!id) throw new Error('id is required');
  return db()
    .prepare(
      `SELECT t.id, t.text, t.done, t.position, t.created_at, t.project_id, t.due_date, ${TODO_LINKS},
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

/** Set a todo's deadline (`YYYY-MM-DD`), or clear it with an empty / null due. */
export function setTodoDue({ id, due } = {}) {
  if (!id) throw new Error('id is required');
  const dueDate = normDue(due);
  const row = db().prepare('SELECT id FROM todos WHERE id = ?').get(id);
  if (!row) throw new Error(`todo not found: ${id}`);
  db().prepare('UPDATE todos SET due_date = ? WHERE id = ?').run(dueDate, id);
  return { id, due_date: dueDate };
}

export function deleteTodos({ ids } = {}) {
  const list = normIds(ids);
  return tx((d) => {
    // No `foreign_keys` on this connection: drop the session links explicitly.
    const links = d.prepare('DELETE FROM todo_runs WHERE todo_id = ?');
    const stmt = d.prepare('DELETE FROM todos WHERE id = ?');
    const deleted = list.filter((id) => {
      links.run(id);
      return stmt.run(id).changes > 0;
    });
    return { deleted: deleted.length, requested: list.length };
  });
}

export function clearDoneTodos() {
  return tx((d) => {
    d.prepare('DELETE FROM todo_runs WHERE todo_id IN (SELECT id FROM todos WHERE done = 1)').run();
    const r = d.prepare('DELETE FROM todos WHERE done = 1').run();
    return { deleted: Number(r.changes) };
  });
}

/** Link a todo to the current project, or detach it with scope="global". */
export function setTodoProject({ id, scope = 'project' } = {}) {
  if (!id) throw new Error('id is required');
  if (!db().prepare('SELECT id FROM todos WHERE id = ?').get(id)) {
    throw new Error(`todo not found: ${id}`);
  }
  if (scope === 'global') {
    db().prepare('UPDATE todos SET project_id = NULL WHERE id = ?').run(id);
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
// `<app_data>` is derived from DEVDY_DB_PATH (data.db sits at its root).
// Projects symlink to these sources (.claude/skills, .claude/rules, .agents/skills),
// so content edits here are live in every project. What these tools do NOT do is
// the rest of the sync (AGENTS.md rule blocks, generated Codex skills of rules,
// new links after a rename/target change) — callers get a warning to re-apply via
// the Devdy app. On rename/delete they only drop the project links that would
// otherwise dangle.

export function appDataDir() {
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

/** Paths of projects a skill/rule is applied to. */
function appliedProjectPaths(kind, id) {
  const sql =
    kind === 'skill'
      ? 'SELECT p.path AS path FROM project_skills ps JOIN projects p ON p.id = ps.project_id WHERE ps.skill_id = ?'
      : 'SELECT p.path AS path FROM project_rules pr JOIN projects p ON p.id = pr.project_id WHERE pr.rule_id = ?';
  return db()
    .prepare(sql)
    .all(id)
    .map((r) => r.path);
}

/** Remove `linkPath` if it is a symlink pointing at `target`; anything else is left alone. */
function unlinkIfPointsTo(linkPath, target) {
  try {
    if (lstatSync(linkPath).isSymbolicLink() && resolve(dirname(linkPath), readlinkSync(linkPath)) === resolve(target)) {
      unlinkSync(linkPath);
    }
  } catch {
    /* missing or unreadable — nothing to drop */
  }
}

/** `to` already exists and is not just `from` under another letter case. */
function occupied(from, to) {
  if (!existsSync(to)) return false;
  try {
    return realpathSync(to) !== realpathSync(from);
  } catch {
    return true;
  }
}

/** Where Devdy generates the Codex-skill form of a rule (see rules.rs `codex_skill_source`). */
const ruleCodexSkillDir = (sourcePath, name) => join(dirname(sourcePath), '.codex-skills', name);

/**
 * Remove Devdy's managed block `kind`/`name` from a project's AGENTS.md, collapsing the blank
 * lines around it; an AGENTS.md left empty is deleted. Mirrors agents_block.rs `remove`.
 */
function removeAgentsBlock(projectPath, kind, name) {
  const file = join(projectPath, 'AGENTS.md');
  let content;
  try {
    content = readFileSync(file, 'utf8');
  } catch {
    return;
  }
  const start = `<!-- devdy:${kind}:${name} START -->`;
  const end = `<!-- devdy:${kind}:${name} END -->`;
  const s = content.indexOf(start);
  if (s === -1) return;
  const eRel = content.slice(s).indexOf(end);
  if (eRel === -1) return;
  const before = content.slice(0, s).trimEnd();
  const after = content.slice(s + eRel + end.length).trimStart();
  const merged = before && after ? `${before}\n\n${after}` : before || after;
  if (!merged) rmSync(file, { force: true });
  else writeFileSync(file, merged.endsWith('\n') ? merged : `${merged}\n`);
}

/**
 * Drop what a skill left in its projects under `name` (after a rename or before a delete):
 * links to `sourcePath` (dangling ones included) and the copy-era AGENTS.md pointer block.
 * Copy-era folders (.codex/skills, real dirs) are left for the user.
 */
function unlinkSkillFromProjects(id, name, sourcePath) {
  for (const p of appliedProjectPaths('skill', id)) {
    unlinkIfPointsTo(join(p, '.claude', 'skills', name), sourcePath);
    unlinkIfPointsTo(join(p, '.agents', 'skills', name), sourcePath);
    removeAgentsBlock(p, 'skill', name);
  }
}

/** Same for a rule: its links, its AGENTS.md block and its generated Codex skill. */
function unlinkRuleFromProjects(id, name, sourcePath) {
  const codexSkill = ruleCodexSkillDir(sourcePath, name);
  for (const p of appliedProjectPaths('rule', id)) {
    unlinkIfPointsTo(join(p, '.claude', 'rules', `${name}.md`), sourcePath);
    unlinkIfPointsTo(join(p, '.agents', 'skills', name), codexSkill);
    removeAgentsBlock(p, 'rule', name);
  }
  rmSync(codexSkill, { recursive: true, force: true });
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
  // null = field omitted → keep current value (COALESCE below), so an omitted
  // field can't clobber a concurrent writer's change to it.
  const descParam = description != null ? String(description).trim() : null;
  if (descParam === '') throw new Error('description cannot be empty');
  const targetParam = target != null ? validateTarget(target) : null;
  const newName = name != null ? validateLibName(name, 'skill') : row.name;
  if (newName !== row.name && db().prepare('SELECT id FROM skills WHERE name = ? AND id != ?').get(newName, id)) {
    throw new Error(`skill '${newName}' already exists`);
  }
  let sourcePath = row.source_path;
  if (newName !== row.name) {
    sourcePath = join(skillsDir(), newName);
    if (occupied(row.source_path, sourcePath)) throw new Error(`skill folder '${newName}' already exists`);
    // Move first: if it fails the projects keep working links.
    renameSync(row.source_path, sourcePath);
    unlinkSkillFromProjects(id, row.name, row.source_path);
  }
  if (content != null) writeFileSync(join(sourcePath, 'SKILL.md'), String(content));
  const now = nowIso();
  db()
    .prepare(
      'UPDATE skills SET name = ?, description = COALESCE(?, description), target = COALESCE(?, target), source_path = ?, updated_at = ? WHERE id = ?',
    )
    .run(newName, descParam, targetParam, sourcePath, now, id);
  const updated = db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM skills WHERE id = ?')
    .get(id);
  return {
    skill: updated,
    renamed: newName !== row.name,
    retargeted: updated.target !== row.target,
    applied: appliedProjects('skill', id),
  };
}

export function deleteSkill(id) {
  if (!id) throw new Error('skill id is required');
  const row = db().prepare('SELECT name, source_path FROM skills WHERE id = ?').get(id);
  if (!row) throw new Error(`skill not found: ${id}`);
  const applied = appliedProjects('skill', id);
  unlinkSkillFromProjects(id, row.name, row.source_path);
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
  // null = field omitted → keep current value (COALESCE below), so an omitted
  // field can't clobber a concurrent writer's change to it.
  const descParam = description != null ? String(description).trim() : null;
  if (descParam === '') throw new Error('description cannot be empty');
  const targetParam = target != null ? validateTarget(target) : null;
  const newName = name != null ? validateLibName(name, 'rule') : row.name;
  if (newName !== row.name && db().prepare('SELECT id FROM rules WHERE name = ? AND id != ?').get(newName, id)) {
    throw new Error(`rule '${newName}' already exists`);
  }
  let sourcePath = row.source_path;
  if (newName !== row.name) {
    sourcePath = join(rulesDir(), `${newName}.md`);
    // renameSync would silently replace another rule's file — and every project linking to it.
    if (occupied(row.source_path, sourcePath)) throw new Error(`rule file '${newName}.md' already exists`);
    // Move first: if it fails the projects keep working links.
    renameSync(row.source_path, sourcePath);
    unlinkRuleFromProjects(id, row.name, row.source_path);
  }
  if (content != null) writeFileSync(sourcePath, String(content));
  const now = nowIso();
  db()
    .prepare(
      'UPDATE rules SET name = ?, description = COALESCE(?, description), target = COALESCE(?, target), source_path = ?, updated_at = ? WHERE id = ?',
    )
    .run(newName, descParam, targetParam, sourcePath, now, id);
  const updated = db()
    .prepare('SELECT id, name, description, target, source_path, updated_at FROM rules WHERE id = ?')
    .get(id);
  return {
    rule: updated,
    renamed: newName !== row.name,
    retargeted: updated.target !== row.target,
    applied: appliedProjects('rule', id),
  };
}

export function deleteRule(id) {
  if (!id) throw new Error('rule id is required');
  const row = db().prepare('SELECT name, source_path FROM rules WHERE id = ?').get(id);
  if (!row) throw new Error(`rule not found: ${id}`);
  const applied = appliedProjects('rule', id);
  unlinkRuleFromProjects(id, row.name, row.source_path);
  try {
    rmSync(row.source_path, { force: true });
  } catch {
    /* best effort */
  }
  db().prepare('DELETE FROM project_rules WHERE rule_id = ?').run(id);
  db().prepare('DELETE FROM rules WHERE id = ?').run(id);
  return { name: row.name, applied };
}
