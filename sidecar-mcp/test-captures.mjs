#!/usr/bin/env node
// Smoke test for the capture tools (`slack_threads_*` + `web_pages_*`) of the
// built-in `devdy` MCP server.
//
//   node sidecar-mcp/test-captures.mjs
//
// Builds a throwaway <app_data> (temp dir) with a data.db at the 0038/0039 schema
// (docs/srs-slack-thread-inbox.md §4), seeds Slack threads + attachment files,
// applies the 0040 rename (docs/srs-web-page-capture.md §2) on top, seeds web
// pages, then drives index.mjs over stdio JSON-RPC and checks every tool against
// the real DB and filesystem. Nothing outside the temp dir is touched.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { basename, dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, mkdirSync, writeFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

const SERVER = join(dirname(fileURLToPath(import.meta.url)), 'index.mjs');
const PROJECT = 'proj-1';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

// ---- Fixture ----------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-captures-'));
const dbPath = join(appData, 'data.db');
const outsideName = `${basename(appData)}-outside.txt`;
const outsidePath = join(dirname(appData), outsideName);
const db = new DatabaseSync(dbPath);
db.exec(`
CREATE TABLE IF NOT EXISTS slack_threads (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL DEFAULT '',
  title_custom INTEGER NOT NULL DEFAULT 0,
  content     TEXT NOT NULL DEFAULT '',
  workspace   TEXT,
  channel     TEXT,
  thread_url  TEXT,
  thread_ts   TEXT,
  exported_at TEXT,
  project_id  TEXT,
  position    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_threads_position ON slack_threads(position);
CREATE INDEX IF NOT EXISTS idx_slack_threads_project  ON slack_threads(project_id);
CREATE INDEX IF NOT EXISTS idx_slack_threads_url      ON slack_threads(thread_url);
CREATE TABLE IF NOT EXISTS slack_thread_attachments (
  id         TEXT PRIMARY KEY,
  thread_id  TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  name       TEXT NOT NULL,
  file_path  TEXT NOT NULL,
  size       INTEGER NOT NULL,
  mime       TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_thread_att_thread ON slack_thread_attachments(thread_id);
`);

