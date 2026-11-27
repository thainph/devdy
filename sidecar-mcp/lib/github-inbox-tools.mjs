// Tool definitions for the PR Inbox and GitHub Mentions (read-only).
//
// Both lists live in the app's memory and are fetched with the user's GitHub
// accounts, so they come through the broker (runs/broker/github_inbox.rs) — the
// same lists the app's windows show, never a token. Past PR reviews are the
// `review_pr` runs in SQLite. Acting on what is found (reviewing, replying) is
// left to the agent's own tools (`gh`, sessions).

import { askBroker } from './broker.mjs';
import { db } from './store.mjs';
import { readRunTranscript } from './transcript.mjs';

const BROKER_TIMEOUT_MS = 150_000;
const REVIEW_STATUSES = ['unreviewed', 'updated', 'reviewed'];
const CATEGORIES = ['reply', 'fyi', 'done'];

const REFRESH = {
  refresh: {
    type: 'boolean',
    description: 'Refetch from GitHub now. A list older than 5 minutes is refetched anyway.',
  },
};

async function loadList(tool, refresh) {
  const resp = await askBroker(
    {
      project_id: process.env.DEVDY_PROJECT_ID,
      run_id: process.env.DEVDY_RUN_ID || null,
      tool,
      argv: refresh ? ['refresh'] : [],
      host: null,
    },
    { timeoutMs: BROKER_TIMEOUT_MS },
  );
  if (!resp || resp.decision !== 'allow' || !resp.data) throw new Error((resp && resp.reason) || 'request denied');
  // `reason` on an allow: the refetch failed and the cached list is returned.
  return { list: resp.data, warning: resp.reason || null };
}

export const ago = (iso, now = Date.now()) => {
  const t = Date.parse(iso || '');
  if (!Number.isFinite(t)) return '?';
  const s = Math.max(0, Math.round((now - t) / 1000));
  if (s < 60) return 'just now';
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
};

const intIn = (v, def, min, max) => {
  const n = Number.parseInt(v, 10);
  return Number.isFinite(n) ? Math.min(max, Math.max(min, n)) : def;
};

const oneLine = (s, max) => {
  const t = String(s || '').replace(/\s+/g, ' ').trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
};

const lower = (s) => String(s ?? '').trim().toLowerCase();

/** `owner/repo` matches `repo` given as `owner/repo` or a bare repo name. */
const repoMatches = (owner, repo, want) => {
  const w = lower(want);
  if (!w) return true;
  const full = `${owner}/${repo}`.toLowerCase();
  return w.includes('/') ? full === w : repo.toLowerCase() === w;
};

const projectMatches = (item, want) => {
  const w = lower(want);
  return !w || lower(item.project_id) === w || lower(item.project_name) === w;
};

const accountErrors = (list) =>
  (list.errors || []).map((e) => `! ${e.account_label}: ${e.message}`);

const listHeader = (title, list, count, total, warning, now) =>
  [
    `${title} (fetched ${ago(list.fetched_at, now)}): ${count} of ${total} shown`,
    ...(warning ? [`! Refresh failed, showing the cached list: ${warning}`] : []),
    ...accountErrors(list),
  ].join('\n');

// ---- PR Inbox ------------------------------------------------------------------

/** unreviewed (no Devdy review) | updated (new commits since it) | reviewed. */
export function reviewStatus(pr) {
  if (!pr.existing_run_id) return 'unreviewed';
  return pr.updated_since_review ? 'updated' : 'reviewed';
}

export function filterPrs(items, a = {}) {
  const statuses = Array.isArray(a.status) ? a.status.filter((s) => REVIEW_STATUSES.includes(s)) : [];
  const author = lower(a.author).replace(/^@/, '');
  const q = lower(a.query);
  return items.filter(
    (pr) =>
      (a.include_drafts !== false || !pr.draft) &&
      (a.include_bots !== false || !pr.author_is_bot) &&
      (!statuses.length || statuses.includes(reviewStatus(pr))) &&
      repoMatches(pr.owner, pr.repo, a.repo) &&
      projectMatches(pr, a.project) &&
      (!author || lower(pr.author_login) === author) &&
      (!q || `${pr.owner}/${pr.repo}#${pr.number} ${pr.title} ${(pr.labels || []).join(' ')}`.toLowerCase().includes(q)),
  );
}

