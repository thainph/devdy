// Tool definitions for Project Pulse (docs/project-pulse-spec.md §8.2).
//
// Watch / unwatch Slack threads and GitHub issues per project and read the
// progress digests the app produced. Writes only SQLite (BR-11): the app picks
// up new watches and refresh requests, so nothing here touches the network.
// `pulse_status({ refresh: true })` queues a refresh and waits up to 90 s for
// the app to mark it done, then answers with whatever is stored (`stale: true`
// on timeout).

import * as store from './pulse-store.mjs';
import { projectHealth, compareProjects, compareWatches, HEALTH_ICON } from './pulse-health.mjs';

const WHEN = 'Use when the user asks to watch / stop watching a Slack thread or GitHub issue, or asks how a project is progressing.';
const REFRESH_TIMEOUT_MS = 90_000;
const REFRESH_POLL_MS = 500;
const DECISIONS_MAX = 5;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const oneLine = (s, max = 160) => {
  const t = String(s ?? '').replace(/\s+/g, ' ').trim();
  return t.length > max ? `${t.slice(0, max - 1)}…` : t;
};
const label = (w) => w.title || w.source_ref;
const sourceIcon = (w) => (w.source_type === 'slack' ? 'slack' : 'github');

function urlsOf(a) {
  const list = [...(Array.isArray(a.urls) ? a.urls : []), ...(a.url ? [a.url] : [])]
    .map((u) => String(u ?? '').trim())
    .filter(Boolean);
  if (!list.length) throw new Error('url or urls is required');
  return [...new Set(list)];
}

/** Scope of list / status: `all` → every project, else `project` → DEVDY_PROJECT_ID. */
function scopeOf(a) {
  if (a.all) return { project: null };
  return { project: store.resolveProject(a.project) };
}

/** How long since `iso`: `30h` under two days, else `6d`. */
export function ageOf(iso, now = Date.now()) {
  const t = Date.parse(iso ?? '');
  if (!Number.isFinite(t)) return null;
  const h = Math.max(0, Math.floor((now - t) / 3600_000));
  return h < 48 ? `${h}h` : `${Math.floor(h / 24)}d`;
}

const since = (iso) => {
  const a = ageOf(iso);
  return a ? ` · ${a}` : '';
};

function watchLine(w) {
  const h = w.health;
  const parts = [`${HEALTH_ICON[h.health] ?? ''} [${w.id}] ${sourceIcon(w)} · ${label(w)} (${w.source_ref})`];
  if (h.finished) parts.push('finished');
  if (w.resolve_status !== 'ready') parts.push(`resolve: ${w.resolve_status}`);
  if (w.last_error) parts.push(`error: ${w.last_error}`);
  if (w.digest) {
    parts.push(`status: ${w.digest.status}`, `ball: ${w.digest.ball}${since(w.digest.ball_since)}`);
    if (h.next_due) parts.push(`next deadline: ${h.next_due.title || '(untitled)'} — ${h.next_due.due_at}`);
    if (w.digest.summary) parts.push(oneLine(w.digest.summary));
    parts.push(`summarised ${w.digest.created_at}`);
  } else {
    parts.push('no digest yet');
  }
  if (w.checked_at) parts.push(`checked ${w.checked_at}`);
  return `- ${parts.join(' · ')}`;
}

/** Watches grouped per project with BR-09 health, cards in BR-09 order. */
function groupByProject(items) {
  const groups = new Map();
  for (const w of items) {
    if (!groups.has(w.project_id)) groups.set(w.project_id, { name: w.project_name, items: [] });
    groups.get(w.project_id).items.push(w);
  }
  return [...groups.values()]
    .map((g) => {
      g.items.sort((a, b) => compareWatches(a.health, b.health));
      return { ...g, ...projectHealth(g.items.map((w) => w.health)) };
    })
    .sort(compareProjects);
}

