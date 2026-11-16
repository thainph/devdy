// Tool definitions for Slack threads (docs/srs-slack-thread-translate.md §14).
//
// Threads are fetched from Slack and translated to Vietnamese by the Devdy app;
// these tools read them (Vietnamese by default, original on request), search,
// rename, re-link and delete. Adding a thread, refreshing it and replying need
// the Slack token, which only the app holds, so they go through Devdy's broker
// (tools `slack_add` / `slack_refresh` / `slack_reply`); a reply is always
// confirmed by the user in the app first. There is deliberately no update /
// append / reorder / read_attachment: content belongs to Slack, ordering to the
// UI, and `read` already returns absolute file paths.

import { setTimeout as sleep } from 'node:timers/promises';
import * as store from './slack-store.mjs';
import { askBroker } from './broker.mjs';

const READ_MAX_DEFAULT = 20000;
const ORIGINAL_MODES = ['none', 'both', 'only'];

// ---- Slack mrkdwn → plain markdown ---------------------------------------------

const decodeEntities = (s) => s.replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&amp;/g, '&');

/** One `<…>` token (inner text, without the brackets) → readable text. */
function renderToken(inner, { users, channels }) {
  const bar = inner.indexOf('|');
  const head = bar < 0 ? inner : inner.slice(0, bar);
  const label = bar < 0 ? '' : inner.slice(bar + 1);
  if (head.startsWith('@')) {
    const id = head.slice(1);
    return `@${users[id]?.name || label || id}`;
  }
  if (head.startsWith('#')) {
    const id = head.slice(1);
    return `#${label || channels[id] || id}`;
  }
  if (head.startsWith('!')) {
    const cmd = head.slice(1);
    if (['here', 'channel', 'everyone'].includes(cmd)) return `@${cmd}`;
    if (label) return decodeEntities(label);
    return `@${cmd.split('^')[0]}`;
  }
  const url = decodeEntities(head);
  const text = decodeEntities(label);
  return text && text !== url ? `[${text}](${url})` : url;
}

/**
 * Slack mrkdwn → plain markdown: `<@U…>` → `@Name` (slack_users of the thread's
 * workspace), `<#C…|x>` → `#x`, `<url|label>` → `[label](url)`, `<!here>` →
 * `@here`, entities decoded. Code (``` blocks and `inline`) keeps its content
 * as-is apart from entity decoding (Slack escapes `< > &` everywhere).
 */
export function mrkdwnToPlain(text, names) {
  const src = String(text ?? '');
  const out = [];
  const code = /```[\s\S]*?```|`[^`\n]+`/g;
  let last = 0;
  let m;
  while ((m = code.exec(src))) {
    out.push(renderProse(src.slice(last, m.index), names), decodeEntities(m[0]));
    last = m.index + m[0].length;
  }
  out.push(renderProse(src.slice(last), names));
  return out.join('');
}

function renderProse(s, names) {
  // Decode entities per segment so a literal "&lt;@U1&gt;" never turns into a mention.
  return s
    .split(/(<[^<>\n]+>)/)
    .map((part, i) => (i % 2 ? renderToken(part.slice(1, -1), names) : decodeEntities(part)))
    .join('');
}

// ---- Formatting helpers ---------------------------------------------------------

const tsToDate = (ts) => {
  const n = Number(ts);
  return Number.isFinite(n) && n > 0 ? new Date(Math.floor(n * 1000)) : null;
};
const tsToIso = (ts) => tsToDate(ts)?.toISOString() ?? null;
const tsToUtc = (ts) => {
  const d = tsToDate(ts);
  return d ? `${d.toISOString().slice(0, 16).replace('T', ' ')} UTC` : `ts ${ts}`;
};

const titleOf = (t) => t.title || 'Slack thread';
const channelOf = (t) => (t.channel ? `#${t.channel}` : t.channel_id ? `#${t.channel_id}` : null);
const workspaceOf = (t) => t.workspace || t.workspace_domain || null;

function authorOf(m, users) {
  return (m.user_id && users[m.user_id]?.name) || m.author_name || m.user_id || m.bot_id || 'unknown';
}

