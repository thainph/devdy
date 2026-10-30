#!/usr/bin/env node
// Todo tools of the built-in `devdy` MCP server with the Focus columns
// (migration 0067, docs/focus-board-spec.md §6.2): source normalisation
// (shared vectors with src-tauri/src/focus/source.rs), field parsing, and
// todos_add / todos_set_due / todos_update / todos_done / todos_list keeping
// the same invariants as the app (status, completed_at, started_at,
// updated_at).
//
// Builds a throwaway DB from every real migration, then drives `index.mjs`
// over stdio. No network.
//
// Usage: node sidecar-mcp/test-todos.mjs

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { INVALID_SOURCE_MESSAGE, normalizeSource } from './lib/todo-source.mjs';
import { formatEstimate, normInstant, parseEstimate, parsePriority } from './lib/todo-fields.mjs';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const VECTORS = join(HERE, '..', 'src-tauri', 'src', 'focus', 'source_vectors.json');
const PROJECT = 'proj-1';
const T0 = '2026-01-01T00:00:00Z';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};
const throws = (fn) => {
  try {
    fn();
    return null;
  } catch (e) {
    return e.message;
  }
};

// ---- Pure helpers ----------------------------------------------------------------

for (const v of JSON.parse(readFileSync(VECTORS, 'utf8'))) {
  if (v.error) {
    check(throws(() => normalizeSource(v.input)) === INVALID_SOURCE_MESSAGE, `source vector rejected: ${v.input}`);
    continue;
  }
  const got = normalizeSource(v.input);
  const want = v.type == null ? null : { source_type: v.type, source_ref: v.ref, source_url: v.url };
  check(JSON.stringify(got) === JSON.stringify(want), `source vector: ${JSON.stringify(v.input)}`);
}

check(['1', 1, 'P1', 'p1'].every((p) => parsePriority(p) === 1) && parsePriority('P3') === 3, 'priority: 1 / "P1" accepted');
check(throws(() => parsePriority(4)) && throws(() => parsePriority('high')), 'priority: out of range rejected');
const est = { '30m': 30, '2h': 120, '1h30': 90, '1h 30m': 90, '1.5h': 90, 90: 90, '45 min': 45 };
check(Object.entries(est).every(([k, n]) => parseEstimate(k) === n), 'estimate: 30m / 2h / 1h30 / 1.5h / minutes');
check(parseEstimate('') === null && throws(() => parseEstimate('4m')) && throws(() => parseEstimate('41h')), 'estimate: blank clears, range enforced');
check(throws(() => parseEstimate('soon')) != null, 'estimate: garbage rejected');
check(formatEstimate(90) === '1h30' && formatEstimate(120) === '2h' && formatEstimate(45) === '45m', 'estimate: formatted');
check(normInstant('x', '2026-10-30T14:00+07:00') === '2026-10-30T14:00:00+07:00', 'instant: seconds added');
check(normInstant('x', '2026-10-30T07:00:00.5z') === '2026-10-30T07:00:00.5Z', 'instant: Z kept');
check(throws(() => normInstant('snooze_until', 'tomorrow'))?.startsWith('snooze_until'), 'instant: garbage rejected');

