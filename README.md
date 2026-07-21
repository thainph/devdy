# Devdy

> A desktop app for developers — centrally manage AI Skills / Rules / MCP servers and automate GitHub & GitLab Issue analysis / Pull Request & Merge Request review, running directly on your existing CLI subscription.

## What is Devdy?

**Devdy** is a macOS-first desktop app built on **Tauri 2 + Vue 3 + Rust**. It is an IDE companion that drives two AI engines — `claude` and `codex` — to analyze code, review PRs, and run free-form agent sessions, while centrally managing your reusable **Skills**, **Rules**, and **MCP servers**.

Core differentiators:

- ✅ **No API keys** — runs on your existing CLI login/subscription (`claude` logged into a subscription, `codex` logged into ChatGPT).
- ✅ **Local-first** — all data lives in local SQLite + log files; no cloud sync.
- ✅ **Secure by design** — every secret (PATs, MCP credentials) stays in the OS Keychain and is brokered to runs over an isolated per-run channel, never written to disk, logs, or the engine's environment. See [Security](#security--credential-isolation).
- ✅ **Interoperable** — mirrors the same session transcripts used by the Claude CLI and VS Code extension, so your history is shared, not siloed.

---

## Features

### 1. Runs — the heart of the app

A **run** is one execution of an AI engine. Devdy supports three kinds, working across both **GitHub and GitLab**:

- 🔍 **Issue analysis** — fetch a GitHub issue or GitLab issue + its comments (bots filtered out) and let the agent investigate.
- 👀 **Pull Request / Merge Request review** — fetch the full diff + existing reviews (GitHub PR or GitLab MR) and have the agent review it.
- 💬 **Free session** — an open-ended, multi-turn agent chat scoped to a project.

Run capabilities:

- **Multi-turn conversations** with full input control (send messages, attach images, end input, let the agent finish).
- **Resume** a finished session to keep going.
- **Concurrent streaming** — multiple runs stream output at the same time; output is preserved across navigation.
- **Re-run / re-fetch** — re-execute a completed run, or re-download the latest issue/PR data into an existing run.
- **History management** — pin, rename, and delete runs; bulk delete.
- **Cancel / interrupt** an in-flight run at any time.

### 2. Two interchangeable engines + handoff

- `claude` — via the **Claude Agent SDK**.
- `codex` — via **codex app-server** (JSON-RPC), translated into Claude-shaped stream-json so the UI is shared.
- **Handoff** — carry the full context from one engine to the other to continue with a different model.
- Per-run **engine and model overrides**, falling back to a global default engine and per-engine default models.

### 3. Skills & Rules management

Two parallel, engine-aware governance systems:

- **Skills** — reusable prompt/instruction packages (with YAML frontmatter), editable in-app with **CodeMirror 6** and a live markdown preview.
- **Rules** — project conventions / policies as markdown.
- **Target selection** — apply to `claude`, `codex`, or **both** (writes to `.claude/`, `.codex/`, and managed `AGENTS.md` blocks).
- **Import / export** — Skills as ZIP packages, Rules as `.md` files.
- **Hash-tracked sync** — when a project's applied copy drifts from the source, a **sync conflict** is raised (tracked independently per engine) for controlled resolution (use source / keep local / custom).

### 4. MCP servers — centralized management

Manage **Model Context Protocol** servers in one place and enable them per project, the same way you manage Skills and Rules.

- **Two transports** — `stdio` (command + args + env) and remote **HTTP / SSE** (url + headers).
- **Central definition, per-project enable** — define a server once, then toggle which projects may use it.
- **Injected at launch** — enabled servers are wired into a run automatically: Claude via the Agent SDK `mcpServers` option, Codex via `-c mcp_servers.*` config overrides.
- **Engine-aware** — Codex runs use `stdio` and streamable HTTP servers; legacy SSE servers are skipped for Codex and a note is written to the run log. Resolution follows the run's *actual* engine (honoring per-run overrides).
- **Test connection** — verify a server with a real MCP `initialize` handshake before saving.
- **Import / export** server definitions as JSON.
- **Secrets protected** — `env` / header values are stored in the **OS Keychain**; the database keeps only the key names, never the values.

