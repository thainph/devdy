# Devdy Inbox API — Slack threads & web pages (contract for the Chrome extension)

API version: `1`. The Devdy desktop app runs a small HTTP server on the user's
machine; the extension pushes an exported Slack thread or web page to it.
Everything below is what the server enforces today. Both kinds are stored as
"captures" (`kind = slack | web`).

## 1. Connection

| Item | Value |
|---|---|
| Base URL | `http://127.0.0.1:<port>` (also `http://localhost:<port>`) |
| Port | first free port in `47821`…`47830` (usually `47821`). Shown in Devdy → Settings → Inbox API. To discover it, probe `GET /health` on each port in order. |
| Auth | `Authorization: Bearer <token>` on every `/v1/*` request. 64-char hex token, shown/copyable/regenerable in Devdy → Settings → Inbox API. Regenerating invalidates the old token immediately. |
| Binding | loopback only; not reachable from the network. |

### Browser guards

- `Origin`: if present, it MUST start with `chrome-extension://` or
  `moz-extension://`, otherwise `403`. Requests without `Origin` (curl, service
  worker fetches that omit it) are allowed.
- `Host`: MUST be `127.0.0.1:<port>` or `localhost:<port>`, otherwise `403`
  (DNS-rebinding guard).
- CORS: `OPTIONS` preflight is answered `204` with
  `Access-Control-Allow-Methods: GET, POST, OPTIONS`,
  `Access-Control-Allow-Headers: Authorization, Content-Type, X-Devdy-Project-Id, X-Devdy-Title, X-Devdy-Mode`,
  `Access-Control-Allow-Private-Network: true`, `Access-Control-Max-Age: 600`.
  Responses echo the allowed `Origin` in `Access-Control-Allow-Origin`.
  (Adding `http://127.0.0.1/*` to the extension's `host_permissions` avoids CORS
  entirely.)

## 2. Endpoints

| Method | Path | Auth | Success |
|---|---|---|---|
| GET | `/health` | no | `200 {"app":"devdy","version":"0.8.0","api":1}` |
| GET | `/v1/projects` | yes | `200 [{"id":"…","name":"…"}]` (ordered like the Devdy sidebar) |
| POST | `/v1/slack-threads` | yes | `201 {"id":"…","status":"created"}` or `200 {"id":"…","status":"updated"}` |
| POST | `/v1/web-pages` | yes | same as above (see §4) |

Errors are always JSON `{"error":"<message>"}`:

| Status | When |
|---|---|
| 400 | bad payload: invalid zip, zip-slip path, >200 zip entries, no/several candidate `.md`, invalid UTF-8, invalid YAML front matter, empty body, unknown `X-Devdy-Project-Id` |
| 401 | missing / wrong bearer token |
| 403 | forbidden `Origin` or `Host` |
| 404 | unknown path |
| 413 | body > 50 MB, or zip uncompressed content > 200 MB |
| 415 | unsupported `Content-Type` |
| 500 | storage failure |

## 3. `POST /v1/slack-threads` payload

| Case | `Content-Type` | Body |
|---|---|---|
| Thread has attachments | `application/zip` (also `application/x-zip-compressed`) | zip archive |
| No attachments | `text/markdown` or `text/plain` (`; charset=utf-8` allowed) | the raw `.md`, UTF-8 |

Optional request headers (override the front matter):

| Header | Effect |
|---|---|
| `X-Devdy-Project-Id` | link the thread to this Devdy project (id from `/v1/projects`; unknown id → 400). On update, omitting it keeps the current link. |
| `X-Devdy-Title` | title to use instead of front matter / fallback. Send UTF-8. |

### Zip layout

- Exactly ONE `.md` is the conversation: the `.md` at the zip root; if none at
  the root, the only `.md` anywhere; several candidates → 400.
- Every other regular file (any depth) is an attachment. Its path relative to
  the zip root (e.g. `attachments/screenshot.png`) is kept as the display name;
  files are stored flattened with collision-safe names.
- `__MACOSX/`, `.DS_Store` and directory entries are ignored.
- Limits: absolute paths or `..` segments → 400; max 200 entries; max 50 MB
  body; max 200 MB total uncompressed.

Example:

```
thread.md
attachments/screenshot.png
attachments/build.log
```

### Markdown + front matter

Front matter is optional YAML between leading `---` lines. Known keys (all
optional, unknown keys ignored):

```yaml
---
title: "Deploy failed on staging"
workspace: "malme"            # Slack workspace/team name or domain
channel: "#dev-backend"
thread_url: "https://malme.slack.com/archives/C0123/p1700000000123456"
thread_ts: "1700000000.123456"   # quote it to keep it a string
exported_at: "2026-10-08T09:00:00Z"
---
**Alice** 09:00 — staging deploy failed …
```

- Only the body (after the front matter) is stored as the thread content.
- Title: `X-Devdy-Title` → `title` → first non-empty body line without leading
  `#` (max 120 chars) → `"Slack thread"`.

### Dedup / update

If `thread_url` is non-empty and matches an existing thread — or, without a
`thread_url`, both `channel` and `thread_ts` match — that thread is UPDATED
(content, metadata and ALL attachments replaced; id, list position and project
link kept unless `X-Devdy-Project-Id` is sent) and the response is
`200 status:"updated"`. Otherwise a new thread is created at the top of the list
(`201 status:"created"`). Always send `thread_url` so re-exports update in place.

