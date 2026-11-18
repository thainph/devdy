// Delivery Radar data for the built-in MCP server.
//
// Reads the boards the app saved in `radar_snapshots` (one per project, the
// last `list_milestone_board` result) and the radar window's settings mirrored
// to `settings.radar_settings`. This process holds no GitHub credentials: a
// stale or forced refresh asks the broker (tool `radar`), which refetches in
// the app with each project's accounts and saves new snapshots; then we re-read.

import { db, findProject } from './store.mjs';
import { askBroker } from './broker.mjs';
import { DEFAULT_THRESHOLDS } from './radar.mjs';

/** Same as the radar window: a snapshot younger than this is served as-is. */
export const REVALIDATE_MS = 5 * 60 * 1000;
const REFRESH_TIMEOUT_MS = 180_000;

/** Projects the radar covers: those with a GitHub repo (as the app's `radar_project_ids`). */
function coveredProjects() {
  return db()
    .prepare(
      `SELECT p.id, p.name FROM projects p
       WHERE EXISTS (SELECT 1 FROM repos r WHERE r.project_id = p.id
                       AND COALESCE(NULLIF(r.provider, ''), 'github') = 'github'
                       AND COALESCE(r.github_owner, '') <> '' AND COALESCE(r.github_repo, '') <> '')
       ORDER BY p.name`,
    )
    .all();
}

/** The radar window's settings, with defaults for anything it never saved. */
export function readSettings() {
  const row = db().prepare(`SELECT value FROM settings WHERE key = 'radar_settings'`).get();
  let parsed = null;
  try {
    parsed = row ? JSON.parse(row.value) : null;
  } catch {
    parsed = null;
  }
  const stages = parsed?.stages && typeof parsed.stages === 'object' && !Array.isArray(parsed.stages) ? parsed.stages : {};
  const logins = (v) => (Array.isArray(v) ? v.filter((l) => typeof l === 'string') : []);
  return {
    saved: !!parsed,
    thresholds: { ...DEFAULT_THRESHOLDS, ...(parsed?.thresholds ?? {}) },
    stages,
    testers: logins(parsed?.testers),
    hidden: logins(parsed?.hidden),
    hideUnassigned: parsed?.hideUnassigned === true,
  };
}

function readSnapshots(ids) {
  const get = db().prepare('SELECT board, saved_at FROM radar_snapshots WHERE project_id = ?');
  const out = new Map();
  for (const id of ids) {
    const row = get.get(id);
    if (!row) continue;
    try {
      out.set(id, { board: JSON.parse(row.board), savedAt: Number(row.saved_at) });
    } catch {
      // A corrupt snapshot counts as missing (it gets refetched).
    }
  }
  return out;
}

/** Projects in scope: one by id/name, the current one, or every covered project. */
function targetProjects({ project, scope }) {
  if (project != null && String(project).trim()) return [findProject(project)];
  if (scope === 'project') {
    const id = process.env.DEVDY_PROJECT_ID;
    if (!id) throw new Error('no current project in this session — pass project or scope="all"');
    return [findProject(id)];
  }
  return coveredProjects();
}

async function refreshViaBroker(ids) {
  const resp = await askBroker(
    {
      project_id: process.env.DEVDY_PROJECT_ID,
      run_id: process.env.DEVDY_RUN_ID || null,
      tool: 'radar',
      argv: ids,
      host: null,
    },
    { timeoutMs: REFRESH_TIMEOUT_MS },
  );
  if (!resp || resp.decision !== 'allow') throw new Error((resp && resp.reason) || 'refresh denied');
  return resp.errors || {};
}

/**
 * Open issues of the projects in scope (tagged with their project), the
 * settings, and per-project freshness. Snapshots older than REVALIDATE_MS (or
 * all of them with `refresh`) are refetched first; when that fails the cached
 * boards are still returned and `warnings` says why.
 */
export async function loadRadar({ project, scope = 'all', refresh = false, now = Date.now() } = {}) {
  const projects = targetProjects({ project, scope });
  const ids = projects.map((p) => p.id);
  let snaps = readSnapshots(ids);
  const warnings = [];
  let errors = {};

  const stale = ids.filter((id) => refresh || !snaps.has(id) || now - snaps.get(id).savedAt >= REVALIDATE_MS);
  if (stale.length) {
    try {
      errors = await refreshViaBroker(stale);
      snaps = readSnapshots(ids);
    } catch (e) {
      warnings.push(`Could not refresh from GitHub (${e.message}); showing the last saved boards.`);
    }
  }

  const issues = [];
  const meta = [];
  for (const p of projects) {
    const snap = snaps.get(p.id);
    meta.push({
      id: p.id,
      name: p.name,
      savedAt: snap?.savedAt ?? null,
      error: errors[p.id] ?? null,
      truncated: !!snap?.board?.truncated,
      boardLinked: !!snap?.board?.boardLinked,
      skippedNonGithub: !!snap?.board?.skippedNonGithub,
      milestones: snap?.board?.milestones ?? [],
    });
    for (const g of snap?.board?.milestones ?? []) {
      for (const issue of g.issues ?? []) issues.push({ ...issue, projectId: p.id, projectName: p.name });
    }
  }
  return { issues, settings: readSettings(), projects: meta, warnings };
}

/** Status names seen on the boards in scope (options first, then on issues), with their counts. */
export function statusNames(radar) {
  const byKey = new Map();
  const add = (name) => {
    const k = name.trim().toLowerCase();
    if (!k) return null;
    if (!byKey.has(k)) byKey.set(k, { name: name.trim(), count: 0 });
    return byKey.get(k);
  };
  for (const p of radar.projects) {
    const snap = readSnapshots([p.id]).get(p.id);
    for (const o of snap?.board?.statusOptions ?? []) add(typeof o === 'string' ? o : o.name);
  }
  for (const i of radar.issues) {
    if (!i.status) continue;
    const s = add(i.status);
    if (s) s.count++;
  }
  return [...byKey.values()];
}
