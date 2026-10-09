# SRS — Slack Thread Inbox (Chrome extension → Devdy)

Status: frozen for implementation (v1). Branch: `feat/slack-thread-inbox`.

## 1. Goal

A Chrome extension exports a Slack thread and pushes it to Devdy over a **local
HTTP API**. Devdy stores it, shows a management screen analogous to Quick Notes,
and exposes MCP tools (`mcp__devdy__slack_threads_*`) so AI sessions can work
with threads exactly like `notes_*`.

The Chrome extension itself is OUT OF SCOPE for v1 — only the Devdy side plus a
contract doc + a curl test script.

## 2. Payload contract (what the extension sends)

Two accepted forms on the same endpoint:

| Case | Content-Type | Body |
|---|---|---|
| Thread has attachments | `application/zip` | zip archive |
| No attachments | `text/markdown` (also accept `text/plain`) | the raw `.md` (UTF-8) |

Zip layout rules:
- Exactly ONE `.md` file is the conversation. Pick the `.md` at the zip root; if
  none at root, the only `.md` anywhere; if ambiguous (several) → 400.
- Every other regular file (any depth) is an attachment. Keep its path relative
  to the zip root (e.g. `attachments/screenshot.png`) for display, store files
  flattened under the thread's attachment dir with collision-safe names.
- Ignore `__MACOSX/`, `.DS_Store`, directories.
- Security: reject absolute paths / `..` (zip-slip), max 200 entries, max
  50 MB request body, max 200 MB total uncompressed (zip-bomb guard).

Markdown front matter (OPTIONAL, YAML between leading `---` lines). Known keys:

```yaml
---
title: "Deploy failed on staging"
workspace: "malme"            # Slack workspace/team name or domain
channel: "#dev-backend"
thread_url: "https://malme.slack.com/archives/C0123/p1700000000123456"
thread_ts: "1700000000.123456"
exported_at: "2026-10-08T09:00:00Z"
---
```

- Unknown keys are ignored. Body (after front matter) is stored as `content`
  (store the FULL original md including front matter? → NO: store body only in
  `content`, parsed fields in columns).
- `title` fallback: first non-empty line of the body stripped of `#`, max 120
  chars; else `"Slack thread"`.
- Optional request headers that override front matter: `X-Devdy-Project-Id`
  (Devdy project id to link), `X-Devdy-Title`.

Dedup / upsert: if `thread_url` (non-empty) — or else (`channel`,`thread_ts`)
both non-empty — matches an existing row, UPDATE that row (replace content,
metadata, and ALL attachments; keep id, position, project_id unless header
overrides). Otherwise INSERT at the top of the list.

## 3. Local HTTP API

- Bind `127.0.0.1` only. Port: try `47821`, fall back sequentially up to `47830`.
  Record the bound port in managed state.
- Auth: `Authorization: Bearer <token>`. Token = random 32-byte hex, generated on
  first start, persisted in the `settings` table under key
  `inbox_api_token`. Regenerable from Settings.
- Origin check: if an `Origin` header is present it must start with
  `chrome-extension://` (or `moz-extension://`), else 403. `Host` header must be
  `127.0.0.1:<port>` or `localhost:<port>`, else 403 (DNS-rebinding guard).
- CORS: answer `OPTIONS` preflight for allowed origins (allow headers
  `Authorization, Content-Type, X-Devdy-Project-Id, X-Devdy-Title`).

Endpoints:

| Method | Path | Auth | Response |
|---|---|---|---|
| GET | `/health` | no | `{"app":"devdy","version":"…","api":1}` |
| GET | `/v1/projects` | yes | `[{"id","name"}]` so the extension can pick a project |
| POST | `/v1/slack-threads` | yes | `201 {"id","status":"created"}` or `200 {"id","status":"updated"}` |

Errors: JSON `{"error":"…"}` with 400 (bad payload), 401, 403, 413, 415, 500.

On every create/update/delete the backend emits Tauri event
`slack-threads://changed` with `{ "id": "<id>", "action": "created|updated|deleted" }`
to all windows, and shows an OS notification "Slack thread received: <title>"
on create/update via the API.

## 4. Storage

Migration `src-tauri/migrations/0038_slack_threads.sql`:

```sql
CREATE TABLE IF NOT EXISTS slack_threads (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL DEFAULT '',
  content     TEXT NOT NULL DEFAULT '',      -- markdown body (no front matter)
  workspace   TEXT,
  channel     TEXT,
  thread_url  TEXT,
  thread_ts   TEXT,
  exported_at TEXT,
  project_id  TEXT,
  position    INTEGER NOT NULL DEFAULT 0,
  created_at  TEXT NOT NULL,
  updated_at  TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_threads_position ON slack_threads(position);
CREATE INDEX IF NOT EXISTS idx_slack_threads_project  ON slack_threads(project_id);
CREATE INDEX IF NOT EXISTS idx_slack_threads_url      ON slack_threads(thread_url);

CREATE TABLE IF NOT EXISTS slack_thread_attachments (
  id         TEXT PRIMARY KEY,
  thread_id  TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  name       TEXT NOT NULL,      -- original relative path inside the zip
  file_path  TEXT NOT NULL,      -- path RELATIVE to <app_data>, e.g. slack-threads/<id>/files/x.png
  size       INTEGER NOT NULL,
  mime       TEXT,
  created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_thread_att_thread ON slack_thread_attachments(thread_id);
```

Files: `<app_data>/slack-threads/<thread_id>/files/<safe_name>` where
`<app_data>` is the dir containing `data.db` (same as skills/rules). Deleting a
thread deletes its row, attachment rows AND the directory (do not rely only on
FK cascade — delete attachment rows explicitly). `position` semantics identical
to `notes` (ascending, new rows = MIN(position)-1).