function renderStatus(items, { stale }) {
  const out = [];
  if (stale) out.push('stale: true — the app did not finish refreshing within 90 s; showing the stored digests.\n');
  const groups = groupByProject(items);
  if (!groups.length) return `${out.join('')}No watched sources. Watch a Slack thread or GitHub issue with pulse_watch.`;
  for (const g of groups) {
    const c = g.counts;
    out.push(`## ${HEALTH_ICON[g.health]} ${g.name}`);
    out.push(`- health: ${g.health}`);
    const m = g.next_milestone;
    out.push(`- next milestone: ${m ? `${m.title || '(untitled)'} — ${m.due_at}` : 'none'}`);
    out.push(`- needs me: ${c.needs_me} · waiting on customer: ${c.waiting_customer} · overdue: ${c.overdue} · watching: ${c.watching}`);
    const open = g.items.filter((w) => !w.health.finished);
    const mine = open.filter((w) => w.health.needs_me);
    const customer = open.filter((w) => w.health.waiting_customer);
    out.push('', '### Needs me');
    out.push(...(mine.length ? mine.map((w) => `- ${label(w)}${since(w.digest.ball_since)}: ${oneLine(w.digest.next_action || w.digest.summary)}`) : ['- none']));
    out.push('', '### Waiting on customer');
    out.push(...(customer.length ? customer.map((w) => `- ${label(w)}${since(w.digest.ball_since)}: ${oneLine(w.digest.summary)}`) : ['- none']));
    const decisions = g.items
      .flatMap((w) => (w.digest?.decisions ?? []).map((d) => ({ ...d, w })))
      .sort((a, b) => String(b.decided_at ?? '').localeCompare(String(a.decided_at ?? '')))
      .slice(0, DECISIONS_MAX);
    out.push('', '### Recent decisions');
    out.push(...(decisions.length ? decisions.map((d) => `- ${d.decided_at ? `${d.decided_at}: ` : ''}${oneLine(d.text)} (${label(d.w)})`) : ['- none']));
    out.push('', '### Sources');
    for (const w of open) {
      out.push(watchLine(w));
      if (w.digest?.next_action) out.push(`  - next: ${oneLine(w.digest.next_action, 200)}`);
      for (const d of w.digest?.deadlines ?? []) out.push(`  - deadline: ${d.title || '(untitled)'} — ${d.due_at}`);
      for (const q of w.digest?.open_questions ?? []) out.push(`  - open question (${q.asked_by || '?'}): ${oneLine(q.text)}`);
    }
    const finished = g.items.filter((w) => w.health.finished);
    if (finished.length) {
      out.push('', '### Done');
      out.push(...finished.map((w) => watchLine(w)));
    }
    out.push('');
  }
  return out.join('\n').trimEnd();
}