export function prLine(pr, now = Date.now()) {
  const facts = [`@${pr.author_login}${pr.author_is_bot ? ' (bot)' : ''}`];
  if (pr.draft) facts.push('draft');
  if (pr.additions != null) {
    facts.push(`+${pr.additions}/-${pr.deletions ?? 0}${pr.changed_files != null ? ` in ${pr.changed_files} file(s)` : ''}`);
  }
  if (pr.ci_state) facts.push(`CI ${pr.ci_state}`);
  if (pr.review_decision) facts.push(pr.review_decision);
  facts.push(`requested ${ago(pr.review_requested_at || pr.updated_at, now)}`);
  if (pr.labels?.length) facts.push(`labels: ${pr.labels.join(', ')}`);
  facts.push(`as ${(pr.reviewer_accounts?.length ? pr.reviewer_accounts : [pr]).map((r) => r.account_label).join(', ')}`);
  const others = (pr.requested_reviewers || []).map((r) => (r.kind === 'team' ? `team ${r.login_or_slug}` : `@${r.login_or_slug}`));
  if (others.length) facts.push(`also waiting on: ${others.join(', ')}`);

  const status = reviewStatus(pr);
  const review =
    status === 'unreviewed'
      ? 'Devdy review: none'
      : `Devdy review: ${status === 'updated' ? 'new commits since the last review' : 'done'} (run ${pr.existing_run_id}, ${pr.existing_run_status || '?'})`;
  const project = pr.mapped ? `project ${pr.project_name}` : 'repo not in any Devdy project';
  return `- ${pr.owner}/${pr.repo}#${pr.number} ${pr.title}\n  ${facts.join(' · ')}\n  ${review} · ${project}\n  ${pr.html_url}`;
}

