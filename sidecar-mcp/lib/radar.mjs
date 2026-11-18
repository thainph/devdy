// Delivery Radar classification for the built-in MCP server.
//
// A plain-JS copy of the pure logic in `src/lib/deliveryRadar.ts` (this server
// is bundled on its own, without the app's TypeScript): classify each open
// issue against its board deadline, decide who still has work on it by role
// (a developer is done from QA on, a tester only at done), and roll issues up
// per assignee or per project. `src/lib/__tests__/deliveryRadarParity.test.mjs`
// runs both on the same fixtures — change them together.

export const RADAR_STATUSES = ['overdue', 'due-soon', 'not-started', 'stale', 'no-deadline', 'on-track'];

const SEVERITY = { overdue: 0, 'due-soon': 1, 'not-started': 2, stale: 3, 'no-deadline': 4, 'on-track': 5 };

// milestoneLagPct: a milestone is behind schedule when its share of elapsed
// time exceeds its share of closed issues by more than this many points.
export const DEFAULT_THRESHOLDS = { dueSoonDays: 3, staleDays: 5, milestoneLagPct: 20 };

const DAY = 86_400_000;
const NOT_STARTED = new Set(['', 'todo', 'to do', 'backlog', 'triage', 'ready', 'new', 'no status', 'chưa bắt đầu']);
const DONE_PATTERN = /releas|done|closed|complet|shipped|resolved|merged|cancel|won.?t ?fix|hoàn thành|xong|hủy/;
const QA_PATTERN = /qa|test|uat|verif|staging|deploy/;

/** Local midnight of a `YYYY-MM-DD` (or ISO) date; null when unparsable. */
export function dayStart(date) {
  if (!date) return null;
  const m = /^(\d{4})-(\d{2})-(\d{2})/.exec(date);
  if (!m) return null;
  return new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime();
}

function today(now) {
  const d = new Date(now);
  return new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
}

/** Whole days from today to the deadline: negative = late, 0 = due today. */
export function daysUntil(deadline, now) {
  const dl = dayStart(deadline);
  if (dl === null) return null;
  return Math.round((dl - today(now)) / DAY);
}

const normStatus = (status) => (status ?? '').trim().toLowerCase();

/** Stage of a board status by name: Ready for QA, Testing → qa; Ready for release, Done → done. */
export function defaultStage(status) {
  const s = normStatus(status);
  if (!s) return 'dev';
  if (DONE_PATTERN.test(s)) return 'done';
  if (QA_PATTERN.test(s)) return 'qa';
  return 'dev';
}

/** The user's pick for the status, else `defaultStage`. */
export function stageOf(status, stages) {
  return stages?.[normStatus(status)] ?? defaultStage(status);
}

export function isDone(issue, stages, role = 'dev') {
  if (issue.state === 'CLOSED') return true;
  const stage = stageOf(issue.status, stages);
  return role === 'tester' ? stage === 'done' : stage !== 'dev';
}

/** overdue → due soon → not started → stale → no deadline → on track. */
export function classifyIssue(issue, now, th) {
  const left = daysUntil(issue.deadline, now);
  if (left !== null && left < 0) return 'overdue';
  if (left !== null && left <= th.dueSoonDays) return 'due-soon';
  const start = dayStart(issue.startDate);
  if (start !== null && start <= today(now) && NOT_STARTED.has(normStatus(issue.status))) return 'not-started';
  const updated = new Date(issue.updatedAt).getTime();
  if (!Number.isNaN(updated) && now - updated > th.staleDays * DAY) return 'stale';
  if (left === null) return 'no-deadline';
  return 'on-track';
}

const issueKey = (projectId, repo, number) => `${projectId}::${repo.toLowerCase()}#${number}`;

/**
 * Open work only: an issue stays while one of its followed assignees still has
 * something to do on it (`rules.hidden` / `rules.hideUnassigned` drop people).
 */