const T0 = '2026-01-01T00:00:00.000Z';
const insThread = db.prepare(
  `INSERT INTO slack_threads (id, title, content, workspace, channel, thread_url, thread_ts, exported_at, project_id, position, created_at, updated_at)
   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
);
insThread.run('th-a', 'Deploy failed on staging', 'alice: deploy broke\nbob: rollback done', 'malme', '#dev-backend',
  'https://malme.slack.com/archives/C01/p1', '1700000000.1', T0, PROJECT, 0, T0, T0);
insThread.run('th-b', 'Design review', 'carol: new mockups attached', 'malme', '#design',
  null, null, null, null, 1, T0, T0);

const insAtt = db.prepare(
  `INSERT INTO slack_thread_attachments (id, thread_id, name, file_path, size, mime, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)`,
);
const addFile = (attId, threadId, name, file, data, mime) => {
  const rel = `slack-threads/${threadId}/files/${file}`;
  mkdirSync(join(appData, dirname(rel)), { recursive: true });
  writeFileSync(join(appData, rel), data);
  insAtt.run(attId, threadId, name, rel, Buffer.byteLength(data), mime, T0);
};
addFile('att-log', 'th-a', 'attachments/deploy.log', 'deploy.log', 'ERROR: migration 42 failed\n', 'text/plain');
addFile('att-png', 'th-a', 'attachments/screenshot.png', 'screenshot.png',
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0]), 'image/png');
addFile('att-big', 'th-b', 'big.txt', 'big.txt', 'x'.repeat(1024 * 1024 + 1), 'text/plain');
addFile('att-latin1', 'th-b', 'latin1.txt', 'latin1.txt', Buffer.from([0x63, 0x61, 0x66, 0xe9]), 'application/octet-stream');
// A row whose stored path tries to escape <app_data> must be refused.
insAtt.run('att-evil', 'th-b', 'evil', `../${outsideName}`, 1, 'text/plain', T0);
writeFileSync(outsidePath, 'secret');
// A sibling dir a traversal-y delete id would hit if ids weren't validated.
mkdirSync(join(appData, 'keep-me'), { recursive: true });

// ---- Migration 0040 (verbatim from the SRS) on top of the seeded 0038/0039 data ----
db.exec(`
ALTER TABLE slack_threads RENAME TO captures;
ALTER TABLE captures ADD COLUMN kind TEXT NOT NULL DEFAULT 'slack';
ALTER TABLE captures ADD COLUMN source_url  TEXT;
ALTER TABLE captures ADD COLUMN url_key     TEXT;
ALTER TABLE captures ADD COLUMN site_name   TEXT;
ALTER TABLE captures ADD COLUMN author      TEXT;
ALTER TABLE captures ADD COLUMN published_at TEXT;
ALTER TABLE captures ADD COLUMN description TEXT;
ALTER TABLE captures ADD COLUMN selection   INTEGER NOT NULL DEFAULT 0;
ALTER TABLE slack_thread_attachments RENAME TO capture_attachments;
ALTER TABLE capture_attachments RENAME COLUMN thread_id TO capture_id;
CREATE INDEX IF NOT EXISTS idx_captures_kind    ON captures(kind);
CREATE INDEX IF NOT EXISTS idx_captures_url_key ON captures(url_key);
`);
const fk = db.prepare('PRAGMA foreign_key_list(capture_attachments)').all();
check(fk.length === 1 && fk[0].table === 'captures' && fk[0].from === 'capture_id', 'migration 0040: capture_attachments FK points at captures(capture_id)');
check(db.prepare("SELECT COUNT(*) AS n FROM captures WHERE kind = 'slack'").get().n === 2 &&
  db.prepare('SELECT COUNT(*) AS n FROM capture_attachments').get().n === 5, 'migration 0040: slack rows + attachments kept, kind defaults to slack');

const insCapture = db.prepare(
  `INSERT INTO captures (id, kind, title, content, source_url, url_key, site_name, author, published_at, description, selection,
                         exported_at, project_id, position, created_at, updated_at)
   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
);
// A long web page: three sections, the middle one big enough to need slicing.
const bigSection = Array.from({ length: 400 }, (_, i) => `line ${i} of the configuration guide`).join('\n');
const longDoc = [
  '# Tauri capabilities',
  'Intro paragraph.',
  '## Install',
  'Run the installer.',
  '```sh',
  '# not a heading, inside a code fence',
  '```',
  '## Configure',
  bigSection,
  '### Permissions',
  'Grant only what you need.',
  '## FAQ',
  'Last section.',
].join('\n');
insCapture.run('wp-1', 'web', 'Tauri capabilities', longDoc, 'https://v2.tauri.app/security/capabilities/',
  'https://v2.tauri.app/security/capabilities', 'Tauri', 'Tauri team', '2026-09-01', 'How to configure capabilities', 0,
  '2026-10-09T10:00:00Z', PROJECT, 0, T0, T0);
insCapture.run('wp-2', 'web', 'Selected excerpt', 'Only this paragraph.', 'https://www.example.com/post?id=1', null,
  null, null, null, null, 1, T0, null, 1, T0, T0);
// Turndown-style setext headings, plus a `---` thematic break after a blank line.
const setextDoc = [
  'Getting started',
  '===============',
  'Welcome.',
  '',
  '---',
  '',
  'More welcome text.',
  'Setup',
  '-----',
  'Install it.',
  '```',
  'not a heading',
  '---',
  '```',
  'Usage',
  '-----',
  'Use it.',
].join('\n');
insCapture.run('wp-3', 'web', 'Setext page', setextDoc, 'https://docs.example.org/start', null,
  null, null, null, null, 0, T0, PROJECT, 2, T0, T0);
const webRel = 'web-pages/wp-1/files/diagram.svg';
mkdirSync(join(appData, dirname(webRel)), { recursive: true });
writeFileSync(join(appData, webRel), '<svg/>');
db.prepare('INSERT INTO capture_attachments (id, capture_id, name, file_path, size, mime, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)')
  .run('att-web', 'wp-1', 'images/diagram.svg', webRel, 6, 'text/plain', T0);

