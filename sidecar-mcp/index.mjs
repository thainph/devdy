#!/usr/bin/env node
// Devdy built-in MCP server (stdio transport, JSON-RPC 2.0 line-delimited).
//
// Injected automatically into every run under the server key `devdy`, so tools
// are namespaced `mcp__devdy__<name>` on the AI side. Two capabilities:
//   • Quick notes  — read/write Devdy's markdown scratchpad (per-project or global)
//   • Session recall — search & read past run transcripts across sessions
//
// Scope: env `DEVDY_PROJECT_ID` / `DEVDY_PROJECT_PATH` pin the "current project";
// `DEVDY_DB_PATH` points at Devdy's SQLite store. Hand-rolled protocol keeps this
// dependency-free (nothing to `npm install` / bundle). Only JSON-RPC frames go to
// stdout; everything else must use stderr.

import * as store from './lib/store.mjs';
import { readRunTranscript, renderTranscript, findSnippet } from './lib/transcript.mjs';
import { fileTree, gitStatus, gitDiff } from './lib/project.mjs';
import { runOnServer } from './lib/ssh.mjs';

const SERVER_INFO = { name: 'devdy', version: '0.1.0' };
const DEFAULT_PROTOCOL = '2025-06-18';

const log = (...a) => process.stderr.write(`[devdy-mcp] ${a.join(' ')}\n`);
const firstLine = (s = '') => String(s).split('\n').find((l) => l.trim()) || '';
// `runs.title` is normally set (Devdy derives it from the first user message);
// fall back to an engine/type label when it's empty.
const titleOf = (run) => run.title || `${run.engine} ${run.type}`.trim();

// ---- Tool registry ---------------------------------------------------------
// Each: { description, inputSchema (JSON Schema), handler(args) -> string }.

const tools = {
  notes_list: {
    description:
      'List Devdy quick notes (markdown scratchpad). Defaults to the current project; pass scope="all" for every project. Returns id, title and a short preview.',
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
          return `- [${n.id}] ${title}${preview && preview !== title ? ` — ${preview}` : ''} (updated ${n.updated_at})`;
        })
        .join('\n');
    },
  },

  notes_read: {
    description: 'Read the full markdown body of one note by id.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const n = store.readNote(a.id);
      if (!n) return `Note not found: ${a.id}`;
      return `# ${n.title || '(untitled)'}\n\n${n.content || '(empty)'}`;
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

  sessions_recent: {
    description:
      'List recent Devdy runs/sessions (chat history). Defaults to the current project; scope="all" spans every project. Use the returned run ids with session_read.',
    inputSchema: {
      type: 'object',
      properties: {
        scope: { type: 'string', enum: ['project', 'all'] },
        limit: { type: 'integer', minimum: 1, maximum: 200 },
      },
    },
    handler: (a) => {
      const rows = store.recentRuns(a);
      if (!rows.length) return 'No sessions found.';
      return rows
        .map(
          (r) =>
            `- [${r.id}] ${titleOf(r)} · ${r.engine}/${r.status}${a.scope === 'all' ? ` · ${r.project_name}` : ''} (${r.created_at})`,
        )
        .join('\n');
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
    description: 'List Devdy todos (a global quick task list). Set include_done=false to hide completed items.',
    inputSchema: {
      type: 'object',
      properties: { include_done: { type: 'boolean' } },
    },
    handler: (a) => {
      const rows = store.listTodos({ includeDone: a.include_done !== false });
      if (!rows.length) return 'No todos.';
      return rows.map((t) => `- [${t.done ? 'x' : ' '}] ${t.text}  (${t.id})`).join('\n');
    },
  },

  todos_add: {
    description: 'Add a todo to the global task list. Use to capture follow-up work that surfaces during a session.',
    inputSchema: {
      type: 'object',
      properties: { text: { type: 'string' } },
      required: ['text'],
    },
    handler: (a) => {
      const t = store.addTodo(a);
      return `Added todo ${t.id}.`;
    },
  },

  todos_done: {
    description: 'Mark a todo done (or undo with done=false).',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' }, done: { type: 'boolean' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.setTodoDone({ id: a.id, done: a.done !== false });
      return `Todo ${r.id} marked ${r.done ? 'done' : 'not done'}.`;
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
      try {
        const text = tool.handler((params && params.arguments) || {});
        reply(id, { content: [{ type: 'text', text: String(text) }] });
      } catch (e) {
        // Surface errors as tool results (isError) so the model can react,
        // rather than as protocol errors that abort the call.
        reply(id, { content: [{ type: 'text', text: `Error: ${e && e.message ? e.message : e}` }], isError: true });
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
process.stdin.on('end', () => process.exit(0));
process.on('SIGTERM', () => process.exit(0));
process.on('SIGINT', () => process.exit(0));

log(`ready (project=${process.env.DEVDY_PROJECT_ID || 'none'})`);
