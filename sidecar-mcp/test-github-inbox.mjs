#!/usr/bin/env node
// PR Inbox / GitHub Mentions tools of the built-in `devdy` MCP server
// (pr_inbox_list, pr_review_history, mentions_list, mention_read): the lists
// come from the broker (a fake one here, answering like
// runs/broker/github_inbox.rs), past reviews from review_pr runs.
//
// Builds a throwaway DB from every real migration, then drives `index.mjs`
// over stdio. No network.
//
// Usage: node sidecar-mcp/test-github-inbox.mjs

import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import net from 'node:net';

const HERE = dirname(fileURLToPath(import.meta.url));
const SERVER = join(HERE, 'index.mjs');
const MIGRATIONS = join(HERE, '..', 'src-tauri', 'migrations');
const T0 = '2026-01-01T00:00:00Z';
const HOUR = 3_600_000;
const iso = (msAgo) => new Date(Date.now() - msAgo).toISOString();

const failures = [];
const check = (ok, label, detail = '') => {
  if (!ok) failures.push(label);
  process.stdout.write(`${ok ? '  ok  ' : '  FAIL'} ${label}\n`);
  if (!ok && detail) process.stdout.write(`${detail}\n`);
};

// ---- Fixture -------------------------------------------------------------------

const appData = mkdtempSync(join(tmpdir(), 'devdy-gh-inbox-'));
const projectPath = join(appData, 'alpha');
mkdirSync(join(projectPath, '.devdy', 'runs'), { recursive: true });
const dbPath = join(appData, 'data.db');
const db = new DatabaseSync(dbPath);
for (const f of readdirSync(MIGRATIONS).filter((f) => /^\d{4}_.*\.sql$/.test(f)).sort()) {
  db.exec(readFileSync(join(MIGRATIONS, f), 'utf8'));
}
db.prepare('INSERT INTO projects (id, name, path, created_at) VALUES (?, ?, ?, ?)').run('p1', 'Alpha', projectPath, T0);
db.prepare("INSERT INTO repos (id, project_id, name, path, github_owner, github_repo, created_at) VALUES ('r1', 'p1', 'web', ?, 'Acme', 'Web', ?)")
  .run(projectPath, T0);
const addReview = (id, createdAt, status, conclusion) => {
  db.prepare(
    "INSERT INTO runs (id, project_id, repo_id, type, ref_number, status, engine, created_at, finished_at) VALUES (?, 'p1', 'r1', 'review_pr', 12, ?, 'claude', ?, ?)",
  ).run(id, status, createdAt, status === 'done' ? createdAt : null);
  if (conclusion) {
    const log = [
      { type: 'user', message: { role: 'user', content: 'Review this PR' } },
      { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'Reading the diff…' }] } },
      { type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: conclusion }] } },
    ];
    writeFileSync(join(projectPath, '.devdy', 'runs', `${id}.log`), log.map((l) => JSON.stringify(l)).join('\n'));
  }
};
addReview('rev-old', '2026-01-02T00:00:00Z', 'done', 'Old verdict: request changes');
addReview('rev-new', '2026-01-05T00:00:00Z', 'done', `New verdict: LGTM. ${'x'.repeat(400)}`);

