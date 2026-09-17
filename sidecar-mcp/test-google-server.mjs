#!/usr/bin/env node
// Offline contract test for the built-in `google` MCP server.
//
//   node sidecar-mcp/test-google-server.mjs
//
// Needs no Google account: it only drives the JSON-RPC handshake and inspects
// the advertised tool surface. What it guards is exactly what breaks silently
// when the three product groups share one server:
//   - a name collision between groups (Drive and Gmail both define `search` and
//     `delete`, so an unprefixed merge would drop one of them without erroring),
//   - a tool that lost its `account` argument and so can't be pointed at a
//     specific connected account,
//   - anything printed to stdout that isn't a JSON-RPC frame, which corrupts the
//     protocol stream for the host.

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const SERVER = join(dirname(fileURLToPath(import.meta.url)), 'google.mjs');
const GROUPS = { drive_: 15, mail_: 15, cal_: 9 };

const failures = [];
const check = (ok, label) => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
};

/** Send the frames, collect stdout, resolve with parsed frames + raw noise. */
function drive(frames) {
  return new Promise((resolve, reject) => {
    // Empty GOOGLE_ACCOUNTS: the server must still advertise its tools; only the
    // handlers need credentials.
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, GOOGLE_ACCOUNTS: '[]', GOOGLE_CLIENT_ID: '', GOOGLE_CLIENT_SECRET: '' },
      stdio: ['pipe', 'pipe', 'pipe'],
    });
    let out = '';
    let err = '';
    child.stdout.on('data', (d) => (out += d));
    child.stderr.on('data', (d) => (err += d));
    child.on('error', reject);
    child.on('close', () => {
      const lines = out.split('\n').filter((l) => l.trim());
      const parsed = [];
      const noise = [];
      for (const line of lines) {
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

const { parsed, noise, stderr } = await drive([
  { jsonrpc: '2.0', id: 1, method: 'initialize', params: {} },
  { jsonrpc: '2.0', id: 2, method: 'tools/list' },
]);

const init = parsed.find((m) => m.id === 1);
const list = parsed.find((m) => m.id === 2);

check(noise.length === 0, `stdout carries only JSON-RPC frames${noise.length ? ` (saw: ${noise[0].slice(0, 60)})` : ''}`);
check(!!init && init.result.serverInfo.name === 'google', 'initialize reports serverInfo.name = "google"');

const tools = (list && list.result && list.result.tools) || [];
const names = tools.map((t) => t.name);
const expected = Object.values(GROUPS).reduce((a, b) => a + b, 0) + 1; // + list_accounts
check(names.length === expected, `advertises ${expected} tools (got ${names.length})`);

const dupes = names.filter((n, i) => names.indexOf(n) !== i);
check(dupes.length === 0, `no duplicate tool names${dupes.length ? ` (got ${[...new Set(dupes)].join(', ')})` : ''}`);

for (const [prefix, count] of Object.entries(GROUPS)) {
  const got = names.filter((n) => n.startsWith(prefix)).length;
  check(got === count, `group ${prefix}* has ${count} tools (got ${got})`);
}

check(names.includes('list_accounts'), 'list_accounts is present (shared across groups)');

const missingAccount = tools.filter(
  (t) => t.name !== 'list_accounts' && !(t.inputSchema && t.inputSchema.properties && t.inputSchema.properties.account),
);
check(missingAccount.length === 0, `every tool takes an \`account\` argument${missingAccount.length ? ` (missing on ${missingAccount.map((t) => t.name).join(', ')})` : ''}`);

const undescribed = tools.filter((t) => !t.description || t.description.length < 20);
check(undescribed.length === 0, `every tool has a usable description${undescribed.length ? ` (thin: ${undescribed.map((t) => t.name).join(', ')})` : ''}`);

// The whole point of the merge is that these stayed distinguishable.
for (const n of ['drive_search', 'mail_search', 'drive_delete', 'mail_delete', 'cal_delete_event']) {
  check(names.includes(n), `collision-prone tool kept its own name: ${n}`);
}

// Calendar writes must default to NOT emailing attendees (see calendar.mjs).
for (const n of ['cal_create_event', 'cal_update_event', 'cal_delete_event', 'cal_respond_event']) {
  const t = tools.find((x) => x.name === n);
  const notify = t && t.inputSchema.properties.notify;
  check(!!notify && /defaults to false/i.test(notify.description || ''), `${n} documents notify=false as the default`);
}

if (failures.length) {
  process.stderr.write(`\n${failures.length} check(s) failed.\n`);
  if (stderr.trim()) process.stderr.write(`server stderr:\n${stderr}\n`);
  process.exit(1);
}
process.stdout.write(`\nAll checks passed (${names.length} tools).\n`);
