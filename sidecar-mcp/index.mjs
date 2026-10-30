#!/usr/bin/env node
// Devdy built-in MCP server (stdio transport, JSON-RPC 2.0 line-delimited).
//
// Injected automatically into every run under the server key `devdy`, so tools
// are namespaced `mcp__devdy__<name>` on the AI side. Capabilities:
//   • Quick notes  — full CRUD over Devdy's markdown scratchpad (per-project or global)
//   • Slack threads (slack_threads_*) fetched + translated to Vietnamese by the app:
//     list/read (Vietnamese, original on request)/search/rename/set_project/delete
//   • Session recall — search & read past run transcripts across sessions
//   • Todos / project context / VPS — full CRUD on todos (deadline, priority, estimate,
//     status, source; ranked in the app's Focus board), git, managed servers
//   • Skills & Rules library — CRUD the reusable skill/rule definitions (source-only)
//
// Scope: env `DEVDY_PROJECT_ID` / `DEVDY_PROJECT_PATH` pin the "current project";
// `DEVDY_DB_PATH` points at Devdy's SQLite store. Hand-rolled protocol keeps this
// dependency-free (nothing to `npm install` / bundle). Only JSON-RPC frames go to
// stdout; everything else must use stderr.

import * as store from './lib/store.mjs';
import { readRunTranscript, renderTranscript, findSnippet } from './lib/transcript.mjs';
import { fileTree, gitStatus, gitDiff } from './lib/project.mjs';
import { runOnServer } from './lib/ssh.mjs';
import { slackThreadTools } from './lib/slack-thread-tools.mjs';
import { formatEstimate } from './lib/todo-fields.mjs';

const SERVER_INFO = { name: 'devdy', version: '0.1.0' };
const DEFAULT_PROTOCOL = '2025-06-18';

const log = (...a) => process.stderr.write(`[devdy-mcp] ${a.join(' ')}\n`);
const firstLine = (s = '') => String(s).split('\n').find((l) => l.trim()) || '';
// One todo as a list line: `- [ ] text · due 2026-10-30 15:00 · P1 · ~2h · doing · …  (id)`.
const todoLine = (t) => {
  const bits = [];
  if (t.due_date) bits.push(`due ${t.due_date}${t.due_time ? ` ${t.due_time}` : ''}`);
  if (t.priority != null && t.priority !== 2) bits.push(`P${t.priority}`);
  if (t.estimate_min != null) bits.push(`~${formatEstimate(t.estimate_min)}`);
  if (!t.done && t.status && t.status !== 'todo') bits.push(t.status);
  if (!t.done && t.snoozed_until) bits.push(`snoozed until ${t.snoozed_until}`);
  if (t.project_name) bits.push(t.project_name);
  if (t.source_url) bits.push(`source ${t.source_ref || t.source_url}`);
  if (t.linked_runs) bits.push(`sessions: ${t.linked_runs}`);
  return `- [${t.done ? 'x' : ' '}] ${t.text}${bits.map((b) => ` · ${b}`).join('')}  (${t.id})`;
};
// A one-line excerpt centred on the first case-insensitive hit; falls back to the
// head of the text when the match was in the title rather than the body.
const snippetAround = (text = '', query = '', width = 120) => {
  const body = String(text).replace(/\s+/g, ' ').trim();
  const i = body.toLowerCase().indexOf(String(query || '').trim().toLowerCase());
  const start = i < 0 ? 0 : Math.max(0, i - Math.floor(width / 3));
  const end = start + width;
  return `${start > 0 ? '…' : ''}${body.slice(start, end)}${end < body.length ? '…' : ''}`;
};
// `runs.title` is normally set (Devdy derives it from the first user message);
// fall back to an engine/type label when it's empty.
const titleOf = (run) => run.title || `${run.engine} ${run.type}`.trim();
const fmtDuration = (secs) => {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return h ? `${h}h${String(m).padStart(2, '0')}m` : m ? `${m}m` : `${secs}s`;
};
const fmtTokens = (n) => (n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n));

// ---- Tool registry ---------------------------------------------------------
// Each: { description, inputSchema (JSON Schema), handler(args) -> string }.

