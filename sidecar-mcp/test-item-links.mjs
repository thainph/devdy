#!/usr/bin/env node
// Smoke test for the session links of notes / todos in the built-in `devdy`
// MCP server (migration 0044: note_runs / todo_runs): list / read show the
// linked session (run) ids of runs that still exist, and deletes drop the links
// (the sidecar connection has no `foreign_keys`).
//
// Builds a throwaway DB from every real migration, seeds a project, runs, notes,
// todos and links, then drives `index.mjs` over stdio. No network.
//
// Usage: node sidecar-mcp/test-item-links.mjs

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, readFileSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const PROJECT = 'proj-1';
const T0 = '2026-01-01T00:00:00Z';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

// ---- Fixture -------------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-item-links-'));
const dbPath = join(appData, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
db.prepare("INSERT INTO projects (id, name, path, created_at) VALUES (?, 'proj', '/tmp/proj-1', ?)").run(PROJECT, T0);
const insRun = db.prepare(
  "INSERT INTO runs (id, project_id, type, status, engine, created_at) VALUES (?, ?, 'session', 'done', 'claude', ?)",
);
insRun.run('r-a', PROJECT, T0);
insRun.run('r-b', PROJECT, T0);
db.prepare(
  "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) VALUES ('n-1', 'Spec', 'body', ?, 0, ?, ?)",
).run(PROJECT, T0, T0);
db.prepare(
  "INSERT INTO todos (id, text, done, position, created_at, project_id) VALUES ('t-1', 'ship it', 1, 0, ?, ?)",
).run(T0, PROJECT);
const noteLink = db.prepare(
  "INSERT INTO note_runs (note_id, run_id, project_id, source, linked_at) VALUES ('n-1', ?, ?, 'manual', ?)",
);
noteLink.run('r-b', PROJECT, '2026-01-02T00:00:00Z');
noteLink.run('r-a', PROJECT, '2026-01-03T00:00:00Z');
noteLink.run('r-gone', PROJECT, '2026-01-04T00:00:00Z'); // run row missing: hidden
db.prepare(
  "INSERT INTO todo_runs (todo_id, run_id, project_id, source, linked_at) VALUES ('t-1', 'r-a', ?, 'captured', ?)",
).run(PROJECT, T0);

// ---- Drive the server -------------------------------------------------------------

function drive(frames) {
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

const calls = [
  ['notes-list', 'notes_list', {}],
  ['notes-read', 'notes_read', { id: 'n-1' }],
  ['todos-list', 'todos_list', {}],
  ['notes-delete', 'notes_delete', { id: 'n-1' }],
  ['todos-clear', 'todos_clear_done', {}],
];
const byId = await drive(
  calls.map(([, name, args], i) => ({ jsonrpc: '2.0', id: i + 1, method: 'tools/call', params: { name, arguments: args } })),
);
const out = Object.fromEntries(calls.map(([label], i) => [label, byId.get(i + 1)?.result?.content?.[0]?.text ?? '']));

check(out['notes-list'].includes('[n-1] Spec') && out['notes-list'].endsWith('· sessions: r-b, r-a'),
  'notes_list: linked session ids in link order, missing runs hidden');
check(out['notes-read'].includes('- linked_sessions: r-b, r-a\n'), 'notes_read: linked session ids');
check(out['todos-list'].includes('ship it · proj · sessions: r-a'), 'todos_list: linked session ids');
const count = (table) => db.prepare(`SELECT COUNT(*) AS n FROM ${table}`).get().n;
check(count('notes') === 0 && count('note_runs') === 0, 'notes_delete: note and its links removed');
check(count('todos') === 0 && count('todo_runs') === 0, 'todos_clear_done: done todos and their links removed');

db.close();
rmSync(appData, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} check(s) failed.\n`);
  process.exit(1);
}
process.stdout.write('\nAll checks passed.\n');
