// Data access for Slack threads (migration 0041, docs/srs-slack-thread-translate.md §5.1).
//
// Threads are fetched from Slack by the app and stored per message (original
// mrkdwn in `text_raw`, Vietnamese in `text_vi`), with `slack_files`,
// `slack_users` and `slack_channels` alongside. The app owns fetching and
// translation; the sidecar only reads, renames, re-links and deletes.
// File paths are stored RELATIVE to <app_data> and returned ABSOLUTE. The
// sidecar connection does not enable `foreign_keys`, so deletes remove child
// rows explicitly instead of relying on ON DELETE CASCADE. Sessions (runs)
// linked to a thread (`slack_thread_runs`, migration 0043) come back as
// `linked_runs`, the run ids that still exist, comma-separated in link order.

import { existsSync, rmSync } from 'node:fs';
import { resolve, sep } from 'node:path';
import { db, tx, normIds, requireProject, nowRfc3339, appDataDir, attachmentAbsPath } from './store.mjs';

const TITLE_MAX = 200;
const THREAD_COLS = `t.id, t.title, t.title_custom, t.project_id, t.position, t.created_at, t.updated_at,
  t.workspace_domain, t.team_id, t.workspace, t.channel_id, t.channel, t.thread_ts, t.thread_url,
  t.fetch_status, t.fetch_error_code, t.fetch_error, t.fetched_at, t.reply_count, t.last_reply_ts,
  t.warnings, t.legacy, t.legacy_content,
  (SELECT COUNT(*) FROM slack_files f WHERE f.thread_id = t.id) AS file_count,
  (SELECT group_concat(run_id, ', ') FROM (SELECT l.run_id FROM slack_thread_runs l
    JOIN runs r ON r.id = l.run_id WHERE l.thread_id = t.id ORDER BY l.linked_at, l.run_id)) AS linked_runs`;
const ORDER = 'ORDER BY t.position ASC, t.updated_at DESC';
const MESSAGE_COLS = `ts, seq, user_id, bot_id, author_name, author_icon_url, subtype, text_raw, text_vi,
  translate_status, translate_error, edited_ts, reactions, file_ids`;

const clampLimit = (limit, def) => Math.max(1, Math.min(200, Number(limit) || def));
const norm = (s) => s.normalize('NFC').toLowerCase();
const parseJson = (s, fallback) => {
  try {
    const v = JSON.parse(s);
    return Array.isArray(v) ? v : fallback;
  } catch {
    return fallback;
  }
};

/** WHERE fragment + params for scope="project" (default) | "all", mirroring notes. */
function scopeFilter(scope) {
  const projectId = process.env.DEVDY_PROJECT_ID || null;
  return scope !== 'all' && projectId ? { sql: 't.project_id = ?', params: [projectId] } : { sql: '1 = 1', params: [] };
}

function decorate(row) {
  return row && { ...row, warnings: parseJson(row.warnings, []) };
}

/** <app_data>/slack-threads/<id>, or null when the id isn't a safe single path segment. */
function threadDir(id) {
  if (!/^[A-Za-z0-9_-]+$/.test(String(id))) return null;
  const base = resolve(appDataDir(), 'slack-threads');
  const target = resolve(base, String(id));
  return target.startsWith(base + sep) ? target : null;
}

export function listThreads({ scope = 'project', limit = 50 } = {}) {
  const f = scopeFilter(scope);
  return db()
    .prepare(`SELECT ${THREAD_COLS} FROM slack_threads t WHERE ${f.sql} ${ORDER} LIMIT ?`)
    .all(...f.params, clampLimit(limit, 50))
    .map(decorate);
}

/**
 * One thread with everything needed to render it: messages (by seq), files
 * (with ABSOLUTE `path` when downloaded), and the user / channel name maps of
 * its workspace. Returns undefined for an unknown id.
 */
export function readThread(id) {
  if (!id) throw new Error('slack thread id is required');
  const thread = decorate(db().prepare(`SELECT ${THREAD_COLS} FROM slack_threads t WHERE t.id = ?`).get(id));
  if (!thread) return undefined;
  const messages = db()
    .prepare(`SELECT ${MESSAGE_COLS} FROM slack_messages WHERE thread_id = ? ORDER BY seq ASC, ts ASC`)
    .all(id)
    .map((m) => ({ ...m, reactions: parseJson(m.reactions, []), file_ids: parseJson(m.file_ids, []) }));
  const files = db()
    .prepare(
      `SELECT id, slack_file_id, message_ts, name, title, mime, filetype, size, file_path, permalink,
              download_status, download_error, created_at
       FROM slack_files WHERE thread_id = ? ORDER BY created_at ASC, name ASC`,
    )
    .all(id)
    .map((f) => {
      let path = null;
      if (f.download_status === 'done' && f.file_path) {
        try {
          path = attachmentAbsPath(f.file_path);
        } catch {
          path = null;
        }
      }
      return { ...f, path };
    });
  return { ...thread, messages, files, ...nameMaps(thread.team_id) };
}

/**
 * user_id → { name, real_name } and channel_id → name for one workspace. Legacy
 * rows have no team_id yet, so they fall back to every cached workspace.
 */