// ---- Drive the server ---------------------------------------------------------

function drive(frames) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: PROJECT },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    let err = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (err += d));
    child.on('error', reject);
    child.on('close', () => {
      const parsed = [];
      const noise = [];
      for (const line of out.split('\n').filter((l) => l.trim())) {
        try {
          parsed.push(JSON.parse(line));
        } catch {
          noise.push(line);
        }
      }
      resolve({ parsed, noise, stderr: err });
    });
    for (const f of frames) child.stdin.write(`${JSON.stringify(f)}\n`);
    child.stdin.end();
  });
}

let nextId = 1;
const call = (name, args = {}) => {
  const id = ++nextId;
  return { jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } };
};

// Ordered script; results are looked up by label below.
const script = [
  ['list-project', call('slack_threads_list')],
  ['list-all', call('slack_threads_list', { scope: 'all' })],
  ['read-a', call('slack_threads_read', { id: 'th-a' })],
  ['read-missing', call('slack_threads_read', { id: 'nope' })],
  ['search-channel', call('slack_threads_search', { query: '#design', scope: 'all' })],
  ['search-body', call('slack_threads_search', { query: 'rollback' })],
  ['search-literal', call('slack_threads_search', { query: '%', scope: 'all' })],
  ['update', call('slack_threads_update', { id: 'th-b', title: 'Design review v2' })],
  ['update-empty', call('slack_threads_update', { id: 'th-b' })],
  ['append', call('slack_threads_append', { id: 'th-a', text: '## Summary\nRolled back.' })],
  ['set-project', call('slack_threads_set_project', { id: 'th-b' })],
  ['set-global', call('slack_threads_set_project', { id: 'th-a', scope: 'global' })],
  ['reorder', call('slack_threads_reorder', { ids: ['th-b'] })],
  ['att-text', call('slack_threads_read_attachment', { id: 'att-log' })],
  ['att-png', call('slack_threads_read_attachment', { id: 'att-png' })],
  ['att-big', call('slack_threads_read_attachment', { id: 'att-big' })],
  ['att-latin1', call('slack_threads_read_attachment', { id: 'att-latin1' })],
  ['att-evil', call('slack_threads_read_attachment', { id: 'att-evil' })],
  ['delete-traversal', call('slack_threads_delete', { ids: ['../keep-me', '..'] })],
  ['delete-a', call('slack_threads_delete', { id: 'th-a' })],
];

const { parsed, noise, stderr } = await drive([
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: {} },
  { jsonrpc: '2.0', id: 0, method: 'tools/list' },
  ...script.map(([, f]) => f),
]);

const byId = new Map(parsed.map((m) => [m.id, m]));
const res = Object.fromEntries(
  script.map(([label, f]) => {
    const m = byId.get(f.id);
    return [label, { text: m?.result?.content?.[0]?.text ?? '', isError: !!m?.result?.isError }];
  }),
);
const show = (label) => process.stdout.write(`\n--- ${label} ---\n${res[label].text}\n`);

// ---- Assertions ---------------------------------------------------------------

check(noise.length === 0, 'stdout carries only JSON-RPC frames');
check(byId.get(1)?.result?.serverInfo?.name === 'devdy', 'initialize reports serverInfo.name = "devdy"');
const names = (byId.get(0)?.result?.tools || []).map((t) => t.name);
const SUFFIXES = ['list', 'read', 'search', 'update', 'rename', 'append', 'delete', 'set_project', 'reorder', 'read_attachment'];
for (const prefix of ['slack_threads', 'web_pages']) {
  check(SUFFIXES.every((s) => names.includes(`${prefix}_${s}`)), `tools/list advertises all 10 ${prefix}_* tools`);
}

show('list-project');
check(res['list-project'].text.includes('[th-a]') && !res['list-project'].text.includes('[th-b]'), 'list: scope=project only shows the current project');
check(/2 attachment\(s\)/.test(res['list-project'].text) && res['list-project'].text.includes('#dev-backend'), 'list: shows channel + attachment count');
check(res['list-all'].text.includes('[th-a]') && res['list-all'].text.includes('[th-b]'), 'list: scope=all shows every thread');
check(!res['list-all'].text.includes('[wp-'), 'list: slack tools never show web rows');

