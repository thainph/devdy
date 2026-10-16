# Devdy Inbox API — Slack threads & translation (contract for the Chrome extension)

API version: `1`. The Devdy desktop app runs a small HTTP server on the user's
machine; the extension sends the link of a Slack thread for Devdy to fetch and
translate (§3), and can ask Devdy to translate text (§5). Everything below is
what the server enforces today. Slack threads are fetched by Devdy itself from
the Slack Web API.

Web page capture (`POST /v1/web-pages`, zip/markdown uploads) has been
removed: that route now answers `404`.

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
  `Access-Control-Allow-Headers: Authorization, Content-Type, X-Devdy-Project-Id, X-Devdy-Title`
  (the two `X-Devdy-*` headers are accepted for old extensions but ignored, see §3),
  `Access-Control-Allow-Private-Network: true`, `Access-Control-Max-Age: 600`.
  Responses echo the allowed `Origin` in `Access-Control-Allow-Origin`.
  `Accept` (e.g. `text/event-stream` for §5) is a CORS-safelisted header, so it
  needs no entry in `Access-Control-Allow-Headers`.
  (Adding `http://127.0.0.1/*` to the extension's `host_permissions` avoids CORS
  entirely.)

## 2. Endpoints

| Method | Path | Auth | Success |
|---|---|---|---|
| GET | `/health` | no | `200 {"app":"devdy","version":"0.8.0","api":1,"features":["translate","slack-thread-link"]}` |
| GET | `/v1/projects` | yes | `200 [{"id":"…","name":"…"}]` (ordered like the Devdy sidebar) |
| POST | `/v1/slack-threads` | yes | `202 {"id":"…","status":"created"\|"existing","fetchStatus":"…"}` (see §3) |
| POST | `/v1/translate` | yes | `200 {"text":"…","targetLang":"vi"}` or an SSE stream (see §5) |

`features` in `/health` lists optional capabilities: check for `"translate"`
before calling `/v1/translate` and for `"slack-thread-link"` before sending
Slack links to `/v1/slack-threads` (older Devdy builds expected an exported
markdown/zip there).

Errors are always JSON `{"error":"<message>"}` (plus `"code"` on
`/v1/slack-threads` 400s):

| Status | When |
|---|---|
| 400 | slack threads: `invalid_json`, `invalid_link`, `dm_unsupported`, `invalid_project`, `invalid_title`; translate: invalid JSON, empty `text` |
| 401 | missing / wrong bearer token |
| 403 | forbidden `Origin` or `Host` |
| 404 | unknown path |
| 413 | slack threads: body > 16 KB; translate: body > 64 KB or `text` > 8000 characters |
| 415 | unsupported `Content-Type` (slack threads: anything but JSON, see §3) |
| 429 | translate: 4 translations are already in flight (the 5th concurrent request is rejected) — retry later |
| 500 | storage failure |
| 502 | translate (JSON mode): the translation engine returned an error |
| 503 | translate: `"translation engine unavailable: <detail>"` (e.g. sidecar spawn failure). `"server stopping"`: the API was turned off while the request was in flight — only `/v1/translate` (JSON mode; SSE gets an `error` event instead), and `/v1/slack-threads` (checked after the body is validated). `/health` and `/v1/projects` never return it. On switch-off the port is released immediately; requests already in progress may still complete. |
| 504 | translate (JSON mode): no result within 90 s |

## 3. `POST /v1/slack-threads` — add a thread by link

The extension only sends the thread's link. Devdy fetches the whole thread
from the Slack Web API with the user's own Slack token (the
`SLACK_MCP_XOXP_TOKEN` of a Slack MCP server configured in Devdy), downloads
its files, translates every message into Vietnamese and keeps the original.
The request returns right away (`202`); the work runs in the background.

| Item | Value |
|---|---|
| Headers | `Authorization: Bearer <token>`, `Content-Type: application/json` (`; charset=utf-8` allowed) |
| Body | `{ "url": string, "projectId"?: string \| null, "title"?: string }`, ≤ 16 KB |
| `url` | `https://<workspace>.slack.com/archives/<channel>/p<16 digits>` (the "Copy link" of a message). A reply link with `?thread_ts=…&cid=…` resolves to its thread. `http` is accepted; surrounding `<…>` are stripped. Direct messages (`/archives/D…`) are rejected. |
| `projectId` | optional Devdy project id (from `/v1/projects`); unknown → 400 `invalid_project` |
| `title` | optional title, 1–200 characters after trimming, any UTF-8 (replaces the old `X-Devdy-Title` header) |