const tools = {
  notes_list: {
    description:
      'List Devdy quick notes (markdown scratchpad). Defaults to the current project; pass scope="all" for every project. Returns id, title, a short preview and the linked session (run) ids.',
    inputSchema: {
      type: 'object',
      properties: {
        scope: { type: 'string', enum: ['project', 'all'], description: 'project (default) or all' },
        limit: { type: 'integer', minimum: 1, maximum: 200 },
      },
    },
    handler: (a) => {
      const rows = store.listNotes(a);
      if (!rows.length) return 'No notes found.';
      return rows
        .map((n) => {
          const title = n.title || firstLine(n.content) || '(untitled)';
          const preview = firstLine(n.content).slice(0, 120);
          const sessions = n.linked_runs ? ` · sessions: ${n.linked_runs}` : '';
          return `- [${n.id}] ${title}${preview && preview !== title ? ` — ${preview}` : ''} (updated ${n.updated_at})${sessions}`;
        })
        .join('\n');
    },
  },

  notes_read: {
    description: 'Read the full markdown body of one note by id (plus the linked session (run) ids, when any).',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const n = store.readNote(a.id);
      if (!n) return `Note not found: ${a.id}`;
      const sessions = n.linked_runs ? `\n- linked_sessions: ${n.linked_runs}` : '';
      return `# ${n.title || '(untitled)'}${sessions}\n\n${n.content || '(empty)'}`;
    },
  },

  notes_create: {
    description:
      'Create a new quick note. Links to the current project by default; pass scope="global" for a project-less note. Use this to persist decisions, specs or context for later sessions.',
    inputSchema: {
      type: 'object',
      properties: {
        content: { type: 'string', description: 'Markdown body' },
        title: { type: 'string', description: 'Optional short title' },
        scope: { type: 'string', enum: ['project', 'global'] },
      },
      required: ['content'],
    },
    handler: (a) => {
      const n = store.createNote(a);
      return `Created note ${n.id}.`;
    },
  },

  notes_update: {
    description: 'Replace a note\'s title and/or content. Omitted fields are left unchanged.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        title: { type: 'string' },
        content: { type: 'string' },
      },
      required: ['id'],
    },
    handler: (a) => {
      store.updateNote(a);
      return `Updated note ${a.id}.`;
    },
  },

  notes_append: {
    description: 'Append a markdown block to an existing note (adds a blank line separator).',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        text: { type: 'string' },
      },
      required: ['id', 'text'],
    },
    handler: (a) => {
      store.appendNote(a);
      return `Appended to note ${a.id}.`;
    },
  },

  notes_search: {
    description:
      'Search notes by substring across title and body. Defaults to the current project; pass scope="all" for every project.',
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
      const rows = store.searchNotes(a);
      if (!rows.length) return `No notes match "${String(a.query).trim()}".`;
      return rows
        .map((n) => {
          const title = n.title || firstLine(n.content) || '(untitled)';
          return `- [${n.id}] ${title} — ${snippetAround(n.content, a.query)} (updated ${n.updated_at})`;
        })
        .join('\n');
    },
  },

  notes_delete: {
    description:
      'Delete one or more notes by id (permanent). Pass a single id or a list; the batch is applied in one transaction.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        ids: { type: 'array', items: { type: 'string' }, description: 'Bulk delete' },
      },
    },
    handler: (a) => {
      const r = store.deleteNotes({ ids: a.ids ?? a.id });
      return `Deleted ${r.deleted}/${r.requested} note(s).`;
    },
  },

  notes_set_project: {
    description:
      'Link a note to the current project, or detach it with scope="global" (which also drops the run backlink).',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        scope: { type: 'string', enum: ['project', 'global'] },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.setNoteProject(a);
      return `Note ${r.id} is now ${r.project_id ? `linked to project ${r.project_id}` : 'global (no project)'}.`;
    },
  },

  notes_reorder: {
    description:
      'Reorder notes. The given ids move to the top in the given order; every other note keeps its relative order below. Pass the full list for an exact ordering.',
    inputSchema: {
      type: 'object',
      properties: { ids: { type: 'array', items: { type: 'string' } } },
      required: ['ids'],
    },
    handler: (a) => {
      const r = store.reorderNotes(a);
      return `Reordered ${r.ordered} note(s).`;
    },
  },

  // ---- Slack threads ---------------------------------------------------------
  // Own tables (migration 0041), fetched from Slack and translated by the app;
  // read-mostly tools (see lib/slack-thread-tools.mjs).
  ...slackThreadTools({ snippetAround }),

  sessions_recent: {
    description:
      'List recent Devdy runs/sessions (chat history) with duration, tokens and cost. Defaults to the current project; scope="all" spans every project. Pass from/to (local YYYY-MM-DD, inclusive) to list a day or date range — e.g. for a daily work summary — and get totals. Use the returned run ids with session_read.',
    inputSchema: {
      type: 'object',
      properties: {
        scope: { type: 'string', enum: ['project', 'all'] },
        limit: { type: 'integer', minimum: 1, maximum: 200, description: 'default 20, or 200 when from/to is set' },
        from: { type: 'string', description: 'inclusive local start date, YYYY-MM-DD' },
        to: { type: 'string', description: 'inclusive local end date, YYYY-MM-DD' },
      },
    },
    handler: (a) => {
      const rows = store.recentRuns(a);
      if (!rows.length) return 'No sessions found.';
      const now = Date.now();
      const ranged = Boolean(a.from || a.to);
      const lines = rows.map((r) => {
        const wall = store.runWallSecs(r, now);
        const ref = r.ref_number != null ? ` #${r.ref_number}` : '';
        const usage = [
          wall != null ? `wall ${fmtDuration(wall)}` : null,
          ranged && r.agent_ms ? `agent ${fmtDuration(Math.round(r.agent_ms / 1000))}` : null,
          r.tokens ? `${fmtTokens(r.tokens)} tok` : null,
          r.cost ? `$${r.cost.toFixed(2)}` : null,
        ].filter(Boolean);
        return (
          `- [${r.id}] ${titleOf(r)}${ref} · ${r.type} · ${r.engine}/${r.status}` +
          `${a.scope === 'all' ? ` · ${r.project_name}` : ''}` +
          `${usage.length ? ` · ${usage.join(' · ')}` : ''} (${r.started_at || r.created_at})`
        );
      });
      if (!ranged) return lines.join('\n');
      const sum = (k) => rows.reduce((n, r) => n + (r[k] || 0), 0);
      const total =
        `Total: ${rows.length} run(s) · agent ${fmtDuration(Math.round(sum('agent_ms') / 1000))}` +
        ` · wall ${fmtDuration(store.mergedWallSecs(rows, now))} (overlaps merged)` +
        ` · ${fmtTokens(sum('tokens'))} tok · $${sum('cost').toFixed(2)}` +
        `\n(tokens/cost/agent count only turns inside the range; wall = run start→finish)`;
      return `${total}\n${lines.join('\n')}`;
    },
  },

  sessions_search: {
    description:
      'Full-text search across past run transcripts (long-term memory). Returns matching sessions with a snippet. Defaults to the current project. Use session_read to open a full match.',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string' },
        scope: { type: 'string', enum: ['project', 'all'] },
        limit: { type: 'integer', minimum: 1, maximum: 50, description: 'max matches to return (default 20)' },
        max_runs: { type: 'integer', minimum: 1, maximum: 500, description: 'how many recent runs to scan (default 80)' },
      },
      required: ['query'],
    },
    handler: (a) => {
      const query = String(a.query || '').trim();
      if (!query) throw new Error('query is required');
      const limit = Math.max(1, Math.min(50, Number(a.limit) || 20));
      const maxRuns = Math.max(1, Math.min(500, Number(a.max_runs) || 80));
      const runs = store.recentRuns({ scope: a.scope, limit: maxRuns });
      const matches = [];
      let scanned = 0;
      for (const r of runs) {
        const t = readRunTranscript(r, r.project_path);
        if (!t) continue;
        scanned += 1;
        const hit = findSnippet(t.messages, query);
        if (hit) {
          matches.push(
            `- [${r.id}] ${titleOf(r)} · ${r.engine}${a.scope === 'all' ? ` · ${r.project_name}` : ''} (${r.created_at})\n    ${hit.role}: ${hit.snippet}`,
          );
          if (matches.length >= limit) break;
        }
      }
      if (!matches.length) return `No matches for "${query}" (scanned ${scanned} transcripts).`;
      return `${matches.length} match(es) for "${query}":\n${matches.join('\n')}`;
    },
  },

  session_read: {
    description:
      'Read the transcript of one past run/session by run id, rendered as markdown. Use to recall what was discussed or decided in an earlier session.',
    inputSchema: {
      type: 'object',
      properties: {
        run_id: { type: 'string' },
        max_chars: { type: 'integer', minimum: 500, maximum: 40000, description: 'truncation cap (default 8000)' },
      },
      required: ['run_id'],
    },
    handler: (a) => {
      const run = store.getRun(a.run_id);
      if (!run) return `Run not found: ${a.run_id}`;
      const t = readRunTranscript(run, run.project_path);
      if (!t) return `No transcript available for run ${a.run_id}.`;
      const head = `# ${titleOf(run)}\n_${run.engine}/${run.status} · ${run.created_at} · ${run.project_name}_\n\n`;
      return head + renderTranscript(t.messages, Math.max(500, Math.min(40000, Number(a.max_chars) || 8000)));
    },
  },

  // ---- Phase 2: Todos ------------------------------------------------------

  todos_list: {
    description:
      'List Devdy todos. Default sort="due": open todos first by deadline (date, then time; no deadline last), then priority; sort="position" gives the manual list order. Set include_done=false to hide completed items, or scope="project" to only show the current project\'s todos. Each line shows deadline, priority, estimate, status, snooze, project, source link and linked session (run) ids. This is not the recommended work order: open Focus in Devdy for the recommended order.',
    inputSchema: {
      type: 'object',
      properties: {
        include_done: { type: 'boolean' },
        scope: { type: 'string', enum: ['all', 'project'], description: 'all (default) or project' },
        sort: { type: 'string', enum: ['due', 'position'], description: 'due (default) or position' },
      },
    },
    handler: (a) => {
      const rows = store.listTodos({ includeDone: a.include_done !== false, scope: a.scope, sort: a.sort || 'due' });
      if (!rows.length) return 'No todos.';
      return rows.map(todoLine).join('\n');
    },
  },

  todos_add: {
    description:
      'Add a todo. Links to the current project by default; pass scope="global" for a project-less task. Optional: deadline due="YYYY-MM-DD" (+ due_time="HH:MM", 24 h), priority 1–3 or "P1".."P3" (default 2), estimate ("30m", "2h", "1h30" or minutes, 5–2400), source_url (Slack thread, GitHub/GitLab issue or PR, run:<id>, or any URL). Use this to capture follow-up work that surfaces during a session; open Focus in Devdy for the recommended order.',
    inputSchema: {
      type: 'object',
      properties: {
        text: { type: 'string' },
        scope: { type: 'string', enum: ['project', 'global'] },
        due: { type: 'string', description: 'Deadline, YYYY-MM-DD (local date)' },
        due_time: { type: 'string', description: 'Deadline time, HH:MM 24 h (needs due)' },
        priority: { type: ['integer', 'string'], description: '1 (high) – 3 (low), or "P1".."P3"; default 2' },
        estimate: { type: ['string', 'integer'], description: '"30m", "2h", "1h30" or minutes' },
        source_url: { type: 'string', description: 'Where the work comes from (link)' },
      },
      required: ['text'],
    },
    handler: (a) => {
      const t = store.addTodo(a);
      return `Added todo ${t.id}${t.project_id ? '' : ' (global)'}: ${todoLine(t).replace(/^- \[ \] /, '')}`;
    },
  },

  todos_set_due: {
    description:
      'Set a todo\'s deadline (due="YYYY-MM-DD", a local date, plus optional due_time="HH:MM"), or clear both by omitting due / passing "".',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        due: { type: 'string', description: 'YYYY-MM-DD; omit or "" to clear' },
        due_time: { type: 'string', description: 'HH:MM 24 h; omit for an all-day deadline' },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.setTodoDue(a);
      return r.due_date
        ? `Todo ${r.id} is due ${r.due_date}${r.due_time ? ` ${r.due_time}` : ''}.`
        : `Cleared the deadline of todo ${r.id}.`;
    },
  },

  todos_done: {
    description: 'Mark a todo done (or undo with done=false). Omit done to flip the current state.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        done: { type: 'boolean', description: 'omit to toggle' },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = a.done == null ? store.toggleTodo({ id: a.id }) : store.setTodoDone({ id: a.id, done: a.done });
      return `Todo ${r.id} marked ${r.done ? 'done' : 'not done'}.`;
    },
  },

  todos_update: {
    description:
      'Update a todo: text, priority (1–3 or "P1".."P3"), estimate ("30m", "2h", "1h30" or minutes; "" clears), status ("todo", "doing" = working on it now, "waiting" = blocked on someone else) and snooze_until (RFC3339 date-time, e.g. 2026-10-30T14:00:00+07:00; "" clears). Only the fields passed change. Open Focus in Devdy for the recommended order.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        text: { type: 'string' },
        priority: { type: ['integer', 'string'] },
        estimate: { type: ['string', 'integer'] },
        status: { type: 'string', enum: ['todo', 'doing', 'waiting'] },
        snooze_until: { type: 'string', description: 'RFC3339; "" to un-snooze' },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.updateTodo(a);
      return `Updated todo ${r.id}: ${todoLine(r).replace(/^- \[.\] /, '')}`;
    },
  },

  todos_delete: {
    description:
      'Delete one or more todos by id (permanent). Pass a single id or a list; the batch is applied in one transaction.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        ids: { type: 'array', items: { type: 'string' }, description: 'Bulk delete' },
      },
    },
    handler: (a) => {
      const r = store.deleteTodos({ ids: a.ids ?? a.id });
      return `Deleted ${r.deleted}/${r.requested} todo(s).`;
    },
  },

  todos_clear_done: {
    description: 'Delete every completed todo in one go.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => {
      const r = store.clearDoneTodos();
      return r.deleted ? `Cleared ${r.deleted} completed todo(s).` : 'No completed todos to clear.';
    },
  },

  todos_set_project: {
    description:
      'Link a todo to the current project, or detach it with scope="global" (which also drops the run backlink).',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        scope: { type: 'string', enum: ['project', 'global'] },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.setTodoProject(a);
      return `Todo ${r.id} is now ${r.project_id ? `linked to project ${r.project_id}` : 'global (no project)'}.`;
    },
  },

  todos_reorder: {
    description:
      'Reorder todos (priority: top first). The given ids move to the top in the given order; every other todo keeps its relative order below. Pass the full list for an exact ordering.',
    inputSchema: {
      type: 'object',
      properties: { ids: { type: 'array', items: { type: 'string' } } },
      required: ['ids'],
    },
    handler: (a) => {
      const r = store.reorderTodos(a);
      return `Reordered ${r.ordered} todo(s).`;
    },
  },

  // ---- Phase 2: Project context --------------------------------------------

  project_info: {
    description: 'Summarise the current Devdy project (name, path, linked git repo) plus git branch/status.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => {
      const p = store.currentProject();
      if (!p) return 'No current project (this run is not scoped to a Devdy project).';
      const repo = p.github_owner && p.github_repo ? `${p.github_owner}/${p.github_repo}` : '(none)';
      let head = `# ${p.name}\n- path: ${p.path}\n- repo: ${repo}\n- default engine: ${p.default_engine || 'inherit'}\n`;
      try {
        head += `\n${gitStatus()}`;
      } catch (e) {
        head += `\n(git unavailable: ${e.message})`;
      }
      return head;
    },
  },

  projects_list: {
    description:
      'List every Devdy project (name, path, default engine and how many repos each has linked). Use this to discover project names/ids to pass to repos_list.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => {
      const rows = store.listProjects();
      if (!rows.length) return 'No Devdy projects.';
      return rows
        .map((p) => {
          const repo = p.github_owner && p.github_repo ? ` · ${p.github_owner}/${p.github_repo}` : '';
          return `- [${p.id}] ${p.name} — ${p.path} · repos=${p.repo_count} · engine=${p.default_engine || 'inherit'}${repo}`;
        })
        .join('\n');
    },
  },

  repos_list: {
    description:
      'List git repos linked to Devdy projects. Defaults to the current project; pass scope="all" for every repo across all projects, or project="<name-or-id>" to target a specific project regardless of the current run.',
    inputSchema: {
      type: 'object',
      properties: {
        scope: { type: 'string', enum: ['project', 'all'] },
        project: { type: 'string', description: 'project id or exact name to target (overrides scope)' },
      },
    },
    handler: (a) => {
      const { project, rows } = store.listRepos({ scope: a.scope, project: a.project });
      const fmt = (r) => {
        const gh = r.github_owner && r.github_repo ? ` · ${r.github_owner}/${r.github_repo}` : '';
        return `- [${r.id}] ${r.name} — ${r.path}${gh}`;
      };
      if (a.project != null && String(a.project).trim() && !project) {
        return `No project matches "${a.project}".`;
      }
      if (!rows.length) {
        if (project) return `Project "${project.name}" has no linked repos (0).`;
        if (a.scope === 'all') return 'No repos in any project.';
        return 'No current project, or it has no linked repos.';
      }
      if (project) {
        return `# ${project.name} — ${rows.length} repo(s)\n${rows.map(fmt).join('\n')}`;
      }
      // scope=all: group by project
      const byProject = new Map();
      for (const r of rows) {
        if (!byProject.has(r.project_name)) byProject.set(r.project_name, []);
        byProject.get(r.project_name).push(r);
      }
      return [...byProject.entries()]
        .map(([name, list]) => `# ${name} — ${list.length} repo(s)\n${list.map(fmt).join('\n')}`)
        .join('\n\n');
    },
  },

  file_tree: {
    description:
      'Show a bounded file tree of the current project (skips node_modules/.git/build dirs). Narrow with subpath and maxDepth for large repos.',
    inputSchema: {
      type: 'object',
      properties: {
        subpath: { type: 'string', description: 'relative subdirectory to root at (default project root)' },
        max_depth: { type: 'integer', minimum: 1, maximum: 8 },
      },
    },
    handler: (a) => fileTree({ subpath: a.subpath, maxDepth: a.max_depth }),
  },

  git_status: {
    description: 'Show git branch, short status and the last 5 commits for the current project.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => gitStatus(),
  },

  git_diff: {
    description: 'Show the git diff for the current project. staged=true for the index; stat=true for a summary only.',
    inputSchema: {
      type: 'object',
      properties: {
        staged: { type: 'boolean' },
        stat: { type: 'boolean' },
      },
    },
    handler: (a) => gitDiff({ staged: !!a.staged, stat: !!a.stat }),
  },

  // ---- Phase 4: VPS / managed servers --------------------------------------

  vps_list: {
    description:
      'List Devdy-managed VPS/servers. Defaults to servers mapped to the current project (with deployment role); scope="all" lists every server. No secrets are returned.',
    inputSchema: {
      type: 'object',
      properties: { scope: { type: 'string', enum: ['project', 'all'] } },
    },
    handler: (a) => {
      const rows = store.listServers(a);
      if (!rows.length) return 'No managed servers.';
      return rows
        .map(
          (s) =>
            `- [${s.id}] ${s.label} — ${s.username}@${s.host}:${s.port} · ${s.auth_method}` +
            `${s.role ? ` · role=${s.role}` : ''}${s.status ? ` · ${s.status}` : ''}`,
        )
        .join('\n');
    },
  },

  vps_run: {
    description:
      'Run a shell command over SSH on a managed server (identified by server_id from vps_list). Non-interactive (BatchMode); passphrase-protected keys must be in ssh-agent. Returns combined stdout/stderr. This mutates a remote host — it is gated by Devdy\'s permission prompt.',
    inputSchema: {
      type: 'object',
      properties: {
        server_id: { type: 'string' },
        command: { type: 'string' },
        timeout_ms: { type: 'integer', minimum: 1000, maximum: 120000 },
      },
      required: ['server_id', 'command'],
    },
    handler: (a) => {
      const server = store.getServer(a.server_id);
      if (!server) return `Server not found: ${a.server_id}`;
      const r = runOnServer(server, a.command, {
        timeoutMs: Math.max(1000, Math.min(120000, Number(a.timeout_ms) || 30000)),
      });
      return `${r.ok ? '✓' : '✗'} ${server.label} (${server.host})\n\n${r.output}`;
    },
  },

  // ---- Skills library ------------------------------------------------------
  // Manage Devdy's reusable skill definitions (the same library shown on the
  // Skills screen). These edit the skill definition + files under
  // <app_data>/skills. Projects symlink to that folder, so content edits are live
  // everywhere; a rename or target change needs a re-apply in the Devdy app.

  skills_list: {
    description:
      'List Devdy skills from the shared library (the same skills shown on the Skills screen). Returns id, name, target (claude/codex/both) and description.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => {
      const rows = store.listSkills();
      if (!rows.length) return 'No skills.';
      return rows
        .map((s) => `- [${s.id}] ${s.name} · target=${s.target} — ${s.description || '(no description)'}`)
        .join('\n');
    },
  },

  skills_read: {
    description: 'Read one skill by id: metadata plus the full SKILL.md markdown body.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const s = store.readSkill(a.id);
      if (!s) return `Skill not found: ${a.id}`;
      return `# ${s.name}\n- target: ${s.target}\n- description: ${s.description}\n- source: ${s.source_path}\n\n---\n\n${s.content || '(empty SKILL.md)'}`;
    },
  },

  skills_create: {
    description:
      'Create a new skill in the library. Writes <app_data>/skills/<name>/SKILL.md and registers it. `content` should be the full SKILL.md (ideally with YAML frontmatter). name must be a slug ([A-Za-z0-9_-]); target defaults to "claude".',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'slug: letters, numbers, hyphens, underscores' },
        description: { type: 'string' },
        target: { type: 'string', enum: ['claude', 'codex', 'both'], description: 'default claude' },
        content: { type: 'string', description: 'Full SKILL.md markdown body' },
      },
      required: ['name', 'description', 'content'],
    },
    handler: (a) => {
      const s = store.createSkill(a);
      return `Created skill '${s.name}' (${s.id}).`;
    },
  },

  skills_update: {
    description:
      'Update an existing skill by id. Omitted fields are left unchanged; pass `content` to replace the SKILL.md body. Projects symlink to the skill folder, so content changes are live in every project at once. Renaming moves the source folder and removes the old links from projects — after a rename or target change, re-apply via the Devdy app to install it again.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        name: { type: 'string' },
        description: { type: 'string' },
        target: { type: 'string', enum: ['claude', 'codex', 'both'] },
        content: { type: 'string' },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.updateSkill(a);
      let msg = `Updated skill '${r.skill.name}'.`;
      if (r.applied.length) {
        msg += r.renamed || r.retargeted
          ? `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. ${r.renamed ? 'Renamed — the old links were removed from those projects' : 'Target changed'}; re-apply via the Devdy app to install it again.`
          : `\nApplied to ${r.applied.length} project(s) via symlink — the change is already live there.`;
      }
      return msg;
    },
  },

  skills_delete: {
    description:
      'Delete a skill from the library: removes the source folder, its DB record, project-apply links and the symlinks projects had to it. Old copies from before Devdy switched to symlinks (.codex/skills, AGENTS.md pointer) are NOT removed.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.deleteSkill(a.id);
      let msg = `Deleted skill '${r.name}'.`;
      if (r.applied.length) {
        msg += `\nWas applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Their symlinks were removed; any pre-symlink copies stay on disk.`;
      }
      return msg;
    },
  },

  // ---- Skill reference files -----------------------------------------------
  // Read/write the extra files inside a skill folder (references/, scripts/,
  // assets/, templates…) beyond SKILL.md. Paths are confined to the skill
  // folder. Projects symlink to the folder, so changes are live everywhere.

  skills_list_files: {
    description:
      'List every file inside a skill folder (SKILL.md plus references/, scripts/, assets/, etc.). Returns relative paths and byte sizes. Use skills_read_file / skills_write_file to work with a specific file.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.listSkillFiles(a.id);
      if (!r.files.length) return `Skill '${r.skill.name}' has no files.`;
      const body = r.files.map((f) => `- ${f.path} (${f.size} bytes)`).join('\n');
      return `# ${r.skill.name} — files\n- source: ${r.skill.source_path}\n\n${body}`;
    },
  },

  skills_read_file: {
    description:
      'Read one reference file inside a skill folder by its relative path (e.g. "references/github.md"). For SKILL.md prefer skills_read.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        path: { type: 'string', description: 'path relative to the skill folder, e.g. references/checklist.md' },
      },
      required: ['id', 'path'],
    },
    handler: (a) => {
      const f = store.readSkillFile(a);
      return `# ${f.skill}/${f.path}\n\n---\n\n${f.content || '(empty file)'}`;
    },
  },

  skills_write_file: {
    description:
      'Create or overwrite a reference file inside a skill folder (e.g. "references/api.md"). Parent directories are created automatically. `content` replaces the whole file. Confined to the skill folder — relative paths only. Projects symlink to the skill folder, so the change is live in every project.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        path: { type: 'string', description: 'path relative to the skill folder, e.g. references/api.md' },
        content: { type: 'string', description: 'full file contents' },
      },
      required: ['id', 'path', 'content'],
    },
    handler: (a) => {
      const r = store.writeSkillFile(a);
      let msg = `${r.created ? 'Created' : 'Updated'} ${r.skill}/${r.path}.`;
      if (r.applied.length) {
        msg += `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Re-apply via the Devdy app to sync the change.`;
      }
      return msg;
    },
  },

  skills_delete_file: {
    description:
      'Delete a reference file (or subfolder) inside a skill folder by relative path. SKILL.md cannot be deleted. Live in every project (they symlink to the skill folder).',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        path: { type: 'string', description: 'path relative to the skill folder' },
      },
      required: ['id', 'path'],
    },
    handler: (a) => {
      const r = store.deleteSkillFile(a);
      let msg = `Deleted ${r.skill}/${r.path}.`;
      if (r.applied.length) {
        msg += `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Re-apply via the Devdy app to sync the change.`;
      }
      return msg;
    },
  },

  // ---- Rules library -------------------------------------------------------
  // Manage Devdy's reusable rule/convention definitions (the Rules screen).
  // Claude's .claude/rules/<name>.md symlinks to the source (live); Codex gets an
  // AGENTS.md block or generated skill that only updates on re-apply in the app.

  rules_list: {
    description:
      'List Devdy rules from the shared library (the same rules shown on the Rules screen). Returns id, name, target (claude/codex/both) and description.',
    inputSchema: { type: 'object', properties: {} },
    handler: () => {
      const rows = store.listRules();
      if (!rows.length) return 'No rules.';
      return rows
        .map((r) => `- [${r.id}] ${r.name} · target=${r.target} — ${r.description || '(no description)'}`)
        .join('\n');
    },
  },

  rules_read: {
    description: 'Read one rule by id: metadata plus the full markdown body.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.readRule(a.id);
      if (!r) return `Rule not found: ${a.id}`;
      return `# ${r.name}\n- target: ${r.target}\n- description: ${r.description}\n- source: ${r.source_path}\n\n---\n\n${r.content || '(empty rule file)'}`;
    },
  },

  rules_create: {
    description:
      'Create a new rule in the library. Writes <app_data>/rules/<name>.md and registers it. `content` is the full markdown (optionally with YAML frontmatter). name must be a slug ([A-Za-z0-9_-]); target defaults to "both". For Codex the rule is inlined into AGENTS.md; add `codex_mode: skill` to the frontmatter to ship it to Codex as an on-demand skill instead (good for long, situational rules).',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'slug: letters, numbers, hyphens, underscores' },
        description: { type: 'string' },
        target: { type: 'string', enum: ['claude', 'codex', 'both'], description: 'default both' },
        content: { type: 'string', description: 'Full rule markdown body' },
      },
      required: ['name', 'description', 'content'],
    },
    handler: (a) => {
      const r = store.createRule(a);
      return `Created rule '${r.name}' (${r.id}).`;
    },
  },

  rules_update: {
    description:
      'Update an existing rule by id. Omitted fields are left unchanged; pass `content` to replace the body. Claude sees content changes at once (projects symlink the rule file); Codex\'s copy (AGENTS.md block, or generated skill with `codex_mode: skill`) updates only after a re-apply via the Devdy app. Renaming moves the source file and removes the old links/AGENTS.md block from projects — re-apply to install it again.',
    inputSchema: {
      type: 'object',
      properties: {
        id: { type: 'string' },
        name: { type: 'string' },
        description: { type: 'string' },
        target: { type: 'string', enum: ['claude', 'codex', 'both'] },
        content: { type: 'string' },
      },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.updateRule(a);
      let msg = `Updated rule '${r.rule.name}'.`;
      if (r.applied.length) {
        msg += r.renamed || r.retargeted
          ? `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. ${r.renamed ? 'Renamed — the old links and AGENTS.md block were removed from those projects' : 'Target changed'}; re-apply via the Devdy app to install it again.`
          : `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Claude already sees the change (symlink); re-apply via the Devdy app to update Codex's copy (AGENTS.md block / generated skill).`;
      }
      return msg;
    },
  },

  rules_delete: {
    description:
      'Delete a rule from the library: removes the source file, its DB record, project-apply links, and from each project its .claude/rules symlink, AGENTS.md block and generated Codex skill.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.deleteRule(a.id);
      let msg = `Deleted rule '${r.name}'.`;
      if (r.applied.length) {
        msg += `\nWas applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Its links and AGENTS.md blocks there were removed.`;
      }
      return msg;
    },
  },
};

