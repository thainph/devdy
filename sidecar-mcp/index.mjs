#!/usr/bin/env node
// Devdy built-in MCP server (stdio transport, JSON-RPC 2.0 line-delimited).
//
// Injected automatically into every run under the server key `devdy`, so tools
// are namespaced `mcp__devdy__<name>` on the AI side. Capabilities:
//   • Quick notes  — read/write Devdy's markdown scratchpad (per-project or global)
//   • Session recall — search & read past run transcripts across sessions
//   • Todos / project context / VPS — quick tasks, git, managed servers
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
  // Skills screen). Source-only: these edit the skill definition + files under
  // <app_data>/skills; they do NOT push changes into projects that already
  // applied the skill — re-apply via the Devdy app for that.

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
      'Update an existing skill by id. Omitted fields are left unchanged; pass `content` to replace the SKILL.md body. Renaming moves the source folder. Source-only — if the skill is already applied to projects, re-apply via the Devdy app to propagate changes.',
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
        msg += `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Re-apply via the Devdy app to sync the new content${r.renamed ? ' (renamed — old artifacts remain under the previous name)' : ''}.`;
      }
      return msg;
    },
  },

  skills_delete: {
    description:
      'Delete a skill from the library: removes the source folder and its DB record (and project-apply links). Files already copied into projects (.claude/.codex/AGENTS.md) are NOT removed — clean those via the Devdy app.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.deleteSkill(a.id);
      let msg = `Deleted skill '${r.name}'.`;
      if (r.applied.length) {
        msg += `\n⚠ Was applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Their copied files remain on disk — remove them via the Devdy app.`;
      }
      return msg;
    },
  },

  // ---- Skill reference files -----------------------------------------------
  // Read/write the extra files inside a skill folder (references/, scripts/,
  // assets/, templates…) beyond SKILL.md. Paths are confined to the skill
  // folder. Source-only, same re-apply caveat as skills_update.

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
      'Create or overwrite a reference file inside a skill folder (e.g. "references/api.md"). Parent directories are created automatically. `content` replaces the whole file. Confined to the skill folder — relative paths only. Source-only: if the skill is already applied to projects, re-apply via the Devdy app to propagate.',
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
      'Delete a reference file (or subfolder) inside a skill folder by relative path. SKILL.md cannot be deleted. Source-only, same re-apply caveat as skills_write_file.',
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
  // Source-only, same caveat as skills.

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
      'Create a new rule in the library. Writes <app_data>/rules/<name>.md and registers it. `content` is the full markdown (optionally with YAML frontmatter). name must be a slug ([A-Za-z0-9_-]); target defaults to "both".',
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
      'Update an existing rule by id. Omitted fields are left unchanged; pass `content` to replace the body. Renaming moves the source file. Source-only — if the rule is already applied to projects, re-apply via the Devdy app to propagate changes.',
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
        msg += `\n⚠ Applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Re-apply via the Devdy app to sync the new content${r.renamed ? ' (renamed — old artifacts remain under the previous name)' : ''}.`;
      }
      return msg;
    },
  },

  rules_delete: {
    description:
      'Delete a rule from the library: removes the source file and its DB record (and project-apply links). Artifacts already written into projects (.claude/rules + AGENTS.md block) are NOT removed — clean those via the Devdy app.',
    inputSchema: {
      type: 'object',
      properties: { id: { type: 'string' } },
      required: ['id'],
    },
    handler: (a) => {
      const r = store.deleteRule(a.id);
      let msg = `Deleted rule '${r.name}'.`;
      if (r.applied.length) {
        msg += `\n⚠ Was applied to ${r.applied.length} project(s): ${r.applied.join(', ')}. Their written artifacts remain on disk — remove them via the Devdy app.`;
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