export function classifyAll(issues, now, th, rules = {}) {
  const testers = new Set((rules.testers ?? []).map((l) => l.toLowerCase()));
  const hidden = new Set((rules.hidden ?? []).map((l) => l.toLowerCase()));
  const seen = new Set();
  const out = [];
  for (const issue of issues) {
    const doneFor = (role) => isDone(issue, rules.stages, role);
    let activeAssignees = [];
    if (issue.assignees.length) {
      activeAssignees = issue.assignees.filter(
        (a) => !hidden.has(a.toLowerCase()) && !doneFor(testers.has(a.toLowerCase()) ? 'tester' : 'dev'),
      );
      if (!activeAssignees.length) continue;
    } else if (rules.hideUnassigned || doneFor('dev')) {
      continue;
    }
    const key = issueKey(issue.projectId, issue.repo, issue.number);
    if (seen.has(key)) continue;
    seen.add(key);
    out.push({ ...issue, key, radar: classifyIssue(issue, now, th), daysLeft: daysUntil(issue.deadline, now), activeAssignees });
  }
  return out;
}

export function emptyCounts() {
  return { overdue: 0, 'due-soon': 0, 'not-started': 0, stale: 0, 'no-deadline': 0, 'on-track': 0 };
}

export function countByStatus(issues) {
  const counts = emptyCounts();
  for (const i of issues) counts[i.radar]++;
  return counts;
}

/** Most urgent first, then the nearest deadline, then least recently updated. */
export function compareIssues(a, b) {
  const s = SEVERITY[a.radar] - SEVERITY[b.radar];
  if (s !== 0) return s;
  const ad = a.daysLeft ?? Infinity;
  const bd = b.daysLeft ?? Infinity;
  if (ad !== bd) return ad - bd;
  return a.updatedAt.localeCompare(b.updatedAt);
}

export function parentKey(issue) {
  return issue.parent ? issueKey(issue.projectId, issue.parent.repo, issue.parent.number) : null;
}

/** Sub-issues right under their parent (when listed), subtrees ordered by their most urgent issue. */
export function nestIssues(issues) {
  const keys = new Set(issues.map((i) => i.key));
  const children = new Map();
  const roots = [];
  for (const issue of issues) {
    const pk = parentKey(issue);
    if (pk && pk !== issue.key && keys.has(pk)) {
      const list = children.get(pk);
      if (list) list.push(issue);
      else children.set(pk, [issue]);
    } else {
      roots.push(issue);
    }
  }

  const lead = new Map();
  const leadOf = (issue, path) => {
    const cached = lead.get(issue.key);
    if (cached) return cached;
    path.add(issue.key);
    let best = issue;
    for (const c of children.get(issue.key) ?? []) {
      if (path.has(c.key)) continue;
      const l = leadOf(c, path);
      if (compareIssues(l, best) < 0) best = l;
    }
    path.delete(issue.key);
    lead.set(issue.key, best);
    return best;
  };
  const bySubtree = (a, b) => compareIssues(leadOf(a, new Set()), leadOf(b, new Set())) || compareIssues(a, b);

  const rows = [];
  const placed = new Set();
  const place = (issue, depth) => {
    if (placed.has(issue.key)) return;
    placed.add(issue.key);
    const kids = [...(children.get(issue.key) ?? [])].sort(bySubtree);
    rows.push({ issue, depth, hasChildren: kids.length > 0 });
    for (const c of kids) place(c, depth + 1);
  };
  for (const r of [...roots].sort(bySubtree)) place(r, 0);
  for (const issue of issues) place(issue, 0);
  return rows;
}

