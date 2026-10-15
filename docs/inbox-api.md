# Devdy Inbox API — Slack threads, web pages & translation (contract for the Chrome extension)

API version: `1`. The Devdy desktop app runs a small HTTP server on the user's
machine; the extension pushes an exported Slack thread or web page to it, and
can ask Devdy to translate text (§6). Everything below is what the server
enforces today. Both capture kinds are stored as "captures"
(`kind = slack | web`).

The whole API can be turned off in Devdy → Settings → Inbox API ("Enable Inbox
API", setting `inbox_api_enabled`, on by default). When off, nothing listens on
the port — every request fails at the connection level (connection refused),
and `/health` probes find nothing. Turning it back on binds the first free port
again (it may differ from the previous one).

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
  `Accept` (e.g. `text/event-stream` for §6) is a CORS-safelisted header, so it
  needs no entry in `Access-Control-Allow-Headers`.
  (Adding `http://127.0.0.1/*` to the extension's `host_permissions` avoids CORS
  entirely.)

## 2. Endpoints

| Method | Path | Auth | Success |
|---|---|---|---|
| GET | `/health` | no | `200 {"app":"devdy","version":"0.8.0","api":1,"features":["translate"]}` |
| GET | `/v1/projects` | yes | `200 [{"id":"…","name":"…"}]` (ordered like the Devdy sidebar) |
| POST | `/v1/slack-threads` | yes | `201 {"id":"…","status":"created"}` or `200 {"id":"…","status":"updated"}` |
| POST | `/v1/web-pages` | yes | same as above (see §4) |
| POST | `/v1/translate` | yes | `200 {"text":"…","targetLang":"vi"}` or an SSE stream (see §6) |

`features` in `/health` lists optional capabilities; check for `"translate"`
before calling `/v1/translate`.

Errors are always JSON `{"error":"<message>"}`:

| Status | When |
|---|---|
| 400 | bad payload: invalid zip, zip-slip path, >200 zip entries, no/several candidate `.md`, invalid UTF-8, invalid YAML front matter, empty body, unknown `X-Devdy-Project-Id`; translate: invalid JSON, empty `text` |
| 401 | missing / wrong bearer token |
| 403 | forbidden `Origin` or `Host` |
| 404 | unknown path |
| 413 | body > 50 MB, or zip uncompressed content > 200 MB; translate: body > 64 KB or `text` > 8000 characters |
| 415 | unsupported `Content-Type` |
| 429 | translate: 4 translations are already in flight (the 5th concurrent request is rejected) — retry later |
| 500 | storage failure |
| 502 | translate (JSON mode): the translation engine returned an error |
| 503 | translate: `"translation engine unavailable: <detail>"` (e.g. sidecar spawn failure). `"server stopping"`: the API was turned off while the request was in flight — only `/v1/translate` (JSON mode; SSE gets an `error` event instead) and capture uploads (`/v1/slack-threads`, `/v1/web-pages`; checked once, after the payload is parsed and before it is stored). `/health` and `/v1/projects` never return it. On switch-off the port is released immediately; requests already in progress may still complete. |
| 504 | translate (JSON mode): no result within 90 s |

## 3. `POST /v1/slack-threads` payload

| Case | `Content-Type` | Body |
|---|---|---|
| Thread has attachments | `application/zip` (also `application/x-zip-compressed`) | zip archive |
| No attachments | `text/markdown` or `text/plain` (`; charset=utf-8` allowed) | the raw `.md`, UTF-8 |

Optional request headers (override the front matter):

| Header | Effect |
|---|---|
| `X-Devdy-Project-Id` | link the thread to this Devdy project (id from `/v1/projects`; unknown id → 400). On update, omitting it keeps the current link. |
| `X-Devdy-Title` | title to use instead of front matter / fallback. Only send ASCII titles via this header: browsers' `fetch` cannot send non-Latin-1 header values (it throws `TypeError`). For non-ASCII titles use the front-matter `title` instead; an encoded header form is pending a product decision. |

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

## 6. `POST /v1/translate`

Translates a short text with Devdy's own translation engine — the same warm
session the in-app "Translate" popover uses, so it bills against the user's
existing Claude / Codex login. Engine, model and style always come from Devdy →
Settings (Translate); only the target language can be overridden per request.

Request:

| Item | Value |
|---|---|
| Headers | `Authorization: Bearer <token>`, `Content-Type: application/json` (`; charset=utf-8` allowed) |
| Body | `{ "text": string, "targetLang"?: string }`, ≤ 64 KB |
| `text` | non-empty (after trimming), ≤ 8000 characters |
| `targetLang` | optional language code (`vi`, `en`, `ja`, `zh`, `ko`, `fr`, `de`, `es`) or a language name; defaults to the target language in Devdy settings |
| `Accept` | contains `text/event-stream` → streamed SSE response; anything else → one JSON response |

At most 4 translations run at once: while 4 are in flight, a 5th concurrent
request gets `429`. Validation errors and
`401`/`413`/`415`/`429`/`503` are returned BEFORE any streaming, as normal JSON
`{"error":"…"}` responses with that status — also in SSE mode.

### JSON response

```
200 {"text":"Xin chào thế giới","targetLang":"vi"}
```

`502 {"error":"…"}` when the engine fails, `504 {"error":"timeout"}` after 90 s.

Engine error messages (`502` body / SSE `error` event):

| `error` | Meaning |
|---|---|
| `"translation ended unexpectedly"` | the engine session ended before answering |
| `"empty translation"` | the engine answered with no text |
| `"translation failed"` | the engine reported a failure without details |
| `"failed to send the request: <detail>"` | the prompt could not be handed to the engine |
| other text | the engine's own error message, passed through |

A request that times out gives up its turn: if that turn is still blocking the
shared engine, the next translation starts a fresh engine session.

### SSE response (`Accept: text/event-stream`)

`200 Content-Type: text/event-stream`, then:

```
event: chunk
data: {"delta":"Xin "}

event: chunk
data: {"delta":"chào thế giới"}

event: done
data: {"text":"Xin chào thế giới","targetLang":"vi"}

```

- `chunk` events carry display-only deltas; the `done` event carries the full,
  authoritative text (use it as the final result rather than the joined deltas).
- The stream ends right after ONE terminal event: `done`, or
  `event: error` / `data: {"error":"…"}` (engine error, `"timeout"` when no event
  arrives for 90 s, `"server stopping"` when the API is turned off mid-stream).
- Keep-alive comment lines (`:`) are sent periodically; ignore them.

```bash
curl -s -X POST http://127.0.0.1:$PORT/v1/translate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"text":"Hello world","targetLang":"vi"}'

curl -sN -X POST http://127.0.0.1:$PORT/v1/translate \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -H "Accept: text/event-stream" \
  -d '{"text":"Hello world"}'
```

Reading the stream with `fetch` (`EventSource` cannot send POST or headers):

```js
const res = await fetch(`http://127.0.0.1:${port}/v1/translate`, {
  method: 'POST',
  headers: {
    Authorization: `Bearer ${token}`,
    'Content-Type': 'application/json',
    Accept: 'text/event-stream',
  },
  body: JSON.stringify({ text, targetLang: 'vi' }),
});
if (!res.ok) throw new Error((await res.json()).error); // 400/401/413/415/429/503

