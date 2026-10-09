# SRS addendum — Web page capture + generic "captures" core

Status: frozen for implementation (v1). Branch: `feat/slack-thread-inbox`.
Extends `docs/srs-slack-thread-inbox.md` (everything there still applies unless
overridden here). Decisions approved by the user: architecture B (shared core,
per-kind surfaces), dedup by normalized URL, ONE combined composer picker, no
phase-2 extras (tags, re-capture, unified inbox).

## 1. Goal

The Chrome extension can already export any web page to a `.md` file. It must be
able to push that md (or a zip with md + images) to Devdy exactly like a Slack
thread. Devdy stores it as a capture of kind `web`, shows it in a "Web" tab,
lets AI read it via `mcp__devdy__web_pages_*`, and the composer picker inserts
references to either kind.

## 2. Storage — migration `0040_captures.sql`

Rename + extend (nothing is shipped yet, but the dev DB already applied 0038/0039,
so DO NOT edit those):

```sql
ALTER TABLE slack_threads RENAME TO captures;
ALTER TABLE captures ADD COLUMN kind TEXT NOT NULL DEFAULT 'slack';   -- 'slack' | 'web'
ALTER TABLE captures ADD COLUMN source_url  TEXT;   -- web: page URL as sent
ALTER TABLE captures ADD COLUMN url_key     TEXT;   -- web: normalized URL (dedup key)
ALTER TABLE captures ADD COLUMN site_name   TEXT;
ALTER TABLE captures ADD COLUMN author      TEXT;
ALTER TABLE captures ADD COLUMN published_at TEXT;
ALTER TABLE captures ADD COLUMN description TEXT;
ALTER TABLE captures ADD COLUMN selection   INTEGER NOT NULL DEFAULT 0; -- 1 = only the user's selection
ALTER TABLE slack_thread_attachments RENAME TO capture_attachments;
ALTER TABLE capture_attachments RENAME COLUMN thread_id TO capture_id;
CREATE INDEX IF NOT EXISTS idx_captures_kind    ON captures(kind);
CREATE INDEX IF NOT EXISTS idx_captures_url_key ON captures(url_key);
```
(Old index names may remain; that is fine. Verify FK on capture_attachments still
points at `captures` after the rename — SQLite ≥ 3.26 rewrites it; add a test.)

Column reuse: `exported_at` = capture/export time for both kinds (front matter
`captured_at` for web maps to it). Slack-only columns (`workspace`, `channel`,
`thread_url`, `thread_ts`) stay NULL for web. `title_custom` works for both.
`position` ordering is per kind (MIN(position) taken within the same kind).

Files: slack → `<app_data>/slack-threads/<id>/files/` (unchanged), web →
`<app_data>/web-pages/<id>/files/`. Delete removes the dir for the row's kind.

## 3. HTTP API

New endpoint `POST /v1/web-pages` — same auth/guards/limits/content types/zip
rules/headers (`X-Devdy-Project-Id`, `X-Devdy-Title`) as `/v1/slack-threads`.
`/v1/slack-threads` is unchanged externally.

Web front matter (all optional, unknown keys ignored):
```yaml
---
title: "How to configure Tauri capabilities"
url: "https://v2.tauri.app/security/capabilities/"
site_name: "Tauri"
author: "…"
published_at: "2026-09-01"
captured_at: "2026-10-09T10:00:00Z"
description: "…"
selection: false
---
```
Title fallback: header → `title` → first body line → domain of `url` → `"Web page"`.

Dedup (web):
- `url_key` = normalize(`url`): lowercase scheme+host, drop default port, drop
  `#fragment`, drop query params `utm_*`, `fbclid`, `gclid`, `mc_cid`, `mc_eid`,
  `ref`, `ref_src`; keep other params sorted; strip a single trailing `/` from the
  path (but keep the root `/`).
- If `url_key` matches an existing `kind='web'` row with `selection=0` → UPDATE
  (same rules as Slack: replace content/metadata/attachments, keep id/position/
  project unless header; keep renamed title when `title_custom=1`). → `200 updated`.
- `selection: true` → ALWAYS create a new row (never dedups, never is a dedup target).
- Header `X-Devdy-Mode: new` → always create a new row.
- No `url` → always create.

Response `{ "id", "status" }` as for Slack. Event + OS notification
("Web page received: <title>").

## 4. Tauri commands — generic (REPLACE the slack_* commands; FE is the only caller)

