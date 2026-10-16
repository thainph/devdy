#!/usr/bin/env node
// Smoke test for the capture tools of the built-in `devdy` MCP server:
// `web_pages_*` (shared `captures` core) and `slack_threads_*` (own tables since
// migration 0041, docs/srs-slack-thread-translate.md §14).
//
//   node sidecar-mcp/test-captures.mjs
//
// Builds a throwaway <app_data> (temp dir) whose data.db is created by running
// every real migration in src-tauri/migrations up to 0040, seeds legacy Slack
// captures + web pages, then applies 0041 — the real migration file when it
// exists, else the SQL block of the frozen SRS §5.1 — and seeds fetched Slack
// threads into the new tables. It then drives index.mjs over stdio JSON-RPC and
// checks every tool against the real DB and filesystem. Nothing outside the temp
// dir is touched.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { basename, dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const SRS = join(HERE, '..', 'docs', 'srs-slack-thread-translate.md');
const PROJECT = 'proj-1';

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

// ---- Fixture: migrations ≤ 0040 -------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-captures-'));
const dbPath = join(appData, 'data.db');
const outsideName = `${basename(appData)}-outside.txt`;
const outsidePath = join(dirname(appData), outsideName);
const db = new DatabaseSync(dbPath);

const migrationFiles = readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort();
for (const f of migrationFiles.filter((f) => f < '0041')) db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));

let migration0041;
let migration0041Source;
const real0041 = migrationFiles.find((f) => f.startsWith('0041_'));
if (real0041) {
  migration0041 = readFileSync(join(MIGRATIONS, real0041), 'utf8');
  migration0041Source = `src-tauri/migrations/${real0041}`;
} else {
  const srs = readFileSync(SRS, 'utf8');
  const section = srs.slice(srs.indexOf('### 5.1'));
  migration0041 = section.slice(section.indexOf('```sql') + 6, section.indexOf('```', section.indexOf('```sql') + 6));
  migration0041Source = 'SRS §5.1 SQL (0041 migration file not written yet)';
}
process.stdout.write(`migration 0041 from: ${migration0041Source}\n`);

const T0 = '2026-01-01T00:00:00.000Z';
const T1 = '2026-02-01T00:00:00.000Z';