const reader = res.body.pipeThrough(new TextDecoderStream()).getReader();
let buf = '';
let shown = '';
for (;;) {
  const { value, done } = await reader.read();
  if (done) break;
  buf += value;
  let sep;
  while ((sep = buf.indexOf('\n\n')) !== -1) {
    const block = buf.slice(0, sep);
    buf = buf.slice(sep + 2);
    let event = 'message';
    let data = '';
    for (const line of block.split('\n')) {
      if (line.startsWith('event:')) event = line.slice(6).trim();
      else if (line.startsWith('data:')) data += line.slice(5).trim();
    }
    if (!data) continue; // keep-alive comment
    const payload = JSON.parse(data);
    if (event === 'chunk') render((shown += payload.delta));
    else if (event === 'done') render(payload.text); // authoritative
    else if (event === 'error') throw new Error(payload.error);
  }
}
```

## 7. Extension fetch sketch

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

## 8. What Devdy does on receipt

The capture appears in Devdy's Slack / Web list immediately (event
`captures://changed` with `{ id, kind, action }`), an OS notification
"Slack thread received: <title>" / "Web page received: <title>" is shown, and AI
sessions can read it via the `mcp__devdy__slack_threads_*` /
`mcp__devdy__web_pages_*` tools (attachments are exposed as absolute file paths;
files live under `<app_data>/slack-threads/<id>/files/` or
`<app_data>/web-pages/<id>/files/`).