show('read-a');
check(res['read-a'].text.includes(join(appData, 'slack-threads/th-a/files/deploy.log')), 'read: attachment paths are ABSOLUTE');
check(res['read-a'].text.includes('thread_url: https://malme.slack.com/archives/C01/p1'), 'read: metadata included');
check(res['read-missing'].text.startsWith('Slack thread not found'), 'read: unknown id reported');

show('search-channel');
check(res['search-channel'].text.includes('[th-b]') && !res['search-channel'].text.includes('[th-a]'), 'search: matches channel');
check(res['search-body'].text.includes('[th-a]') && res['search-body'].text.includes('rollback'), 'search: matches body with snippet');
check(res['search-literal'].text.startsWith('No Slack threads match'), 'search: LIKE wildcards are literal');

const rowB = db.prepare('SELECT * FROM captures WHERE id = ?').get('th-b');
const rowA = db.prepare('SELECT * FROM captures WHERE id = ?');
check(!res.update.isError && rowB.title === 'Design review v2' && rowB.content === 'carol: new mockups attached', 'update: title changed, content kept');
check(rowB.updated_at !== T0, 'update/set_project: updated_at bumped');
// Same shape as Rust's chrono `Utc::now().to_rfc3339()`: `+00:00`, never `Z`.
const RFC3339_RUST = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.(\d{3}|\d{6}|\d{9}))?\+00:00$/;
check(RFC3339_RUST.test(rowB.updated_at), `timestamps match chrono to_rfc3339 (got ${rowB.updated_at})`);
check(rowB.title_custom === 1, 'update: a changed title sets title_custom = 1');
check(res['update-empty'].isError, 'update: rejects a call with neither title nor content');
check(rowB.project_id === PROJECT, 'set_project: links to the current project');

show('att-text');
check(res['att-text'].text.includes('ERROR: migration 42 failed'), 'read_attachment: small utf-8 text inline');
show('att-png');
check(res['att-png'].text.includes('Not returned inline') && res['att-png'].text.includes(join(appData, 'slack-threads/th-a/files/screenshot.png')), 'read_attachment: image → absolute path + hint');
check(/larger than/.test(res['att-big'].text), 'read_attachment: >1MB → path + hint');
check(/not valid UTF-8/.test(res['att-latin1'].text), 'read_attachment: non-utf8 octet-stream → path + hint');
check(res['att-evil'].isError && /escapes/.test(res['att-evil'].text), 'read_attachment: refuses paths outside app_data');

check(existsSync(join(appData, 'keep-me')) && existsSync(outsidePath), 'delete: traversal ids touch nothing on disk');
show('delete-a');
check(!rowA.get('th-a'), 'delete: thread row removed');
check(db.prepare('SELECT COUNT(*) AS n FROM capture_attachments WHERE capture_id = ?').get('th-a').n === 0, 'delete: attachment rows removed');
check(!existsSync(join(appData, 'slack-threads/th-a')), 'delete: <app_data>/slack-threads/<id> removed');
check(existsSync(join(appData, 'slack-threads/th-b')), 'delete: other threads\' files kept');

// Append / set_project(global) / reorder ran before the delete; th-a is gone now,
// so verify them through the server's own replies.
check(/^Appended to Slack thread th-a/.test(res.append.text), 'append: succeeded');
check(/global \(no project\)/.test(res['set-global'].text), 'set_project: scope=global detaches');
const posB = db.prepare('SELECT position FROM captures WHERE id = ?').get('th-b').position;
check(!res.reorder.isError && posB === 0, 'reorder: moved id goes to the top');

// Append is verifiable on th-b with a second quick run.
const { parsed: p2 } = await drive([
  { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'slack_threads_append', arguments: { id: 'th-b', text: 'follow-up' } } },
]);
check(!p2[0]?.result?.isError && db.prepare('SELECT content FROM captures WHERE id = ?').get('th-b').content === 'carol: new mockups attached\n\nfollow-up', 'append: text appended with blank-line separator');

