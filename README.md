# Devdy

> A desktop app for developers — centrally manage AI Skills/Rules and automate GitHub Issue analysis / Pull Request review, running directly on your existing CLI subscription.

## What is Devdy?

**Devdy** is a macOS-first desktop app built on **Tauri 2 + Vue 3 + Rust**. It is an IDE companion that drives two AI engines — `claude` and `codex` — to analyze code, review PRs, and run free-form agent sessions, while centrally managing your reusable **Skills** and **Rules**.

Core differentiators:

- ✅ **No API keys** — runs on your existing CLI login/subscription (`claude` logged into a subscription, `codex` logged into ChatGPT).
- ✅ **Local-first** — all data lives in local SQLite + log files; no cloud sync.
- ✅ **Secure** — GitHub PATs are kept only in the OS Keychain, never written to disk or logs.
- ✅ **Interoperable** — mirrors the same session transcripts used by the Claude CLI and VS Code extension, so your history is shared, not siloed.

---

## Features

### 1. Runs — the heart of the app

A **run** is one execution of an AI engine. Devdy supports three kinds:

- 🔍 **GitHub Issue analysis** — fetch an issue + its comments (bots filtered out) and let the agent investigate.
- 👀 **Pull Request review** — fetch the full diff + existing reviews and have the agent review it.
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
- Per-run **engine and model overrides**, falling back to per-project and global defaults.

### 3. Skills & Rules management

Two parallel, engine-aware governance systems:

- **Skills** — reusable prompt/instruction packages (with YAML frontmatter), editable in-app with **CodeMirror 6** and a live markdown preview.
- **Rules** — project conventions / policies as markdown.
- **Target selection** — apply to `claude`, `codex`, or **both** (writes to `.claude/`, `.codex/`, and managed `AGENTS.md` blocks).
- **Import / export** — Skills as ZIP packages, Rules as `.md` files.
- **Hash-tracked sync** — when a project's applied copy drifts from the source, a **sync conflict** is raised (tracked independently per engine) for controlled resolution (use source / keep local / custom).

### 4. Multi-project workspace

- Manage many projects; auto-detect repos and GitHub metadata inside a folder.
- Support for **sub-repos** within a project.
- **Workspace tabs** for parallel multi-project browsing, remembering the last-viewed run per project (persisted to localStorage).
- **Active Runs dock** — a sidebar panel showing every streaming / awaiting-permission run across all projects, with live status indicators and a permission counter.

### 5. Permission center

- Interactive **permission prompts** for tool use, with **diff previews** for `Edit` / `Write` / `MultiEdit`.
- Structured **question prompts** (`AskUserQuestion`) with selectable options.
- **Allow / deny / auto-allow** per session, with session-wide allowlist tracking.
- Toast notifications when a run needs permission or completes.
- Configurable default permission mode.

### 6. Integrated file viewer & code navigation

- **Multi-format viewer**: syntax-highlighted code (CodeMirror + Shiki), rendered & raw markdown, images, video, audio, PDF, with external fallback.
- **Mermaid diagrams** rendered inline with zoom & pan, theme-aware caching, and source fallback.
- **Clickable file links** — inline code that matches a project file becomes a link; line references (`#L100`, `#L100-L200`, `:100`) jump to position.
- **Mentioned files** panel — tracks every file touched in a session, grouped by action (read / edit / write) with visit counts.
- **Pop-out file window** for full-screen viewing.
- Open in **VS Code**, **Finder**, or **Terminal** (configurable terminal app).

### 7. Usage, cost & budget tracking

- **Stats dashboard** — daily token & cost charts (Chart.js), with doughnut breakdowns by engine / project / model and time-range filters (7 / 30 / 90 days / all-time).
- **Real rate-limit utilization** of your claude.ai plan (from `/usage` data), including 5h and 7-day windows with reset countdowns.
- **Per-run usage ledger** — prefers the real cost from the Claude SDK, estimates for Codex; rows are self-contained so they survive deletion of the run or project.
- **Token budget guardrail** — monthly / weekly / rolling window limit with a soft warning threshold and a hard block; sourced from the real plan first, falling back to a self-imposed token limit.
- **Context meter** — real-time context-window usage vs. the model's limit, with a `/compact` quick action.
- **Backfill** usage from existing run logs, or **reset** the ledger.

### 8. Session mirroring

- Auto-discovers and mirrors shared transcripts from the **Claude CLI / VS Code extension** (`~/.claude/projects`) and **Codex CLI** (`~/.codex/sessions`).
- A background **file watcher** (debounced) keeps Devdy in sync with external edits live.
- On-demand reconcile commands and automatic reconcile on project open.
- Deleted-session tombstones prevent re-importing sessions you removed.

### 9. Account management & security

- Manage multiple **GitHub accounts**; validate PAT scopes and cache the username.
- Link a GitHub account per project for issue/PR fetching.
- PATs stored **only in the OS Keychain** (`keyring`); the DB keeps just a reference, never the secret.
- Engines authenticate via the existing CLI login — Devdy manages no API keys.

### 10. Settings

- Default engine and per-engine model selection.
- CLI paths and extra arguments per engine.
- Theme (dark / light / system) — dark-first indigo design system.
- Token budget period & limit, context-warning threshold, default permission mode.
- Customizable **Analyze Issue** / **Review PR** prompts.
- Terminal app selection.
- Read-only subscription plan monitoring.

### 11. Storage management

- Disk-usage breakdown across Devdy run logs, Claude CLI sessions, and Codex CLI sessions.
- Clean up transcripts by category (non-destructive for Devdy logs).

---

## Architecture overview

The data flow of a **run** spans four layers:

```
Vue (liveRuns store) ──invoke──▶ Rust commands ──spawn──▶ Node sidecar ──stdio──▶ claude/codex CLI
       ▲                              │                         │
       └────── Tauri events ──────────┴── NDJSON drain ─────────┘
```

1. **Frontend** (Vue 3) calls `start_run` / `resume_run` / `send_user_message` via Tauri `invoke`, and listens to per-run events.
2. **Rust commands** resolve engine + model + paths, spawn the **Node sidecar**, register it in the `RunRegistry`, and launch a `drain_sidecar` task.
3. **Sidecars** translate between the broker and the actual engine:
   - `sidecar/` — hosts the **Claude Agent SDK**.
   - `sidecar-codex/` — drives `codex app-server` and **translates its output into Claude-shaped stream-json**.
4. **`drain_sidecar`** reads the sidecar's stdout line-by-line, persists the stream-json log, captures usage, and re-emits to the frontend.

### Storage layout

```
~/.devdy/
├── data.db                    # SQLite (projects, runs, skills, rules, usage ledger, settings)
├── skills/{name}/SKILL.md     # Skill sources
└── rules/{name}.md            # Rule sources

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
| Backend | Rust, Tauri 2, sqlx (SQLite), `keyring`, `notify` (file watcher) |
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