/** Per assignee (unassigned bucket last) or per project; groups with the most late work first. */
export function buildGroups(issues, mode) {
  const groups = new Map();
  const UNASSIGNED = '\u0000unassigned';
  for (const issue of issues) {
    const targets =
      mode === 'project'
        ? [{ id: issue.projectId, key: issue.projectId, label: issue.projectName }]
        : issue.assignees.length
          ? issue.activeAssignees.map((a) => ({ id: a.toLowerCase(), key: a, label: a }))
          : [{ id: UNASSIGNED, key: null, label: '' }];
    for (const t of targets) {
      let g = groups.get(t.id);
      if (!g) {
        g = { key: t.key, label: t.label, counts: emptyCounts(), total: 0, worstLateDays: null, related: [], issues: [], rows: [] };
        groups.set(t.id, g);
      }
      g.issues.push(issue);
      g.counts[issue.radar]++;
      g.total++;
      if (issue.daysLeft !== null && issue.daysLeft < 0) g.worstLateDays = Math.max(g.worstLateDays ?? 0, -issue.daysLeft);
      const related = mode === 'project' ? issue.activeAssignees : [issue.projectName];
      for (const r of related) if (!g.related.includes(r)) g.related.push(r);
    }
  }
  const list = [...groups.values()];
  for (const g of list) {
    g.issues.sort(compareIssues);
    g.rows = nestIssues(g.issues);
    g.related.sort((a, b) => a.localeCompare(b));
  }
  return list.sort(
    (a, b) =>
      (a.key === null ? 1 : 0) - (b.key === null ? 1 : 0) ||
      b.counts.overdue - a.counts.overdue ||
      (b.worstLateDays ?? 0) - (a.worstLateDays ?? 0) ||
      b.counts['due-soon'] - a.counts['due-soon'] ||
      b.counts['not-started'] + b.counts.stale - (a.counts['not-started'] + a.counts.stale) ||
      b.total - a.total ||
      a.label.localeCompare(b.label),
  );
}

/** Keep only issues of `status` (when set) whose title/repo/number/assignee matches `query`. */
export function filterIssues(issues, opts) {
  const q = (opts.query ?? '').trim().toLowerCase();
  return issues.filter((i) => {
    if (opts.status && i.radar !== opts.status) return false;
    if (!q) return true;
    const num = q.replace(/^#/, '');
    return (
      i.title.toLowerCase().includes(q) ||
      !!i.parent?.title.toLowerCase().includes(q) ||
      i.repo.toLowerCase().includes(q) ||
      String(i.number) === num ||
      i.assignees.some((a) => a.toLowerCase().includes(q))
    );
  });
}

// ── Milestones (estimate) ───────────────────────────────────────────────────

/**
 * An estimated risk of a milestone, if any: overdue (due date passed, issues
 * still open) wins over lag (time elapsed % − issues closed % above `lagPct`).
 * GitHub milestones have no start date, so the earliest board Start date of
 * their issues stands in for one; without it only the due-date check applies.
 */
export function milestoneRisk(m, now, lagPct) {
  const due = dayStart(m.dueOn);
  if (due === null || m.openCount <= 0) return null;
  const base = { title: m.title, dueOn: m.dueOn, openCount: m.openCount };
  const left = daysUntil(m.dueOn, now);
  if (left !== null && left < 0) {
    return { ...base, kind: 'overdue', lateDays: -left, elapsedPct: null, closedPct: null };
  }
  // Progress needs the closed count: without GitHub's totals, only "overdue" can be judged.
  if (m.countsKnown === false) return null;
  const starts = (m.issues ?? []).map((i) => dayStart(i.startDate)).filter((s) => s !== null);
  if (!starts.length) return null;
  const start = Math.min(...starts);
  // The due date is the last working day: plan until its end.
  const end = due + DAY;
  if (start >= end || today(now) < start) return null;
  const elapsedPct = Math.min(100, ((now - start) / (end - start)) * 100);
  const total = m.openCount + m.closedCount;
  const closedPct = total > 0 ? (m.closedCount / total) * 100 : 0;
  if (elapsedPct - closedPct <= lagPct) return null;
  return {
    ...base, kind: 'lag', lateDays: null, elapsedPct: Math.round(elapsedPct), closedPct: Math.round(closedPct),
  };
}

/** Overdue before lag; the most overdue first. */
export function compareMilestoneRisks(a, b) {
  if (a.kind !== b.kind) return a.kind === 'overdue' ? -1 : 1;
  return (b.lateDays ?? 0) - (a.lateDays ?? 0) || a.title.localeCompare(b.title);
}