// Content is stored verbatim (no trim), like the Rust backend; the title is trimmed.
const verbatim = '  indented first line\n\ntrailing newline\n';
const { parsed: p3 } = await drive([
  { jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'slack_threads_update', arguments: { id: 'th-b', title: '  Padded  ', content: verbatim } } },
]);
const rowB2 = db.prepare('SELECT title, content, updated_at FROM captures WHERE id = ?').get('th-b');
check(!p3[0]?.result?.isError && rowB2.content === verbatim && rowB2.title === 'Padded', 'update: content kept verbatim, title trimmed');
check(RFC3339_RUST.test(rowB2.updated_at), `update: updated_at in chrono format (got ${rowB2.updated_at})`);

// title_custom: only a real title change flips it; rename always sets it.
insCapture.run('th-c', 'slack', 'Exported title', 'body', null, null, null, null, null, null, 0, null, null, 5, T0, T0);
const rename = (args, id) => ({ jsonrpc: '2.0', id, method: 'tools/call', params: { name: 'slack_threads_rename', arguments: args } });
const upd = (args, id) => ({ jsonrpc: '2.0', id, method: 'tools/call', params: { name: 'slack_threads_update', arguments: args } });
const flag = () => db.prepare('SELECT title_custom FROM captures WHERE id = ?').get('th-c').title_custom;
const { parsed: p4 } = await drive([upd({ id: 'th-c', content: 'new body' }, 1), upd({ id: 'th-c', title: ' Exported title ' }, 2)]);
check(!p4.some((m) => m.result?.isError) && flag() === 0, 'update: content-only / same title leaves title_custom = 0');
const long = 'あ'.repeat(201);
const { parsed: p5 } = await drive([
  rename({ id: 'th-c', title: '   ' }, 1),
  rename({ id: 'th-c', title: long }, 2),
  rename({ id: 'nope', title: 'x' }, 3),
  rename({ id: 'th-c', title: '  Renamed by AI  ' }, 4),
]);
const r5 = new Map(p5.map((m) => [m.id, m.result]));
check(r5.get(1)?.isError && /required/.test(r5.get(1).content[0].text), 'rename: rejects an empty/blank title');
check(r5.get(2)?.isError && /200/.test(r5.get(2).content[0].text), 'rename: rejects > 200 characters');
check(r5.get(3)?.isError && /not found/.test(r5.get(3).content[0].text), 'rename: unknown id reported');
const rowC = db.prepare('SELECT title, title_custom, updated_at FROM captures WHERE id = ?').get('th-c');
process.stdout.write(`\n--- rename ---\n${r5.get(4)?.content?.[0]?.text}\n`);
check(!r5.get(4)?.isError && rowC.title === 'Renamed by AI' && rowC.title_custom === 1, 'rename: title trimmed + title_custom = 1');
check(RFC3339_RUST.test(rowC.updated_at), `rename: updated_at in chrono format (got ${rowC.updated_at})`);
const { parsed: p6 } = await drive([rename({ id: 'th-c', title: 'b'.repeat(200) }, 1)]);
check(!p6[0]?.result?.isError, 'rename: accepts exactly 200 characters');