### 5. Multi-project workspace

- Manage many projects; auto-detect repos and GitHub metadata inside a folder.
- Support for **sub-repos** within a project.
- **Workspace tabs** for parallel multi-project browsing, remembering the last-viewed run per project (persisted to localStorage).
- **Active Runs dock** — a sidebar panel showing every streaming / awaiting-permission run across all projects, with live status indicators and a permission counter.

### 6. Permission center

- Interactive **permission prompts** for tool use, with **diff previews** for `Edit` / `Write` / `MultiEdit`.
- Structured **question prompts** (`AskUserQuestion`) with selectable options.
- **Allow / deny / auto-allow** per session, with session-wide allowlist tracking.
- Toast notifications when a run needs permission or completes.
- Configurable default permission mode.

### 7. Integrated file viewer & code navigation

- **Multi-format viewer**: syntax-highlighted code (CodeMirror + Shiki), rendered & raw markdown, images, video, audio, PDF, with external fallback.
- **Mermaid diagrams** rendered inline with zoom & pan, theme-aware caching, and source fallback.
- **Clickable file links** — inline code that matches a project file becomes a link; line references (`#L100`, `#L100-L200`, `:100`) jump to position.
- **Mentioned files** panel — tracks every file touched in a session, grouped by action (read / edit / write) with visit counts.
- **Pop-out file window** for full-screen viewing.
- Open in **VS Code**, **Finder**, or **Terminal** (configurable terminal app).

### 8. Usage, cost & budget tracking

- **Stats dashboard** — daily token & cost charts (Chart.js), with doughnut breakdowns by engine / project / model and time-range filters (7 / 30 / 90 days / all-time).
- **Real rate-limit utilization** of your claude.ai plan (from `/usage` data), including 5h and 7-day windows with reset countdowns.
- **Per-run usage ledger** — prefers the real cost from the Claude SDK, estimates for Codex; rows are self-contained so they survive deletion of the run or project.
- **Token budget guardrail** — monthly / weekly / rolling window limit with a soft warning threshold and a hard block; sourced from the real plan first, falling back to a self-imposed token limit.
- **Context meter** — real-time context-window usage vs. the model's limit, with a `/compact` quick action.
- **Backfill** usage from existing run logs, or **reset** the ledger.

### 9. Session mirroring

- Auto-discovers and mirrors shared transcripts from the **Claude CLI / VS Code extension** (`~/.claude/projects`) and **Codex CLI** (`~/.codex/sessions`).
- A background **file watcher** (debounced) keeps Devdy in sync with external edits live.
- On-demand reconcile commands and automatic reconcile on project open.
- Deleted-session tombstones prevent re-importing sessions you removed.

### 10. Account management