function fileLine(f) {
  if (f.path) return `- 📎 ${f.name} — ${f.path} · ${f.size} bytes · ${f.mime || 'unknown mime'}`;
  const where = f.permalink || 'no permalink';
  if (f.download_status === 'done') {
    // Downloaded per the DB, but the stored path is missing or escapes <app_data>.
    const why = f.file_path ? `unsafe stored path refused: ${f.file_path}` : 'no stored path';
    return `- 📎 ${f.name} — downloaded but not readable (${why}): ${where}`;
  }
  return `- 📎 ${f.name} — not downloaded (${f.download_status}): ${where}`;
}

const quote = (s) =>
  s
    .split('\n')
    .map((l) => (l ? `> ${l}` : '>'))
    .join('\n');

/** One message block of the read transcript for the given `original` mode. */
function renderMessage(m, files, names, original) {
  const head = `### ${authorOf(m, names.users)} · ${tsToUtc(m.ts)} · ts=${m.ts}${m.edited_ts ? ' · edited' : ''}`;
  const raw = mrkdwnToPlain(m.text_raw, names);
  const lines = [head];
  if (original === 'only') {
    if (raw.trim()) lines.push(raw);
  } else if (m.text_vi != null && m.text_vi.trim()) {
    const vi = mrkdwnToPlain(m.text_vi, names);
    if (vi.trim()) lines.push(vi);
    if (original === 'both' && m.text_vi !== m.text_raw && raw.trim()) lines.push(`> Original:\n${quote(raw)}`);
  } else if (raw.trim()) {
    lines.push(`${raw} [untranslated]`);
  }
  if (files.length) lines.push(files.map(fileLine).join('\n'));
  if (m.reactions.length) lines.push(`reactions: ${m.reactions.map((r) => `:${r.name}: ${r.count}`).join(', ')}`);
  return lines.join('\n');
}

/** Files of a message in message order: file_ids first, then any other file pointing at its ts. */
function filesOf(m, byId, files) {
  const seen = new Set();
  const out = [];
  for (const id of m.file_ids) {
    const f = byId.get(id);
    if (f && !seen.has(f.id)) {
      seen.add(f.id);
      out.push(f);
    }
  }
  for (const f of files) {
    if (f.message_ts === m.ts && !seen.has(f.id)) {
      seen.add(f.id);
      out.push(f);
    }
  }
  return out;
}

/** Slice `body` by chars, never splitting a UTF-16 surrogate pair. */
function sliceBody(body, offset, maxChars) {
  const total = body.length;
  let start = Math.min(Math.max(0, offset), total);
  if (start > 0 && start < total && /[\uDC00-\uDFFF]/.test(body[start])) start -= 1;
  let end = Math.min(total, start + Math.max(1, maxChars));
  if (end < total && /[\uD800-\uDBFF]/.test(body[end - 1])) end -= 1;
  // Always advance by at least one full code point, else the continue offset repeats forever.
  if (end <= start && start < total) end = Math.min(total, start + (body.codePointAt(start) > 0xffff ? 2 : 1));
  return { start, end, total, slice: body.slice(start, end) };
}

const fold = (s) => String(s ?? '').normalize('NFC').toLowerCase();
const lowerIncludes = (hay, needle) => fold(hay).includes(fold(needle));

// ---- Broker-backed actions --------------------------------------------------------

// A reply waits on the user's confirmation in the app, so it gets a long timeout.
const REPLY_TIMEOUT_MS = 15 * 60_000;
const WAIT_FETCH_MS = 90_000;
const WAIT_TRANSLATE_MS = 5 * 60_000;
const POLL_MS = 1_000;
const FETCHING = ['pending', 'fetching'];

async function askSlack(tool, argv, { text, timeoutMs } = {}) {
  const resp = await askBroker(
    {
      project_id: process.env.DEVDY_PROJECT_ID,
      run_id: process.env.DEVDY_RUN_ID || null,
      tool,
      argv,
      host: null,
      ...(text != null ? { text } : {}),
    },
    timeoutMs ? { timeoutMs } : undefined,
  );
  if (!resp || resp.decision !== 'allow') throw new Error((resp && resp.reason) || `${tool} denied`);
  return resp.slack || {};
}

/**
 * Wait (polling the shared DB) until the app has fetched the thread — or, with
 * `translated`, finished translating it. `since` = the `fetched_at` before a
 * refresh: a job already in flight then only counts once the requeued run has
 * been seen pending/fetching (or has finished, which bumps `fetched_at`).
 */
