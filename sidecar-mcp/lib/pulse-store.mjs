// Data access for Project Pulse (migration 0050, docs/project-pulse-spec.md §5).
//
// The sidecar only writes SQLite (BR-11): new / re-activated watches land as
// `resolve_status = 'pending'` plus a `pulse_requests` row, and the app's
// poller resolves them (captures the Slack thread, fetches, summarises). No
// network here. The watch rules mirror src-tauri/src/pulse/store.rs.

import { randomUUID } from 'node:crypto';
import { db, nowRfc3339 } from './store.mjs';
import { parseSource, PulseError } from './pulse-source.mjs';

export { PulseError };

const WATCH_COLS = `w.id, w.project_id, p.name AS project_name, w.source_type, w.source_ref, w.source_url,
  w.slack_thread_id, w.title, w.active, w.added_via, w.added_run_id, w.resolve_status, w.last_error,
  w.checked_at, w.created_at, w.updated_at`;
const WATCH_FROM = 'pulse_watches w JOIN projects p ON p.id = w.project_id';
const DIGEST_COLS = `d.id, d.watch_id, d.content_hash, d.status, d.ball, d.ball_since, d.summary, d.next_action,
  d.deadlines, d.decisions, d.open_questions, d.truncated, d.model, d.created_at`;

const BUSY_ATTEMPTS = 5;
const isBusy = (e) => /SQLITE_BUSY|database is locked/i.test(String(e?.message ?? e)) || e?.errcode === 5;
const sleepSync = (ms) => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, ms);

/**
 * Run `fn` in a `BEGIN IMMEDIATE` transaction: the write lock is taken up
 * front, so the read-check-write of the watch rules cannot interleave with the
 * app or another session. Retries with backoff while the database stays busy
 * past busy_timeout.
 */
function writeTx(fn) {
  for (let attempt = 1; ; attempt += 1) {
    const d = db();
    try {
      d.exec('BEGIN IMMEDIATE');
    } catch (e) {
      if (isBusy(e) && attempt < BUSY_ATTEMPTS) {
        sleepSync(50 << attempt);
        continue;
      }
      throw e;
    }
    try {
      const out = fn(d);
      d.exec('COMMIT');
      return out;
    } catch (e) {
      try {
        d.exec('ROLLBACK');
      } catch {
        /* the transaction is already gone — surface the original error */
      }
      if (isBusy(e) && attempt < BUSY_ATTEMPTS) {
        sleepSync(50 << attempt);
        continue;
      }
      throw e;
    }
  }
}

const parseList = (s) => {
  try {
    const v = JSON.parse(s);
    return Array.isArray(v) ? v : [];
  } catch {
    return [];
  }
};

/**
 * `{ id, name }` of the project named by `input` (id, or name matched
 * case-insensitively), else of DEVDY_PROJECT_ID (BR-03).
 */
export function resolveProject(input) {
  const wanted = String(input ?? '').trim();
  const fallback = String(process.env.DEVDY_PROJECT_ID ?? '').trim();
  const key = wanted || fallback;
  if (!key) throw new PulseError('project_required', 'choose a project for this source');
  const byId = db().prepare('SELECT id, name FROM projects WHERE id = ?').get(key);
  if (byId) return { id: byId.id, name: byId.name };
  if (!wanted) throw new PulseError('project_not_found', `project not found: ${key}`);
  const byName = db()
    .prepare('SELECT id, name FROM projects WHERE lower(trim(name)) = lower(?) LIMIT 2')
    .all(key);
  if (byName.length === 1) return { id: byName[0].id, name: byName[0].name };
  if (byName.length > 1) throw new PulseError('project_ambiguous', `several projects are named ${key}; use the project id`);
  throw new PulseError('project_not_found', `project not found: ${key}`);
}

/** `owner/repo` of the project's GitHub repos (for a bare `#<n>`). */
export function projectGithubRepos(projectId) {
  if (!projectId) return [];
  return db()
    .prepare(
      `SELECT DISTINCT lower(github_owner || '/' || github_repo) AS r FROM repos
       WHERE project_id = ? AND provider = 'github'
         AND COALESCE(github_owner, '') <> '' AND COALESCE(github_repo, '') <> ''`,
    )
    .all(projectId)
    .map((x) => x.r);
}

/** BR-02 with DB context: also accepts a Devdy Slack thread id. */
export function normalize(input, projectId) {
  try {
    return parseSource(input, projectGithubRepos(projectId));
  } catch (e) {
    const id = String(input ?? '').trim();
    if (e.code === 'invalid_source' && /^[A-Za-z0-9-]+$/.test(id)) {
      const row = db().prepare('SELECT thread_url FROM slack_threads WHERE id = ?').get(id);
      if (row?.thread_url) return parseSource(row.thread_url, []);
    }
    throw e;
  }
}