// Legacy kind='slack' captures (0038/0039 era), carried over by 0041.
const insLegacy = db.prepare(
  `INSERT INTO captures (id, kind, title, title_custom, content, workspace, channel, thread_url, thread_ts, exported_at,
                         project_id, position, created_at, updated_at)
   VALUES (?, 'slack', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
);
insLegacy.run('lg-a', 'Legacy deploy thread', 1, '# Legacy deploy\nalice: deploy broke\nbob: rollback done', 'malme', '#dev-backend',
  'https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359', '1789877283.026359', T0, PROJECT, 3, T0, T0);
insLegacy.run('lg-dup-old', 'Dup old title', 0, 'old copy', 'malme', '#ops',
  'https://malme.slack.com/archives/C0DUPDUPDUP/p1700000000000100', null, T0, null, 7, T0, T0);
insLegacy.run('lg-dup-new', 'Dup new title', 1, 'new copy', 'malme', '#ops',
  'https://malme.slack.com/archives/C0DUPDUPDUP/p1700000000000100', null, T0, PROJECT, 4, T0, T1);
insLegacy.run('lg-nourl', 'No URL thread', 0, 'carol: mockups', 'malme', '#design', null, null, null, null, 9, T0, T0);

const insAtt = db.prepare(
  'INSERT INTO capture_attachments (id, capture_id, name, file_path, size, mime, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)',
);
const addFile = (attId, captureId, dir, name, file, data, mime) => {
  const rel = `${dir}/${captureId}/files/${file}`;
  mkdirSync(join(appData, dirname(rel)), { recursive: true });
  writeFileSync(join(appData, rel), data);
  insAtt.run(attId, captureId, name, rel, Buffer.byteLength(data), mime, T0);
  return rel;
};
const legacyLogRel = addFile('att-log', 'lg-a', 'slack-threads', 'attachments/deploy.log', 'deploy.log', 'ERROR: migration 42 failed\n', 'text/plain');
addFile('att-dup-old', 'lg-dup-old', 'slack-threads', 'old.txt', 'old.txt', 'old', 'text/plain');
addFile('att-dup-new', 'lg-dup-new', 'slack-threads', 'new.txt', 'new.txt', 'new', 'text/plain');

// Web pages (unchanged by 0041).
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
const webRel = addFile('att-web', 'wp-1', 'web-pages', 'images/diagram.svg', 'diagram.svg', '<svg/>', 'text/plain');
addFile('att-big', 'wp-2', 'web-pages', 'big.txt', 'big.txt', 'x'.repeat(1024 * 1024 + 1), 'text/plain');
addFile('att-latin1', 'wp-2', 'web-pages', 'latin1.txt', 'latin1.txt', Buffer.from([0x63, 0x61, 0x66, 0xe9]), 'application/octet-stream');
addFile('att-png', 'wp-2', 'web-pages', 'shot.png', 'shot.png', Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0]), 'image/png');
// A row whose stored path tries to escape <app_data> must be refused.
insAtt.run('att-evil', 'wp-2', 'evil', `../${outsideName}`, 1, 'text/plain', T0);
writeFileSync(outsidePath, 'secret');
// A sibling dir a traversal-y delete id would hit if ids weren't validated.
mkdirSync(join(appData, 'keep-me'), { recursive: true });

// ---- Migration 0041 ---------------------------------------------------------------

db.exec(migration0041);
const sl = (sql, ...p) => db.prepare(sql).all(...p);
check(sl("SELECT COUNT(*) AS n FROM captures WHERE kind = 'slack'")[0].n === 0, '0041: no kind=slack rows left in captures');
check(sl("SELECT COUNT(*) AS n FROM captures WHERE kind = 'web'")[0].n === 3, '0041: web rows untouched');
check(sl('SELECT COUNT(*) AS n FROM capture_attachments')[0].n === 5, '0041: capture_attachments only keeps web files');
const ids0041 = sl('SELECT id FROM slack_threads ORDER BY id').map((r) => r.id).join(',');
check(ids0041 === 'lg-a,lg-dup-new,lg-nourl', `0041: duplicates merged, newest kept (got ${ids0041})`);
const lgA = sl('SELECT * FROM slack_threads WHERE id = ?', 'lg-a')[0];
check(lgA.title === 'Legacy deploy thread' && lgA.title_custom === 1 && lgA.project_id === PROJECT && lgA.position === 3 &&
  lgA.channel_id === 'C0B83GHDTFG' && lgA.thread_ts === '1789877283.026359' && lgA.channel === 'dev-backend' &&
  lgA.legacy === 1 && lgA.fetch_status === 'pending' && lgA.legacy_content.startsWith('# Legacy deploy'), '0041: legacy row fields carried over');
const lgN = sl('SELECT * FROM slack_threads WHERE id = ?', 'lg-nourl')[0];
check(lgN.channel_id === null && lgN.thread_ts === null && lgN.legacy === 1, '0041: row without thread_url has channel_id NULL');
check(sl('SELECT id FROM slack_files ORDER BY id').map((r) => r.id).join(',') === 'att-dup-new,att-log', '0041: slack_files only from kept rows');
check(sl('SELECT file_path FROM slack_files WHERE id = ?', 'att-log')[0].file_path === legacyLogRel, '0041: legacy file_path kept as-is');
check(sl('SELECT id FROM slack_legacy_orphans').map((r) => r.id).join(',') === 'lg-dup-old', '0041: merged-away id recorded as orphan');

// ---- Fetched (v2) Slack threads -----------------------------------------------------

const insThread = db.prepare(
  `INSERT INTO slack_threads (id, title, title_custom, project_id, position, created_at, updated_at, workspace_domain, team_id,
     workspace, channel_id, channel, thread_ts, thread_url, fetch_status, fetch_error_code, fetch_error, fetched_at,
     reply_count, last_reply_ts, warnings, search_text, legacy, legacy_content)
   VALUES (?, ?, 0, ?, ?, ?, ?, 'malme.slack.com', ?, 'malme', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, '', 0, NULL)`,
);
insThread.run('th-v2', 'Deploy failed on staging', PROJECT, -2, T0, T0, 'T1', 'C0B8TESTAA1', 'dev-backend', '1789877000.000100',
  'https://malme.slack.com/archives/C0B8TESTAA1/p1789877000000100', 'ready', null, null, T1, 4, '1789877400.000500', '["files_scope"]');
insThread.run('th-err', 'Broken thread', null, -1, T0, T0, 'T1', 'C0B8TESTAA2', 'private-x', '1789870000.000100',
  'https://malme.slack.com/archives/C0B8TESTAA2/p1789870000000100', 'error', 'missing_scope', 'needed: groups:history', null, 0, null, '[]');
insThread.run('th-long', 'Long thread', PROJECT, 10, T0, T0, 'T1', 'C0B8TESTAA3', 'general', '1789800000.000000',
  'https://malme.slack.com/archives/C0B8TESTAA3/p1789800000000000', 'ready', null, null, T1, 59, '1789800059.000000', '[]');

const insUser = db.prepare('INSERT INTO slack_users (team_id, user_id, display_name, real_name, avatar_url, avatar_path, is_bot, fetched_at) VALUES (?, ?, ?, ?, NULL, ?, 0, ?)');
insUser.run('T1', 'U1', 'Alice', 'Alice Nguyen', 'slack-cache/avatars/T1/U1-abcd1234.png', T0);
insUser.run('T1', 'U2', '', 'Bob Tanaka', null, T0);
insUser.run('T2', 'U1', 'WrongAlice', null, null, T0); // other workspace: must not leak in
db.prepare('INSERT INTO slack_channels (team_id, channel_id, name, kind, fetched_at) VALUES (?, ?, ?, ?, ?)').run('T1', 'C0B8DESIGN', 'design', 'public', T0);
mkdirSync(join(appData, 'slack-cache/avatars/T1'), { recursive: true });
writeFileSync(join(appData, 'slack-cache/avatars/T1/U1-abcd1234.png'), 'png');

const insMsg = db.prepare(
  `INSERT INTO slack_messages (thread_id, ts, seq, user_id, bot_id, author_name, subtype, text_raw, text_vi, src_hash,
     translate_status, edited_ts, reactions, file_ids)
   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, 'h', ?, ?, ?, ?)`,
);
const codeBlock = '```\nif a &lt; b &amp;&amp; c { run(); }\n```';
insMsg.run('th-v2', '1789877000.000100', 0, 'U1', null, null, null,
  `<@U1> ステージングのデプロイが失敗しました :fire:\n${codeBlock}`,
  `<@U1> Triển khai staging bị lỗi :fire:\n${codeBlock}`, 'done', null, '[{"name":"+1","count":2}]', '["sf-img","sf-gone"]');
insMsg.run('th-v2', '1789877100.000200', 1, 'U2', null, null, null,
  'See <https://example.com/runbook|the runbook> and <#C0B8DESIGN> &amp; <!here>',
  'Xem <https://example.com/runbook|the runbook> và <#C0B8DESIGN> &amp; <!here>', 'done', '1789877150.000000', '[]', '[]');
insMsg.run('th-v2', '1789877200.000300', 2, 'U2', null, null, null, '再起動します `kubectl rollout restart`', null, 'pending', null, '[]', '[]');
insMsg.run('th-v2', '1789877400.000500', 3, null, 'B1', 'deploy-bot', 'bot_message', 'Rollback <mailto:ops@x.io|ops@x.io> done', 'Đã rollback <mailto:ops@x.io|ops@x.io>', 'done', null, '[]', '[]');
for (let i = 0; i < 60; i++) {
  insMsg.run('th-long', `17898000${String(i).padStart(2, '0')}.000000`, i, 'U1', null, null, null,
    `original line ${i}`, `dòng tiếng Việt số ${i} ${'x'.repeat(80)}`, 'done', null, '[]', '[]');
}

const insFile = db.prepare(
  `INSERT INTO slack_files (id, thread_id, slack_file_id, message_ts, name, title, mime, filetype, size, file_path, permalink,
     download_status, download_error, created_at)
   VALUES (?, ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?)`,
);
const imgRel = 'slack-threads/th-v2/files/F001/screenshot.png';
mkdirSync(join(appData, dirname(imgRel)), { recursive: true });
writeFileSync(join(appData, imgRel), 'PNGDATA');
insFile.run('sf-img', 'th-v2', 'F001', '1789877000.000100', 'screenshot.png', 'image/png', 'png', 7, imgRel,
  'https://malme.slack.com/files/U1/F001/screenshot.png', 'done', null, T0);
insFile.run('sf-gone', 'th-v2', 'F002', '1789877000.000100', 'spec.pdf', 'application/pdf', 'pdf', 1234, null,
  'https://malme.slack.com/files/U1/F002/spec.pdf', 'unavailable', null, T0);

// QA regressions: emoji slicing (M-1), unsafe stored path (M-2), empty text_vi (M-3).
insThread.run('th-qa', 'QA thread', null, 20, T0, T0, 'T1', 'C0B8TESTAA4', 'qa', '1789900000.000000',
  'https://malme.slack.com/archives/C0B8TESTAA4/p1789900000000000', 'ready', null, null, T1, 1, '1789900001.000000', '[]');
insMsg.run('th-qa', '1789900000.000000', 0, 'U1', null, null, null, 'emoji 😀😀 end', 'biểu tượng 😀😀 hết', 'done', null, '[]', '["sf-evil"]');
insMsg.run('th-qa', '1789900001.000000', 1, 'U1', null, null, null, '空の翻訳', '', 'done', null, '[]', '[]');
insFile.run('sf-evil', 'th-qa', 'F009', '1789900000.000000', 'evil.txt', 'text/plain', 'text', 6, `../${outsideName}`,
  'https://malme.slack.com/files/U1/F009/evil.txt', 'done', null, T0);

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

const tc = (name, args, id) => ({ jsonrpc: '2.0', id, method: 'tools/call', params: { name, arguments: args } });
/** Run a labelled script in one server process; returns label → { text, isError }. */
async function run(script, extra = []) {
  const frames = script.map(([label, name, args], i) => [label, tc(name, args, i + 100)]);
  const { parsed, noise, stderr } = await drive([...extra, ...frames.map(([, f]) => f)]);
  const byId = new Map(parsed.map((m) => [m.id, m]));
  const out = Object.fromEntries(
    frames.map(([label, f]) => {
      const m = byId.get(f.id);
      return [label, { text: m?.result?.content?.[0]?.text ?? '', isError: !!m?.result?.isError }];
    }),
  );
  return { out, byId, noise, stderr };
}
const show = (r, label) => process.stdout.write(`\n--- ${label} ---\n${r[label].text}\n`);

// ---- tools/list -------------------------------------------------------------------

const head = await drive([
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: {} },
  { jsonrpc: '2.0', id: 2, method: 'tools/list' },
]);
check(head.noise.length === 0, 'stdout carries only JSON-RPC frames');
const headById = new Map(head.parsed.map((m) => [m.id, m]));
check(headById.get(1)?.result?.serverInfo?.name === 'devdy', 'initialize reports serverInfo.name = "devdy"');
const names = (headById.get(2)?.result?.tools || []).map((t) => t.name);
const WEB_SUFFIXES = ['list', 'read', 'search', 'update', 'rename', 'append', 'delete', 'set_project', 'reorder', 'read_attachment'];
check(WEB_SUFFIXES.every((s) => names.includes(`web_pages_${s}`)) && names.filter((n) => n.startsWith('web_pages_')).length === 10,
  'tools/list advertises exactly the 10 web_pages_* tools');
const slackNames = names.filter((n) => n.startsWith('slack_threads_')).sort();
check(slackNames.join(',') === ['delete', 'list', 'read', 'rename', 'search', 'set_project'].map((s) => `slack_threads_${s}`).join(','),
  `tools/list: slack_threads_* = list/read/search/delete/rename/set_project (got ${slackNames.join(',')})`);
check(!['update', 'append', 'reorder', 'read_attachment'].some((s) => names.includes(`slack_threads_${s}`)), 'tools/list: no slack update/append/reorder/read_attachment');

// AC-04: no Slack handling left in the shared captures code.
for (const f of ['lib/capture-tools.mjs', 'lib/store.mjs']) {
  const src = readFileSync(join(HERE, f), 'utf8');
  check(!/kind\s*===\s*'slack'|\bslack\s*:\s*\{|captureTools\('slack'|'slack-threads'/.test(src), `${f}: no Slack kind handling left`);
}
check(!/captureTools\('slack'/.test(readFileSync(SERVER, 'utf8')), 'index.mjs: no captureTools("slack")');

// ---- Slack threads ----------------------------------------------------------------

const s = await run([
  ['list-project', 'slack_threads_list', {}],
  ['list-all', 'slack_threads_list', { scope: 'all' }],
  ['list-limit', 'slack_threads_list', { scope: 'all', limit: 1 }],
  ['read', 'slack_threads_read', { id: 'th-v2' }],
  ['read-both', 'slack_threads_read', { id: 'th-v2', original: 'both' }],
  ['read-only', 'slack_threads_read', { id: 'th-v2', original: 'only' }],
  ['read-bad-mode', 'slack_threads_read', { id: 'th-v2', original: 'ja' }],
  ['read-err', 'slack_threads_read', { id: 'th-err' }],
  ['read-legacy', 'slack_threads_read', { id: 'lg-a' }],
  ['read-missing', 'slack_threads_read', { id: 'nope' }],
  ['read-long', 'slack_threads_read', { id: 'th-long', max_chars: 1000 }],
  ['search-ja', 'slack_threads_search', { query: 'デプロイ', scope: 'all' }],
  ['search-vi', 'slack_threads_search', { query: 'TRIỂN KHAI', scope: 'all' }],
  ['search-vi-ci', 'slack_threads_search', { query: 'triển khai staging', scope: 'all' }],
  ['search-title', 'slack_threads_search', { query: 'broken', scope: 'all' }],
  ['search-project', 'slack_threads_search', { query: 'broken' }],
  ['search-legacy', 'slack_threads_search', { query: 'rollback done', scope: 'all' }],
  ['search-channel', 'slack_threads_search', { query: 'private-x', scope: 'all' }],
  ['search-bot', 'slack_threads_search', { query: 'x.io> DONE', scope: 'all' }],
  ['search-literal', 'slack_threads_search', { query: '%', scope: 'all' }],
  ['web-on-slack', 'web_pages_read', { id: 'lg-a' }],
]);
const S = s.out;

show(S, 'list-project');
check(S['list-project'].text.includes('[th-v2]') && S['list-project'].text.includes('[lg-a]') && !S['list-project'].text.includes('[th-err]'),
  'list: scope=project only shows the current project');
check(S['list-project'].text.includes('- [th-v2] Deploy failed on staging · #dev-backend · malme · ready · 4 replies · last reply 2026-09-20T'),
  'list: line format title · #channel · workspace · status · N replies · last reply <ISO>');
check(/\[lg-a\][^\n]*· pending · 0 replies · legacy/.test(S['list-project'].text), 'list: legacy marker');
check(S['list-project'].text.indexOf('[th-v2]') < S['list-project'].text.indexOf('[lg-a]'), 'list: ordered by position');
show(S, 'list-all');
check(/\[th-err\][^\n]*· error · 0 replies · error: missing_scope/.test(S['list-all'].text), 'list: error code shown');
check(S['list-all'].text.includes('[lg-nourl]') && !S['list-all'].text.includes('[wp-'), 'list: scope=all, never web rows');
check(S['list-limit'].text.split('\n').length === 1, 'list: limit respected');

show(S, 'read');
const R = S.read.text;
check(R.includes('- workspace: malme (malme.slack.com)') && R.includes('- channel: #dev-backend') &&
  R.includes('- thread_url: https://malme.slack.com/archives/C0B8TESTAA1/p1789877000000100') && R.includes('- fetch_status: ready') &&
  R.includes(`- fetched_at: ${T1}`) && R.includes(`- project_id: ${PROJECT}`) && R.includes('- warnings: files_scope'), 'read: metadata header');
check(R.includes('### Alice · 2026-09-20 ') && R.includes(' UTC · ts=1789877000.000100'), 'read: message header with name, UTC time, ts');
check(R.includes('@Alice Triển khai staging bị lỗi :fire:') && !R.includes('WrongAlice') && !R.includes('<@U1>'), 'read: Vietnamese by default, mention → @Name of the thread workspace');
check(R.includes('```\nif a < b && c { run(); }\n```'), 'read: code block kept (entities decoded)');
check(R.includes('Xem [the runbook](https://example.com/runbook) và #design & @here'), 'read: link → [label](url), <#C…> → #name, entity, <!here>');
check(R.includes('### Bob Tanaka · ') && R.includes('ts=1789877100.000200 · edited'), 'read: real_name fallback + edited marker');
check(R.includes('再起動します `kubectl rollout restart` [untranslated]'), 'read: untranslated → original + [untranslated]');
check(R.includes('### deploy-bot · ') && R.includes('Đã rollback [ops@x.io](mailto:ops@x.io)'), 'read: bot author name + mailto');
check(R.includes(`- 📎 screenshot.png — ${join(appData, imgRel)} · 7 bytes · image/png`) && existsSync(join(appData, imgRel)), 'read: downloaded file → ABSOLUTE existing path');
check(R.includes('- 📎 spec.pdf — not downloaded (unavailable): https://malme.slack.com/files/U1/F002/spec.pdf'), 'read: not downloaded → status + permalink');
check(R.includes('reactions: :+1: 2'), 'read: reactions');
check(!R.includes('> Original:') && !R.includes('ステージング'), 'read: original=none hides the original');
show(S, 'read-both');
check(S['read-both'].text.includes('> Original:\n> @Alice ステージングのデプロイが失敗しました :fire:') &&
  S['read-both'].text.includes('@Alice Triển khai staging'), 'read: original=both adds the > Original: block');
check(!S['read-both'].text.includes('[untranslated]\n> Original'), 'read: original=both does not duplicate untranslated text');
check(S['read-only'].text.includes('@Alice ステージングのデプロイが失敗しました') && !S['read-only'].text.includes('Triển khai') &&
  S['read-only'].text.includes('再起動します `kubectl rollout restart`') && !S['read-only'].text.includes('[untranslated]'), 'read: original=only shows the original');
check(S['read-bad-mode'].isError, 'read: rejects an unknown original mode');
check(S['read-err'].text.includes('- error: missing_scope — needed: groups:history') && S['read-err'].text.includes('(no messages fetched yet'), 'read: error + empty thread');
show(S, 'read-legacy');
check(S['read-legacy'].text.includes('- status: legacy snapshot (not yet re-fetched)') && S['read-legacy'].text.includes('# Legacy deploy\nalice: deploy broke') &&
  S['read-legacy'].text.includes(`- 📎 attachments/deploy.log — ${join(appData, legacyLogRel)}`), 'read: legacy → old markdown + legacy files (absolute)');
check(S['read-missing'].text.startsWith('Slack thread not found'), 'read: unknown id reported');
show(S, 'read-long');
const longText = S['read-long'].text;
const offMatch = longText.match(/Continue with offset=(\d+)\./);
check(longText.includes('_chars 0–1000 of ') && !!offMatch && offMatch[1] === '1000', 'read: max_chars slice + continue hint');
const bodyOf = (t) => t.split(/_chars [^_]+_\n\n/)[1]?.split('\n\n---\n… truncated')[0] ?? '';

show(S, 'search-ja');
check(S['search-ja'].text.includes('[th-v2]') && S['search-ja'].text.includes('  - Alice ts=1789877000.000100: @Alice ステージングのデプロイ'),
  'search: term only in the Japanese original → thread + original snippet');
check(S['search-vi'].text.includes('[th-v2]') && S['search-vi'].text.includes('Triển khai staging'), 'search: matches the Vietnamese translation');
check(S['search-vi-ci'].text.includes('[th-v2]'), 'search: case-insensitive (ASCII)');
check(S['search-title'].text.includes('[th-err]') && S['search-project'].text.startsWith('No Slack threads match'), 'search: title match + scope=project filter');
check(S['search-legacy'].text.includes('[lg-a]') && S['search-legacy'].text.includes('legacy snapshot:'), 'search: legacy_content');
check(S['search-bot'].text.includes('[th-v2]') && S['search-bot'].text.includes('  - deploy-bot ts=1789877400.000500: Rollback [ops@x.io](mailto:ops@x.io) done'), 'search: raw-only match on a bot message → original snippet');
check(S['search-channel'].text.includes('[th-err]'), 'search: channel');
check(S['search-literal'].text.startsWith('No Slack threads match'), 'search: LIKE wildcards are literal');
check(/not found/i.test(S['web-on-slack'].text), 'web_pages_read no longer sees carried-over Slack rows');

// Continuation: offset picks up exactly where the previous slice ended.
const s2 = await run([['read-long-next', 'slack_threads_read', { id: 'th-long', max_chars: 1000, offset: 1000 }],
  ['read-long-full', 'slack_threads_read', { id: 'th-long', max_chars: 1000000 }]]);
check(bodyOf(longText) + bodyOf(s2.out['read-long-next'].text) === bodyOf(s2.out['read-long-full'].text).slice(0, 2000),
  'read: offset continues exactly where the previous slice ended');

const qaFull = (await run([['f', 'slack_threads_read', { id: 'th-qa', max_chars: 1000000 }]])).out.f.text;
const qaBody = bodyOf(qaFull);
const emo = qaBody.indexOf('😀');
const qa = (await run([
  ['at', 'slack_threads_read', { id: 'th-qa', max_chars: 1, offset: emo }],
  ['mid', 'slack_threads_read', { id: 'th-qa', max_chars: 1, offset: emo + 1 }],
])).out;
check(bodyOf(qa.at.text) === '😀' && qa.at.text.includes(`Continue with offset=${emo + 2}.`), 'read: max_chars=1 on an emoji returns the full code point and advances (M-1)');
check(bodyOf(qa.mid.text) === '😀' && qa.mid.text.includes(`Continue with offset=${emo + 2}.`), 'read: offset inside a surrogate pair still advances (M-1)');
check(qaFull.includes('- 📎 evil.txt — downloaded but not readable (unsafe stored path refused: ../') && !qaFull.includes('not downloaded (done)') &&
  !qaFull.includes(outsidePath), 'read: done file with unsafe path reported accurately, path not exposed as readable (M-2)');
check(qaFull.includes('空の翻訳 [untranslated]'), 'read: empty text_vi falls back to the original, marked untranslated (M-3)');

// Mutations.
const RFC3339_RUST = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.(\d{3}|\d{6}|\d{9}))?\+00:00$/;
mkdirSync(join(appData, 'slack-threads/th-err/files'), { recursive: true });
const m = await run([
  ['rename-blank', 'slack_threads_rename', { id: 'th-v2', title: '   ' }],
  ['rename-long', 'slack_threads_rename', { id: 'th-v2', title: 'あ'.repeat(201) }],
  ['rename-missing', 'slack_threads_rename', { id: 'nope', title: 'x' }],
  ['rename', 'slack_threads_rename', { id: 'th-v2', title: '  Renamed by AI  ' }],
  ['rename-200', 'slack_threads_rename', { id: 'th-long', title: 'b'.repeat(200) }],
  ['set-project', 'slack_threads_set_project', { id: 'th-err' }],
  ['set-global', 'slack_threads_set_project', { id: 'lg-nourl', scope: 'global' }],
  ['set-missing', 'slack_threads_set_project', { id: 'nope' }],
  ['delete-traversal', 'slack_threads_delete', { ids: ['../keep-me', '..'] }],
  ['delete-web-id', 'slack_threads_delete', { id: 'wp-1' }],
  ['delete', 'slack_threads_delete', { id: 'th-v2' }],
  ['delete-bulk', 'slack_threads_delete', { ids: ['th-err', 'lg-a', 'nope'] }],
  ['delete-none', 'slack_threads_delete', {}],
]);
const M = m.out;
check(M['rename-blank'].isError && /required/.test(M['rename-blank'].text), 'rename: rejects an empty/blank title');
check(M['rename-long'].isError && /200/.test(M['rename-long'].text), 'rename: rejects > 200 characters');
check(M['rename-missing'].isError && /not found/.test(M['rename-missing'].text), 'rename: unknown id reported');
check(!M['rename-200'].isError, 'rename: accepts exactly 200 characters');
const thLong = sl('SELECT title, title_custom, updated_at FROM slack_threads WHERE id = ?', 'th-long')[0];
check(thLong.title === 'b'.repeat(200) && thLong.title_custom === 1 && thLong.updated_at !== T0 && RFC3339_RUST.test(thLong.updated_at),
  `rename: title + title_custom = 1 + updated_at bumped in chrono format (got ${thLong.updated_at})`);
check(M.rename.text === 'Renamed Slack thread th-v2 to "Renamed by AI".', 'rename: trimmed title reported');
check(!M['set-project'].isError && /linked to project proj-1/.test(M['set-project'].text), 'set_project: links to the current project');
check(sl('SELECT project_id FROM slack_threads WHERE id = ?', 'lg-nourl')[0].project_id === null && /global \(no project\)/.test(M['set-global'].text),
  'set_project: scope=global detaches');
check(M['set-missing'].isError, 'set_project: unknown id reported');

check(existsSync(join(appData, 'keep-me')) && existsSync(outsidePath) && /^Deleted 0\/2/.test(M['delete-traversal'].text), 'delete: traversal ids touch nothing on disk');
check(/^Deleted 0\/1/.test(M['delete-web-id'].text) && sl('SELECT COUNT(*) AS n FROM captures WHERE id = ?', 'wp-1')[0].n === 1, 'delete: a web id is not a Slack thread');
show(M, 'delete');
const countIn = (table, col, id) => sl(`SELECT COUNT(*) AS n FROM ${table} WHERE ${col} = ?`, id)[0].n;
check(/^Deleted 1\/1 Slack thread\(s\) \(1 file folder/.test(M.delete.text) && countIn('slack_threads', 'id', 'th-v2') === 0 &&
  countIn('slack_messages', 'thread_id', 'th-v2') === 0 && countIn('slack_files', 'thread_id', 'th-v2') === 0, 'delete: thread, messages and file rows removed');
check(!existsSync(join(appData, 'slack-threads/th-v2')), 'delete: <app_data>/slack-threads/<id> removed');
check(existsSync(join(appData, 'slack-cache/avatars/T1/U1-abcd1234.png')) && countIn('slack_users', 'team_id', 'T1') === 2, 'delete: avatar cache + slack_users kept');
check(/^Deleted 2\/3/.test(M['delete-bulk'].text) && !existsSync(join(appData, 'slack-threads/lg-a')) && !existsSync(join(appData, 'slack-threads/th-err')) &&
  countIn('slack_files', 'thread_id', 'lg-a') === 0, 'delete: bulk ids incl. legacy row + files dir');
check(existsSync(join(appData, 'slack-threads/lg-dup-new')) && countIn('slack_threads', 'id', 'th-long') === 1, 'delete: other threads untouched');
check(M['delete-none'].isError, 'delete: requires an id');

// ---- Web pages (behaviour unchanged) --------------------------------------------------

const wr = await run([
  ['w-list', 'web_pages_list', {}],
  ['w-list-all', 'web_pages_list', { scope: 'all' }],
  ['w-read', 'web_pages_read', { id: 'wp-1', max_chars: 300 }],
  ['w-read-next', 'web_pages_read', { id: 'wp-1', max_chars: 300, offset: 300 }],
  ['w-section', 'web_pages_read', { id: 'wp-1', section: 'configure', max_chars: 500 }],
  ['w-section-full', 'web_pages_read', { id: 'wp-1', section: 'Install' }],
  ['w-section-missing', 'web_pages_read', { id: 'wp-1', section: 'nope' }],
  ['w-read-sel', 'web_pages_read', { id: 'wp-2' }],
  ['w-search-site', 'web_pages_search', { query: 'tauri', scope: 'all' }],
  ['w-search-url', 'web_pages_search', { query: 'example.com', scope: 'all' }],
  ['w-rename', 'web_pages_rename', { id: 'wp-2', title: 'My excerpt' }],
  ['w-update', 'web_pages_update', { id: 'wp-3', title: '  Setext page v2  ' }],
  ['w-update-empty', 'web_pages_update', { id: 'wp-3' }],
  ['w-append', 'web_pages_append', { id: 'wp-2', text: 'note' }],
  ['w-set', 'web_pages_set_project', { id: 'wp-2' }],
  ['w-reorder', 'web_pages_reorder', { ids: ['wp-2'] }],
  ['w-reorder-bad', 'web_pages_reorder', { ids: ['lg-a'] }],
  ['w-att', 'web_pages_read_attachment', { id: 'att-web' }],
  ['w-att-png', 'web_pages_read_attachment', { id: 'att-png' }],
  ['w-att-big', 'web_pages_read_attachment', { id: 'att-big' }],
  ['w-att-latin1', 'web_pages_read_attachment', { id: 'att-latin1' }],
  ['w-att-evil', 'web_pages_read_attachment', { id: 'att-evil' }],
  ['w-att-slack', 'web_pages_read_attachment', { id: 'att-log' }],
  ['w-setext', 'web_pages_read', { id: 'wp-3' }],
  ['w-setext-section', 'web_pages_read', { id: 'wp-3', section: 'setup' }],
  ['w-setext-h1', 'web_pages_read', { id: 'wp-3', section: 'Getting started' }],
  ['w-del', 'web_pages_delete', { id: 'wp-1' }],
]);
const w = wr.out;
const wshow = (label) => show(w, label);

wshow('w-list-all');
check(w['w-list'].text.includes('[wp-1]') && !w['w-list'].text.includes('[wp-2]'), 'web list: scope=project only shows the current project');
check(w['w-list-all'].text.includes('Tauri') && /example\.com · excerpt/.test(w['w-list-all'].text) && !/\[(lg|th)-/.test(w['w-list-all'].text),
  'web list: site/domain + excerpt badge, no slack rows');
check(/Tauri · 1 attachment\(s\)/.test(w['w-list-all'].text), 'web list: attachment count');
wshow('w-read');
const total = longDoc.length;
check(w['w-read'].text.includes(`total_chars: ${total}`), 'web read: total_chars reported');
check(/## Outline[\s\S]*# Tauri capabilities \(offset 0\)[\s\S]*### Permissions/.test(w['w-read'].text) && !w['w-read'].text.includes('# not a heading ('),
  'web read: outline with offsets, code fences skipped');
check(w['w-read'].text.includes('source_url: https://v2.tauri.app/security/capabilities/') && w['w-read'].text.includes('author: Tauri team') &&
  w['w-read'].text.includes('captured_at: 2026-10-09T10:00:00Z') && w['w-read'].text.includes('published_at: 2026-09-01'), 'web read: web metadata shown');
check(w['w-read'].text.includes(`chars 0–300 of ${total}`) && w['w-read'].text.includes('Continue with offset=300.'), 'web read: max_chars slice + continue hint');
check(w['w-read'].text.includes(join(appData, webRel)), 'web read: attachment path is ABSOLUTE under web-pages/');
check(bodyOf(w['w-read'].text) + bodyOf(w['w-read-next'].text) === longDoc.slice(0, 600), 'web read: offset continues exactly where the previous slice ended');
wshow('w-section');
const confStart = longDoc.indexOf('## Configure');
const confEnd = longDoc.indexOf('## FAQ');
check(bodyOf(w['w-section'].text) === longDoc.slice(confStart, confStart + 500) && w['w-section'].text.includes('Continue with offset=500 section="Configure"'),
  'web read: section (case-insensitive) sliced + section-aware hint');
check(bodyOf(w['w-section-full'].text) === longDoc.slice(longDoc.indexOf('## Install'), confStart) && !w['w-section-full'].text.includes('truncated'),
  'web read: section stops at the next same-level heading');
check(longDoc.slice(confStart, confEnd).includes('### Permissions'), 'fixture: subsection belongs to its parent section');
check(w['w-section-missing'].isError && /section not found[\s\S]*"Install"/.test(w['w-section-missing'].text), 'web read: unknown section lists headings');
check(w['w-read-sel'].text.includes('selection: yes'), 'web read: selection flag shown');
check(w['w-search-site'].text.includes('[wp-1]') && w['w-search-url'].text.includes('[wp-2]'), 'web search: matches site_name and source_url');
const wp2 = db.prepare('SELECT * FROM captures WHERE id = ?').get('wp-2');
check(!w['w-rename'].isError && wp2.title === 'My excerpt' && wp2.title_custom === 1, 'web rename: title + title_custom = 1');
const wp3 = db.prepare('SELECT * FROM captures WHERE id = ?').get('wp-3');
check(!w['w-update'].isError && wp3.title === 'Setext page v2' && wp3.title_custom === 1 && wp3.content === setextDoc, 'web update: title trimmed + custom, content kept');
check(w['w-update-empty'].isError, 'web update: rejects a call with neither title nor content');
check(wp2.content === 'Only this paragraph.\n\nnote' && wp2.project_id === PROJECT && RFC3339_RUST.test(wp2.updated_at), 'web append + set_project + rfc3339 updated_at');
const pos = (id) => db.prepare('SELECT position FROM captures WHERE id = ?').get(id).position;
check(!w['w-reorder'].isError && pos('wp-2') === 0, 'web reorder: moved id goes to the top');
check(w['w-reorder-bad'].isError, 'web reorder: rejects an id that is not a web page');
check(w['w-att'].text.includes('<svg/>'), 'web read_attachment: inline text');
check(w['w-att-png'].text.includes('Not returned inline') && w['w-att-png'].text.includes(join(appData, 'web-pages/wp-2/files/shot.png')), 'web read_attachment: image → absolute path + hint');
check(/larger than/.test(w['w-att-big'].text), 'web read_attachment: >1MB → path + hint');
check(/not valid UTF-8/.test(w['w-att-latin1'].text), 'web read_attachment: non-utf8 octet-stream → path + hint');
check(w['w-att-evil'].isError && /escapes/.test(w['w-att-evil'].text), 'web read_attachment: refuses paths outside app_data');
check(w['w-att-slack'].isError, 'web read_attachment: carried-over Slack file ids are not web attachments');
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
check(/^Deleted 1\/1 web page/.test(w['w-del'].text) && !existsSync(join(appData, 'web-pages/wp-1')) &&
  db.prepare('SELECT COUNT(*) AS n FROM capture_attachments WHERE capture_id = ?').get('wp-1').n === 0, 'web delete: row, attachment rows and web-pages/<id> dir removed');

const stderrAll = [head, s, s2, m, wr].map((r) => r.stderr || '').join('');
check(![s, s2, m, wr].some((r) => r.noise.length), 'stdout carries only JSON-RPC frames (all runs)');

db.close();
rmSync(appData, { recursive: true, force: true });
rmSync(outsidePath, { force: true });

if (failures.length) {
  process.stderr.write(`\n${failures.length} check(s) failed.\n`);
  if (stderrAll.trim()) process.stderr.write(`server stderr:\n${stderrAll}\n`);
  process.exit(1);
}
process.stdout.write(`\nAll checks passed (migration 0041 from: ${migration0041Source}).\n`);
