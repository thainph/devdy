// Tool definitions for the Delivery Radar (read-only).
//
// The same view as the app's radar window: open GitHub issues of the Devdy
// projects, classified against the deadline on each project's linked GitHub
// Project board (overdue / due-soon / not-started / stale / no-deadline /
// on-track) and rolled up per assignee or per project. Data comes from the
// snapshots the app saves (see lib/radar-store.mjs); stale ones are refetched
// through the app first.

import { loadRadar, statusNames, REVALIDATE_MS } from './radar-store.mjs';
import {
  RADAR_STATUSES, buildGroups, classifyAll, compareIssues, compareMilestoneRisks, countByStatus, defaultStage,
  filterIssues, milestoneRisk, nestIssues,
} from './radar.mjs';

const SCOPE = {
  project: { type: 'string', description: 'Only this project (id or name, see projects_list)' },
  scope: {
    type: 'string',
    enum: ['all', 'project'],
    description: 'all (default): every project with GitHub repos, like the radar window; project: the current project only',
  },
  refresh: {
    type: 'boolean',
    description: `Refetch from GitHub now. Boards older than ${REVALIDATE_MS / 60000} minutes are refetched anyway.`,
  },
};

const ago = (ms, now) => {
  const s = Math.max(0, Math.round((now - ms) / 1000));
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
};

const countsLine = (counts) =>
  RADAR_STATUSES.filter((s) => counts[s]).map((s) => `${s} ${counts[s]}`).join(' · ') || 'nothing open';

/** `overdue 3d late` / `due-soon due today` / `stale`. */
function radarLabel(i) {
  if (i.radar === 'overdue') return `overdue ${-i.daysLeft}d late`;
  if (i.radar === 'due-soon') return i.daysLeft === 0 ? 'due-soon today' : `due-soon in ${i.daysLeft}d`;
  return i.radar;
}

function issueLine(i, { depth = 0, testers }) {
  const people = i.assignees.length
    ? i.assignees
        .map((a) => {
          const role = testers.has(a.toLowerCase()) ? ' (tester)' : '';
          const done = i.activeAssignees.includes(a) ? '' : ' (done)';
          return `@${a}${role}${done}`;
        })
        .join(', ')
    : 'unassigned';
  const bits = [people, i.status || 'no status'];
  if (i.deadline) bits.push(`deadline ${i.deadline}${i.deadlineInherited ? ' (from parent)' : ''}`);
  if (i.startDate) bits.push(`start ${i.startDate}`);
  bits.push(`updated ${String(i.updatedAt).slice(0, 10)}`);
  bits.push(i.projectName);
  const parent = depth === 0 && i.parent ? ` · parent ${i.parent.repo}#${i.parent.number} ${i.parent.title}` : '';
  return `${'  '.repeat(depth)}- [${radarLabel(i)}] ${i.repo}#${i.number} ${i.title} — ${bits.join(' · ')}${parent}\n${'  '.repeat(depth)}  ${i.htmlUrl}`;
}

/** Freshness / errors / caveats of the projects in scope. */
function footer(radar, now) {
  const lines = [...radar.warnings];
  for (const p of radar.projects) {
    if (p.error) lines.push(`${p.name}: refresh failed — ${p.error}`);
    if (p.savedAt === null) lines.push(`${p.name}: no board loaded yet (open the Delivery Radar or pass refresh=true).`);
    else if (now - p.savedAt >= REVALIDATE_MS) lines.push(`${p.name}: data from ${ago(p.savedAt, now)}.`);
    if (p.savedAt !== null && !p.boardLinked) lines.push(`${p.name}: no GitHub Project board linked — no deadlines/statuses.`);
    if (p.truncated) lines.push(`${p.name}: a repo has more than 1000 open issues; only the latest are counted.`);
    if (p.skippedNonGithub) lines.push(`${p.name}: GitLab repos are not covered.`);
  }
  if (!radar.settings.saved) lines.push('Radar settings never saved in the app: default thresholds, stages and no testers.');
  return lines.length ? `\n\nNotes:\n${lines.map((l) => `- ${l}`).join('\n')}` : '';
}

