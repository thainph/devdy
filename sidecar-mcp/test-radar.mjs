#!/usr/bin/env node
// Delivery Radar tools of the built-in `devdy` MCP server (radar_summary,
// radar_issues, radar_settings): read the board snapshots the app saves
// (migration 0079) with the radar window's settings, and refresh stale ones
// through the broker (a fake one here, answering like runs/broker/radar.rs).
//
// Builds a throwaway DB from every real migration, then drives `index.mjs`
// over stdio. No network.
//
// Usage: node sidecar-mcp/test-radar.mjs

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import net from 'node:net';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const T0 = '2026-01-01T00:00:00Z';
const DAY = 86_400_000;

const failures = [];
const check = (ok, label, detail = '') => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
  if (!ok && detail) process.stdout.write(`${detail}\n`);
};

const ymd = (offsetDays) => {
  const d = new Date(Date.now() + offsetDays * DAY);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, '0')}-${String(d.getDate()).padStart(2, '0')}`;
};
let seq = 0;
const issue = (extra = {}) => {
  seq += 1;
  return {
    repo: 'web', number: seq, title: `Issue ${seq}`, htmlUrl: `https://github.com/acme/web/issues/${seq}`,
    createdAt: T0, updatedAt: new Date(Date.now() - 3_600_000).toISOString(), comments: 0, assignees: ['alice'], labels: [],
    milestoneTitle: null, milestoneDueOn: null, startDate: null, deadline: null, status: 'In progress', statusColor: null,
    state: 'OPEN', closedAt: null, parent: null, deadlineInherited: false, ...extra,
  };
};
const board = (issues, extra = {}) => JSON.stringify({
  milestones: [{ title: 'No milestone', dueOn: null, repos: ['web'], openCount: issues.length, closedCount: 0, issues }],
  fetchedAt: T0, boardLinked: true, truncated: false, skippedNonGithub: false,
  statusOptions: [{ name: 'Todo', color: 'GRAY' }, { name: 'In progress', color: 'BLUE' }, { name: 'Ready for QA', color: 'YELLOW' }, { name: 'Done', color: 'GREEN' }],
  ...extra,
});