function toolList() {
  return Object.entries(tools).map(([name, t]) => ({
    name,
    description: t.description,
    inputSchema: t.inputSchema,
  }));
}

// ---- JSON-RPC plumbing ------------------------------------------------------

function send(msg) {
  process.stdout.write(`${JSON.stringify(msg)}\n`);
}
function reply(id, result) {
  send({ jsonrpc: '2.0', id, result });
}
function replyError(id, code, message) {
  send({ jsonrpc: '2.0', id, error: { code, message } });
}

function handle(msg) {
  const { id, method, params } = msg;
  const isRequest = id !== undefined && id !== null;

  switch (method) {
    case 'initialize':
      reply(id, {
        protocolVersion: (params && params.protocolVersion) || DEFAULT_PROTOCOL,
        capabilities: { tools: { listChanged: false } },
        serverInfo: SERVER_INFO,
      });
      return;
    case 'notifications/initialized':
    case 'notifications/cancelled':
      return; // notifications: no response
    case 'ping':
      if (isRequest) reply(id, {});
      return;
    case 'tools/list':
      reply(id, { tools: toolList() });
      return;
    case 'tools/call': {
      const name = params && params.name;
      const tool = tools[name];
      if (!tool) {
        replyError(id, -32602, `Unknown tool: ${name}`);
        return;
      }
      const ok = (text) => reply(id, { content: [{ type: 'text', text: String(text) }] });
      // Surface errors as tool results (isError) so the model can react,
      // rather than as protocol errors that abort the call.
      const fail = (e) =>
        reply(id, { content: [{ type: 'text', text: `Error: ${e && e.message ? e.message : e}` }], isError: true });
      try {
        const text = tool.handler((params && params.arguments) || {});
        if (text && typeof text.then === 'function') {
          // Async tool (e.g. one waiting on the app or a server): keep the process
          // alive until it answers, even if stdin closes meanwhile.
          inflight += 1;
          text.then(ok, fail).finally(() => {
            inflight -= 1;
            if (stdinEnded && inflight === 0) process.exit(0);
          });
        } else {
          ok(text);
        }
      } catch (e) {
        fail(e);
      }
      return;
    }
    case 'resources/list':
      reply(id, { resources: [] });
      return;
    case 'prompts/list':
      reply(id, { prompts: [] });
      return;
    default:
      if (isRequest) replyError(id, -32601, `Method not found: ${method}`);
  }
}

let buffer = '';
let inflight = 0;
let stdinEnded = false;
process.stdin.setEncoding('utf8');
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let nl;
  while ((nl = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, nl).trim();
    buffer = buffer.slice(nl + 1);
    if (!line) continue;
    let msg;
    try {
      msg = JSON.parse(line);
    } catch (e) {
      log('failed to parse line:', e.message);
      continue;
    }
    try {
      handle(msg);
    } catch (e) {
      log('handler crashed:', e && e.stack ? e.stack : e);
      if (msg && msg.id != null) replyError(msg.id, -32603, 'Internal error');
    }
  }
});
process.stdin.on('end', () => {
  stdinEnded = true;
  if (inflight === 0) process.exit(0);
});
process.on('SIGTERM', () => process.exit(0));
process.on('SIGINT', () => process.exit(0));

log(`ready (project=${process.env.DEVDY_PROJECT_ID || 'none'})`);