- Manage multiple **GitHub** and **GitLab** accounts; validate PAT scopes and cache the username (GitLab also stores host + commit email).
- Link an account per project for issue/PR fetching and git operations during runs.
- All PATs are stored **only in the OS Keychain** — see [Security](#security--credential-isolation).
- Engines authenticate via the existing CLI login — Devdy manages no API keys.

### 11. Settings

- Global default engine and per-engine model selection.
- CLI paths and extra arguments per engine.
- Theme (dark / light / system) — dark-first indigo design system.
- Token budget period & limit, context-warning threshold, default permission mode.
- Customizable **Analyze Issue** / **Review PR** prompts.
- Terminal app selection.
- Read-only subscription plan monitoring.

### 12. Storage management

- Disk-usage breakdown across Devdy run logs, Claude CLI sessions, and Codex CLI sessions.
- Clean up transcripts by category (non-destructive for Devdy logs).

---

## Security & credential isolation

Devdy treats every credential as a secret that must never reach disk, logs, or the AI engine's process environment. Security is enforced in Rust, at the layer that spawns runs.

- **No API keys.** Engines authenticate through your existing CLI login (Claude subscription / ChatGPT). Devdy never stores or transmits an engine API key.
- **Secrets in the OS Keychain only.** GitHub/GitLab PATs and MCP `env`/header values live in the OS Keychain (via the `keyring` crate). SQLite stores only a reference or the *key names* — never the secret value. Secrets are never logged, traced, or placed in error messages.
- **Per-run credential broker.** Tokens are never exported into the sidecar/engine environment (no `GH_TOKEN` / `GITLAB_TOKEN`). Instead, each run gets an isolated `gh` / `glab` / git-credential **shim** prepended to its `PATH`, which talks to an in-process broker over a **per-run Unix socket**. The real token is released only at the socket, per request, for the linked account.
- **Fail-closed.** If no account is linked (or its PAT is missing), the broker returns nothing and an *allowed* action collapses to a *deny* at the socket layer — it never silently falls back to ambient/global credentials.
- **Isolated git config.** Devdy configures git's credential helper per-run via `GIT_CONFIG_COUNT`/`GIT_CONFIG_*` env, so a run never reads from or writes to your global `~/.gitconfig`.
- **Policy, approval & audit.** Broker requests pass through a policy layer with an approval step and an audit trail (`runs/broker/{policy,approver,audit,token}.rs`).
- **MCP secrets brokered like PATs.** MCP credentials follow the same rule: stored in the Keychain, materialized only when building the engine config at launch, and dropped for HTTP/SSE servers on Codex runs.
- **Local-first.** All data — SQLite, run logs, transcripts — is stored locally. There is no cloud sync.

---

## Architecture overview

The data flow of a **run** spans four layers:

```
Vue (liveRuns store) ──invoke──▶ Rust commands ──spawn──▶ Node sidecar ──stdio──▶ claude/codex CLI
       ▲                              │  ▲                      │
       │                              │  └── credential broker ─┘ (per-run Unix socket)
       └────── Tauri events ──────────┴── NDJSON drain ─────────┘
```

1. **Frontend** (Vue 3) calls `start_run` / `resume_run` / `send_user_message` via Tauri `invoke`, and listens to per-run events.
2. **Rust commands** resolve engine + model + paths + enabled MCP servers, wire the **credential broker**, spawn the **Node sidecar**, register it in the `RunRegistry`, and launch a `drain_sidecar` task.
3. **Sidecars** translate between the broker and the actual engine:
   - `sidecar/` — hosts the **Claude Agent SDK**.
   - `sidecar-codex/` — drives `codex app-server` and **translates its output into Claude-shaped stream-json**.
4. **`drain_sidecar`** reads the sidecar's stdout line-by-line, persists the stream-json log, captures usage, and re-emits to the frontend.

### Storage layout

```
~/.devdy/
├── data.db                    # SQLite (projects, runs, skills, rules, MCP servers, usage ledger, settings)
├── skills/{name}/SKILL.md     # Skill sources
└── rules/{name}.md            # Rule sources

OS Keychain                    # GitHub/GitLab PATs + MCP env/header secret values (never in the DB)

<project>/.devdy/
├── runs/{run_id}.log          # NDJSON stream-json transcript
└── tasks/issue-{n}/, pr-{n}/  # Fetched GitHub issue / PR markdown

~/.claude/projects/.../*.jsonl # Shared with Claude CLI / VS Code
~/.codex/sessions/.../*.jsonl  # Shared with Codex CLI
```

---

## Tech stack

| Layer | Technology |
|-------|------------|
| Frontend | Vue 3, Pinia, TanStack Query, Vue Router, Tailwind v4, CodeMirror 6, Shiki, markdown-it, Mermaid, Chart.js, lucide-vue-next |
| Backend | Rust, Tauri 2, sqlx (SQLite), `keyring` (OS Keychain), `notify` (file watcher) |
| Sidecar | Node.js (≥ 22), `@anthropic-ai/claude-agent-sdk`, `codex app-server` |
| Storage | Local SQLite + stream-json log files + OS Keychain |

---

## Requirements

- **Node ≥ 22**, **pnpm** (via corepack)
- **Rust stable** + Tauri 2 toolchain
- `claude` CLI logged into a subscription
- `codex` CLI logged into ChatGPT

## Quick start

```bash
pnpm install                  # frontend deps
npm --prefix sidecar install  # Claude Agent SDK sidecar deps

pnpm tauri dev                # run the app (Vite + Tauri, hot reload)
pnpm tauri build              # build a production bundle
```

---