const pr = (extra) => ({
  account_id: 'a1', account_label: 'work', owner: 'acme', repo: 'web', title: 'PR', html_url: 'https://github.com/acme/web/pull/0',
  author_login: 'bob', author_avatar_url: null, author_is_bot: false, draft: false, labels: [], updated_at: iso(HOUR),
  mapped: true, project_id: 'p1', repo_id: 'r1', project_name: 'Alpha', existing_run_id: null, existing_run_status: null,
  reviewer_accounts: [{ account_id: 'a1', account_label: 'work' }], requested_reviewers: [], updated_since_review: false,
  created_at: null, additions: null, deletions: null, changed_files: null, review_decision: null, ci_state: null,
  review_requested_at: iso(2 * 86_400_000), reviewers_known: true, ...extra,
});
const PRS = {
  items: [
    pr({ number: 12, title: 'Checkout flow', additions: 120, deletions: 30, changed_files: 5, ci_state: 'FAILURE', existing_run_id: 'rev-new', existing_run_status: 'done', updated_since_review: true,
      requested_reviewers: [{ login_or_slug: 'carol', kind: 'user' }, { login_or_slug: 'acme/qa', kind: 'team' }] }),
    pr({ number: 13, title: 'Bump deps', author_login: 'dependabot[bot]', author_is_bot: true, labels: ['deps'] }),
    pr({ number: 14, title: 'WIP search', draft: true, existing_run_id: 'rev-x', existing_run_status: 'done' }),
    pr({ owner: 'other', repo: 'api', number: 3, title: 'Unmapped API change', html_url: 'https://github.com/other/api/pull/3', mapped: false, project_id: null, project_name: null }),
  ],
  errors: [{ account_id: 'a2', account_label: 'oss', message: 'Bad credentials' }],
  fetched_at: iso(60_000),
};
const mention = (extra) => ({
  thread_id: 't', account_id: 'a1', account_label: 'work', account_login: 'me', reason: 'mention', unread: true, kind: 'PullRequest',
  owner: 'acme', repo: 'web', number: 12, title: 'Checkout flow', html_url: 'https://github.com/acme/web/pull/12', updated_at: iso(HOUR),
  newer_comments: 0, answered: false, answered_at: null, project_id: 'p1', repo_id: 'r1', project_name: 'Alpha', existing_run_id: null,
  existing_run_status: null, state: 'open', reacted: false, user_is_author: false, audience: 1, cc: false, later: [], checked_at: '',
  category: 'reply', category_source: 'ai', category_reason: null, summary: null, triage_pending: false, ...extra,
});
const MENTIONS = {
  items: [
    mention({
      key: 'a1:100', summary: 'Asks whether the retry belongs in the client', newer_comments: 2,
      trigger: { id: 1, author_login: 'carol', author_avatar_url: '', body: `@me should the retry live in the client?\n${'y'.repeat(300)}`,
        html_url: 'https://github.com/acme/web/pull/12#discussion_r1', created_at: iso(3 * HOUR), review_comment: true, path: 'src/pay.ts', source: 'review_comment' },
      later: [{ author_login: 'dave', body: '+1, curious too' }],
    }),
    mention({
      key: 'a1:101', kind: 'Issue', number: 40, title: 'Release notes', category: 'done', category_source: 'rule', category_reason: 'answered',
      answered: true, answered_at: iso(HOUR), html_url: 'https://github.com/acme/web/issues/40',
      trigger: { id: 2, author_login: 'erin', author_avatar_url: '', body: '@me please check the notes', html_url: 'https://github.com/acme/web/issues/40#c2',
        created_at: iso(5 * HOUR), review_comment: false, path: null, source: 'comment' },
    }),
    mention({
      key: 'a1:102', owner: 'other', repo: 'api', number: 7, title: 'API docs', category: 'fyi', cc: true, project_id: null, project_name: null,
      trigger: { id: 3, author_login: 'frank', author_avatar_url: '', body: 'cc @me', html_url: 'https://github.com/other/api/issues/7#c3',
        created_at: iso(HOUR), review_comment: false, path: null, source: 'comment' },
    }),
  ],
  errors: [],
  fetched_at: iso(120_000),
};

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
    const data = req.tool === 'github_prs' ? PRS : MENTIONS;
    // A forced refresh fails: the cached list comes back with the error.
    const reason = req.argv.includes('refresh') ? 'Timed out fetching review requests from GitHub' : undefined;
    c.end(`${JSON.stringify({ decision: 'allow', data, ...(reason ? { reason } : {}) })}\n`);
  });
});
await new Promise((r) => broker.listen(sock, r));

// ---- Drive the server -------------------------------------------------------------

function drive(calls, env = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [SERVER], {
      env: { ...process.env, DEVDY_DB_PATH: dbPath, DEVDY_PROJECT_ID: 'p1', DEVDY_BROKER_SOCK: sock, DEVDY_RUN_ID: 'r1', ...env },
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
      resolve(Object.fromEntries(calls.map(([label], i) => {
        const r = byId.get(i + 1)?.result;
        return [label, { text: r?.content?.[0]?.text ?? '', isError: !!r?.isError }];
      })));
    });
    calls.forEach(([, name, args], i) =>
      child.stdin.write(`${JSON.stringify({ jsonrpc: '2.0', id: i + 1, method: 'tools/call', params: { name, arguments: args } })}\n`),
    );
    child.stdin.end();
  });
}

let out = await drive([
  ['all', 'pr_inbox_list', {}],
  ['todo', 'pr_inbox_list', { status: ['unreviewed', 'updated'], include_drafts: false, include_bots: false }],
  ['repo', 'pr_inbox_list', { repo: 'other/api' }],
  ['refresh', 'pr_inbox_list', { refresh: true, limit: 1 }],
  ['history', 'pr_review_history', { pr: 'https://github.com/acme/web/pull/12', max_chars: 200 }],
  ['history-short', 'pr_review_history', { pr: 'ACME/web#12', limit: 1 }],
  ['history-none', 'pr_review_history', { pr: 'acme/web#99' }],
  ['history-bad', 'pr_review_history', { pr: 'not a pr' }],
  ['mentions', 'mentions_list', {}],
  ['reply', 'mentions_list', { category: ['reply'], unanswered_only: true }],
  ['m-query', 'mentions_list', { query: 'notes' }],
  ['read', 'mention_read', { key: 'a1:100' }],
  ['read-missing', 'mention_read', { key: 'a1:999' }],
]);