async function waitFetched(id, { translated = false, since } = {}) {
  const deadline = Date.now() + (translated ? WAIT_TRANSLATE_MS : WAIT_FETCH_MS);
  let sawFetching = since === undefined;
  for (;;) {
    const t = store.threadStatus(id);
    if (!t) return { t: null, done: true };
    if (FETCHING.includes(t.fetch_status)) sawFetching = true;
    const fresh = sawFetching || (t.fetched_at && t.fetched_at !== since);
    const settled = t.fetch_status === 'error' || (fresh && (translated ? t.fetch_status === 'ready' : !FETCHING.includes(t.fetch_status)));
    if (settled) return { t, done: true };
    if (Date.now() >= deadline) return { t, done: false };
    await sleep(POLL_MS);
  }
}

function fetchOutcome(id, { t, done }) {
  if (!t) return `Slack thread ${id} no longer exists.`;
  if (t.fetch_status === 'error') {
    return `Slack thread ${id}: fetch failed (${t.fetch_error_code || 'error'}${t.fetch_error ? ` — ${t.fetch_error}` : ''}).`;
  }
  const head = `Slack thread ${id} "${titleOf(t)}": ${t.fetch_status}, ${t.reply_count} replies.`;
  if (!done) return `${head} Still working — call slack_threads_read later (untranslated messages show their original).`;
  return t.fetch_status === 'ready'
    ? `${head} Read it with slack_threads_read.`
    : `${head} Fetched; translation in progress — slack_threads_read shows untranslated messages in the original.`;
}

// ---- Tools ------------------------------------------------------------------------