## 5. Tauri commands (backend ⇄ frontend contract)

Module `src-tauri/src/commands/slack_threads.rs` (+ `src-tauri/src/inbox_api/`
for the HTTP server). Names/shapes are FIXED — FE codes against them:

```ts
interface SlackThreadAttachment { id: string; threadId: string; name: string; filePath: string /* ABSOLUTE */; size: number; mime: string | null; createdAt: string }
interface SlackThread {
  id: string; title: string; content: string;
  workspace: string | null; channel: string | null; threadUrl: string | null; threadTs: string | null; exportedAt: string | null;
  projectId: string | null; position: number; createdAt: string; updatedAt: string;
  attachmentCount: number;
}
interface SlackThreadDetail extends SlackThread { attachments: SlackThreadAttachment[] }
interface InboxApiInfo { running: boolean; port: number | null; token: string; baseUrl: string | null }
```
(serde `rename_all = "camelCase"`.)

| Command | Args | Returns |
|---|---|---|
| `list_slack_threads` | `projectId?: string` | `SlackThread[]` (ORDER BY position ASC, updated_at DESC; content included) |
| `get_slack_thread` | `id` | `SlackThreadDetail` |
| `update_slack_thread` | `id, title?: string, content?: string` | `SlackThread` |
| `set_slack_thread_project` | `id, projectId: string \| null` | `()` |
| `delete_slack_threads` | `ids: string[]` | `()` |
| `reorder_slack_threads` | `ids: string[]` | `()` |
| `import_slack_thread_file` | `path: string` (a .zip or .md chosen in a file dialog) | `{ id, status }` — same parsing as the HTTP API (manual import fallback) |
| `get_inbox_api_info` | — | `InboxApiInfo` |
| `regenerate_inbox_api_token` | — | `InboxApiInfo` |

Every mutating command emits `slack-threads://changed`.

## 6. Frontend

Follow skill `fe-ui-consistency`. Mirror the Notes UX:
- Add a third kind `slack` to the item list window (`src/views/ItemListWindow.vue`,
  `src/lib/itemListWindow.ts`) — or a sibling window if cleaner — listing threads as
  cards: title, `#channel · workspace`, attachment count, relative time,
  project badge. Search (title/content/channel), project filter, multi-delete
  with confirm, drag reorder if notes have it.
- Detail view (item window or equivalent): markdown render of `content`
  (reuse `MarkdownPreview.vue`), metadata header with "Open in Slack" (opener
  plugin on `threadUrl`), attachment list (name, size; click = open with OS via
  opener; images show inline thumbnail via asset protocol `convertFileSrc`),
  edit title, assign project, delete. Content editing optional (textarea like
  notes) — implement if the notes item window already supports it cheaply.
- Entry point: wherever Notes/Todos list is opened (tray/menu/sidebar/shortcut),
  add "Slack threads" next to it. Also a manual "Import .zip/.md" button.
- Live refresh on `slack-threads://changed`.
- Settings: new section "Inbox API (Chrome extension)": status, base URL,
  token (masked, copy, regenerate with confirm), short help text.
- Pinia store `src/stores/slackThreads.ts`. All UI strings via i18n in every
  locale file (run `pnpm check:i18n`).

## 7. MCP tools (sidecar-mcp, server `devdy`)

Mirror `notes_*` in `sidecar-mcp/index.mjs` + `lib/store.mjs` (reads/writes the
same SQLite via `DEVDY_DB_PATH`; scope semantics identical to notes:
`scope=project` (default, uses `DEVDY_PROJECT_ID`) | `all`).

| Tool | Behaviour |
|---|---|
| `slack_threads_list` | id, title, channel, attachment count, updated_at, preview |
| `slack_threads_read` | full markdown + metadata + attachment list with ABSOLUTE paths, size, mime (so the AI can open files with its own Read tool) |
| `slack_threads_search` | query over title/content/channel, snippet |
| `slack_threads_update` | title and/or content |
| `slack_threads_append` | append text to content |
| `slack_threads_delete` | ids[]; also removes `<app_data>/slack-threads/<id>` dir |
| `slack_threads_set_project` | link to current project or unlink |
| `slack_threads_reorder` | ids[] |
| `slack_threads_read_attachment` | `id` (attachment id): text-like files (≤ 1 MB, utf-8 decodable) returned inline; otherwise return path + mime and tell the AI to use its file reader |

Writes from MCP must bump `updated_at`. (The app will not get a Tauri event from
the sidecar; the FE should also refetch on window focus, same as notes if they do.)

## 8. Acceptance criteria

1. `curl` POST of a `.md` → 201, row visible in UI within 1 s without reload.
2. `curl` POST of a zip (md + 2 files incl. a png) → attachments stored, listed, png thumbnail shown, open-with-OS works.
3. Same `thread_url` posted twice → one row, status `updated`, attachments replaced.
4. Missing/invalid token → 401; `Origin: https://evil.com` → 403; bad Host → 403; zip with `../x` → 400; >50 MB → 413; zip with 2 root md → 400.
5. Delete from UI removes row + files on disk.
6. MCP: `slack_threads_list/read/search/update/append/delete/set_project/reorder/read_attachment` work against real DB (smoke test script).
7. Settings shows port + token; regenerate invalidates old token immediately.
8. `cargo check`, `cargo test` (unit tests for zip/front-matter parsing + guards), `pnpm typecheck`, `pnpm check:i18n`, lint pass.

## 9. Deliverables besides code

- `docs/slack-thread-inbox-api.md` — contract for the extension developer (copy of §2–3 with curl examples).
- `scripts/send-slack-thread.sh` — `./scripts/send-slack-thread.sh <file.md|file.zip> [token] [port]`.
