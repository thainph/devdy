#!/usr/bin/env node
// Test for the Project Pulse tools of the built-in `devdy` MCP server
// (migration 0050, docs/project-pulse-spec.md §8.2).
//
//   node sidecar-mcp/test-pulse.mjs
//
// 1. Runs the shared normalisation vectors (src-tauri/src/pulse/source_vectors.json,
//    also run by the Rust tests) against lib/pulse-source.mjs.
// 2. Builds a throwaway DB from every real migration, seeds projects / repos / a
//    Slack thread / a digest, then drives `index.mjs` over stdio and checks the
//    watch rules, list / status output and the refresh request round trip
//    (a fake "app" marks requests done). No network.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { parseSource } from './lib/pulse-source.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const VECTORS = join(HERE, '..', 'src-tauri', 'src', 'pulse', 'source_vectors.json');
const T0 = '2026-01-01T00:00:00+00:00';
const SLACK = 'https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359';
const SLACK_REPLY = 'https://malme.slack.com/archives/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG';
const SLACK_REF = 'C0B83GHDTFG:1789877283.026359';
const THREAD_ID = '5f0c7a52-0000-4000-8000-000000000001';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

// ---- 1. Shared normalisation vectors ------------------------------------------------

const vectors = JSON.parse(readFileSync(VECTORS, 'utf8'));
let vectorFails = 0;
for (const v of vectors) {
  let got;
  try {
    const s = parseSource(v.input, v.repos ?? []);
    got = v.error ? `ok ${s.source_ref}` : s.source_type === v.type && s.source_ref === v.ref && s.source_url === v.url ? 'pass' : JSON.stringify(s);
  } catch (e) {
    got = v.error === e.code ? 'pass' : `error ${e.message}`;
  }
  if (got !== 'pass') {
    vectorFails += 1;
    process.stdout.write(`        vector ${JSON.stringify(v.input)} → ${got}\n`);
  }
}
check(vectors.length > 20 && vectorFails === 0, `shared vectors: ${vectors.length - vectorFails}/${vectors.length} match the Rust expectations`);

// ---- 2. Fixture ------------------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-pulse-'));
const dbPath = join(appData, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
const insProject = db.prepare('INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)');
insProject.run('proj-1', 'Alpha', '/tmp/proj-1', T0);
insProject.run('proj-2', 'Beta', '/tmp/proj-2', T0);
insProject.run('proj-3', 'beta', '/tmp/proj-3', T0);
db.prepare(
  "INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES ('r1', 'proj-1', 'app', '/tmp/a', 'Org', 'App', ?)",
).run(T0);
db.prepare(
  'INSERT INTO slack_threads (id, title, created_at, updated_at, channel_id, thread_ts, thread_url) VALUES (?, ?, ?, ?, ?, ?, ?)',
).run(THREAD_ID, 'Deploy', T0, T0, 'C0B83GHDTFG', '1789877283.026359', SLACK);

function drive(frames, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: 'proj-1', DEVDY_RUN_ID: 'run-9', ...env },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.on('error', reject);
    child.on('close', () => {
      const byId = new Map();
      for (const line of out.split('\n').filter((l) => l.trim())) {
        try {
          const m = JSON.parse(line);
          byId.set(m.id, m);
        } catch {
          /* not a frame */
        }
      }
      resolve(byId);
    });
    for (const f of frames) child.stdin.write(`${JSON.stringify(f)}\n`);
    child.stdin.end();
  });
}

/** Run tool calls in one server process; returns label → { text, isError }. */
async function run(calls, env) {
  const byId = await drive(
    calls.map(([, name, args], i) => ({ jsonrpc: '2.0', id: i + 1, method: 'tools/call', params: { name, arguments: args } })),
    env,
  );
  return Object.fromEntries(
    calls.map(([label], i) => {
      const r = byId.get(i + 1)?.result;
      return [label, { text: r?.content?.[0]?.text ?? '', isError: !!r?.isError }];
    }),
  );
}
const json = (r) => {
  try {
    return JSON.parse(r.text);
  } catch {
    return null;
  }
};
const count = (sql, ...p) => db.prepare(sql).get(...p).n;

// ---- 3. Watch rules ---------------------------------------------------------------------

const toolNames = (await drive([{ jsonrpc: '2.0', id: 1, method: 'tools/list' }])).get(1)?.result?.tools?.map((t) => t.name) ?? [];
check(['pulse_watch', 'pulse_unwatch', 'pulse_list', 'pulse_status'].every((n) => toolNames.includes(n)), 'tools/list exposes the pulse_* tools');