// ---- Web pages -------------------------------------------------------------------
const tc = (name, args, id) => ({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } });
const webScript = [
  ['w-list', tc('web_pages_list', {})],
  ['w-list-all', tc('web_pages_list', { scope: 'all' })],
  ['w-read', tc('web_pages_read', { id: 'wp-1', max_chars: 300 })],
  ['w-read-next', tc('web_pages_read', { id: 'wp-1', max_chars: 300, offset: 300 })],
  ['w-section', tc('web_pages_read', { id: 'wp-1', section: 'configure', max_chars: 500 })],
  ['w-section-full', tc('web_pages_read', { id: 'wp-1', section: 'Install' })],
  ['w-section-missing', tc('web_pages_read', { id: 'wp-1', section: 'nope' })],
  ['w-read-sel', tc('web_pages_read', { id: 'wp-2' })],
  ['w-read-slack', tc('web_pages_read', { id: 'th-b' })],
  ['s-read-web', tc('slack_threads_read', { id: 'wp-1' })],
  ['w-search-site', tc('web_pages_search', { query: 'tauri', scope: 'all' })],
  ['w-search-url', tc('web_pages_search', { query: 'example.com', scope: 'all' })],
  ['w-rename', tc('web_pages_rename', { id: 'wp-2', title: 'My excerpt' })],
  ['w-rename-slack', tc('web_pages_rename', { id: 'th-b', title: 'hijack' })],
  ['w-append', tc('web_pages_append', { id: 'wp-2', text: 'note' })],
  ['w-set', tc('web_pages_set_project', { id: 'wp-2' })],
  ['w-reorder', tc('web_pages_reorder', { ids: ['wp-2'] })],
  ['w-reorder-slack', tc('web_pages_reorder', { ids: ['th-b'] })],
  ['w-att', tc('web_pages_read_attachment', { id: 'att-web' })],
  ['w-att-slack', tc('web_pages_read_attachment', { id: 'att-big' })],
  ['w-setext', tc('web_pages_read', { id: 'wp-3' })],
  ['w-setext-section', tc('web_pages_read', { id: 'wp-3', section: 'setup' })],
  ['w-setext-h1', tc('web_pages_read', { id: 'wp-3', section: 'Getting started' })],
  ['w-del-slack', tc('slack_threads_delete', { id: 'wp-1' })],
  ['w-del', tc('web_pages_delete', { id: 'wp-1' })],
].map(([label, f], i) => [label, { ...f, id: i + 1 }]);
const { parsed: pw } = await drive(webScript.map(([, f]) => f));
const wById = new Map(pw.map((m) => [m.id, m.result]));
const w = Object.fromEntries(webScript.map(([label, f]) => [label, { text: wById.get(f.id)?.content?.[0]?.text ?? '', isError: !!wById.get(f.id)?.isError }]));
const wshow = (label) => process.stdout.write(`\n--- ${label} ---\n${w[label].text}\n`);