function nameMaps(teamId) {
  const where = teamId ? 'WHERE team_id = ?' : '';
  const params = teamId ? [teamId] : [];
  const users = {};
  for (const u of db()
    .prepare(`SELECT user_id, display_name, real_name FROM slack_users ${where} ORDER BY fetched_at ASC`)
    .all(...params)) {
    users[u.user_id] = { name: u.display_name || u.real_name || u.user_id, real_name: u.real_name };
  }
  const channels = {};
  for (const c of db()
    .prepare(`SELECT channel_id, name FROM slack_channels ${where} ORDER BY fetched_at ASC`)
    .all(...params)) {
    if (c.name) channels[c.channel_id] = c.name;
  }
  return { users, channels };
}

/**
 * Case-insensitive substring search over title, channel, legacy_content and the
 * messages' text_vi / text_raw. Matching runs in JS (NFC + toLowerCase) because
 * SQLite's LIKE only folds ASCII case, which would miss Vietnamese "Triển" vs
 * "TRIỂN". Each hit carries up to 3 matching messages and the name maps needed
 * to render them.
 */
export function searchThreads({ query, scope = 'project', limit = 20 } = {}) {
  const q = norm(String(query || '').trim());
  if (!q) throw new Error('query is required');
  const lim = clampLimit(limit, 20);
  const f = scopeFilter(scope);
  const threads = db().prepare(`SELECT ${THREAD_COLS} FROM slack_threads t WHERE ${f.sql} ${ORDER}`).all(...f.params);
  const msgsStmt = db().prepare(`SELECT ${MESSAGE_COLS} FROM slack_messages WHERE thread_id = ? ORDER BY seq ASC, ts ASC`);
  const has = (v) => v != null && norm(String(v)).includes(q);
  const out = [];
  for (const r of threads) {
    if (out.length >= lim) break;
    const hits = msgsStmt.all(r.id).filter((m) => has(m.text_vi) || has(m.text_raw));
    if (!hits.length && !has(r.title) && !has(r.channel) && !has(r.legacy_content)) continue;
    const t = decorate(r);
    out.push({ ...t, hits: hits.slice(0, 3), ...nameMaps(t.team_id) });
  }
  return out;
}

/** Set a custom title (1–200 chars, trimmed); title_custom = 1 keeps it on refresh / re-import. */
export function renameThread({ id, title } = {}) {
  if (!id) throw new Error('slack thread id is required');
  const t = String(title ?? '').trim();
  if (!t) throw new Error('title is required');
  if ([...t].length > TITLE_MAX) throw new Error(`title must be at most ${TITLE_MAX} characters`);
  const now = nowRfc3339();
  const res = db()
    .prepare('UPDATE slack_threads SET title = ?, title_custom = 1, updated_at = ? WHERE id = ?')
    .run(t, now, id);
  if (res.changes === 0) throw new Error(`slack thread not found: ${id}`);
  return { id, title: t, title_custom: 1, updated_at: now };
}

/** Link a thread to the current project, or detach it with scope="global". */
export function setThreadProject({ id, scope = 'project' } = {}) {
  if (!id) throw new Error('slack thread id is required');
  if (!db().prepare('SELECT 1 FROM slack_threads WHERE id = ?').get(id)) {
    throw new Error(`slack thread not found: ${id}`);
  }
  const projectId = scope === 'global' ? null : requireProject();
  db()
    .prepare('UPDATE slack_threads SET project_id = ?, updated_at = ? WHERE id = ?')
    .run(projectId, nowRfc3339(), id);
  return { id, project_id: projectId };
}

/**
 * Delete threads: slack_messages, slack_files and slack_thread_runs rows
 * explicitly, then the thread row, all in one transaction; afterwards each
 * deleted thread's <app_data>/slack-threads/<id>/ dir is removed (only for safe
 * ids). The shared avatar cache (slack-cache/) is never touched.
 */
export function deleteThreads({ ids } = {}) {
  const list = normIds(ids);
  const deleted = tx((d) => {
    const delMsgs = d.prepare('DELETE FROM slack_messages WHERE thread_id = ?');
    const delFiles = d.prepare('DELETE FROM slack_files WHERE thread_id = ?');
    const delLinks = d.prepare('DELETE FROM slack_thread_runs WHERE thread_id = ?');
    const delRow = d.prepare('DELETE FROM slack_threads WHERE id = ?');
    const exists = d.prepare('SELECT 1 FROM slack_threads WHERE id = ?');
    return list.filter((id) => {
      if (!exists.get(id)) return false;
      delMsgs.run(id);
      delFiles.run(id);
      delLinks.run(id);
      return delRow.run(id).changes > 0;
    });
  });
  let dirsRemoved = 0;
  for (const id of deleted) {
    const dir = threadDir(id);
    if (!dir || !existsSync(dir)) continue;
    try {
      rmSync(dir, { recursive: true, force: true });
      dirsRemoved += 1;
    } catch {
      /* best effort — the rows are already gone */
    }
  }
  return { deleted: deleted.length, requested: list.length, dirs_removed: dirsRemoved };
}