// ---- Fixture -------------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-todos-'));
const dbPath = join(appData, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
db.prepare("INSERT INTO projects (id, name, path, created_at) VALUES (?, 'proj', '/tmp/proj-1', ?)").run(PROJECT, T0);
// A todo from before Focus: column defaults apply.
db.prepare("INSERT INTO todos (id, text, done, position, created_at) VALUES ('old', 'legacy', 0, 5, ?)").run(T0);
const legacy = db.prepare("SELECT status, priority FROM todos WHERE id = 'old'").get();
check(legacy.status === 'todo' && legacy.priority === 2, 'migration: old todos default to status todo, P2');
const pulse = db.prepare("SELECT COUNT(*) AS n FROM sqlite_master WHERE name LIKE 'pulse_%'").get().n;
check(pulse === 0, 'migration: pulse tables dropped');

// ---- Drive the server -------------------------------------------------------------

function drive(calls) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: PROJECT },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    child.stdout.on('data', (d) => (out += d));
    child.on('error', reject);
    child.on('close', () => {
      const byId = new Map();
      for (const line of out.split('\n').filter((l) => l.trim())) {
        const m = JSON.parse(line); // only JSON-RPC frames may reach stdout
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

const row = (id) => db.prepare('SELECT * FROM todos WHERE id = ?').get(id);
const idOf = (text) => /Added todo ([0-9a-f-]{36})/.exec(text)?.[1];

let out = await drive([
  ['add-full', 'todos_add', {
    text: 'Fix CSV export',
    due: '2026-10-30',
    due_time: '15:00',
    priority: 'P1',
    estimate: '1h30',
    source_url: 'https://github.com/Papay/Devdy/issues/123',
  }],
  ['add-plain', 'todos_add', { text: 'Reply to customer', scope: 'global', estimate: 30 }],
  ['add-bad-time', 'todos_add', { text: 'x', due_time: '15:00' }],
  ['add-bad-prio', 'todos_add', { text: 'x', priority: 7 }],
  ['add-bad-src', 'todos_add', { text: 'x', source_url: 'not a link' }],
]);
const full = idOf(out['add-full']);
const plain = idOf(out['add-plain']);
check(full && out['add-full'].includes('due 2026-10-30 15:00 · P1 · ~1h30'), 'todos_add: echoes deadline, priority, estimate');
const r = row(full);
check(
  r.status === 'todo' && r.priority === 1 && r.estimate_min === 90 && r.estimate_source === 'user' &&
    r.due_time === '15:00' && r.project_id === PROJECT,
  'todos_add: Focus columns stored',
);
check(
  r.source_type === 'github' && r.source_ref === 'github:papay/devdy#123' && r.source_url === 'https://github.com/Papay/Devdy/issues/123',
  'todos_add: source normalised, url kept',
);
check(/^\d{4}-\d{2}-\d{2}T[\d:.]+\+00:00$/.test(r.updated_at) && r.updated_at === r.created_at, 'todos_add: updated_at stamped');
check(row(plain).project_id === null && row(plain).priority === 2, 'todos_add: global, default priority');
check(out['add-bad-time'].includes('due_time needs a due date'), 'todos_add: due_time without due rejected');
check(out['add-bad-prio'].includes('priority must be'), 'todos_add: bad priority rejected');
check(out['add-bad-src'].includes('invalid source'), 'todos_add: bad source rejected');

out = await drive([
  ['update', 'todos_update', { id: plain, status: 'doing', priority: 3, estimate: '' }],
  ['update-again', 'todos_update', { id: plain, status: 'doing' }],
  ['snooze', 'todos_update', { id: full, snooze_until: '2026-10-30T09:00+07:00', status: 'waiting' }],
  ['update-none', 'todos_update', { id: full }],
  ['update-bad', 'todos_update', { id: full, status: 'done' }],
  ['due', 'todos_set_due', { id: full, due: '2026-11-02', due_time: '09:30' }],
  ['list-due', 'todos_list', { include_done: false }],
  ['list-pos', 'todos_list', { include_done: false, sort: 'position' }],
]);
const p = row(plain);
check(p.status === 'doing' && p.priority === 3 && p.estimate_min === null && p.estimate_source === null, 'todos_update: status / priority / estimate cleared');
check(p.started_at && out['update-again'].includes('doing'), 'todos_update: doing stamps started_at');
const startedAt = p.started_at;
check(row(plain).started_at === startedAt, 'todos_update: started_at kept on a second start');
const f = row(full);
check(f.snoozed_until === '2026-10-30T09:00:00+07:00' && f.status === 'waiting', 'todos_update: snooze + waiting');
check(out['update-none'].includes('nothing to update'), 'todos_update: empty patch rejected');
check(out['update-bad'].includes('status must be'), 'todos_update: bad status rejected');
check(out.due === `Todo ${full} is due 2026-11-02 09:30.` && f.due_time === '09:30', 'todos_set_due: date + time');
const lines = out['list-due'].split('\n');
check(lines[0].includes('Fix CSV export') && lines[0].includes('waiting') && lines[0].includes('source github:papay/devdy#123'),
  'todos_list: due sort puts the dated todo first, shows status and source');
check(out['list-pos'].split('\n')[0].includes('Reply to customer'), 'todos_list: position sort keeps the manual order');

out = await drive([
  ['done', 'todos_done', { id: plain }],
  ['undue', 'todos_set_due', { id: full }],
]);
const d = row(plain);
check(d.done === 1 && d.completed_at && d.status === 'doing', 'todos_done: done stamps completed_at');
const completedAt = d.completed_at;
out = await drive([
  ['done-again', 'todos_done', { id: plain, done: true }],
  ['list-all', 'todos_list', {}],
]);
check(row(plain).completed_at === completedAt, 'todos_done: re-marking keeps completed_at');
check(out['list-all'].split('\n').at(-1).startsWith('- [x] Reply to customer'), 'todos_list: done todos last');
check(row(full).due_date === null && row(full).due_time === null, 'todos_set_due: clearing drops the time too');
out = await drive([['undo', 'todos_done', { id: plain }]]);
const u = row(plain);
check(u.done === 0 && u.completed_at === null && u.status === 'todo', 'todos_done: undo clears completed_at, back to todo');

db.close();
rmSync(appData, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} check(s) failed.\n`);
  process.exit(1);
}
process.stdout.write('\nAll checks passed.\n');
