# Devdy

> A desktop app for developers — manage AI Skills/Rules and automate GitHub Issue analysis / Pull Request review, running directly on your existing CLI subscription.

## What is Devdy?

**Devdy** is a desktop app (macOS-first) built on **Tauri 2 + Vue 3 + Rust**. It centrally manages **AI Skills / Rules** and automates development tasks by driving two AI engines: `claude` and `codex`.

Core differentiators:

- ✅ **No API keys** — runs on your existing CLI login/subscription (`claude` logged into a subscription, `codex` logged into ChatGPT).
- ✅ **Local-first** — all data stored in local SQLite, no cloud sync.
- ✅ **Secure** — GitHub PATs kept only in the OS Keychain, never written to disk or logs.

## Key features

### 1. Run an AI engine (the "run")
The heart of the app. Each **run** is one execution of an AI engine over a prompt, in three forms:

- 🔍 **GitHub Issue analysis**
- 👀 **Pull Request review**
- 💬 **Free session**

Supports:
- **Multi-turn** — multiple conversation turns within one session.
- **Resume** — continue a finished session.
- **Concurrent streaming** — multiple runs streaming output at once; output is preserved across navigation.

### 2. Two interchangeable engines
- `claude` — via the **Claude Agent SDK**.
- `codex` — via **codex app-server** (JSON-RPC).
- **Handoff** — carry the full context from one engine to the other.

Both engines speak the same internal protocol, so the rendering UI, permission modals, etc. are shared without modification.

### 3. Skills & Rules management
- Edit directly in-app with **CodeMirror 6**.
- **Apply** into a project's working tree, targeting `claude`, `codex`, or both.
- Hash-tracked sync; when a project's copy drifts from the source → a **sync conflict** is created for controlled resolution.

### 4. Account management & security
- GitHub PATs kept only in the **OS Keychain** (via `keyring`).
- The DB stores only the key reference, never the secret.
- Engines authenticate via the existing CLI login — the app manages no API keys.

### 5. Usage & cost tracking
- Shows the real **rate-limit utilization** of your claude.ai plan (`/usage` data).
- Records **tokens & cost** per run: prefers the real cost from the Claude SDK, estimates for Codex.
- Usage rows are stored self-contained, so they survive deletion of the run or project.

### 6. Session mirroring
Automatically discovers and mirrors shared Claude transcripts (from the CLI / VSCode) via a file watcher.

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
   - `sidecar/` — hosts the Claude Agent SDK.
   - `sidecar-codex/` — drives `codex app-server` and **translates its output into Claude-shaped stream-json**.
4. **`drain_sidecar`** reads the sidecar's stdout line-by-line, persists the stream-json log, captures usage, and re-emits to the frontend.

## Tech stack

| Layer | Technology |
|-------|------------|
| Frontend | Vue 3, Pinia, TanStack Query, Tailwind v4, CodeMirror 6 |
| Backend | Rust, Tauri 2, sqlx (SQLite) |
| Sidecar | Node.js (≥ 22), `@anthropic-ai/claude-agent-sdk`, codex app-server |
| Storage | Local SQLite + stream-json log files |

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