wshow('w-list-all');
check(w['w-list'].text.includes('[wp-1]') && !w['w-list'].text.includes('[wp-2]'), 'web list: scope=project only shows the current project');
check(w['w-list-all'].text.includes('Tauri') && /example\.com · excerpt/.test(w['w-list-all'].text) && !w['w-list-all'].text.includes('[th-'), 'web list: site/domain + excerpt badge, no slack rows');
wshow('w-read');
const total = longDoc.length;
check(w['w-read'].text.includes(`total_chars: ${total}`), 'web read: total_chars reported');
check(/## Outline[\s\S]*# Tauri capabilities \(offset 0\)[\s\S]*### Permissions/.test(w['w-read'].text) && !w['w-read'].text.includes('# not a heading ('), 'web read: outline with offsets, code fences skipped');
check(w['w-read'].text.includes('source_url: https://v2.tauri.app/security/capabilities/') && w['w-read'].text.includes('author: Tauri team') &&
  w['w-read'].text.includes('captured_at: 2026-10-09T10:00:00Z') && w['w-read'].text.includes('published_at: 2026-09-01'), 'web read: web metadata shown');
check(w['w-read'].text.includes(`chars 0–300 of ${total}`) && w['w-read'].text.includes('Continue with offset=300.'), 'web read: max_chars slice + continue hint');
check(w['w-read'].text.includes(join(appData, webRel)), 'web read: attachment path is ABSOLUTE under web-pages/');
const bodyOf = (t) => t.split(/_chars [^_]+_\n\n/)[1]?.split('\n\n---\n… truncated')[0] ?? '';
check(bodyOf(w['w-read'].text) + bodyOf(w['w-read-next'].text) === longDoc.slice(0, 600), 'web read: offset continues exactly where the previous slice ended');
wshow('w-section');
const confStart = longDoc.indexOf('## Configure');
const confEnd = longDoc.indexOf('## FAQ');
check(bodyOf(w['w-section'].text) === longDoc.slice(confStart, confStart + 500) && w['w-section'].text.includes('Continue with offset=500 section="Configure"'), 'web read: section (case-insensitive) sliced + section-aware hint');
check(bodyOf(w['w-section-full'].text) === longDoc.slice(longDoc.indexOf('## Install'), confStart) && !w['w-section-full'].text.includes('truncated'), 'web read: section stops at the next same-level heading');
check(longDoc.slice(confStart, confEnd).includes('### Permissions'), 'fixture: subsection belongs to its parent section');
check(w['w-section-missing'].isError && /section not found[\s\S]*"Install"/.test(w['w-section-missing'].text), 'web read: unknown section lists headings');
check(w['w-read-sel'].text.includes('selection: yes'), 'web read: selection flag shown');
check(/not found/i.test(w['w-read-slack'].text) && /not found/i.test(w['s-read-web'].text), 'kind isolation: read refuses the other kind');
check(w['w-search-site'].text.includes('[wp-1]') && w['w-search-url'].text.includes('[wp-2]'), 'web search: matches site_name and source_url');
const wp2 = db.prepare('SELECT * FROM captures WHERE id = ?').get('wp-2');
check(!w['w-rename'].isError && wp2.title === 'My excerpt' && wp2.title_custom === 1, 'web rename: title + title_custom = 1');
check(w['w-rename-slack'].isError && db.prepare('SELECT title FROM captures WHERE id = ?').get('th-b').title !== 'hijack', 'kind isolation: web rename refuses a slack id');
check(wp2.content === 'Only this paragraph.\n\nnote' && wp2.project_id === PROJECT && RFC3339_RUST.test(wp2.updated_at), 'web append + set_project + rfc3339 updated_at');
const pos = (id) => db.prepare('SELECT position FROM captures WHERE id = ?').get(id).position;
check(!w['w-reorder'].isError && pos('wp-2') === 0 && pos('th-b') === 0, 'web reorder: per kind (slack positions untouched)');
check(w['w-reorder-slack'].isError, 'kind isolation: web reorder rejects a slack id');
check(w['w-att'].text.includes('<svg/>'), 'web read_attachment: inline text');
check(w['w-att-slack'].isError, 'kind isolation: web read_attachment refuses a slack attachment');
wshow('w-setext');
const outlineOf = (t) => (t.split('## Outline\n')[1] || '').split('\n\n')[0];
const setextOutline = outlineOf(w['w-setext'].text);
const at = (needle) => setextDoc.indexOf(needle);
check(setextOutline.includes(`- # Getting started (offset ${at('Getting started')})`) &&
  setextOutline.includes(`  - ## Setup (offset ${at('Setup')})`) &&
  setextOutline.includes(`  - ## Usage (offset ${at('Usage')})`), 'setext: `===` → h1, `---` → h2, offsets at the heading text line');
check(setextOutline.split('\n').length === 3 && !/More welcome|Welcome\.|not a heading/.test(setextOutline), 'setext: `---` after a blank line (hr) and inside a code fence is not a heading');
check(bodyOf(w['w-setext-section'].text) === setextDoc.slice(at('Setup'), at('Usage')), 'setext: section read (case-insensitive) stops at the next h2');
check(bodyOf(w['w-setext-h1'].text) === setextDoc, 'setext: h1 section spans its h2 subsections');
check(/^Deleted 0\/1/.test(w['w-del-slack'].text), 'kind isolation: slack delete ignores a web id');
check(/^Deleted 1\/1 web page/.test(w['w-del'].text) && !existsSync(join(appData, 'web-pages/wp-1')) &&
  db.prepare('SELECT COUNT(*) AS n FROM capture_attachments WHERE capture_id = ?').get('wp-1').n === 0, 'web delete: row, attachment rows and web-pages/<id> dir removed');

db.close();
rmSync(appData, { recursive: true, force: true });
rmSync(outsidePath, { force: true });

if (failures.length) {
  process.stderr.write(`\n${failures.length} check(s) failed.\n`);
  if (stderr.trim()) process.stderr.write(`server stderr:\n${stderr}\n`);
  process.exit(1);
}
process.stdout.write(`\nAll checks passed.\n`);