Renamed threads: if the user renamed the thread in Devdy, an update KEEPS that
title (the front-matter / fallback title is ignored). Only an explicit
`X-Devdy-Title` header replaces it — and a title sent via `X-Devdy-Title`
(on create or update) is itself treated as user-chosen, so later re-exports
without the header keep it too.

## 4. `POST /v1/web-pages` payload

Same auth, guards, limits, content types, zip layout and headers
(`X-Devdy-Project-Id`, `X-Devdy-Title`) as `/v1/slack-threads`. Images of the
page go in the zip next to the `.md` (e.g. `images/fig1.png`).

Extra optional header:

| Header | Effect |
|---|---|
| `X-Devdy-Mode: new` | always create a new capture, never update an existing one. Other values are ignored. (Ignored on `/v1/slack-threads`.) |

Front matter (all optional, unknown keys ignored):

```yaml
---
title: "How to configure Tauri capabilities"
url: "https://v2.tauri.app/security/capabilities/"
site_name: "Tauri"
author: "…"
published_at: "2026-09-01"
captured_at: "2026-10-09T10:00:00Z"
description: "…"
selection: false     # true = only the user's selection was captured
---
```

- Title: `X-Devdy-Title` → `title` → first non-empty body line (max 120 chars)
  → domain of `url` (without `www.`) → `"Web page"`.
- `selection` accepts `true`/`false` (also `"true"`, `"yes"`, `1`).

### Dedup (web)

The dedup key is the normalized `url`: lowercase scheme + host, default port
dropped (`:80` http, `:443` https), `#fragment` dropped, query params `utm_*`,
`fbclid`, `gclid`, `mc_cid`, `mc_eid`, `ref`, `ref_src` removed, remaining
params sorted, one trailing `/` stripped from the path (root `/` kept). E.g.
`HTTPS://Example.com:443/docs/?utm_source=x&b=2&a=1#top` →
`https://example.com/docs?a=1&b=2`.

| Request | Result |
|---|---|
| key matches an existing full-page web capture (`selection: false`) | UPDATE it → `200 updated` (same rules as Slack: content/metadata/attachments replaced; id, position and project kept unless header; a renamed title is kept unless `X-Devdy-Title`). If several full-page captures share the key (e.g. after `X-Devdy-Mode: new`), the one updated most recently is the target. |
| `selection: true` | always a NEW capture; selection captures are never update targets either |
| `X-Devdy-Mode: new` | always a NEW capture |
| no `url` (or not a `scheme://host…` URL) | always a NEW capture |
| otherwise | NEW capture (`201 created`) at the top of the Web list |

Slack threads and web pages never dedup against each other.

## 5. curl examples

```bash
TOKEN=<token from Devdy settings>
PORT=47821

curl -s http://127.0.0.1:$PORT/health

curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:$PORT/v1/projects

# Markdown only
curl -s -X POST http://127.0.0.1:$PORT/v1/slack-threads \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: text/markdown; charset=utf-8" \
  --data-binary @thread.md

# Zip with attachments, linked to a project, custom title
curl -s -X POST http://127.0.0.1:$PORT/v1/slack-threads \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/zip" \
  -H "X-Devdy-Project-Id: <project-id>" \
  -H "X-Devdy-Title: Staging deploy incident" \
  --data-binary @thread.zip

# Web page (md only)
curl -s -X POST http://127.0.0.1:$PORT/v1/web-pages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: text/markdown; charset=utf-8" \
  --data-binary @page.md

# Web page with images, forced as a new capture
curl -s -X POST http://127.0.0.1:$PORT/v1/web-pages \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/zip" \
  -H "X-Devdy-Mode: new" \
  --data-binary @page.zip
```

Or use the helpers: `./scripts/send-slack-thread.sh <file.md|file.zip> [token] [port]`
and `./scripts/send-web-page.sh <file.md|file.zip> [token] [port]`
(token/port also from `DEVDY_INBOX_TOKEN` / `DEVDY_INBOX_PORT`; port is
auto-detected when omitted; `DEVDY_INBOX_PROJECT_ID` / `DEVDY_INBOX_TITLE` set the optional
headers; `DEVDY_INBOX_MODE=new` sends `X-Devdy-Mode: new` for web pages).

## 6. Extension fetch sketch

```js
// endpoint: 'slack-threads' or 'web-pages'
const res = await fetch(`http://127.0.0.1:${port}/v1/${endpoint}`, {
  method: 'POST',
  headers: {
    Authorization: `Bearer ${token}`,
    'Content-Type': hasAttachments ? 'application/zip' : 'text/markdown; charset=utf-8',
  },
  body: hasAttachments ? zipBlob : markdownString,
});
const json = await res.json(); // { id, status } or { error }
```

## 7. What Devdy does on receipt

The capture appears in Devdy's Slack / Web list immediately (event
`captures://changed` with `{ id, kind, action }`), an OS notification
"Slack thread received: <title>" / "Web page received: <title>" is shown, and AI
sessions can read it via the `mcp__devdy__slack_threads_*` /
`mcp__devdy__web_pages_*` tools (attachments are exposed as absolute file paths;
files live under `<app_data>/slack-threads/<id>/files/` or
`<app_data>/web-pages/<id>/files/`).