const all = out.all.text;
check(/PRs waiting for your review \(fetched 1m ago\): 4 of 4 shown/.test(all), 'pr list: header with count and age', all);
check(all.includes('! oss: Bad credentials'), 'pr list: account error surfaced', all);
check(
  all.includes('- acme/web#12 Checkout flow') && all.includes('+120/-30 in 5 file(s) · CI FAILURE') &&
    all.includes('also waiting on: @carol, team acme/qa') && all.includes('new commits since the last review (run rev-new, done)'),
  'pr list: size, CI, other reviewers, review status',
  all,
);
check(all.includes('requested 2d ago') && all.includes('as work'), 'pr list: requested age and account', all);
check(all.includes('Devdy review: none · repo not in any Devdy project'), 'pr list: unmapped repo flagged', all);
const todo = out.todo.text;
check(todo.includes('#12') && !todo.includes('#13') && !todo.includes('#14') && todo.includes('#3 '), 'pr list: status + draft + bot filters', todo);
check(out.repo.text.includes('other/api#3') && !out.repo.text.includes('acme/web'), 'pr list: repo filter', out.repo.text);
check(
  out.refresh.text.includes('! Refresh failed, showing the cached list: Timed out') && out.refresh.text.includes('… 3 more'),
  'pr list: failed refresh keeps cache, limit',
  out.refresh.text,
);
check(brokerReqs.some((r) => r.tool === 'github_prs' && r.argv[0] === 'refresh' && r.project_id === 'p1'), 'broker: refresh flag and project sent');

const h = out.history.text;
check(h.includes('# Devdy reviews of acme/web#12 (2)') && h.indexOf('rev-new') < h.indexOf('rev-old'), 'history: newest first', h);
check(h.includes('New verdict: LGTM') && !h.includes('Reading the diff') && h.includes('[truncated'), 'history: last reply, capped', h);
check(out['history-short'].text.includes('rev-new') && !out['history-short'].text.includes('rev-old'), 'history: case-insensitive ref, limit', out['history-short'].text);
check(out['history-none'].text.startsWith('No Devdy review of acme/web#99'), 'history: none', out['history-none'].text);
check(out['history-bad'].isError, 'history: bad ref is an error', out['history-bad'].text);

const m = out.mentions.text;
check(/GitHub mentions \(fetched 2m ago\): 3 of 3 shown\nAll: reply 1 · fyi 1 · done 1/.test(m), 'mentions: header and counts', m);
check(
  m.includes('- [reply] acme/web#12 (PullRequest, open) Checkout flow') && m.includes('@carol 3h ago on src/pay.ts') &&
    m.includes('AI: Asks whether the retry belongs in the client') && m.includes('key a1:100'),
  'mentions: line',
  m,
);
check(m.includes('cc only'), 'mentions: cc flagged', m);
check(out.reply.text.includes('a1:100') && !out.reply.text.includes('a1:101') && !out.reply.text.includes('a1:102'), 'mentions: category + unanswered filter', out.reply.text);
check(out['m-query'].text.includes('a1:101') && !out['m-query'].text.includes('a1:100'), 'mentions: query', out['m-query'].text);

const r = out.read.text;
check(
  r.includes('# acme/web#12 Checkout flow') && r.includes('Review comment on src/pay.ts') && r.includes('y'.repeat(300)) &&
    r.includes('- @dave: +1, curious too') && r.includes('Category: reply (AI triage)') && r.includes('pulls/12/comments'),
  'mention_read: full detail',
  r,
);
check(out['read-missing'].text.startsWith('Mention not found: a1:999'), 'mention_read: missing key', out['read-missing'].text);

// Not a live run: the broker's deny reaches the agent as an error.
out = await drive([['denied', 'pr_inbox_list', {}]], { DEVDY_RUN_ID: 'r9' });
check(out.denied.isError && out.denied.text.includes('no live run'), 'deny surfaces as tool error', out.denied.text);
// No broker at all.
out = await drive([['nobroker', 'mentions_list', {}]], { DEVDY_BROKER_SOCK: '' });
check(out.nobroker.isError && out.nobroker.text.includes('broker not configured'), 'no broker: tool error', out.nobroker.text);

broker.close();
rmSync(appData, { recursive: true, force: true });
if (failures.length) {
  process.stdout.write(`\n${failures.length} failure(s)\n`);
  process.exit(1);
}
process.stdout.write('\nall passed\n');