```ts
type CaptureKind = 'slack' | 'web'
interface CaptureAttachment { id; captureId; name; filePath /*ABSOLUTE*/; size; mime: string|null; createdAt }
interface Capture {
  id; kind: CaptureKind; title; content; titleCustom: boolean;
  projectId: string|null; position; createdAt; updatedAt; exportedAt: string|null; attachmentCount: number;
  // slack
  workspace: string|null; channel: string|null; threadUrl: string|null; threadTs: string|null;
  // web
  sourceUrl: string|null; siteName: string|null; author: string|null; publishedAt: string|null; description: string|null; selection: boolean;
}
interface CaptureDetail extends Capture { attachments: CaptureAttachment[] }
```

| Command | Args | Returns |
|---|---|---|
| `list_captures` | `kind?: CaptureKind, projectId?: string` | `Capture[]` |
| `get_capture` | `id` | `CaptureDetail` |
| `rename_capture` | `id, title` | `Capture` |
| `update_capture` | `id, title?, content?` | `Capture` |
| `set_capture_project` | `id, projectId: string\|null` | `()` |
| `delete_captures` | `ids: string[]` | `()` |
| `reorder_captures` | `ids: string[]` | `()` |
| `import_capture_file` | `path, kind: CaptureKind` | `{ id, status }` |
| `get_inbox_api_info` / `regenerate_inbox_api_token` | unchanged | unchanged |

Event: `captures://changed` with `{ id, kind, action }` (replaces
`slack-threads://changed`).

## 5. MCP (sidecar-mcp)

Refactor the store functions to be kind-parameterized over `captures` /
`capture_attachments`. Keep the 10 `slack_threads_*` tools (now filtered by
`kind='slack'`) and add the same 10 as `web_pages_*` (`kind='web'`):
list / read / search / rename / update / append / delete / set_project / reorder /
read_attachment. Web list/read show `site_name`/domain, `source_url`, author,
published/captured dates, `selection`.

Long content — BOTH `*_read` tools gain optional args:
- `max_chars` (default 20000), `offset` (default 0) → return that slice;
- `section` (heading text, case-insensitive) → return only that section (until the
  next heading of same or higher level).
The output always starts with metadata + an outline (markdown headings with their
char offsets) + `total_chars`, and ends with a "continue with offset=N" hint when
truncated.

Search across a kind matches title/content/channel (slack) or title/content/
site_name/source_url (web).

## 6. Frontend

- Generic store `src/stores/captures.ts` (replaces `slackThreads.ts`).
- List window: tabs `todo | note | slack | web`. Web card: domain letter-avatar
  (no network fetch), title, `domain · relative age`, a small "Excerpt" badge when
  `selection`. Same search/project filter/rename/multi-select delete/Import as Slack.
- Detail window: generalize `SlackThreadWindow.vue` → `CaptureWindow.vue` (route
  param `?captureWindow=1&id=…`); same layout. Kind-specific: Slack keeps
  `#channel · workspace` + "Open in Slack"; Web shows `domain · author · captured`
  + "Open original" (opener on `sourceUrl`), plus the "Excerpt" badge, plus a
  collapsible **outline** (TOC) built from the markdown headings, click = scroll.
  Titlebar icon by kind (Slack / Globe).
- Composer picker: rename `SlackThreadPicker.vue` → `CapturePicker.vue`, one icon
  button, segmented filter `All | Slack | Web` (default All, remembered in
  localStorage), same grouping (this project first) and search. Inserted text:
  - slack: unchanged format.
  - web: `[Web page] <title> (<domain>) — id: <id>` + newline +
    `Read it with the web_pages_read tool (use max_chars/offset or section for long pages).`
  RunView.vue: the USER has uncommitted edits there — change ONLY the existing
  import line + component tag (+ handler name if needed). Nothing else.
- Settings Inbox API help text mentions both endpoints. i18n en + vi.

## 7. Acceptance criteria

1. POST md to `/v1/web-pages` → 201, visible in Web tab live.
2. Re-POST same URL with `?utm_source=x#frag` → 200 updated, same id; renamed title kept.
3. `selection: true` twice for the same URL → 2 new rows; the full-page row untouched.
4. `X-Devdy-Mode: new` → new row.
5. Zip with images → attachments under `web-pages/<id>/files/`; delete removes the dir.
6. Existing Slack flows still pass (all previous ACs), `/v1/slack-threads` unchanged.
7. Migration 0040 applies cleanly on a DB that already has 0038/0039 data (slack rows keep data + attachments).
8. MCP `web_pages_*` all work; `*_read` with `max_chars/offset/section` works on a long doc.
9. Composer picker filter All/Slack/Web inserts the right reference.
10. cargo check/test, pnpm typecheck, check:i18n, eslint, MCP smoke test pass.

## 8. Docs

Update `docs/slack-thread-inbox-api.md` → rename to `docs/inbox-api.md` (keep a
one-line stub at the old path pointing to the new one) with a Web pages section.