Old headers `X-Devdy-Project-Id`, `X-Devdy-Title` and `X-Devdy-Mode` are ignored on this
endpoint.

Responses:

```
202 {"id":"…","status":"created","fetchStatus":"pending"}
202 {"id":"…","status":"existing","fetchStatus":"pending"}
202 {"id":"…","status":"created","fetchStatus":"error","errorCode":"no_token"}
```

- `status: "existing"`: the same thread (same channel + thread ts) is already
  in Devdy. It is re-fetched (new / edited replies are translated, deleted ones
  disappear); its id and list position are kept, a title the user chose is
  kept (otherwise `title` is applied and becomes the user's title), and
  `projectId` is only applied when the thread has no project yet.
  `fetchStatus` may be `fetching` / `translating` when a job is already
  running — it then runs once more afterwards.
- `errorCode: "no_token"`: no Slack token is configured in Devdy; the thread is
  saved in an error state and fetched once the user adds a token and refreshes.
- When the background job ends, Devdy shows an OS notification ("Đã dịch xong
  thread Slack: <title>" or "Không lấy được thread Slack: <title>").

Errors (`{"error": "<message>", "code": "<code>"}`):

| Status | `code` | When |
|---|---|---|
| 400 | `invalid_json` | body is not a JSON object |
| 400 | `invalid_link` | `url` missing / not a Slack thread link (`"invalid Slack thread link (expected https://<workspace>.slack.com/archives/<channel>/p<16 digits>)"`) |
| 400 | `dm_unsupported` | direct-message link (`"direct-message threads are not supported"`) |
| 400 | `invalid_project` | unknown or non-string `projectId` |
| 400 | `invalid_title` | `title` empty / > 200 characters / not a string |
| 413 | — | body > 16 KB |
| 415 | — | `Content-Type` is not JSON (zip / markdown included): `"Content-Type must be application/json ({\"url\": \"<slack thread link>\"}); zip/markdown uploads are no longer supported"` |

Errors that happen later (token without access, missing Slack scopes, thread
deleted on Slack, rate limits …) are shown in Devdy, not returned here.

## 4. curl examples

```bash
TOKEN=<token from Devdy settings>
PORT=47821

curl -s http://127.0.0.1:$PORT/health

curl -s -H "Authorization: Bearer $TOKEN" http://127.0.0.1:$PORT/v1/projects

# Slack thread by link (optional project + title)
curl -s -X POST http://127.0.0.1:$PORT/v1/slack-threads \
  -H "Authorization: Bearer $TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"url":"https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359","projectId":"<project-id>","title":"Sự cố deploy staging"}'
```

Or use the helper `./scripts/send-slack-thread.sh <slack-link> [token] [port]`
(JSON body; `DEVDY_INBOX_PROJECT_ID` / `DEVDY_INBOX_TITLE` fill `projectId` /
`title`). Token/port also come from `DEVDY_INBOX_TOKEN` / `DEVDY_INBOX_PORT`;
the port is auto-detected when omitted.

## 5. `POST /v1/translate`

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

## 6. Extension fetch sketch

```js
// Slack thread: send the link only.
const health = await (await fetch(`http://127.0.0.1:${port}/health`)).json();
if (health.features?.includes('slack-thread-link')) {
  const res = await fetch(`http://127.0.0.1:${port}/v1/slack-threads`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: JSON.stringify({ url: threadLink, projectId, title }),
  });
  const json = await res.json(); // 202 { id, status, fetchStatus, errorCode? } or { error, code }
}
```

## 7. What Devdy does on receipt

- Slack thread: it appears in Devdy's Slack thread window right away
  (`slack-threads://changed`), then progresses `pending → fetching →
  translating → ready` (or `error`) with `slack-threads://progress` events.
  Messages are stored with the original text and the Vietnamese translation;
  files are downloaded to `<app_data>/slack-threads/<id>/files/<slack_file_id>/`.
  The OS notification is shown when the job ends. AI sessions read threads via
  the `mcp__devdy__slack_threads_*` tools.