/** Milestones at risk (estimate) of the projects in scope, one line each. */
function milestoneLines(radar, now) {
  const lag = radar.settings.thresholds.milestoneLagPct;
  const out = [];
  for (const p of radar.projects) {
    const risks = (p.milestones ?? [])
      .map((m) => milestoneRisk(m, now, lag))
      .filter((m) => m !== null)
      .sort(compareMilestoneRisks);
    for (const m of risks) {
      out.push(
        m.kind === 'overdue'
          ? `- ${p.name} · ${m.title}: past due ${m.dueOn?.slice(0, 10)} by ${m.lateDays}d, ${m.openCount} issue(s) open`
          : `- ${p.name} · ${m.title}: behind schedule — ${m.elapsedPct}% of the time used, ${m.closedPct}% of issues closed (due ${m.dueOn?.slice(0, 10)})`,
      );
    }
  }
  return out;
}

function header(radar, open, now) {
  const loaded = radar.projects.filter((p) => p.savedAt !== null);
  const oldest = loaded.length ? Math.min(...loaded.map((p) => p.savedAt)) : null;
  const th = radar.settings.thresholds;
  return [
    `Delivery Radar — ${radar.projects.length} project(s) · ${open.length} open issue(s)${oldest ? ` · updated ${ago(oldest, now)}` : ''}`,
    `Totals: ${countsLine(countByStatus(open))}`,
    `Rules: due-soon = deadline within ${th.dueSoonDays}d · stale = no update for more than ${th.staleDays}d · ` +
      `milestone behind = time used − issues closed > ${th.milestoneLagPct} points · ` +
      `testers: ${radar.settings.testers.join(', ') || 'none'}`,
  ].join('\n');
}

async function classified(a) {
  const now = Date.now();
  const radar = await loadRadar({ project: a.project, scope: a.scope, refresh: !!a.refresh, now });
  const { thresholds, stages, testers } = radar.settings;
  const open = classifyAll(radar.issues, now, thresholds, { stages, testers });
  return { now, radar, open, testers: new Set(testers.map((t) => t.toLowerCase())) };
}

const intIn = (v, def, min, max) => {
  const n = Number.parseInt(v, 10);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : def;
};