const decorateWatch = (w) => w && { ...w, active: !!w.active };

export function getWatch(id) {
  return decorateWatch(db().prepare(`SELECT ${WATCH_COLS} FROM ${WATCH_FROM} WHERE w.id = ?`).get(id));
}

export function findBySource(sourceType, sourceRef) {
  return decorateWatch(
    db()
      .prepare(`SELECT ${WATCH_COLS} FROM ${WATCH_FROM} WHERE w.source_type = ? AND w.source_ref = ?`)
      .get(sourceType, sourceRef),
  );
}

function addRequest(d, kind, { watchId = null, projectId = null } = {}) {
  const id = randomUUID();
  d.prepare('INSERT INTO pulse_requests (id, kind, watch_id, project_id, created_at) VALUES (?, ?, ?, ?, ?)').run(
    id,
    kind,
    watchId,
    projectId,
    nowRfc3339(),
  );
  return id;
}

/**
 * BR-01: one watch per source app-wide.
 * - same project, active → already_watching, nothing changes;
 * - same project, inactive → re-activated;
 * - other project, active → `watched_in_other_project` unless `move`;
 * - other project, inactive → taken over by this project.
 * Every new / re-activated / moved watch gets a `resolve` request.
 */
export function watchSource({ source, projectId, move = false, runId = null }) {
  return writeTx((d) => {
    const w = findBySource(source.source_type, source.source_ref);
    if (w) {
      if (w.project_id === projectId) {
        if (w.active) return { watch: w, already_watching: true };
      } else if (w.active && !move) {
        throw new PulseError('watched_in_other_project', `this source is already watched in project ${w.project_name}`);
      }
      const r = d
        .prepare(
          `UPDATE pulse_watches SET project_id = ?, active = 1, resolve_status = 'pending', updated_at = ?
           WHERE id = ? AND project_id = ? AND active = ?`,
        )
        .run(projectId, nowRfc3339(), w.id, w.project_id, w.active ? 1 : 0);
      if (!r.changes) throw new Error('watch changed concurrently; try again');
      addRequest(d, 'resolve', { watchId: w.id });
      return { watch: getWatch(w.id), already_watching: false };
    }
    const id = randomUUID();
    const ts = nowRfc3339();
    d.prepare(
      `INSERT INTO pulse_watches (id, project_id, source_type, source_ref, source_url, active, added_via,
         added_run_id, resolve_status, created_at, updated_at)
       VALUES (?, ?, ?, ?, ?, 1, 'mcp', ?, 'pending', ?, ?)`,
    ).run(id, projectId, source.source_type, source.source_ref, source.source_url, runId, ts, ts);
    addRequest(d, 'resolve', { watchId: id });
    return { watch: getWatch(id), already_watching: false };
  });
}

/** BR-04: `active = 0`; digests and the Slack thread are kept. */
export function unwatch(id) {
  const r = db().prepare('UPDATE pulse_watches SET active = 0, updated_at = ? WHERE id = ?').run(nowRfc3339(), id);
  if (!r.changes) throw new PulseError('not_found', `watch not found: ${id}`);
}

/** Active watches (+ newest digest) of one project, or every project when `projectId` is null. */
export function listWatches(projectId) {
  const where = projectId ? 'w.active = 1 AND w.project_id = ?' : 'w.active = 1';
  const params = projectId ? [projectId] : [];
  const watches = db()
    .prepare(`SELECT ${WATCH_COLS} FROM ${WATCH_FROM} WHERE ${where} ORDER BY p.name COLLATE NOCASE, w.created_at, w.id`)
    .all(...params)
    .map(decorateWatch);
  const digests = db()
    .prepare(
      `SELECT ${DIGEST_COLS} FROM pulse_digests d JOIN pulse_watches w ON w.id = d.watch_id
       WHERE ${where} AND d.id = (SELECT d2.id FROM pulse_digests d2
         WHERE d2.watch_id = d.watch_id ORDER BY d2.created_at DESC, d2.id DESC LIMIT 1)`,
    )
    .all(...params);
  const byWatch = new Map(
    digests.map((d) => [
      d.watch_id,
      {
        ...d,
        truncated: !!d.truncated,
        deadlines: parseList(d.deadlines),
        decisions: parseList(d.decisions),
        open_questions: parseList(d.open_questions),
      },
    ]),
  );
  return watches.map((w) => ({ ...w, digest: byWatch.get(w.id) ?? null }));
}

/** Queue a refresh for one project (or everything when null); returns the request id. */
export function requestRefresh(projectId) {
  return writeTx((d) => addRequest(d, 'refresh', { projectId }));
}

export function requestDone(id) {
  return !!db().prepare('SELECT done_at FROM pulse_requests WHERE id = ?').get(id)?.done_at;
}