/** @param {{ snippetAround: Function }} fmt shared formatters from index.mjs */
export function slackThreadTools({ snippetAround }) {
  const summary = (t) => {
    const parts = [titleOf(t), channelOf(t), workspaceOf(t), t.fetch_status].filter(Boolean);
    parts.push(`${t.reply_count} replies`);
    const last = tsToIso(t.last_reply_ts);
    if (last) parts.push(`last reply ${last}`);
    if (t.fetch_error_code) parts.push(`error: ${t.fetch_error_code}`);
    if (t.legacy) parts.push('legacy');
    if (t.linked_runs) parts.push(`sessions: ${t.linked_runs}`);
    return `- [${t.id}] ${parts.join(' · ')}`;
  };

  return {
    slack_threads_list: {
      description:
        'List Slack threads saved in Devdy (fetched from Slack by link and translated to Vietnamese by the app). Defaults to the current project; pass scope="all" for every project. Each line: id, title, #channel, workspace, fetch status (pending|fetching|translating|ready|error), reply count, last reply time, plus error code / legacy marker / linked session (run) ids when relevant.',
      inputSchema: {
        type: 'object',
        properties: {
          scope: { type: 'string', enum: ['project', 'all'], description: 'project (default) or all' },
          limit: { type: 'integer', minimum: 1, maximum: 200 },
        },
      },
      handler: (a) => {
        const rows = store.listThreads(a);
        if (!rows.length) return 'No Slack threads found.';
        return rows.map(summary).join('\n');
      },
    },

    slack_threads_read: {
      description: `Read one Slack thread by id as a transcript: metadata (workspace, channel, thread_url, fetch_status, fetched_at, project, linked session (run) ids, warnings, error), then every message as "### <name> · <UTC time> · ts=<ts>" followed by its text. Text is Vietnamese by default (untranslated messages fall back to the original, marked [untranslated]); original="both" adds a "> Original:" block per message, original="only" shows only the original. Mentions are resolved to @Name, links become [label](url), code is kept verbatim. Files are listed under their message with ABSOLUTE paths (open them with your own file reader) or "not downloaded (<status>): <permalink>". Threads not yet re-fetched from Slack (legacy) return the old markdown snapshot and files. Long transcripts are sliced: max_chars (default ${READ_MAX_DEFAULT}) + offset; a "Continue with offset=N" hint is printed when truncated.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          original: {
            type: 'string',
            enum: ORIGINAL_MODES,
            description: 'none (default): Vietnamese only; both: Vietnamese + original; only: original only',
          },
          max_chars: { type: 'integer', minimum: 1, description: `max transcript chars to return (default ${READ_MAX_DEFAULT})` },
          offset: { type: 'integer', minimum: 0, description: 'start char of the transcript slice' },
        },
        required: ['id'],
      },
      handler: (a) => {
        const original = a.original ?? 'none';
        if (!ORIGINAL_MODES.includes(original)) throw new Error(`original must be one of: ${ORIGINAL_MODES.join(', ')}`);
        const t = store.readThread(a.id);
        if (!t) return `Slack thread not found: ${a.id}`;
        const names = { users: t.users, channels: t.channels };
        const byId = new Map(t.files.map((f) => [f.id, f]));
        const attached = new Set();
        let body;
        if (t.legacy) {
          body = t.legacy_content || '(empty)';
        } else if (t.messages.length) {
          body = t.messages
            .map((m) => {
              const files = filesOf(m, byId, t.files);
              files.forEach((f) => attached.add(f.id));
              return renderMessage(m, files, names, original);
            })
            .join('\n\n');
        } else {
          body = `(no messages fetched yet — fetch_status: ${t.fetch_status})`;
        }
        const loose = t.files.filter((f) => !attached.has(f.id));
        const ws = t.workspace && t.workspace_domain ? `${t.workspace} (${t.workspace_domain})` : workspaceOf(t);
        const err = t.fetch_error_code ? `${t.fetch_error_code}${t.fetch_error ? ` — ${t.fetch_error}` : ''}` : null;
        const s = sliceBody(body, a.offset ?? 0, a.max_chars ?? READ_MAX_DEFAULT);
        const meta = [
          ['id', t.id],
          ['workspace', ws],
          ['channel', channelOf(t)],
          ['thread_url', t.thread_url],
          ['fetch_status', t.fetch_status],
          ['fetched_at', t.fetched_at],
          ['project_id', t.project_id],
          ['linked_sessions', t.linked_runs],
          ['replies', String(t.reply_count)],
          ['warnings', t.warnings.length ? t.warnings.join(', ') : null],
          ['error', err],
          ['status', t.legacy ? 'legacy snapshot (not yet re-fetched)' : null],
          ['text', t.legacy ? null : { none: 'Vietnamese', both: 'Vietnamese + original', only: 'original' }[original]],
          ['total_chars', String(s.total)],
        ]
          .filter(([, v]) => v)
          .map(([k, v]) => `- ${k}: ${v}`)
          .join('\n');
        const filesSection = loose.length
          ? `\n\n## ${t.legacy ? 'Files' : 'Other files'} (${loose.length})\n${loose.map(fileLine).join('\n')}`
          : '';
        let out = `# ${titleOf(t)}\n${meta}${filesSection}\n\n---\n_chars ${s.start}–${s.end} of ${s.total}_\n\n${s.slice || '(empty)'}`;
        if (s.end < s.total) {
          out += `\n\n---\n… truncated (${s.total - s.end} chars left). Continue with offset=${s.end}.`;
        }
        return out;
      },
    },

    slack_threads_search: {
      description:
        'Search Slack threads by case-insensitive substring across title, channel, message text (Vietnamese translation AND original) and old legacy snapshots. Defaults to the current project; pass scope="all" for every project. Returns each matching thread plus up to 3 matching messages with a snippet.',
      inputSchema: {
        type: 'object',
        properties: {
          query: { type: 'string' },
          scope: { type: 'string', enum: ['project', 'all'] },
          limit: { type: 'integer', minimum: 1, maximum: 200 },
        },
        required: ['query'],
      },
      handler: (a) => {
        const q = String(a.query ?? '').trim();
        const rows = store.searchThreads(a);
        if (!rows.length) return `No Slack threads match "${q}".`;
        return rows
          .map((t) => {
            const names = { users: t.users, channels: t.channels };
            const lines = [summary(t)];
            for (const m of t.hits) {
              // Snippet from whichever text actually matched (Vietnamese first).
              const text = m.text_vi != null && lowerIncludes(m.text_vi, q) ? m.text_vi : m.text_raw;
              lines.push(`  - ${authorOf(m, names.users)} ts=${m.ts}: ${snippetAround(mrkdwnToPlain(text, names), q)}`);
            }
            if (!t.hits.length && t.legacy_content && lowerIncludes(t.legacy_content, q)) {
              lines.push(`  - legacy snapshot: ${snippetAround(t.legacy_content, q)}`);
            }
            return lines.join('\n');
          })
          .join('\n');
      },
    },

    slack_threads_rename: {
      description:
        'Rename a Slack thread (1–200 chars, trimmed). The name is marked custom, so it is kept when the app refreshes or re-imports the thread from Slack.',
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          title: { type: 'string', description: 'New title (max 200 characters)' },
        },
        required: ['id', 'title'],
      },
      handler: (a) => {
        const r = store.renameThread(a);
        return `Renamed Slack thread ${r.id} to "${r.title}".`;
      },
    },

    slack_threads_delete: {
      description:
        'Delete one or more Slack threads by id (permanent): removes the thread, its messages and file records in one transaction, then its downloaded files folder. Pass a single id or a list.',
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          ids: { type: 'array', items: { type: 'string' }, description: 'Bulk delete' },
        },
      },
      handler: (a) => {
        const r = store.deleteThreads({ ids: a.ids ?? a.id });
        return `Deleted ${r.deleted}/${r.requested} Slack thread(s) (${r.dirs_removed} file folder(s) removed).`;
      },
    },

    slack_threads_add: {
      description:
        'Add a Slack thread to Devdy by its link (https://<workspace>.slack.com/archives/<channel>/p<ts>, a reply link works too). The app fetches it with the user\'s Slack token and translates it to Vietnamese; this waits until the messages are fetched (translation may still be running) and returns the thread id for slack_threads_read. Linked to the current project unless scope="global". Adding a link that is already saved re-imports it.',
      inputSchema: {
        type: 'object',
        properties: {
          url: { type: 'string', description: 'Slack thread link' },
          scope: { type: 'string', enum: ['project', 'global'] },
          wait_translation: { type: 'boolean', description: 'Also wait for the Vietnamese translation (up to 5 min)' },
        },
        required: ['url'],
      },
      handler: async (a) => {
        const url = String(a.url ?? '').trim();
        if (!url) throw new Error('url is required');
        const r = await askSlack('slack_add', a.scope === 'global' ? [url, 'global'] : [url]);
        const outcome = fetchOutcome(r.id, await waitFetched(r.id, { translated: !!a.wait_translation }));
        return `${r.status === 'created' ? 'Added' : 'Re-imported'} Slack thread ${r.id}. ${outcome}`;
      },
    },

    slack_threads_refresh: {
      description:
        'Refetch a saved Slack thread from Slack to pick up new messages (the app re-translates only what is new). Waits until the fetch is done (translation may still be running), then read it with slack_threads_read.',
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          wait_translation: { type: 'boolean', description: 'Also wait for the Vietnamese translation (up to 5 min)' },
        },
        required: ['id'],
      },
      handler: async (a) => {
        const before = store.threadStatus(a.id);
        if (!before) throw new Error(`Slack thread not found: ${a.id}`);
        await askSlack('slack_refresh', [before.id]);
        return fetchOutcome(before.id, await waitFetched(before.id, { translated: !!a.wait_translation, since: before.fetched_at }));
      },
    },

    slack_threads_reply: {
      description:
        'Post a reply in a saved Slack thread, AS THE USER (with their Slack token). The user must confirm the exact text in Devdy before it is sent; if they decline, this fails — do not retry unless they ask. Write `text` in Slack mrkdwn (*bold*, _italic_, `code`, <https://url|label>) and in the language the thread uses (often the original, not Vietnamese — check slack_threads_read with original="only"). Max 8000 characters. The thread is refreshed afterwards.',
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          text: { type: 'string', description: 'Reply text (Slack mrkdwn)' },
        },
        required: ['id', 'text'],
      },
      handler: async (a) => {
        const text = String(a.text ?? '').trim();
        if (!text) throw new Error('text is required');
        if (!store.threadStatus(a.id)) throw new Error(`Slack thread not found: ${a.id}`);
        await askSlack('slack_reply', [String(a.id)], { text, timeoutMs: REPLY_TIMEOUT_MS });
        return `Posted the reply to Slack thread ${a.id}; the thread is being refreshed.`;
      },
    },

    slack_threads_set_project: {
      description: 'Link a Slack thread to the current project, or detach it with scope="global".',
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string' },
          scope: { type: 'string', enum: ['project', 'global'] },
        },
        required: ['id'],
      },
      handler: (a) => {
        const r = store.setThreadProject(a);
        return `Slack thread ${r.id} is now ${r.project_id ? `linked to project ${r.project_id}` : 'global (no project)'}.`;
      },
    },
  };
}