export function radarTools() {
  return {
    radar_summary: {
      description:
        'Delivery Radar overview: who (or which project) is behind on their GitHub issues across Devdy projects. ' +
        'Each open issue is classified against the deadline on the project\'s GitHub Project board — overdue, due-soon, ' +
        'not-started (start date passed, still To do), stale (no update for days), no-deadline, on-track — and counted per ' +
        'assignee (default) or per project, worst first, with their most urgent issues. Done-ness follows the radar\'s roles: ' +
        'a developer is done once the issue reaches a QA stage, a tester only at done. Use radar_issues for the full list.',
      inputSchema: {
        type: 'object',
        properties: {
          group_by: { type: 'string', enum: ['person', 'project'], description: 'person (default) or project' },
          top: { type: 'integer', minimum: 0, maximum: 10, description: 'Most urgent issues shown per group (default 3)' },
          limit: { type: 'integer', minimum: 1, maximum: 200, description: 'Max groups (default 50)' },
          ...SCOPE,
        },
      },
      handler: async (a) => {
        const { now, radar, open, testers } = await classified(a);
        const mode = a.group_by === 'project' ? 'project' : 'person';
        const top = intIn(a.top, 3, 0, 10);
        const groups = buildGroups(open, mode);
        const limit = intIn(a.limit, 50, 1, 200);
        const body = groups.slice(0, limit).map((g) => {
          const name = g.key === null ? '(unassigned)' : mode === 'person' ? `@${g.label}${testers.has(g.label.toLowerCase()) ? ' (tester)' : ''}` : g.label;
          const late = g.worstLateDays ? ` · worst ${g.worstLateDays}d late` : '';
          const related = g.related.length ? ` · ${mode === 'person' ? 'projects' : 'people'}: ${g.related.join(', ')}` : '';
          const urgent = g.issues.slice(0, top).map((i) => `  ${issueLine(i, { testers }).replace(/\n/g, '\n  ')}`);
          return [`## ${name} — ${g.total} issue(s): ${countsLine(g.counts)}${late}${related}`, ...urgent].join('\n');
        });
        const more = groups.length > limit ? `\n\n… ${groups.length - limit} more group(s).` : '';
        const ms = milestoneLines(radar, now);
        const milestones = ms.length ? `\n\nMilestones at risk (estimate):\n${ms.join('\n')}` : '';
        return `${header(radar, open, now)}\n\n${body.join('\n\n') || 'No open work.'}${more}${milestones}${footer(radar, now)}`;
      },
    },

    radar_issues: {
      description:
        'Delivery Radar issue list: open GitHub issues across Devdy projects with their radar status, assignees (role, and ' +
        'whether their part is done), board status, deadline, start date, last update, project and URL — most urgent first, ' +
        'sub-issues nested under their epic/parent. Filter by status, assignee, project or text.',
      inputSchema: {
        type: 'object',
        properties: {
          status: {
            type: 'array',
            items: { type: 'string', enum: RADAR_STATUSES },
            description: 'Only these radar statuses (default: all)',
          },
          assignee: {
            type: 'string',
            description: 'GitHub login whose open work to list (case-insensitive), or "unassigned"',
          },
          query: { type: 'string', description: 'Match in title, parent title, repo, #number or assignee' },
          limit: { type: 'integer', minimum: 1, maximum: 500, description: 'Max issues (default 50)' },
          ...SCOPE,
        },
      },
      handler: async (a) => {
        const { now, radar, open, testers } = await classified(a);
        const statuses = Array.isArray(a.status) ? a.status.filter((s) => RADAR_STATUSES.includes(s)) : [];
        let list = filterIssues(open, { query: a.query });
        if (statuses.length) list = list.filter((i) => statuses.includes(i.radar));
        const who = String(a.assignee ?? '').trim().replace(/^@/, '').toLowerCase();
        if (who === 'unassigned') list = list.filter((i) => !i.assignees.length);
        else if (who) list = list.filter((i) => i.activeAssignees.some((x) => x.toLowerCase() === who));
        list.sort(compareIssues);
        const limit = intIn(a.limit, 50, 1, 500);
        const rows = nestIssues(list).slice(0, limit);
        const head = `${header(radar, open, now)}\nMatching: ${list.length}${list.length ? ` (${countsLine(countByStatus(list))})` : ''}`;
        const body = rows.map((r) => issueLine(r.issue, { depth: r.depth, testers })).join('\n');
        const more = list.length > limit ? `\n\n… ${list.length - limit} more — narrow the filters or raise limit.` : '';
        return `${head}\n\n${body || 'No matching open issues.'}${more}${footer(radar, now)}`;
      },
    },

    radar_settings: {
      description:
        'Delivery Radar settings as set in the app: the due-soon / stale / milestone-lag thresholds, the stage (dev → qa → done) of each ' +
        'board status, the testers, and how fresh each project\'s board is.',
      inputSchema: { type: 'object', properties: { ...SCOPE } },
      handler: async (a) => {
        const now = Date.now();
        const radar = await loadRadar({ project: a.project, scope: a.scope, refresh: !!a.refresh, now });
        const { thresholds, stages, testers, saved } = radar.settings;
        const names = statusNames(radar);
        for (const k of Object.keys(stages)) if (!names.some((n) => n.name.toLowerCase() === k)) names.push({ name: k, count: 0 });
        const stageLines = names.map((n) => {
          const picked = stages[n.name.toLowerCase()];
          const stage = picked ?? defaultStage(n.name);
          return `- ${n.name}: ${stage}${picked ? '' : ' (default)'}${n.count ? ` · ${n.count} open` : ''}`;
        });
        const projects = radar.projects.map(
          (p) => `- ${p.name} (${p.id}): ${p.savedAt === null ? 'not loaded' : `updated ${ago(p.savedAt, now)}`}` +
            `${p.boardLinked ? '' : ' · no board linked'}${p.error ? ` · refresh failed: ${p.error}` : ''}`,
        );
        return [
          `Settings ${saved ? 'from the radar window' : '(defaults — never saved in the app)'}:`,
          `- due-soon: deadline today or within ${thresholds.dueSoonDays} day(s)`,
          `- stale: no update for more than ${thresholds.staleDays} day(s)`,
          `- milestone behind schedule (estimate): time used % − issues closed % above ${thresholds.milestoneLagPct} points`,
          `- testers: ${testers.join(', ') || 'none (everyone counts as a developer)'}`,
          '',
          'Stage per board status (developer is done from qa on, tester only at done):',
          ...(stageLines.length ? stageLines : ['- (no statuses seen yet)']),
          '',
          'Projects:',
          ...(projects.length ? projects : ['- (none with GitHub repos)']),
          ...radar.warnings.map((w) => `\n${w}`),
        ].join('\n');
      },
    },
  };
}