/** `https://github.com/o/r/pull/12`, `o/r#12` or `o/r/pull/12` → {owner, repo, number}. */
export function parsePrRef(ref) {
  const s = String(ref || '').trim();
  const m =
    s.match(/github\.com\/([^/\s]+)\/([^/\s]+)\/pull\/(\d+)/i) ||
    s.match(/^([^/\s#]+)\/([^/\s#]+)(?:#|\/pulls?\/)(\d+)$/i);
  if (!m) throw new Error('pr must be a GitHub PR URL or owner/repo#number');
  return { owner: m[1], repo: m[2], number: Number(m[3]) };
}

function reviewRuns({ owner, repo, number }, limit) {
  return db()
    .prepare(
      `SELECT r.id, r.status, r.engine, r.created_at, r.finished_at, r.session_id, r.transcript_path,
              p.name AS project_name, p.path AS project_path
       FROM runs r JOIN repos rp ON rp.id = r.repo_id JOIN projects p ON p.id = r.project_id
       WHERE r.type = 'review_pr' AND r.ref_number = ?
         AND lower(rp.github_owner) = ? AND lower(rp.github_repo) = ?
       ORDER BY r.created_at DESC LIMIT ?`,
    )
    .all(number, owner.toLowerCase(), repo.toLowerCase(), limit);
}

/** The run's last assistant message: the latest review's conclusion. */
export function lastReply(run, maxChars) {
  const t = readRunTranscript(run, run.project_path);
  const msg = t && [...t.messages].reverse().find((m) => m.role === 'assistant');
  if (!msg) return null;
  return msg.text.length > maxChars ? `${msg.text.slice(0, maxChars)}\n… [truncated — session_read for the rest]` : msg.text;
}

// ---- Mentions ------------------------------------------------------------------

export function filterMentions(items, a = {}) {
  const cats = Array.isArray(a.category) ? a.category.filter((c) => CATEGORIES.includes(c)) : [];
  const q = lower(a.query);
  return items.filter(
    (m) =>
      (!cats.length || cats.includes(m.category)) &&
      (!a.unanswered_only || !m.answered) &&
      repoMatches(m.owner, m.repo, a.repo) &&
      projectMatches(m, a.project) &&
      (!q ||
        `${m.owner}/${m.repo}#${m.number ?? ''} ${m.title} ${m.trigger?.author_login ?? ''} ${m.trigger?.body ?? ''}`
          .toLowerCase()
          .includes(q)),
  );
}

const mentionRef = (m) => `${m.owner}/${m.repo}${m.number != null ? `#${m.number}` : ''}`;

export function mentionLine(m, now = Date.now()) {
  const t = m.trigger;
  const by = t ? `@${t.author_login} ${ago(t.created_at, now)}${t.path ? ` on ${t.path}` : ''}` : `updated ${ago(m.updated_at, now)}`;
  const facts = [m.answered ? `answered ${ago(m.answered_at, now)}` : 'not answered'];
  if (m.newer_comments) facts.push(`${m.newer_comments} newer comment(s)`);
  if (m.cc) facts.push('cc only');
  if (m.user_is_author) facts.push('you opened it');
  if (m.project_name) facts.push(`project ${m.project_name}`);
  const lines = [
    `- [${m.category || '?'}] ${mentionRef(m)} (${m.kind}, ${m.state}) ${m.title}`,
    `  ${by}: "${oneLine(t?.body, 200)}"`,
    `  ${facts.join(' · ')}`,
  ];
  if (m.summary) lines.push(`  AI: ${m.summary}`);
  lines.push(`  key ${m.key} · ${t?.html_url || m.html_url}`);
  return lines.join('\n');
}

const CATEGORY_REASON = {
  answered: 'you answered after the mention',
  merged: 'the PR is merged',
  closed: 'it is closed',
  reacted: 'you reacted to the comment',
  cc: 'you are only cc-ed',
  crowd: 'many people are tagged',
};

export function mentionDetail(m, now = Date.now()) {
  const t = m.trigger;
  const why = [m.category_source === 'user' ? 'set by you' : m.category_source === 'ai' ? 'AI triage' : m.category_source];
  if (m.category_reason) why.push(CATEGORY_REASON[m.category_reason] || m.category_reason);
  const out = [
    `# ${mentionRef(m)} ${m.title}`,
    `${m.kind} · ${m.state} · ${m.html_url}`,
    `Account: ${m.account_label} (@${m.account_login})${m.project_name ? ` · project ${m.project_name}` : ''}`,
    `Category: ${m.category || '?'} (${why.filter(Boolean).join(', ')})${m.triage_pending ? ' · AI triage running' : ''}`,
    m.summary ? `AI: ${m.summary}` : null,
    `Status: ${m.answered ? `answered ${ago(m.answered_at, now)}` : 'not answered'}${m.reacted ? ' · you reacted' : ''}` +
      `${m.user_is_author ? ' · you opened it' : ''}${m.cc ? ' · cc only' : ''}${m.audience > 1 ? ` · ${m.audience} handles tagged` : ''}`,
  ];
  if (t) {
    out.push(
      '',
      `## Mention by @${t.author_login} ${ago(t.created_at, now)} (${t.created_at})`,
      `${t.review_comment ? `Review comment on ${t.path || '?'}` : t.source || 'comment'} · ${t.html_url}`,
      '',
      t.body,
    );
  }
  if (m.later?.length) {
    out.push('', `## Later comments (${m.newer_comments} by others since)`);
    for (const c of m.later) out.push(`- @${c.author_login}: ${oneLine(c.body, 300)}`);
  }
  if (m.existing_run_id) out.push('', `"Check with AI" session: run ${m.existing_run_id} (${m.existing_run_status || '?'})`);
  if (m.number != null) {
    out.push(
      '',
      `Comment bodies above are cut to 600 chars. Full thread: gh api repos/${m.owner}/${m.repo}/issues/${m.number}/comments` +
        (m.kind === 'PullRequest' ? ` and gh api repos/${m.owner}/${m.repo}/pulls/${m.number}/comments` : ''),
    );
  }
  return out.filter((l) => l !== null).join('\n');
}

// ---- Tools ---------------------------------------------------------------------

export function githubInboxTools() {
  return {
    pr_inbox_list: {
      description:
        'PR Inbox: open GitHub pull requests waiting for the user\'s review, across all their linked GitHub accounts — the same list as ' +
        'the app\'s PR Inbox. Each PR shows author, size, CI, review decision, when review was requested, other pending reviewers and the ' +
        'Devdy review status: unreviewed (no Devdy review yet), updated (new commits since the last review) or reviewed. ' +
        'Use pr_review_history for what earlier reviews concluded. Read-only.',
      inputSchema: {
        type: 'object',
        properties: {
          status: {
            type: 'array',
            items: { type: 'string', enum: REVIEW_STATUSES },
            description: 'Only these Devdy review statuses (default: all)',
          },
          repo: { type: 'string', description: 'owner/repo or repo name' },
          author: { type: 'string', description: 'PR author login' },
          project: { type: 'string', description: 'Devdy project id or name tracking the repo' },
          query: { type: 'string', description: 'Match in owner/repo#number, title or labels' },
          include_drafts: { type: 'boolean', description: 'Include draft PRs (default true)' },
          include_bots: { type: 'boolean', description: 'Include bot-authored PRs (default true)' },
          limit: { type: 'integer', minimum: 1, maximum: 200, description: 'Max PRs (default 50)' },
          ...REFRESH,
        },
      },
      handler: async (a) => {
        const now = Date.now();
        const { list, warning } = await loadList('github_prs', !!a.refresh);
        const items = filterPrs(list.items || [], a);
        const limit = intIn(a.limit, 50, 1, 200);
        const head = listHeader('PRs waiting for your review', list, Math.min(items.length, limit), (list.items || []).length, warning, now);
        const body = items.slice(0, limit).map((pr) => prLine(pr, now)).join('\n');
        const more = items.length > limit ? `\n\n… ${items.length - limit} more — narrow the filters or raise limit.` : '';
        return `${head}\n\n${body || 'No matching PRs.'}${more}`;
      },
    },

    pr_review_history: {
      description:
        'Past Devdy reviews of one pull request (review_pr runs, newest first): status, when, and the conclusion of each — the run\'s last ' +
        'assistant message. Use session_read with a run id for the whole review. Read-only.',
      inputSchema: {
        type: 'object',
        properties: {
          pr: { type: 'string', description: 'GitHub PR URL or owner/repo#number' },
          limit: { type: 'integer', minimum: 1, maximum: 20, description: 'Max reviews (default 5)' },
          max_chars: { type: 'integer', minimum: 200, maximum: 20000, description: 'Conclusion cap per review (default 2000)' },
        },
        required: ['pr'],
      },
      handler: (a) => {
        const ref = parsePrRef(a.pr);
        const runs = reviewRuns(ref, intIn(a.limit, 5, 1, 20));
        const name = `${ref.owner}/${ref.repo}#${ref.number}`;
        if (!runs.length) return `No Devdy review of ${name} yet (or its repo is not in a Devdy project).`;
        const maxChars = intIn(a.max_chars, 2000, 200, 20000);
        const parts = runs.map((r) => {
          const when = r.finished_at ? `finished ${r.finished_at}` : `started ${r.created_at}`;
          const reply = lastReply(r, maxChars);
          return `## run ${r.id} — ${r.status} · ${r.engine} · ${when} · project ${r.project_name}\n\n${reply || '(no review output yet)'}`;
        });
        return `# Devdy reviews of ${name} (${runs.length})\n\n${parts.join('\n\n')}`;
      },
    },

    mentions_list: {
      description:
        'GitHub Mentions: recent comments that @-mention the user directly (issues, PRs, discussions) across all their linked GitHub ' +
        'accounts — the same list as the app\'s Mentions window. Each is sorted reply (waiting for the user\'s answer) / fyi / done, with ' +
        'who asked, an excerpt, whether the user answered, and the AI\'s one-line summary. Use mention_read with a key for details. Read-only.',
      inputSchema: {
        type: 'object',
        properties: {
          category: {
            type: 'array',
            items: { type: 'string', enum: CATEGORIES },
            description: 'Only these categories (default: all)',
          },
          unanswered_only: { type: 'boolean', description: 'Only mentions the user has not answered yet' },
          repo: { type: 'string', description: 'owner/repo or repo name' },
          project: { type: 'string', description: 'Devdy project id or name tracking the repo' },
          query: { type: 'string', description: 'Match in owner/repo#number, title, mention author or text' },
          limit: { type: 'integer', minimum: 1, maximum: 200, description: 'Max mentions (default 50)' },
          ...REFRESH,
        },
      },
      handler: async (a) => {
        const now = Date.now();
        const { list, warning } = await loadList('github_mentions', !!a.refresh);
        const items = filterMentions(list.items || [], a);
        const limit = intIn(a.limit, 50, 1, 200);
        const counts = CATEGORIES.map((c) => `${c} ${(list.items || []).filter((m) => m.category === c).length}`).join(' · ');
        const head = `${listHeader('GitHub mentions', list, Math.min(items.length, limit), (list.items || []).length, warning, now)}\nAll: ${counts}`;
        const body = items.slice(0, limit).map((m) => mentionLine(m, now)).join('\n');
        const more = items.length > limit ? `\n\n… ${items.length - limit} more — narrow the filters or raise limit.` : '';
        return `${head}\n\n${body || 'No matching mentions.'}${more}`;
      },
    },

    mention_read: {
      description:
        'One GitHub mention in full: the mentioning comment (and file for a review comment), the later comments, why it is sorted ' +
        'reply / fyi / done, and the "Check with AI" session if one was started. Read-only.',
      inputSchema: {
        type: 'object',
        properties: { key: { type: 'string', description: 'Mention key from mentions_list' } },
        required: ['key'],
      },
      handler: async (a) => {
        const key = String(a.key || '').trim();
        if (!key) throw new Error('key is required');
        const { list } = await loadList('github_mentions', false);
        const m = (list.items || []).find((i) => i.key === key);
        if (!m) return `Mention not found: ${key} (no longer listed — run mentions_list again).`;
        return mentionDetail(m);
      },
    },
  };
}