// ---- Fixture -------------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-radar-'));
const dbPath = join(appData, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
const addProject = (id, name, gh = true) => {
  db.prepare('INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)').run(id, name, `/tmp/${id}`, T0);
  if (gh) {
    db.prepare("INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES (?, ?, 'web', '/tmp', 'acme', ?, ?)")
      .run(`${id}-r`, id, `web-${id}`, T0);
  }
};
addProject('p1', 'Alpha');
addProject('p2', 'Beta');
addProject('p3', 'NoGithub', false);

const epic = issue({ title: 'Checkout epic', deadline: ymd(10), assignees: [] });
const late = issue({
  title: 'Pay button broken', deadline: ymd(-4), assignees: ['alice'],
  parent: { owner: 'acme', repo: 'web', number: epic.number, title: 'Checkout epic', htmlUrl: epic.htmlUrl },
});
const qa = issue({ title: 'Cart totals', deadline: ymd(1), status: 'Ready for QA', assignees: ['alice', 'carol'] });
const todo = issue({ title: 'Coupons', startDate: ymd(-2), status: 'Todo', assignees: ['bob'] });
const done = issue({ title: 'Shipped thing', status: 'Done', assignees: ['bob'] });
const snap = db.prepare('INSERT OR REPLACE INTO radar_snapshots (project_id, board, saved_at) VALUES (?, ?, ?)');
const p1Issues = [epic, late, qa, todo, done];
// A milestone past its due date with issues still open (milestone risk, estimate).
snap.run('p1', board(p1Issues, {
  milestones: [
    { title: 'No milestone', dueOn: null, repos: ['web'], openCount: p1Issues.length, closedCount: 0, issues: p1Issues },
    { title: 'v1.0', dueOn: `${ymd(-3)}T00:00:00Z`, repos: ['web'], openCount: 2, closedCount: 5, issues: [] },
  ],
}), Date.now());
// Beta's snapshot is an hour old: stale.
const betaOld = issue({ title: 'Beta old', deadline: ymd(30), assignees: ['dave'] });
snap.run('p2', board([betaOld]), Date.now() - 3_600_000);
db.prepare("INSERT INTO settings (key, value) VALUES ('radar_settings', ?)").run(
  JSON.stringify({ thresholds: { dueSoonDays: 2, staleDays: 5 }, stages: { 'in progress': 'dev' }, testers: ['carol'] }),
);

// ---- Fake broker ----------------------------------------------------------------

const sock = join(appData, 'broker.sock');
const brokerReqs = [];
const broker = net.createServer((c) => {
  let buf = '';
  c.on('data', (d) => {
    buf += d;
    const nl = buf.indexOf('\n');
    if (nl < 0) return;
    const req = JSON.parse(buf.slice(0, nl));
    brokerReqs.push(req);
    if (req.run_id !== 'r1') return c.end(`${JSON.stringify({ decision: 'deny', reason: 'no live run for this request' })}\n`);
    // Refetch Beta (now with a fresh issue); pretend Alpha failed.
    const errors = {};
    for (const id of req.argv) {
      if (id === 'p2') snap.run('p2', board([issue({ title: 'Beta fresh', deadline: ymd(-1), assignees: ['dave'] })]), Date.now());
      else errors[id] = 'GraphQL error for acme/web-p1: rate limited';
    }
    c.end(`${JSON.stringify({ decision: 'allow', ...(Object.keys(errors).length ? { errors } : {}) })}\n`);
  });
});
await new Promise((r) => broker.listen(sock, r));

// ---- Drive the server -------------------------------------------------------------

function drive(calls, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: 'p1', DEVDY_BROKER_SOCK: '', DEVDY_RUN_ID: '', ...env },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.on('error', reject);
    child.on('close', () => {
      const byId = new Map();
      for (const line of out.split('\n').filter((l) => l.trim())) {
        const m = JSON.parse(line);
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

// No broker: cached boards, with a note that Beta could not be refreshed.
let out = await drive([
  ['summary', 'radar_summary', {}],
  ['by-project', 'radar_summary', { group_by: 'project', top: 0 }],
  ['alice', 'radar_issues', { assignee: '@Alice' }],
  ['carol', 'radar_issues', { assignee: 'carol' }],
  ['overdue', 'radar_issues', { status: ['overdue'] }],
  ['current', 'radar_issues', { scope: 'project' }],
  ['unknown-project', 'radar_issues', { project: 'Nope' }],
  ['settings', 'radar_settings', {}],
]);
const s = out.summary;
check(s.includes('2 project(s)') && s.includes('5 open issue(s)'), 'summary: GitHub projects only, done/QA-for-dev work dropped', s);
check(s.includes('Rules: due-soon = deadline within 2d') && s.includes('testers: carol'), 'summary: settings from the app applied');
check(s.indexOf('## @alice') < s.indexOf('## @bob') && /## @alice — 1 issue\(s\): overdue 1 · worst 4d late/.test(s), 'summary: alice first, overdue 4d', s);
check(s.includes('## @carol (tester) — 1 issue(s): due-soon 1'), 'summary: tester keeps the QA issue');
check(s.includes('## @bob — 1 issue(s): not-started 1'), 'summary: start date passed, still Todo → not-started');
check(s.includes('## (unassigned)'), 'summary: unassigned bucket');
check(s.includes('Could not refresh from GitHub') && s.includes('Beta: data from 1h ago'), 'summary: stale board kept, flagged', s);
check(s.includes('Milestones at risk (estimate):') && s.includes('- Alpha · v1.0: past due') && s.includes('by 3d, 2 issue(s) open'),
  'summary: overdue milestone listed as an estimate', s);
check(s.includes('milestone behind = time used − issues closed > 20 points'), 'summary: default milestone lag threshold');
check(out['by-project'].includes('## Alpha — 4 issue(s)') && out['by-project'].includes('## Beta — 1 issue(s)'), 'summary: per project', out['by-project']);
check(out.alice.includes('Matching: 1') && out.alice.includes('[overdue 4d late] web#2 Pay button broken') && out.alice.includes('parent web#1 Checkout epic'),
  'issues: by assignee, parent shown', out.alice);
check(out.carol.includes('@alice (done), @carol (tester)'), 'issues: per-assignee done-ness', out.carol);
check(out.overdue.includes('Matching: 1') && !out.overdue.includes('Cart totals'), 'issues: status filter');
check(out.current.includes('1 project(s)') && !out.current.includes('Beta'), 'issues: scope=project');
check(out['unknown-project'].startsWith('Error: no project matches'), 'issues: unknown project is an error');
const st = out.settings;
check(st.includes('from the radar window') && st.includes('- In progress: dev ·') && st.includes('- Ready for QA: qa (default)'),
  'settings: picked vs default stages', st);
check(st.includes('issues closed % above 20 points'), 'settings: milestone lag threshold (default)', st);
check(st.includes('Alpha (p1): updated just now') && st.includes('Beta (p2): updated 1h ago'), 'settings: freshness per project');

// Through the broker: only the stale board is refetched; errors are surfaced.
out = await drive([['summary', 'radar_summary', {}]], { DEVDY_BROKER_SOCK: sock, DEVDY_RUN_ID: 'r1' });
check(brokerReqs.length === 1 && brokerReqs[0].tool === 'radar' && JSON.stringify(brokerReqs[0].argv) === '["p2"]' &&
  brokerReqs[0].project_id === 'p1', 'refresh: only stale projects sent to the broker', JSON.stringify(brokerReqs));
check(out.summary.includes('## @dave — 1 issue(s): overdue 1') && !out.summary.includes('Could not refresh'), 'refresh: fresh board read back', out.summary);

out = await drive([['forced', 'radar_issues', { refresh: true, project: 'Alpha' }]], { DEVDY_BROKER_SOCK: sock, DEVDY_RUN_ID: 'r1' });
check(JSON.stringify(brokerReqs.at(-1).argv) === '["p1"]' && out.forced.includes('Alpha: refresh failed — GraphQL error'),
  'refresh: forced, per-project error reported', out.forced);

out = await drive([['denied', 'radar_issues', { refresh: true }]], { DEVDY_BROKER_SOCK: sock, DEVDY_RUN_ID: 'r9' });
check(out.denied.includes('Could not refresh from GitHub (no live run for this request)') && out.denied.includes('Pay button broken'),
  'refresh: denied → cached data', out.denied);

broker.close();
db.close();
rmSync(appData, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} failure(s)\n`);
  process.exit(1);
}
process.stdout.write('\nall radar checks passed\n');