let out = await run([
  ['first', 'pulse_watch', { urls: [SLACK, 'Org/App#12', 'https://github.com/org/app/pull/3', 'nonsense'] }],
  ['again', 'pulse_watch', { url: 'https://github.com/org/app/issues/12' }],
  ['reply-link', 'pulse_watch', { url: SLACK_REPLY }],
  ['thread-id', 'pulse_watch', { url: THREAD_ID }],
  ['bare', 'pulse_watch', { url: '#7' }],
  ['other', 'pulse_watch', { url: 'org/app#12', project: 'proj-2' }],
  ['ambiguous', 'pulse_watch', { url: 'org/app#1', project: 'BETA' }],
  ['missing', 'pulse_watch', { url: 'org/app#1', project: 'Gamma' }],
  ['bare-other', 'pulse_watch', { url: '#7', project: 'proj-2' }],
]);
const first = json(out.first);
check(
  first?.length === 4 &&
    first[0].source_type === 'slack' && first[0].source_ref === SLACK_REF && first[0].project === 'Alpha' && first[0].already_watching === false &&
    first[1].source_type === 'github_issue' && first[1].source_ref === 'org/app#12' &&
    first[2].error === 'invalid_source' && /pull requests/.test(first[2].message) &&
    first[3].error === 'invalid_source',
  'pulse_watch: Slack + issue watched in the session project, PR / junk → invalid_source',
);
const slackId = first?.[0]?.id;
const issueId = first?.[1]?.id;
const row = db.prepare('SELECT * FROM pulse_watches WHERE id = ?').get(issueId);
check(
  row?.resolve_status === 'pending' && row.added_via === 'mcp' && row.added_run_id === 'run-9' && row.active === 1 &&
    row.source_url === 'https://github.com/org/app/issues/12' && /\+00:00$/.test(row.created_at),
  'pulse_watch: row is pending, added_via=mcp, added_run_id from DEVDY_RUN_ID',
);
check(count("SELECT COUNT(*) AS n FROM pulse_requests WHERE kind = 'resolve' AND done_at IS NULL") === 3, 'pulse_watch: one resolve request per new watch (slack, #12, #7)');
check(json(out.again)?.[0]?.already_watching === true && json(out.again)[0].id === issueId, 'pulse_watch: same issue again → already_watching');
check(json(out['reply-link'])?.[0]?.already_watching === true, 'pulse_watch: reply link resolves to the watched thread');
check(json(out['thread-id'])?.[0]?.id === slackId && json(out['thread-id'])[0].already_watching, 'pulse_watch: Devdy Slack thread id accepted');
check(json(out.bare)?.[0]?.source_ref === 'org/app#7', 'pulse_watch: bare #n uses the project’s only GitHub repo');
check(json(out.other)?.[0]?.error === 'watched_in_other_project' && /Alpha/.test(json(out.other)[0].message), 'pulse_watch: other project → watched_in_other_project');
check(out.ambiguous.isError && /project_ambiguous/.test(out.ambiguous.text), 'pulse_watch: duplicate project name → project_ambiguous');
check(out.missing.isError && /project_not_found/.test(out.missing.text), 'pulse_watch: unknown project → project_not_found');
check(json(out['bare-other'])?.[0]?.error === 'repo_ambiguous', 'pulse_watch: bare #n without a single repo → repo_ambiguous');
check(count('SELECT COUNT(*) AS n FROM pulse_watches') === 3, 'pulse_watch: no duplicate rows');

out = await run([['no-project', 'pulse_watch', { url: 'org/app#1' }]], { DEVDY_PROJECT_ID: '' });
check(out['no-project'].isError && /project_required/.test(out['no-project'].text), 'pulse_watch: no session project → project_required');

db.prepare("UPDATE pulse_watches SET resolve_status = 'ready'").run();
db.prepare('UPDATE pulse_requests SET done_at = ?').run(T0);
out = await run([
  ['move', 'pulse_watch', { url: 'org/app#12', project: 'proj-2', move: true }],
  ['unwatch-url', 'pulse_unwatch', { url: SLACK }],
  ['unwatch-again', 'pulse_unwatch', { url: SLACK }],
  ['unwatch-id', 'pulse_unwatch', { id: 'nope' }],
  ['rewatch', 'pulse_watch', { url: SLACK }],
]);
const moved = db.prepare('SELECT project_id, resolve_status FROM pulse_watches WHERE id = ?').get(issueId);
check(json(out.move)?.[0]?.project === 'Beta' && moved.project_id === 'proj-2' && moved.resolve_status === 'pending', 'pulse_watch move=true: moved and re-resolved');
check(json(out['unwatch-url'])?.ok === true && json(out['unwatch-url']).id === slackId, 'pulse_unwatch by url');
check(out['unwatch-again'].isError && /not_found/.test(out['unwatch-again'].text), 'pulse_unwatch: inactive source → not_found');
check(out['unwatch-id'].isError && /not_found/.test(out['unwatch-id'].text), 'pulse_unwatch: unknown id → not_found');
const rewatch = json(out.rewatch)?.[0];
const sRow = db.prepare('SELECT active, resolve_status FROM pulse_watches WHERE id = ?').get(slackId);
check(rewatch?.id === slackId && rewatch.already_watching === false && sRow.active === 1 && sRow.resolve_status === 'pending', 'pulse_watch: unwatched source re-activated');
check(count("SELECT COUNT(*) AS n FROM pulse_requests WHERE kind = 'resolve' AND done_at IS NULL") === 2, 'move / re-activate queue resolve requests');