export function pulseTools() {
  return {
    pulse_watch: {
      description: `Watch Slack threads / GitHub issues in Project Pulse so the Devdy app tracks and summarises their progress. ${WHEN} Accepts a Slack thread link (a reply link resolves to its thread), a GitHub issue link or owner/repo#123 (bare #123 only when the project has exactly one GitHub repo), or a Devdy Slack thread id. Pull requests are not supported. project defaults to the current session's project (error project_required when there is none — ask the user). A source belongs to one project: watching it for another project fails with watched_in_other_project unless move=true. Returns JSON per source: { id, source_type, source_ref, project, already_watching } or { input, error }. The app fetches and summarises in the background.`,
      inputSchema: {
        type: 'object',
        properties: {
          url: { type: 'string', description: 'Slack thread link, GitHub issue link / owner/repo#n, or Devdy Slack thread id' },
          urls: { type: 'array', items: { type: 'string' }, description: 'several sources at once' },
          project: { type: 'string', description: 'project id or name (default: current project)' },
          move: { type: 'boolean', description: 'move a source watched in another project here' },
        },
      },
      handler: (a) => {
        const urls = urlsOf(a);
        const project = store.resolveProject(a.project);
        const runId = process.env.DEVDY_RUN_ID || null;
        const results = urls.map((input) => {
          try {
            const source = store.normalize(input, project.id);
            const r = store.watchSource({ source, projectId: project.id, move: !!a.move, runId });
            return {
              id: r.watch.id,
              source_type: r.watch.source_type,
              source_ref: r.watch.source_ref,
              project: r.watch.project_name,
              project_id: r.watch.project_id,
              already_watching: r.already_watching,
            };
          } catch (e) {
            if (!(e instanceof store.PulseError)) throw e;
            return { input, error: e.code, message: e.detail };
          }
        });
        return JSON.stringify(results, null, 2);
      },
    },

    pulse_unwatch: {
      description: `Stop watching a source in Project Pulse (its digests and the captured Slack thread are kept; watch it again to resume). ${WHEN} Pass the watch id (from pulse_list) or the same link / owner/repo#n used to watch it.`,
      inputSchema: {
        type: 'object',
        properties: {
          id: { type: 'string', description: 'watch id' },
          url: { type: 'string', description: 'Slack thread link or GitHub issue link / owner/repo#n' },
        },
      },
      handler: (a) => {
        let id = String(a.id ?? '').trim();
        if (!id) {
          if (!a.url) throw new Error('id or url is required');
          const source = store.normalize(a.url, process.env.DEVDY_PROJECT_ID || null);
          const w = store.findBySource(source.source_type, source.source_ref);
          if (!w || !w.active) throw new store.PulseError('not_found', `not watching ${source.source_ref}`);
          id = w.id;
        }
        store.unwatch(id);
        return JSON.stringify({ ok: true, id });
      },
    },

    pulse_list: {
      description: `List the sources watched in Project Pulse grouped by project with health (🔴 🟡 🟢 ✅ ⚪), resolve state, latest digest status / ball and a one-line summary. ${WHEN} Defaults to the current project; all=true lists every project.`,
      inputSchema: {
        type: 'object',
        properties: {
          project: { type: 'string', description: 'project id or name (default: current project)' },
          all: { type: 'boolean', description: 'every project' },
        },
      },
      handler: (a) => {
        const { project } = scopeOf(a);
        const items = store.listWatches(project?.id ?? null);
        if (!items.length) return project ? `No watched sources in project ${project.name}.` : 'No watched sources.';
        const out = [];
        for (const g of groupByProject(items)) out.push(`## ${HEALTH_ICON[g.health]} ${g.name} (health: ${g.health})`, ...g.items.map(watchLine), '');
        return out.join('\n').trimEnd();
      },
    },

    pulse_status: {
      description: `Progress report from Project Pulse as markdown: project health (🔴 🟡 🟢 ⚪), next milestone, what needs the user, what waits on the customer, recent decisions and every watched source with its summary, deadlines and open questions. ${WHEN} Answers from the stored digests (no refresh) by default; refresh=true asks the Devdy app to re-fetch and re-summarise first and waits up to 90 s (on timeout the stored data comes back marked stale: true). Defaults to the current project; all=true covers every project.`,
      inputSchema: {
        type: 'object',
        properties: {
          project: { type: 'string', description: 'project id or name (default: current project)' },
          all: { type: 'boolean', description: 'every project' },
          refresh: { type: 'boolean', description: 'refresh before answering (up to 90 s)' },
        },
      },
      handler: async (a) => {
        const { project } = scopeOf(a);
        let stale = false;
        if (a.refresh) {
          const reqId = store.requestRefresh(project?.id ?? null);
          const timeout = Number(process.env.DEVDY_PULSE_REFRESH_TIMEOUT_MS) || REFRESH_TIMEOUT_MS;
          const deadline = Date.now() + timeout;
          while (!store.requestDone(reqId) && Date.now() < deadline) await sleep(REFRESH_POLL_MS);
          stale = !store.requestDone(reqId);
        }
        return renderStatus(store.listWatches(project?.id ?? null), { stale });
      },
    },
  };
}