// ---- 4. List / status ------------------------------------------------------------------

const insDigest = db.prepare(
  `INSERT INTO pulse_digests (id, watch_id, content_hash, status, ball, ball_since, summary, next_action, deadlines, decisions,
     open_questions, created_at) VALUES (?, ?, 'h', ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
);
insDigest.run('d-old', slackId, 'on_track', 'none', null, 'Bản cũ', null, '[]', '[]', '[]', T0);
insDigest.run(
  'd-new', slackId, 'at_risk', 'me', '2026-10-20T01:00:00+00:00', 'Khách hỏi lịch demo, chưa trả lời.', 'Trả lời khách về lịch demo',
  JSON.stringify([{ title: 'Demo', due_at: '2099-10-30T00:00:00+00:00', all_day: true, evidence: 'Demo chốt thứ 6 ngày 30/10' }]),
  JSON.stringify([{ text: 'Dùng staging mới', decided_at: '2026-10-19', evidence: 'ok dùng staging mới' }]),
  JSON.stringify([{ text: 'Có cần SSO không?', asked_by: 'other', evidence: 'SSO?' }]),
  '2026-10-21T00:00:00+00:00',
);
out = await run([
  ['list', 'pulse_list', {}],
  ['list-all', 'pulse_list', { all: true }],
  ['status', 'pulse_status', {}],
  ['status-beta', 'pulse_status', { project: 'proj-2' }],
]);
check(
  out.list.text.startsWith('## Alpha') && out.list.text.includes(`[${slackId}] slack · ${SLACK_REF}`) && out.list.text.includes('status: at_risk') &&
    out.list.text.includes('Khách hỏi lịch demo') && !out.list.text.includes('org/app#12'),
  'pulse_list: current project only, newest digest summary',
);
check(out['list-all'].text.includes('## Alpha') && out['list-all'].text.includes('## Beta') && out['list-all'].text.includes('org/app#12'), 'pulse_list all=true: every project');
check(
  out.status.text.includes('## Alpha') && out.status.text.includes('next milestone: Demo — 2099-10-30T00:00:00+00:00') &&
    /### Needs me\n- .*Trả lời khách về lịch demo/.test(out.status.text) &&
    out.status.text.includes('Dùng staging mới') && out.status.text.includes('open question (other): Có cần SSO không?') &&
    !out.status.text.includes('stale'),
  'pulse_status: milestone, needs-me, decisions, open questions from the stored digest',
);
check(out['status-beta'].text.includes('## Beta') && out['status-beta'].text.includes('no digest yet'), 'pulse_status: source without digest');

// ---- 5. Refresh round trip ---------------------------------------------------------------

out = await run([['stale', 'pulse_status', { refresh: true }]], { DEVDY_PULSE_REFRESH_TIMEOUT_MS: '600' });
const req1 = db.prepare("SELECT * FROM pulse_requests WHERE kind = 'refresh' ORDER BY created_at DESC LIMIT 1").get();
check(out.stale.text.startsWith('stale: true') && out.stale.text.includes('## Alpha'), 'pulse_status refresh: timeout → stored data marked stale');
check(req1?.project_id === 'proj-1' && req1.watch_id === null && req1.done_at === null, 'pulse_status refresh: request row for the current project');

// Fake app: marks every open request done shortly after it appears.
const app = setInterval(() => db.prepare('UPDATE pulse_requests SET done_at = ? WHERE done_at IS NULL').run(T0), 200);
const started = Date.now();
out = await run([['fresh', 'pulse_status', { refresh: true, all: true }]]);
clearInterval(app);
const req2 = db.prepare("SELECT * FROM pulse_requests WHERE kind = 'refresh' ORDER BY created_at DESC LIMIT 1").get();
check(!out.fresh.text.includes('stale') && out.fresh.text.includes('## Beta') && Date.now() - started < 10_000, 'pulse_status refresh: answers once the app marks the request done');
check(req2?.project_id === null && req2.done_at === T0, 'pulse_status refresh all=true: request without project');

db.close();
rmSync(appData, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} check(s) failed.\n`);
  process.exit(1);
}
process.stdout.write('\nAll checks passed.\n');
