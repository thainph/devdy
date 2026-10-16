# SRS — Slack Thread Translate (viewer thread Slack đã dịch tiếng Việt)

Trạng thái: **FROZEN v2** (2026-10-16) — đặc tả + hợp đồng triển khai. Branch: `feature/slack-thread-translate`.
v2 thay v1: Slack **tách hẳn** khỏi core `captures` (bảng riêng, màn hình riêng, event riêng); các câu hỏi mở v1 đã được người dùng chốt (§18).

Thay thế toàn bộ phần Slack của `docs/srs-slack-thread-inbox.md`. Phần web page (`captures` `kind='web'`, `/v1/web-pages`, MCP `web_pages_*`, `CaptureWindow` cho web) **không đổi hành vi**.

Ký hiệu: **[v1]** = bắt buộc đợt này; **[NTH]** = nice-to-have (không chặn nghiệm thu, không có AC bắt buộc).

---

## 0. Hiện trạng (đã đọc code)

- Thread Slack hiện là dòng `captures` `kind='slack'` (markdown từ extension, zip/markdown qua Inbox API), file ở `capture_attachments` + `<app_data>/slack-threads/<id>/files/<safe_name>`.
- Code Slack nằm trong core captures: `inbox_api/ingest.rs` (`CaptureKind::Slack`, front matter `workspace/channel/thread_url/thread_ts/exported_at`), `inbox_api/store.rs` (`find_existing` nhánh slack, cột slack trong `Capture`), `commands/captures.rs`, `inbox_api/server.rs` (`post_slack_thread`), FE `stores/captures.ts`, `CaptureWindow.vue`, `ItemListWindow.vue` (tab Slack), `CapturePicker.vue`, `App.vue` (nav + notification), `lib/appMenu.ts` (`view.slackThreads`), MCP `capture-tools.mjs` + `store.mjs` (`KINDS.slack`).
- Dịch: `commands/translate.rs` — MỘT sidecar warm (tiến trình Agent SDK chạy sẵn) dùng chung, turn FIFO (vào trước ra trước), `MAX_INPUT_CHARS = 8000`.
- Token: MCP server `slack` trong `mcp_servers` (`env_keys = ["SLACK_MCP_XOXP_TOKEN"]`), VALUE trong Keychain (`secrets::get_mcp_secrets(id).env`).
- Dữ liệu thật (đọc read-only 2026-10-16): **10** dòng `kind='slack'`, đều có `thread_url` `https://malme.slack.com/archives/<C…>/p<16 số>` không query, 9 dòng `title_custom=1`, 4 dòng có project, 33 file; không trùng thread. Có đúng 1 MCP server chứa `SLACK_MCP_XOXP_TOKEN`.
- sqlx bật `foreign_keys` mặc định ở app; sidecar MCP (better-sqlite3) thì không → mọi xoá phải xoá con **tường minh**, không dựa vào cascade.

## 1. Mục tiêu

Màn hình thread Slack thành **trình xem thread đã dịch tiếng Việt**, giao diện và cách đọc **giống app Slack**. Devdy **tự lấy** thread từ link qua Slack Web API (gọi trực tiếp từ Rust, không dùng AI session để fetch), lưu từng tin (gốc + tiếng Việt), tải mọi file về máy để mở ngay. Dữ liệu Slack sống ở **bảng riêng**, độc lập với web capture.

## 2. Phạm vi

### 2.1 Trong phạm vi [v1]

1. Bảng riêng `slack_threads`, `slack_messages`, `slack_files`, `slack_users`, `slack_channels`; migration 0041 chuyển 10 dòng cũ sang và xoá khỏi `captures`.
2. Gỡ code Slack khỏi core captures (Rust, FE, MCP) mà web không đổi.
3. Thêm thread bằng link: nút "+ Thêm thread" và Inbox API `POST /v1/slack-threads` JSON `{url, projectId?, title?}` → `202`, xử lý bất đồng bộ.
4. Lấy thread qua Slack Web API (`auth.test` + kiểm scope qua header `x-oauth-scopes`, `conversations.info`, `conversations.replies` phân trang, `users.info` cache, tải file Bearer, tải avatar).
5. Dịch từng tin sang tiếng Việt, gom lô ≤ 8000 ký tự, giữ code/mention/link/emoji; làm mới chỉ dịch tin mới/đã sửa.
6. Làm mới thủ công hoặc import lại cùng thread qua API; khoá chống trùng `channel_id + thread_ts`; import lại giữ tiêu đề tự đặt, project, vị trí.
7. Chặn DM (im/mpim) với thông báo rõ.
8. Tự động lấy lại 10 bản ghi cũ ở lần chạy đầu.
9. Cửa sổ riêng kiểu Slack (sidebar workspace → channel, khung thread, avatar, mrkdwn, reaction, file inline, lightbox, "Xem bản gốc", badge tin mới, icon trạng thái).
10. CapturePicker của composer vẫn chèn được thread Slack (nội dung tiếng Việt, tuỳ chọn kèm bản gốc).
11. MCP `slack_threads_*` thu gọn, đọc từ bảng mới.
12. i18n vi/en.

### 2.2 Ngoài phạm vi

- DM / group DM; tự động làm mới định kỳ / realtime; gửi/sửa/react lên Slack; sửa nội dung thread trong Devdy.
- Import zip/markdown cho Slack (HTTP lẫn file dialog). Web giữ nguyên.
- Link `app.slack.com/client/…`, token `xoxc`/`xoxd`, Slack Connect khác workspace.
- Custom emoji (cần `emoji:read`) → hiện nguyên `:ten:` [NTH]; unfurl/legacy `attachments`/`blocks` rich_text → v1 chỉ dùng `text` [NTH].
- MCP `slack_threads_refresh` (người dùng chốt: không làm v1).
- Dọn cache avatar tự động (cache avatar dùng chung được phép tồn tại).
- Xoá cột Slack cũ khỏi bảng `captures` (`workspace`, `channel`, `thread_url`, `thread_ts`): giữ nguyên cột, không dùng nữa (tránh rebuild bảng web).

## 3. Quyết định kỹ thuật

| # | Quyết định | Lý do |
|---|---|---|
| D1 | Slack có bảng riêng (§5), `captures` chỉ còn web. Enum `CaptureKind` chỉ còn `Web`. | Yêu cầu người dùng; mô hình Slack (tin nhắn, người dùng, trạng thái fetch) khác hẳn web snapshot. |
| D2 | File đính kèm cũ **giữ nguyên chỗ** (`<app_data>/slack-threads/<id>/files/<safe_name>`), migration chỉ chép đường dẫn relative sang `slack_files`. | Thư mục `slack-threads/<id>` vốn đã là thư mục lưu trữ Slack mới và `id` được giữ → không cần di chuyển file; migration thuần SQL, atomic, không có bước I/O có thể hỏng giữa chừng. Sau khi gỡ `CaptureKind::Slack`, không code nào của captures còn trỏ tới `slack-threads/` nên không bị dọn nhầm. |
| D3 | Gốc + dịch đều lưu dạng **Slack mrkdwn** (`text_raw`, `text_vi`); mention/link/code được bảo vệ bằng placeholder khi dịch. | Một renderer cho cả hai bản; mention trong bản dịch vẫn là `<@U…>`; tên người không bị dịch. |
| D4 | Dịch qua **instance warm sidecar thứ hai** `SlackTranslateState` (newtype bọc `TranslateState`), API mới `translate::submit_prompt`, recycle sau 6 turn. | Tái dùng cơ chế spawn/stream/route/abandon + settings engine/model/account; không chặn FIFO của popover và `/v1/translate`; tránh cold start mỗi lô của one-shot. |
| D5 | Token: các MCP server có `SLACK_MCP_XOXP_TOKEN` không rỗng; nhiều token → chọn theo host `auth.test.url` == host link. Scope kiểm lúc chạy qua header `x-oauth-scopes`. | Không cần UI chọn token; hỗ trợ nhiều workspace; scope thiếu → warning, không chặn. |
| D6 | Cửa sổ riêng `slack-threads` (single instance, 2 khung). **Bỏ tab Slack** khỏi ItemListWindow; nav sidebar App + menu `view.slackThreads` + notification mở cửa sổ mới. | Layout Slack cần ≥ 820px, tab list hẹp; một lối vào duy nhất, không duy trì 2 UI. |
| D7 | Worker queue nền trong Rust, DB là nguồn sự thật, tiến trình qua event riêng `slack-threads://progress`; thay đổi dữ liệu qua `slack-threads://changed`. | API trả `202` ngay; tiếp tục được sau khi khởi động lại; tách khỏi `captures://changed`. |
| D8 | Bearer token **chỉ** gửi tới HTTPS `slack.com`, `*.slack.com`, `slack-files.com`; avatar tải không kèm token. | Không rò token. |
| D9 | Bản ghi trùng thread → **gộp**, giữ bản `updated_at` mới nhất (id, title, `title_custom`, project, position), không có trạng thái lỗi. | Người dùng chốt (Q2). |

## 4. Link Slack — parse [v1]

Hàm thuần `slack_threads::link::parse_thread_link(input: &str) -> Result<ThreadLink, LinkError>`.

```rust
pub struct ThreadLink {
    pub host: String,          // "malme.slack.com" (lowercase)
    pub channel_id: String,    // "C0B83GHDTFG"
    pub message_ts: String,    // ts in the path: "1789877283.026359"
    pub thread_ts: String,     // thread root: query thread_ts if present, else message_ts
    pub canonical_url: String, // https://<host>/archives/<channel_id>/p<thread_ts without dot>
}
```

1. Trim khoảng trắng và cặp `<` `>` bao ngoài.
2. Scheme `https` (chấp nhận `http`, chuẩn hoá thành https). Host khớp `^[a-z0-9][a-z0-9-]*(\.enterprise)?\.slack\.com$`.
3. Path khớp `^/archives/([CGD][A-Z0-9]{8,})/p(\d{16})/?$`; `message_ts = d[0..10] + "." + d[10..16]`.
4. Query `thread_ts` (nếu có) khớp `^\d{10}\.\d{6}$` → gốc thread; `cid` (nếu có) phải bằng channel ở path, khác → lỗi. Query khác bỏ qua.
5. Channel bắt đầu `D` → `dm_unsupported`. Còn lại không khớp → `invalid_link`.

Thông điệp (EN): `invalid_link` = `"invalid Slack thread link (expected https://<workspace>.slack.com/archives/<channel>/p<16 digits>)"`; `dm_unsupported` = `"direct-message threads are not supported"`.

| Input | channel_id | thread_ts |
|---|---|---|
| `https://malme.slack.com/archives/C0B83GHDTFG/p1789877283026359` | C0B83GHDTFG | 1789877283.026359 |
| `…/C0B83GHDTFG/p1789877300000100?thread_ts=1789877283.026359&cid=C0B83GHDTFG` | C0B83GHDTFG | 1789877283.026359 |
| `…?thread_ts=1789877283.026359&cid=C999` | `invalid_link` | |
| `https://malme.slack.com/archives/D01ABCDEF/p…` | `dm_unsupported` | |

Link trỏ reply mà không có `thread_ts`: nếu tin đầu của `conversations.replies` có `thread_ts` ≠ `ts` đã hỏi → gọi lại với `thread_ts` đó, cập nhật `thread_ts`/`canonical_url` (§7.3).

## 5. Mô hình dữ liệu

### 5.1 Migration `src-tauri/migrations/0041_slack_threads_v2.sql` (CHÍNH XÁC)

Migration mới nhất hiện tại là `0040_captures.sql` → file mới là **0041**. Lưu ý tên bảng `slack_threads` / `slack_thread_attachments` của 0038 đã bị 0040 rename thành `captures` / `capture_attachments`, nên tên `slack_threads` hiện trống.

```sql
-- Slack Thread Translate: Slack threads leave the shared `captures` core and get
-- their own tables. Threads are fetched from Slack by link and stored per message
-- (original + Vietnamese). Existing kind='slack' captures are carried over as
-- legacy snapshots (old markdown + files kept viewable) and re-fetched by the app.
-- Files stay where they are: <app_data>/slack-threads/<id>/files/… (same dir).

CREATE TABLE IF NOT EXISTS slack_threads (
  id               TEXT PRIMARY KEY,
  title            TEXT NOT NULL DEFAULT '',
  title_custom     INTEGER NOT NULL DEFAULT 0,     -- 1 = user-chosen, kept on refresh / re-import
  project_id       TEXT,
  position         INTEGER NOT NULL DEFAULT 0,     -- ascending; new rows = MIN(position) - 1
  created_at       TEXT NOT NULL,
  updated_at       TEXT NOT NULL,
  workspace_domain TEXT,                            -- link host, e.g. malme.slack.com
  team_id          TEXT,                            -- auth.test team_id
  workspace        TEXT,                            -- auth.test team (name)
  channel_id       TEXT,                            -- C… / G…; NULL only for unparsable legacy rows
  channel          TEXT,                            -- channel name WITHOUT '#'
  thread_ts        TEXT,                            -- thread root ts
  thread_url       TEXT,                            -- canonical link (§4)
  fetch_status     TEXT NOT NULL DEFAULT 'pending', -- pending|fetching|translating|ready|error
  fetch_error_code TEXT,                            -- §8.3
  fetch_error      TEXT,                            -- English detail
  fetched_at       TEXT,                            -- RFC3339 of the last successful fetch
  reply_count      INTEGER NOT NULL DEFAULT 0,
  last_reply_ts    TEXT,                            -- newest message ts
  last_seen_ts     TEXT,                            -- newest ts when the user last viewed it
  warnings         TEXT NOT NULL DEFAULT '[]',      -- JSON array of warning codes (§8.4)
  search_text      TEXT NOT NULL DEFAULT '',        -- generated plain transcript (vi + original) for search / preview
  legacy           INTEGER NOT NULL DEFAULT 0,      -- 1 = old zip/markdown snapshot not yet re-fetched
  legacy_content   TEXT                             -- old markdown body; NULL once re-fetched
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_slack_threads_key
  ON slack_threads(channel_id, thread_ts) WHERE channel_id IS NOT NULL AND thread_ts IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_slack_threads_position ON slack_threads(position);
CREATE INDEX IF NOT EXISTS idx_slack_threads_project  ON slack_threads(project_id);
CREATE INDEX IF NOT EXISTS idx_slack_threads_status   ON slack_threads(fetch_status);

-- One row per Slack message (seq 0 = parent).
CREATE TABLE IF NOT EXISTS slack_messages (
  thread_id        TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  ts               TEXT NOT NULL,
  seq              INTEGER NOT NULL,                -- 0 = parent, then chronological
  user_id          TEXT,
  bot_id           TEXT,
  author_name      TEXT,                            -- bot username / fallback name
  author_icon_url  TEXT,                            -- bot icon URL
  subtype          TEXT,
  text_raw         TEXT NOT NULL DEFAULT '',        -- Slack mrkdwn as returned
  text_vi          TEXT,                            -- Vietnamese, same mrkdwn syntax; NULL = not translated
  src_hash         TEXT NOT NULL,                   -- sha256 hex of text_raw
  translate_status TEXT NOT NULL DEFAULT 'pending', -- pending|done|skipped|error
  translate_error  TEXT,
  edited_ts        TEXT,
  reactions        TEXT NOT NULL DEFAULT '[]',      -- JSON [{"name":"+1","count":2}]
  file_ids         TEXT NOT NULL DEFAULT '[]',      -- JSON array of slack_files.id, message order
  PRIMARY KEY (thread_id, ts)
);
CREATE INDEX IF NOT EXISTS idx_slack_messages_seq ON slack_messages(thread_id, seq);

-- Files of a thread: Slack files (slack_file_id set) and carried-over legacy zip files (NULL).
CREATE TABLE IF NOT EXISTS slack_files (
  id              TEXT PRIMARY KEY,
  thread_id       TEXT NOT NULL REFERENCES slack_threads(id) ON DELETE CASCADE,
  slack_file_id   TEXT,                             -- F…; NULL = legacy zip file
  message_ts      TEXT,                             -- first message carrying it; NULL = legacy
  name            TEXT NOT NULL,
  title           TEXT,
  mime            TEXT,
  filetype        TEXT,
  size            INTEGER NOT NULL DEFAULT 0,
  file_path       TEXT,                             -- RELATIVE to <app_data>; NULL until downloaded
  remote_updated  INTEGER,                          -- file.updated (or created), unix seconds
  permalink       TEXT,
  width           INTEGER,
  height          INTEGER,
  download_status TEXT NOT NULL DEFAULT 'pending',  -- done|pending|error|too_large|unavailable
  download_error  TEXT,
  created_at      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_slack_files_thread ON slack_files(thread_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_slack_files_remote
  ON slack_files(thread_id, slack_file_id) WHERE slack_file_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS slack_users (
  team_id      TEXT NOT NULL,
  user_id      TEXT NOT NULL,
  display_name TEXT,
  real_name    TEXT,
  avatar_url   TEXT,                -- profile.image_72
  avatar_path  TEXT,                -- RELATIVE to <app_data>; NULL until downloaded
  is_bot       INTEGER NOT NULL DEFAULT 0,
  fetched_at   TEXT NOT NULL,
  PRIMARY KEY (team_id, user_id)
);

CREATE TABLE IF NOT EXISTS slack_channels (
  team_id    TEXT NOT NULL,
  channel_id TEXT NOT NULL,
  name       TEXT,
  kind       TEXT,                  -- public|private|im|mpim|unknown
  fetched_at TEXT NOT NULL,
  PRIMARY KEY (team_id, channel_id)
);

-- Ids of merged-away duplicate legacy rows: the app deletes their
-- <app_data>/slack-threads/<id>/ dirs once, then clears this table.
CREATE TABLE IF NOT EXISTS slack_legacy_orphans (id TEXT PRIMARY KEY);

-- ---- Carry over kind='slack' captures ---------------------------------------
-- Best-effort parse of thread_url (/archives/<channel>/p<16 digits>); the app
-- re-parses every legacy row with the full parser before fetching.
CREATE TEMP TABLE _slack_legacy AS
WITH src AS (
  SELECT c.*,
         CASE WHEN instr(c.thread_url, '/archives/') > 0
              THEN substr(c.thread_url, instr(c.thread_url, '/archives/') + 10) END AS rest
  FROM captures c
  WHERE c.kind = 'slack'
),
parsed AS (
  SELECT src.*,
         CASE WHEN instr(rest, '/p') > 1 THEN substr(rest, 1, instr(rest, '/p') - 1) END AS ch,
         CASE WHEN instr(rest, '/p') > 1 THEN substr(rest, instr(rest, '/p') + 2, 16) END AS pd
  FROM src
),
keyed AS (
  SELECT parsed.*,
         CASE WHEN ch GLOB '[CG][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]*'
                   AND ch NOT GLOB '*[^A-Z0-9]*'
                   AND length(pd) = 16 AND pd NOT GLOB '*[^0-9]*'
              THEN ch END AS k_channel,
         CASE WHEN ch GLOB '[CG][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9][A-Z0-9]*'
                   AND ch NOT GLOB '*[^A-Z0-9]*'
                   AND length(pd) = 16 AND pd NOT GLOB '*[^0-9]*'
              THEN substr(pd, 1, 10) || '.' || substr(pd, 11, 6) END AS k_ts
  FROM parsed
)
SELECT keyed.*,
       ROW_NUMBER() OVER (
         PARTITION BY COALESCE(k_channel || '|' || k_ts, id)
         ORDER BY updated_at DESC, created_at DESC, id ASC
       ) AS rn
FROM keyed;

INSERT INTO slack_threads (
  id, title, title_custom, project_id, position, created_at, updated_at,
  workspace_domain, workspace, channel_id, channel, thread_ts, thread_url,
  fetch_status, search_text, legacy, legacy_content
)
SELECT id, title, title_custom, project_id, position, created_at, updated_at,
       NULL, workspace, k_channel, ltrim(channel, '#'), k_ts, thread_url,
       'pending', title || char(10) || COALESCE(content, ''), 1, content
FROM _slack_legacy WHERE rn = 1;

INSERT INTO slack_files (id, thread_id, name, mime, size, file_path, download_status, created_at)
SELECT a.id, a.capture_id, a.name, a.mime, a.size, a.file_path, 'done', a.created_at
FROM capture_attachments a
JOIN _slack_legacy l ON l.id = a.capture_id AND l.rn = 1;

INSERT INTO slack_legacy_orphans (id) SELECT id FROM _slack_legacy WHERE rn > 1;

-- Remove the Slack rows from the captures core (children first; no file I/O here).
DELETE FROM capture_attachments WHERE capture_id IN (SELECT id FROM _slack_legacy);
DELETE FROM captures WHERE id IN (SELECT id FROM _slack_legacy);

DROP TABLE _slack_legacy;
```

Ghi chú bắt buộc:
- Migration KHÔNG đụng file. `DELETE FROM captures` chỉ xoá DB; cascade `capture_attachments` chỉ xoá DB. Không code Rust/JS nào được gọi `store::delete` của captures cho các id này.
- WP-BE phải kiểm tra `CREATE TEMP TABLE … AS WITH …` chạy được trong transaction của `sqlx::migrate!` (SQLite ≥ 3.25 cho `ROW_NUMBER`). Nếu sqlx không cho temp table trong migration → thay bằng bảng thường `_slack_legacy` rồi `DROP` (giữ nguyên logic).
- Test migration (Rust): chạy ≤ 0040, chèn: 2 slack trùng thread (khác `updated_at`), 1 slack không có `thread_url`, 1 web + attachment của từng dòng; chạy full → `slack_threads` có 2 dòng (bản trùng giữ dòng `updated_at` mới nhất với đúng title/`title_custom`/project/position/`created_at`), dòng không URL có `channel_id NULL`, `legacy=1`, `fetch_status='pending'`, `legacy_content` = content cũ; `slack_files` chỉ chứa file của dòng giữ lại với `file_path` cũ; `slack_legacy_orphans` = id bản bị gộp; `captures` chỉ còn dòng web, `capture_attachments` chỉ còn file web.

### 5.2 Bố trí file

```
<app_data>/
  slack-threads/<thread_id>/
    files/<slack_file_id>/<safe_name>   file Slack tải về (v1)
    files/<legacy_safe_name>            file zip cũ (chỉ khi legacy; xoá sau khi re-fetch thành công)
  slack-cache/avatars/<team_id>/<user_id>-<sha256(avatar_url)[0..8]>.<ext>   cache avatar dùng chung
```

- `safe_name` dùng lại `ingest::safe_file_name` (hàm này giữ trong core vì web dùng; Slack gọi được).
- DB lưu RELATIVE, command/MCP trả ABSOLUTE. Ghi file vào `<…>.part` rồi `rename`.
- Xoá thread → xoá dòng con tường minh (`slack_messages`, `slack_files`) rồi dòng thread, sau commit xoá `slack-threads/<id>/`. Avatar cache không xoá (Q8).
- Startup (một lần mỗi lần chạy, chỉ khi bảng có dòng): với mỗi id trong `slack_legacy_orphans` hợp lệ (`[A-Za-z0-9-]+`) → xoá `slack-threads/<id>/`, rồi xoá dòng đó.

## 6. State machine `fetch_status` [v1]

```
 add / re-import / refresh / startup(legacy)
        │
        ▼
     pending ──worker──► fetching ──► translating ──► ready
        ▲                   │              │            │
        │                   └── fatal ─────┴──► error   │
        └──────── refresh / re-import (từ ready|error) ─┘
```

1. Chỉ worker chuyển `pending → fetching → translating → ready|error`. Command/API chỉ đặt `pending` khi thread không đang `fetching`/`translating`; đang chạy → bật cờ `rerun` (bộ nhớ), job xong chạy lại đúng 1 lần.
2. `fetching`: lấy Slack, upsert tin (bản gốc xem được ngay), tải avatar + file. Lỗi fatal (§8.3) → `error`, giữ nguyên dữ liệu cũ (tin, file, `legacy_content`).
3. `translating`: dịch tin `pending|error`. Lô lỗi lẻ → tin `error`, thread vẫn `ready` + warning `translate_partial`. Engine không khởi động được hoặc mọi lô lỗi → `error/translate_failed` (tin gốc vẫn xem được).
4. `ready`: xoá `fetch_error_code`/`fetch_error`, `fetched_at = now`.
5. Startup: dòng `fetching`/`translating` → `pending`; dòng `pending` vào queue; dòng `error` có `legacy=1` hoặc mã tạm thời (`no_token`, `no_token_for_workspace`, `invalid_auth`, `network`, `rate_limited`) → `pending`.
6. Mỗi chuyển trạng thái: ghi DB → emit `slack-threads://changed {id, action:'updated'}` + `slack-threads://progress`.
7. Thread bị xoá giữa chừng: mọi ghi DB của job kiểm `rows_affected`; = 0 → job dừng, xoá `slack-threads/<id>/` nếu job lỡ tạo lại.
8. Worker tối đa 2 job song song; dịch tuần tự qua FIFO `SlackTranslateState`; 1 job/thread.

## 7. Slack Web API [v1]

### 7.1 Token & scope (`slack_threads::token`)

1. `SELECT id, name, env_keys FROM mcp_servers ORDER BY created_at ASC`, giữ server có `env_keys` chứa `"SLACK_MCP_XOXP_TOKEN"`, VALUE từ `secrets::get_mcp_secrets(id).env`, bỏ nếu rỗng hoặc không bắt đầu `xoxp-`/`xoxb-`. Bỏ qua cờ `enabled`.
2. 0 → `no_token`. 1 → dùng. ≥ 2 → `auth.test` từng token (cache bộ nhớ theo `server_id + sha256(token)`, TTL 1 giờ), chọn token có host `url` == host link; không khớp → `no_token_for_workspace`; nhiều khớp → server tạo sớm nhất.
3. **Scope lúc chạy**: đọc header `x-oauth-scopes` của phản hồi `auth.test` (danh sách phẩy). Thiếu `channels:history` → vẫn thử (kênh private chỉ cần `groups:history`), lỗi thật lộ ra ở `conversations.replies`. Thiếu `files:read` → bỏ qua bước tải file, mọi file `download_status='unavailable'`, warning `files_scope`. Thiếu `users:read` → bỏ `users.info`, warning `users_scope`. Thiếu cả `channels:read` lẫn `groups:read` → bỏ `conversations.info`, warning `channel_name_unavailable`. Không có header (token bot cũ…) → không suy luận, đi theo lỗi API.
4. Token không bao giờ xuất hiện trong log, event, lỗi, response API, output MCP, prompt dịch.

### 7.2 Client (`slack_threads::api`)

- `reqwest` (rustls, đã có), base `https://slack.com/api/` (inject được để test bằng mock server), `Authorization: Bearer`, timeout 30 s.
- `{"ok":false,"error","needed"?}` → `SlackError::Api{code, needed}`.
- HTTP 429 → đợi `Retry-After` (mặc định 5 s, tối đa 60 s), thử lại 3 lần → `rate_limited`. Lỗi mạng/5xx → thử lại 2 lần (1 s, 3 s) → `network`.

### 7.3 Chuỗi gọi cho một job

| Bước | API | Scope | Mục đích | Lỗi |
|---|---|---|---|---|
| 1 | `auth.test` | — | `team_id`, `team`, `url`, header `x-oauth-scopes` | `invalid_auth`, `not_authed`, `token_revoked`, `token_expired`, `account_inactive` → fatal `invalid_auth` |
| 2 | `conversations.info?channel=` | `channels:read`/`groups:read` | tên, `is_im`/`is_mpim` | `is_im`/`is_mpim` → fatal `dm_unsupported`; `missing_scope` → warning `channel_name_unavailable` (tên = id); `channel_not_found` → fatal |
| 3 | `conversations.replies?channel=&ts=&limit=200&inclusive=true&cursor=` | `channels:history`/`groups:history` | mọi tin, lặp tới hết `next_cursor` | `missing_scope` có `needed` chứa `im:`/`mpim:` → `dm_unsupported`; khác → fatal `missing_scope`; `thread_not_found`/`message_not_found` → fatal `thread_not_found`; `not_in_channel` → fatal |
| 3b | lặp bước 3 với `ts = thread_ts` | | link trỏ reply (§4) | |
| 4 | `users.info?user=` (user trong tin + mention, chưa cache hoặc cache > 7 ngày) | `users:read` | `display_name` → `real_name` → `name`, `image_72`, `is_bot` | `missing_scope` → warning `users_scope`; `user_not_found` → lưu tên = id |
| 5 | `conversations.info` cho `<#C…>` không nhãn, chưa cache | như 2 | tên trong mention | lỗi → bỏ qua |
| 6 | GET `url_private_download` (fallback `url_private`) | `files:read` | tải file §7.5 | §7.5 |
| 7 | GET `image_72`, không token | — | avatar | lỗi → bỏ qua (FE dùng chữ cái đầu) |

- Tối đa 1000 tin/thread (vượt → dừng, warning `truncated`). Bỏ `subtype` ∈ {`channel_join`, `channel_leave`}. `tombstone` → lưu, `translate_status='skipped'`.
- Trích: `ts`, `user`, `bot_id`, `username`/`bot_profile.name`, `bot_profile.icons.image_72`/`icons.image_72`, `subtype`, `text`, `edited.ts`, `reactions[].{name,count}`, `files[]`.
- Upsert (1 transaction): tin có → insert/update; tin không còn → DELETE. `src_hash` đổi → `text_vi = NULL`, `pending`; giữ nguyên → giữ bản dịch (trừ `error` → `pending`). `seq` đánh lại theo `ts` tăng dần.
- Cập nhật thread: `team_id`, `workspace`, `workspace_domain`, `channel_id`, `channel`, `thread_ts`, `thread_url`, `reply_count`, `last_reply_ts`, `search_text` (mỗi tin một dòng `Tên: text_vi` và `Tên: text_raw`, mention → `@Tên`), `title` nếu `title_custom = 0` (dòng đầu tin cha, ưu tiên `text_vi`, plain, ≤ 120 ký tự, rỗng → `"Slack thread"`; tính lại sau khi dịch).
- **Legacy thành công** (sau bước 3): trong cùng transaction ghi tin + thread, `DELETE FROM slack_files WHERE thread_id=? AND slack_file_id IS NULL`, `legacy = 0`, `legacy_content = NULL`; sau commit xoá các file thường nằm trực tiếp trong `slack-threads/<id>/files/` (không đụng thư mục con).

### 7.4 Scope

Cần: `channels:history`, `groups:history`, `channels:read`, `groups:read`, `users:read`, `files:read`. Không dùng: `im:history`, `mpim:history`, `search:read`. Scope thiếu được phát hiện lúc chạy (§7.1-3) và hiện warning — không chặn.

### 7.5 Tải file

Theo `files[].id` (gộp cả thread):
- `mode` `tombstone`/`hidden_by_limit`/`external`, hoặc không có `url_private*`, hoặc thiếu `files:read` → `unavailable` (thẻ mở `permalink`).
- `size > 100 MB`, hoặc tổng thread > 500 MB → `too_large`.
- **Không tải lại** khi đã có dòng `(thread_id, slack_file_id)` cùng `remote_updated`, `download_status='done'`, file tồn tại đúng `size`.
- Host phải HTTPS thuộc `*.slack.com` / `slack-files.com`, không thì `unavailable` (không gửi token).
- `Content-Type: text/html` khi file không phải html → `error` (`"no access to file (files:read?)"`), warning `files_scope`. 403/404 → `error`, warning `files_partial`.
- File không còn trong lần fetch → xoá dòng + thư mục `files/<slack_file_id>/`.
- 3 file song song; progress phase `files` `done/total`.

## 8. Dịch [v1]

### 8.1 Bảo vệ token (`slack_threads::mrkdwn`, thuần)

`protect(text_raw) -> (masked, slots)` thay theo thứ tự bằng `⟦n⟧` (U+27E6/U+27E7): (1) code block ` ```…``` `; (2) inline `` `…` ``; (3) mọi `<…>` (`<@U…>`, `<#C…|x>`, `<!here>`, `<!subteam^…>`, `<url>`, `<url|label>`, `<mailto:…>`); (4) emoji `:[a-z0-9_+\-']+:` (kèm `::skin-tone-N:`).
`restore(translated, slots)` thay ngược; `<`/`>` thừa do model sinh → `&lt;`/`&gt;` (Slack luôn escape `< > &` nên text gốc không có `<` trần — thẻ lô không đụng nội dung).
Tin chỉ có placeholder/khoảng trắng → `text_vi = text_raw`, `skipped`.

### 8.2 Gom lô & định dạng

- Duyệt tin `pending|error` theo `seq`, gom tới khi chuỗi lô (gồm thẻ) vượt `BATCH_MAX_CHARS = 8000` (= `translate::MAX_INPUT_CHARS`).
- Tin > 7500 ký tự → cắt ở `\n\n`, rồi `\n`, rồi khoảng trắng (không cắt giữa placeholder), block id `N.k`, ghép lại đúng dấu phân cách.
- Định dạng:

```
<m id="3">
masked text of message seq 3
</m>
<m id="4">
…
</m>
```

- Prompt (`slack_threads::translate::build_batch_prompt`):

```
You translate Slack messages into Vietnamese. The input is a list of <m id="…">…</m> blocks.
Rules:
- Output exactly the same blocks with the same ids in the same order; each block contains only the Vietnamese translation of its own content. Output nothing outside the blocks.
- Tokens like ⟦3⟧ are placeholders for mentions, links, code and emoji: copy every placeholder unchanged, exactly once, at the natural position.
- Keep Slack formatting markers (*bold*, _italic_, ~strike~, leading "> " quotes, bullets, numbering, line breaks) and HTML entities (&lt; &gt; &amp;) unchanged.
- Keep person names, product names, file names, identifiers and URLs unchanged. If a block is already Vietnamese, copy it unchanged.
<style_instruction(translate_style)>

<m id="…">…
```

- Đích luôn tiếng Việt (bỏ `translate_target_lang`, Q5); engine/model/account theo settings. Không giới hạn/không hỏi xác nhận với thread dài (Q7).
- Parse: `(?s)<m id="([0-9]+(?:\.[0-9]+)?)">\n?(.*?)\n?</m>`. Block hợp lệ: id thuộc lô, đúng 1 lần, nội dung không rỗng, multiset placeholder == nguồn → `restore` → `done`.
- Block lỗi → thử lại 1 lần, mỗi tin một lô riêng; vẫn lỗi → `error` + `translate_error` (`missing_block` / `placeholder_mismatch` / `timeout` / thông điệp engine).
- Timeout lô 180 s (gọi `abandon_turn`). Progress phase `translate` `done/total` = số lô.

### 8.3 Mã lỗi thread (`fetch_error_code`)

| Code | Khi nào |
|---|---|
| `no_token` | không có token (§7.1) |
| `no_token_for_workspace` | ≥ 2 token, không token nào thuộc workspace link |
| `invalid_auth` | §7.3 bước 1 |
| `missing_scope` | thiếu scope history (`fetch_error` ghi `needed`) |
| `dm_unsupported` | DM/MPIM |
| `channel_not_found` | token không thuộc workspace / không có quyền kênh |
| `not_in_channel` | |
| `thread_not_found` | |
| `rate_limited` | 429 sau 3 lần thử |
| `network` | mạng/5xx sau 2 lần thử |
| `translate_failed` | engine không khởi động / mọi lô lỗi |
| `legacy_no_url` | bản cũ không có / không parse được `thread_url` |
| `slack_error` | lỗi Slack khác (`fetch_error` = mã Slack) |
| `internal` | DB/đĩa |

(Mã `duplicate` của v1 bị bỏ — trùng thì gộp, §12.)

### 8.4 Warning (`warnings`, thread vẫn `ready`; ghi đè mỗi lần job xong)

`files_scope`, `files_partial`, `users_scope`, `channel_name_unavailable`, `translate_partial`, `truncated`.

## 9. Backend (WP-BE)

### 9.1 Module mới

```
src-tauri/src/slack_threads/
  mod.rs        SlackThreadsState (queue + đang chạy + rerun), enqueue, start_worker, startup_recover
                (reset trạng thái, enqueue, dọn slack_legacy_orphans), emit_changed, emit_progress,
                CHANGED_EVENT = "slack-threads://changed", PROGRESS_EVENT = "slack-threads://progress"
  link.rs       parse_thread_link (thuần)
  token.rs      resolve_token, token_status, scope parse (x-oauth-scopes)
  api.rs        SlackClient (auth_test, conversations_info, conversations_replies_all, users_info, download)
  mrkdwn.rs     protect/restore, plain_text (mention → @Tên), transcript, prompt_text (§13.9) (thuần)
  translate.rs  build_batches, build_batch_prompt, parse_batch_output (thuần) + run_translation
  files.rs      đồng bộ file + avatar
  store.rs      SQL các bảng slack_*: add_or_requeue (+ gộp trùng), list, get_view, mark_seen, rename,
                set_project, reorder, delete (xoá con tường minh + dir)
  job.rs        run_job(id)
src-tauri/src/commands/slack_threads.rs   Tauri commands §10
```

### 9.2 Thay đổi file hiện có (gỡ Slack khỏi core captures, web không đổi)

- `inbox_api/ingest.rs`: xoá biến thể `CaptureKind::Slack` và mọi nhánh/field Slack (`workspace`, `channel`, `thread_url`, `thread_ts`, `exported_at` front matter của Slack); `CaptureKind` chỉ còn `Web` (giữ enum để không đổi chữ ký command web; serde từ chối `"slack"`). Test parser hiện dùng `CaptureKind::Slack` chuyển sang `Web` (cùng logic zip/front matter). `safe_file_name`, `guess_mime` giữ `pub`.
- `inbox_api/store.rs`: bỏ nhánh slack trong `find_existing`, bỏ field slack khỏi `Capture` (DB cột vẫn còn, không SELECT nữa); test slack chuyển sang web hoặc xoá nếu trùng lặp; test `migration_0040_*` giữ (vẫn chạy ≤ 0040 rồi full — cập nhật assert: sau full, dòng `s1` nằm ở `slack_threads`, không còn ở `captures`).
- `inbox_api/server.rs`: `/v1/slack-threads` → handler mới §11 (gọi `slack_threads::store::add_or_requeue` + enqueue); `post_capture` chỉ còn web; `/health.features` = `["translate","slack-thread-link"]`.
- `inbox_api/mod.rs`: `notify_received` chỉ còn cho web; thông báo Slack ở `slack_threads::mod` (§9.3).
- `commands/captures.rs`: không còn nhánh Slack (tự nhiên khi enum chỉ còn Web).
- `commands/translate.rs`: thêm `pub async fn submit_prompt(app, db, state, prompt, want_events) -> Result<(u64, Option<UnboundedReceiver<TurnEvent>>), String>` (chung thân `submit`, bỏ `build_prompt`/target); `TranslateState::with_recycle_after(n)`; public `style_instruction` + đọc `translate_style`. Popover và `/v1/translate` không đổi hành vi.
- `lib.rs`: manage `SlackTranslateState`, `SlackThreadsState`; start worker + `startup_recover` sau DB; đăng ký command §10.2.
- `docs/inbox-api.md` §3/§5/§7/§8 cập nhật theo §11; `scripts/send-slack-thread.sh <slack-link> [token] [port]` gửi JSON.

### 9.3 Thông báo OS (Q4)

Chỉ cho job xuất phát từ Inbox API khi kết thúc: `ready` → `"Đã dịch xong thread Slack: <title>"`, `error` → `"Không lấy được thread Slack: <title>"`. Click → event `capture-notification-clicked {id, kind:'slack'}` (giữ tên event để App.vue chỉ cần đổi nhánh mở cửa sổ).

## 10. Tauri commands & events (hợp đồng BE ⇄ FE)

Serde `rename_all = "camelCase"`. Lỗi command là `String` `"<code>: <detail>"` (`code` ∈ §4, §8.3, `invalid_project`, `invalid_title`, `not_found`); FE tách phần trước `:` để tra i18n.

### 10.1 Kiểu (export từ `src/stores/slackThreads.ts`)

```ts
export type SlackFetchStatus = 'pending' | 'fetching' | 'translating' | 'ready' | 'error'
export type SlackTranslateStatus = 'pending' | 'done' | 'skipped' | 'error'
export type SlackDownloadStatus = 'done' | 'pending' | 'error' | 'too_large' | 'unavailable'

export interface SlackThreadSummary {
  id: string
  title: string
  titleCustom: boolean
  projectId: string | null
  position: number
  createdAt: string
  updatedAt: string
  workspace: string | null
  workspaceDomain: string | null
  teamId: string | null
  channel: string | null          // WITHOUT '#'
  channelId: string | null
  threadUrl: string | null
  threadTs: string | null
  fetchStatus: SlackFetchStatus
  fetchErrorCode: string | null
  fetchError: string | null
  fetchedAt: string | null
  replyCount: number
  lastReplyTs: string | null
  lastReplyAt: string | null      // ISO from lastReplyTs
  newCount: number                // messages with ts > last_seen_ts; 0 when last_seen_ts is NULL
  warnings: string[]
  legacy: boolean
  fileCount: number
  preview: string                 // parent first line (vi, plain), ≤ 140 chars
}

export interface SlackUser {
  userId: string
  displayName: string             // never empty
  realName: string | null
  avatarPath: string | null       // ABSOLUTE
  isBot: boolean
}

export interface SlackReaction { name: string; count: number }

export interface SlackFile {
  id: string
  slackFileId: string | null      // null = legacy zip file
  messageTs: string | null
  name: string
  title: string | null
  mime: string | null
  filetype: string | null
  size: number
  filePath: string | null         // ABSOLUTE; null unless downloadStatus = 'done'
  downloadStatus: SlackDownloadStatus
  downloadError: string | null
  width: number | null
  height: number | null
  permalink: string | null
  isImage: boolean                // mime image/* and downloaded
}

export interface SlackMessage {
  ts: string
  seq: number
  userId: string | null
  botId: string | null
  authorName: string
  avatarPath: string | null       // ABSOLUTE (user avatar or cached bot icon); null → initials
  subtype: string | null
  textRaw: string                 // Slack mrkdwn
  textVi: string | null           // Slack mrkdwn
  translateStatus: SlackTranslateStatus
  translateError: string | null
  editedTs: string | null
  postedAt: string                // ISO
  reactions: SlackReaction[]
  fileIds: string[]               // SlackFile.id
}

export interface SlackThreadView extends SlackThreadSummary {
  messages: SlackMessage[]        // by seq
  users: Record<string, SlackUser>    // every user in messages AND mentions
  channels: Record<string, string>    // channel_id → name
  files: SlackFile[]              // Slack files + legacy files (slackFileId null)
  legacyContent: string | null    // only while legacy
}

export interface SlackThreadProgress {
  id: string
  status: SlackFetchStatus
  phase: 'queued' | 'auth' | 'messages' | 'users' | 'files' | 'translate' | 'done'
  done: number
  total: number | null
  errorCode: string | null
  error: string | null
}

export interface SlackThreadsChanged { id: string; action: 'created' | 'updated' | 'deleted' }

export interface AddSlackThreadResult {
  id: string
  status: 'created' | 'existing'
  fetchStatus: SlackFetchStatus
}

export interface SlackTokenStatus {
  state: 'ok' | 'none' | 'multiple'
  sources: { serverId: string; serverName: string }[]
}

export interface SlackPromptText {
  text: string
  truncated: boolean
  chars: number
}
```

### 10.2 Commands

| Command | Args | Returns | Ghi chú |
|---|---|---|---|
| `add_slack_thread` | `url: string, projectId?: string \| null` | `AddSlackThreadResult` | Parse đồng bộ; `projectId` lạ → `invalid_project`. Mới → insert đầu danh sách (`MIN(position)-1`), title `"Slack thread"`, `pending`, enqueue; `token_status = none` → đặt ngay `error/no_token`, không enqueue. Trùng → §12. Emit `slack-threads://changed`. |
| `list_slack_threads` | `projectId?: string \| null, query?: string \| null` | `SlackThreadSummary[]` | ORDER BY position ASC, updated_at DESC. `query` khớp (không phân biệt hoa thường) title, channel, workspace, `search_text`, `legacy_content`. |
| `get_slack_thread` | `id` | `SlackThreadView` | `not_found` |
| `refresh_slack_thread` | `id` | `SlackThreadSummary` | `pending` + enqueue / cờ rerun. Legacy không parse được URL → `legacy_no_url`. |
| `mark_slack_thread_seen` | `id` | `()` | `last_seen_ts = last_reply_ts`, không bump `updated_at`. |
| `rename_slack_thread` | `id, title` | `SlackThreadSummary` | trim, 1–200 ký tự (`invalid_title`), `title_custom = 1`. |
| `set_slack_thread_project` | `id, projectId: string \| null` | `()` | |
| `reorder_slack_threads` | `ids: string[]` | `()` | position = index (như notes). |
| `delete_slack_threads` | `ids: string[]` | `()` | xoá con + dòng + dir; emit `deleted` từng id. |
| `get_slack_token_status` | — | `SlackTokenStatus` | không gọi mạng |
| `get_slack_thread_prompt_text` | `id, includeOriginal: boolean` | `SlackPromptText` | §13.9 |

Mọi command ghi dữ liệu emit `slack-threads://changed`.

### 10.3 Events

| Event | Payload | Khi nào |
|---|---|---|
| `slack-threads://changed` | `SlackThreadsChanged` | tạo/sửa/xoá/đổi trạng thái/ghi tin/xong lô dịch/mark seen |
| `slack-threads://progress` | `SlackThreadProgress` | đổi phase + tiến độ; throttle ≤ 4/giây/thread; event cuối `phase:'done'` hoặc `status:'error'` |
| `capture-notification-clicked` (có sẵn) | `{ id, kind: 'slack' \| 'web' }` | click thông báo |

`captures://changed` không còn phát cho Slack.

## 11. Inbox API `POST /v1/slack-threads` [v1]

Auth, Host/Origin guard, CORS như cũ. Các endpoint khác không đổi (chỉ thêm `"slack-thread-link"` vào `/health.features`).

| Mục | Giá trị |
|---|---|
| Headers | `Authorization: Bearer <token>`, `Content-Type: application/json` (`; charset=utf-8` OK) |
| Body ≤ 16 KB | `{ "url": string, "projectId"?: string \| null, "title"?: string }` |
| `title` | 1–200 ký tự sau trim, UTF-8 (thay `X-Devdy-Title`) |

```
202 {"id":"…","status":"created","fetchStatus":"pending"}
202 {"id":"…","status":"existing","fetchStatus":"pending"}
202 {"id":"…","status":"created","fetchStatus":"error","errorCode":"no_token"}
```

| Status | Khi (`{"error","code"?}`) |
|---|---|
| 400 | `invalid_json`, `invalid_link`, `dm_unsupported`, `invalid_project`, `invalid_title` |
| 401 / 403 / 404 | như cũ |
| 413 | body > 16 KB |
| 415 | Content-Type khác JSON (gồm zip/markdown/plain): `"Content-Type must be application/json ({\"url\": \"<slack thread link>\"}); zip/markdown uploads are no longer supported"` |
| 503 | `server stopping` |
| 500 | lỗi lưu trữ |

`X-Devdy-Project-Id` / `X-Devdy-Title` bị bỏ qua trên endpoint này.

## 12. Dedup, import lại & gộp [v1]

Khoá `(channel_id, thread_ts)`. Tra theo khoá, hoặc theo `thread_url` canonical với dòng legacy chưa có `channel_id`.
- Thread đã có (add/API): giữ `id`, `position`; title giữ nếu `title_custom = 1`, nếu `= 0` và request có `title` → đặt + `title_custom = 1`; `projectId` chỉ gán khi thread **chưa có project** (Q3); đặt `pending` + enqueue; trả `existing`.
- **Gộp trùng (Q2)**: khi job (thường gặp ở legacy có `channel_id NULL` được parse lại) thấy khoá đã thuộc dòng khác → giữ dòng có `updated_at` mới nhất (id, title, `title_custom`, project, position của nó), xoá dòng còn lại (con + dir) và tiếp tục job trên dòng được giữ. Không có trạng thái lỗi. Emit `deleted` cho dòng bị xoá.

## 13. UI (WP-FE)

Theo skill `fe-ui-consistency`: dùng lại `@/components/ui`, `DetailWindowTitlebar`, `DetailWindowFooter`, `ProjectSelect`, `InlineRenameInput`, `WindowPinButton`, `useConfirm`, `useToast`, `useFloatingWindow`, `useMarkdown`, token màu Tailwind có sẵn. "Giống Slack" = bố cục & tương tác, không chép màu Slack; dark/light theo theme Devdy.

### 13.1 Cửa sổ & lối vào [v1]

- `src/views/SlackThreadsWindow.vue`, mở bằng `src/lib/slackWindow.ts → openSlackWindow(opts?: { id?: string; add?: boolean; projectId?: string | null })`; URL `index.html?slackWindow=1[&id=][&add=1][&projectId=]`; label cố định `slack-threads` (đã mở → focus + emit `slack-window:select {id}` / `slack-window:add {projectId}`). 1100×760, min 820×520. Thêm `slack-threads` vào `src-tauri/capabilities/default.json` (`windows`), tray switcher (`trackWindowForTray`), `App.vue` route theo `slackWindow=1`.
- **Bỏ tab Slack** khỏi `ItemListWindow` (`KINDS = ['todo','note','web']`; `asKind('slack')` / `ITEM_LIST_SET_KIND` với `slack` → gọi `openSlackWindow()` thay vì đổi tab).
- Nav sidebar trong `App.vue` ("Thread Slack") và menu `view.slackThreads` (`lib/appMenu.ts`) → `openSlackWindow({ projectId: routeCaptureContext().projectId })` (lọc sẵn project đang xem).
- Notification `kind:'slack'` → `openSlackWindow({id})`.
- `CaptureWindow.vue`, `stores/captures.ts`, `ItemListWindow.vue`: gỡ mọi nhánh Slack (`channelLabel`, `threadUrl`, wording `capture.slack.*`), kiểu `CaptureKind = 'web'`. Web không đổi hành vi. Khoá i18n `capture.slack.*` không còn dùng thì xoá (cả vi/en).

### 13.2 Bố cục

```
┌ DetailWindowTitlebar (icon Slack · "Thread Slack" · pin · reload) ──────────────────────────┐
├──────────── Sidebar 260px ────────────┬──────────────── Thread pane ────────────────────────┤
│ [🔍 Tìm thread…           ]            │ Header: Tiêu đề (rename) · #channel · workspace     │
│ [Project ▾]       [+ Thêm thread]      │   [Xem bản gốc ◻] [Làm mới ⟳] [Mở trên Slack ↗]     │
│ ▾ malme                                │ Banner trạng thái / lỗi / warning / legacy          │
│   ▾ # dev-backend                      │ tin cha                                             │
│     Deploy lỗi staging   2h  (3) ⟳     │ ──── 12 phản hồi ────                               │
│     Hỏi về API export    1d            │ reply… (gộp tin liên tiếp)                          │
│   ▸ # customer-abc                     │                                                     │
├────────────────────────────────────────┴─────────────────────────────────────────────────────┤
│ DetailWindowFooter editable=false: ProjectSelect (trái) · ⋯ (Copy ID · Xoá)                  │
└──────────────────────────────────────────────────────────────────────────────────────────────┘
```

`DetailWindowFooter` thêm prop `editable?: boolean` (mặc định `true`); `false` ẩn nút Edit. Nơi đang dùng không đổi.

### 13.3 Sidebar [v1]

- Nhóm workspace (`workspace` ?? `workspaceDomain` ?? "Khác") → channel (`#channel` ?? `#channelId` ?? "Khác"); header thu gọn/mở, lưu `localStorage` `slackThreads.collapsed`.
- Thứ tự nhóm theo `min(position)`, thread trong nhóm theo `position ASC`. Kéo thả: [NTH] (command `reorder_slack_threads` có sẵn).
- Dòng thread: tiêu đề (truncate), `lastReplyAt` (`relativeAge`, tooltip ngày đủ), badge `newCount` > 0, icon: spinner cho `pending|fetching|translating` (tooltip phase + `done/total`), cảnh báo đỏ cho `error` (tooltip lỗi i18n), chấm vàng khi có warning, badge "Cũ" khi `legacy`.
- Tìm kiếm debounce 250 ms → `list_slack_threads({query})`. Lọc project `AppSelect` (Tất cả / từng project / Không project), client-side; giá trị đầu từ URL `projectId`.
- Chọn thread → khung phải, cập nhật URL `id` (không reload), `mark_slack_thread_seen` sau khi thread `ready` hiển thị ≥ 1 s.
- Context menu dòng: [NTH]. Làm mới / Xoá có ở header + footer.

### 13.4 Dialog "+ Thêm thread" [v1]

- Ô link (autofocus), `ProjectSelect` (mặc định = filter project hiện tại), nút "Thêm".
- Regex §4 báo sớm phía client; lỗi thật từ `add_slack_thread` hiện inline theo `code`.
- `get_slack_token_status().state === 'none'` → cảnh báo trong dialog + nút mở màn MCP; vẫn cho thêm.
- Thành công → đóng, chọn thread; `existing` → toast "Thread đã có — đang làm mới".

### 13.5 Khung thread [v1]

- **Header**: tiêu đề (click/F2 → `InlineRenameInput` → `rename_slack_thread`), `#channel · workspace`, toggle "Xem bản gốc" toàn thread (`localStorage` `slackThreads.showOriginal`), "Làm mới" (disable + spinner khi đang chạy), "Mở trên Slack" (`openUrl(threadUrl)`).
- **Banner**: đang chạy → thanh tiến trình + nhãn phase ("Đang lấy tin nhắn…", "Đang tải tệp 3/8", "Đang dịch 2/5"), tin đã có vẫn hiện; `error` → banner đỏ (i18n theo mã + `fetchError` nhỏ) + "Thử lại" (ẩn với `dm_unsupported`, `legacy_no_url`); warnings → banner vàng; `legacy` và chưa có tin → render `legacyContent` bằng `useMarkdown` + thẻ file legacy, banner "Bản lưu cũ — chưa lấy lại được từ Slack".
- **Danh sách**: tin cha; divider "N phản hồi" (ẩn khi 0); reply theo `seq`.
- **Một tin** (`src/components/slack/SlackMessage.vue`): avatar 36px (`convertFileSrc(avatarPath)`; null/lỗi → chữ cái đầu, màu nền theo hash userId trong bảng token có sẵn), tên đậm, giờ `HH:mm` (tooltip ngày giờ đủ), "(đã chỉnh sửa)", badge "APP" cho bot. Gộp kiểu Slack: cùng `userId`/`botId`, cách ≤ 5 phút, không phải tin cha, không có divider giữa → ẩn avatar + tên, hover hiện giờ nhỏ. Nội dung `textVi` hoặc `textRaw` theo toggle; `textVi == null` → `textRaw` + nhãn "Chưa dịch" / "Đang dịch…" / "Không dịch được" (tooltip `translateError`). Hover action: "Xem bản gốc"/"Xem bản dịch" (riêng tin), "Sao chép" (plain, mention → `@Tên`). Reaction chip `emoji count` (chỉ xem).
- **File**: ảnh `done` → thumbnail (≤ 360×240, `object-contain`), click → `SlackImageLightbox.vue` (Esc đóng, ←/→ ảnh khác trong thread, "Mở bằng ứng dụng mặc định" = `openPath`); file khác `done` → thẻ (icon theo filetype/mime, tên, dung lượng), click `openPath`; `error|too_large|unavailable|pending` → thẻ mờ + lý do + "Mở trên Slack" (`permalink`).

### 13.6 Renderer mrkdwn (`src/lib/slackMrkdwn.ts`) [v1]

`renderSlackMrkdwn(text, ctx: { users, channels, workspaceDomain }) : string` → HTML qua `DOMPurify`.

| Cú pháp | Kết quả |
|---|---|
| ` ```code``` ` | `<pre><code>`, không xử lý bên trong |
| `` `code` `` | `<code>` |
| `*bold*` `_italic_` `~strike~` | `<strong>` `<em>` `<del>` (ranh giới từ như Slack) |
| dòng `>` / `&gt;` | blockquote |
| dòng `• ` / `- ` / `1. ` | giữ nguyên, `white-space: pre-wrap` |
| `<@U…>` / `<@U…|name>` | chip `@displayName` (`bg-primary/10 text-primary`), tooltip realName |
| `<#C…|name>` / `<#C…>` | `#name` (tra `channels`), click mở `https://<workspaceDomain>/archives/C…` |
| `<!here>` `<!channel>` `<!everyone>` `<!subteam^…|@x>` | chip `@here`… / `@x` |
| `<url|label>` / `<url>` / `<mailto:…>` | `<a>` mở bằng `openUrl`, không điều hướng webview |
| `:smile:` | unicode qua bảng shortcode offline (FE chọn lib nhẹ hoặc bảng tĩnh); không biết → giữ nguyên |
| `&lt;` `&gt;` `&amp;` | giải mã |

### 13.7 Store & đồng bộ

- `src/stores/slackThreads.ts`: state `threads`, `selectedId`, `view`, `progress`; actions `fetchList(query?)`, `open(id)`, `add`, `refresh`, `markSeen`, `rename`, `setProject`, `remove(ids)` (confirm qua `useConfirm`), `promptText(id, includeOriginal)`.
- Nghe `slack-threads://changed` → refetch list (debounce 300 ms), refetch view nếu là thread đang mở; `slack-threads://progress` → cập nhật `progress[id]`. Refetch khi focus (sidecar MCP ghi DB không emit event).
- Giữ vị trí cuộn khi refetch; đang ở cuối → cuộn theo tin mới.

### 13.8 i18n

Namespace mới `slackThread` (`src/i18n/locales/{vi,en}/slackThread.json`), `pnpm check:i18n` xanh. Khoá tối thiểu: `window.title`, `nav`, `sidebar.{search,add,other,allProjects,noProject}`, `status.*` (5), `phase.*` (7), `errors.<code>` cho **mọi** mã §4 + §8.3 + `invalid_project`/`invalid_title`/`not_found`, `warnings.<code>` (6), `header.{showOriginal,refresh,openInSlack}`, `message.{edited,untranslated,translating,translateFailed,showOriginal,showTranslation,copy,copied,app}`, `replies` (số nhiều), `file.{open,openInSlack,tooLarge,unavailable,error}`, `add.{title,urlLabel,urlPlaceholder,project,submit,noToken,existing}`, `legacy.{badge,banner}`, `lightbox.{openDefault,close}`, `picker.{includeOriginal,truncated}`. Câu mẫu vi:
- `no_token`: "Chưa có token Slack. Hãy thêm MCP server Slack có biến SLACK_MCP_XOXP_TOKEN trong Cài đặt → MCP."
- `dm_unsupported`: "Không hỗ trợ thread trong tin nhắn trực tiếp (DM) — token không có quyền đọc DM."
- `missing_scope`: "Token Slack thiếu quyền ({detail}). Cần channels:history, groups:history."
- `channel_not_found`: "Không tìm thấy kênh — token không thuộc workspace này hoặc không có quyền vào kênh."
- warning `files_scope`: "Token thiếu quyền files:read nên không tải được tệp; bấm vào tệp để mở trên Slack."

### 13.9 CapturePicker (composer) [v1]

- Nguồn Slack đổi từ `useCapturesStore` sang `useSlackThreadsStore().threads` (`list_slack_threads` không filter); web/note/todo giữ nguyên. Row Slack: title, `#channel`, `📎fileCount`, `relativeAge(updatedAt)`, icon trạng thái nếu chưa `ready`; `hay` = title + channel + workspace + preview. Nghe `slack-threads://changed` để refetch.
- Footer picker có checkbox "Kèm bản gốc" (chỉ hiện khi filter `all`/`slack`; lưu `localStorage` `devdy.capturePicker.slackOriginal`).
- Chọn Slack → gọi `get_slack_thread_prompt_text(id, includeOriginal)` → `emit('insert', text)`. Web/note/todo vẫn chèn reference 1 dòng như cũ. Lỗi command → toast, không chèn.
- Định dạng `text` (do BE sinh, `mrkdwn::prompt_text`):

```
[Slack] <title> (id: <id>) — #<channel> · <workspace> · <thread_url>
<Tên> (2026-10-08 09:12 UTC): <nội dung tiếng Việt, plain markdown: mention → @Tên, <url|label> → [label](url), code giữ nguyên>
  > Gốc: <text_raw dạng plain>          ← chỉ khi includeOriginal
  📎 <name> — <ABSOLUTE path | "not downloaded: <permalink>">
…
```

  Tin chưa dịch → dùng bản gốc + hậu tố ` [chưa dịch]`. Legacy chưa fetch → `legacy_content`. Thread chưa `ready` → dòng thứ 2 `(status: <fetch_status>)`.
- Giới hạn 20 000 ký tự: vượt → cắt ở ranh giới tin, `truncated = true`, thêm dòng cuối `… (truncated — read the rest with mcp__devdy__slack_threads_read id=<id> offset=<N>)`, `N` = offset ký tự tương ứng trong output của `slack_threads_read` cùng chế độ `original` (BE và MCP dùng cùng quy ước offset = số ký tự phần thân transcript đã chèn; chênh lệch nhỏ chấp nhận được, chỉ là gợi ý).

## 14. MCP (WP-MCP, `sidecar-mcp`)

- `captureTools` chỉ còn web: `capture-tools.mjs` bỏ `KINDS.slack`; `store.mjs` bỏ `CAPTURE_KINDS.slack`; `index.mjs` bỏ `...captureTools('slack', …)`, cập nhật mô tả server.
- File mới `sidecar-mcp/lib/slack-thread-tools.mjs` + `sidecar-mcp/lib/slack-store.mjs` (SQL trên bảng §5.1, `DEVDY_DB_PATH`, `<app_data>` = thư mục chứa DB). Scope `project` (mặc định, `DEVDY_PROJECT_ID`) | `all` như notes.

| Tool | Hành vi |
|---|---|
| `slack_threads_list` | `scope`, `limit` (≤ 200). Dòng: `- [id] title · #channel · workspace · <fetch_status> · N replies · last reply <ISO>`; thêm `· error: <code>`, `· legacy`. |
| `slack_threads_read` | `id`, `original`: `"none"` (mặc định) \| `"both"` \| `"only"`, `max_chars` (20000), `offset`. Header metadata (workspace, channel, thread_url, fetch_status, fetched_at, project_id, warnings, error). Mỗi tin: `### <Tên> · <YYYY-MM-DD HH:mm UTC> · ts=<ts>[ · edited]` rồi nội dung (vi; thiếu → gốc + `[untranslated]`; `both` thêm khối `> Original:`; `only` chỉ gốc). Mention `<@U…>` → `@Tên` (`slack_users` theo `team_id`), `<#C…|x>` → `#x`, `<url|label>` → `[label](url)`, entity giải mã, code giữ nguyên. File: `- 📎 name — <ABSOLUTE path> · size · mime` hoặc `— not downloaded (<status>): <permalink>`. Reaction `reactions: :+1: 2, …`. Legacy chưa fetch → `legacy_content` + file legacy + dòng `status: legacy snapshot (not yet re-fetched)`. Cắt theo `max_chars`/`offset` + "Continue with offset=N". |
| `slack_threads_search` | `query`, `scope`, `limit`. Substring không phân biệt hoa thường trên title, channel, `text_vi`, `text_raw`, `legacy_content`; trả thread + ≤ 3 tin khớp `  - <Tên> ts=<ts>: …snippet…`. |
| `slack_threads_delete` | `id` \| `ids`: xoá tường minh `slack_messages`, `slack_files`, rồi `slack_threads` (1 transaction), sau đó xoá `slack-threads/<id>/` (chỉ id an toàn). |
| `slack_threads_rename` | 1–200 ký tự, `title_custom = 1`, bump `updated_at`. |
| `slack_threads_set_project` | `scope` `project` \| `global`. |
| ~~`update`, `append`, `reorder`, `read_attachment`~~ | **bỏ** (nội dung do Slack quyết định; sắp xếp là việc UI; `read` đã trả đường dẫn tuyệt đối). |

- `test-captures.mjs`: phần web giữ; thêm smoke test Slack trên DB tạm đã chạy toàn bộ migration (dựng schema bằng chính SQL §5.1 nếu test không chạy được sqlx).

## 15. Chuyển đổi 10 bản ghi cũ [v1]

1. Migration 0041 (§5.1): chép sang `slack_threads` (giữ id, title, `title_custom`, project, position, `created_at`, `updated_at`, `thread_url`, `channel_id`/`thread_ts` parse best-effort, `legacy=1`, `legacy_content`, `pending`), chép file sang `slack_files` (đường dẫn cũ, `done`), gộp trùng (giữ `updated_at` mới nhất), xoá khỏi `captures`/`capture_attachments`. Không đụng file.
2. Startup: dọn dir của `slack_legacy_orphans`; enqueue các dòng `pending` (theo `position`).
3. Job: parse lại `thread_url` bằng `parse_thread_link` (lỗi → `error/legacy_no_url`, giữ nguyên); khoá đã thuộc dòng khác → gộp §12.
4. Fetch thành công bước 3 §7.3 → thay snapshot cũ (§7.3 "Legacy thành công"); title/project/position giữ (title tự sinh nếu `title_custom = 0`).
5. Lỗi fatal → `error`, giữ `legacy_content` + file cũ; lỗi tạm thời tự thử lại ở lần khởi động sau hoặc bấm "Làm mới".

## 16. Gói việc song song & hợp đồng

| Gói | Sở hữu file (không gói nào sửa file gói khác) | Dựa trên |
|---|---|---|
| **WP-BE** | `src-tauri/**` (trừ `src-tauri/capabilities/default.json`), `docs/inbox-api.md`, `scripts/send-slack-thread.sh` | §4–§12, §13.9 (định dạng prompt text), §15 |
| **WP-FE** | `src/**`, `src-tauri/capabilities/default.json` | §10, §13. Code trước bằng mock `invoke` theo §10.1 |
| **WP-MCP** | `sidecar-mcp/**` | §5.1, §5.2, §8.1 (quy ước mrkdwn/mention), §14 |

Hợp đồng chốt (đổi phải báo TL):
1. SQL §5.1 nguyên văn (bảng, cột, enum `fetch_status`/`translate_status`/`download_status`, JSON `reactions`/`file_ids`/`warnings`).
2. Tên/args/kiểu command §10.1–10.2; lỗi `"<code>: <detail>"`.
3. Event §10.3.
4. Mã lỗi §4/§8.3, mã warning §8.4.
5. `text_raw`/`text_vi` là Slack mrkdwn với `<@U…>`; FE/MCP tự resolve tên.
6. DB lưu đường dẫn relative; command/MCP trả absolute; `filePath = null` khi chưa tải.
7. Sau 0041, `captures` không còn `kind='slack'`; FE/MCP không được đọc Slack từ `captures`.

Thứ tự tích hợp: WP-BE merge migration + command đọc DB (list/get/rename/project/delete/prompt text) sớm nhất để FE/MCP chạy dữ liệu thật; pipeline fetch/dịch hoàn thiện sau, không đổi hợp đồng.

## 17. Acceptance Criteria

Chuẩn bị: token thật workspace `malme` (MCP server `slack`); 1 thread công khai có tiếng Nhật + Anh, ≥ 1 ảnh, ≥ 1 file khác, mention, code block, reaction, tin đã sửa; một **bản sao** DB thật có 10 bản ghi cũ.

**Migration & tách core**
- **AC-01** Test migration §5.1 (Rust) xanh: chép đủ trường, gộp trùng giữ bản mới nhất, dòng không URL `channel_id NULL`, `captures` chỉ còn web, `capture_attachments` chỉ còn file web, `slack_legacy_orphans` đúng.
- **AC-02** Trên bản sao DB thật: sau khi chạy bản mới, `SELECT COUNT(*) FROM captures WHERE kind='slack'` = 0, `slack_threads` có 10 dòng giữ đúng id/title/`title_custom`/project/position/`created_at`; 33 file vẫn tồn tại trên đĩa ngay sau migration (trước khi re-fetch).
- **AC-03** Web không đổi: test web hiện có (`web_dedup_*`, `web_pages_endpoint`, `end_to_end_contract` phần web…) xanh; POST `/v1/web-pages` zip/markdown như cũ; màn Web trong ItemListWindow và CaptureWindow web hoạt động; MCP `web_pages_*` đủ 10 tool, kết quả như trước.
- **AC-04** Không còn `CaptureKind::Slack` / `kind === 'slack'` trong `inbox_api/*`, `commands/captures.rs`, `stores/captures.ts`, `CaptureWindow.vue`, `ItemListWindow.vue`, `capture-tools.mjs`, `store.mjs` (grep); `invoke('list_captures', {kind:'slack'})` bị từ chối.

**Link & thêm thread**
- **AC-05** Unit test `parse_thread_link` đủ 4 dòng bảng §4 + host sai + thiếu `p` + 15 chữ số.
- **AC-06** UI: dán link hợp lệ → thread hiện ở sidebar `pending/fetching` ≤ 1 s, sau đó `ready` với bản tiếng Việt.
- **AC-07** Link reply (`?thread_ts&cid`) → lấy đủ thread cha, không tạo dòng thứ hai so với link tin cha.
- **AC-08** Link DM → lỗi inline, không tạo dòng.
- **AC-09** Thêm lại cùng link → không tạo dòng mới, toast "đã có — đang làm mới", giữ title/project/position.

**Inbox API**
- **AC-10** `POST` JSON `{"url":…}` → `202 created/pending`; gửi lại → `202 existing`, cùng id.
- **AC-11** zip/markdown → `415` thông điệp §11; `{"url":"abc"}` → `400 invalid_link`; `projectId` lạ → `400 invalid_project`; JSON hỏng → `400 invalid_json`; > 16 KB → `413`.
- **AC-12** Re-import thread đã đổi tên + có project + đã đổi vị trí, gửi kèm `projectId` khác → title/project/position giữ nguyên; reply mới xuất hiện. Thread chưa có project + `projectId` → được gán.
- **AC-13** `/health.features` có `"slack-thread-link"`.
- **AC-14** Thông báo OS chỉ xuất hiện cho job từ API (ready/error), không cho thêm bằng UI hay chuyển đổi legacy; click mở cửa sổ Slack đúng thread.

**Token, scope & lỗi Slack**
- **AC-15** Không có token → dòng `error/no_token` + banner; dialog có cảnh báo; API `202 … "errorCode":"no_token"`.
- **AC-16** Token sai → Làm mới → `error/invalid_auth`, dữ liệu cũ vẫn hiển thị.
- **AC-17** Mock Slack (base URL inject): `missing_scope` history → `missing_scope`; `is_mpim` → `dm_unsupported`; `channel_not_found`; 429 `Retry-After: 1` ×2 rồi ok → thành công; 429 liên tục → `rate_limited`; 3 trang `next_cursor` → đủ tin đúng thứ tự.
- **AC-18** Mock `auth.test` header `x-oauth-scopes` thiếu `files:read` → không gọi tải file, file `unavailable`, warning `files_scope`; thiếu `users:read` → không gọi `users.info`, warning `users_scope`; thread vẫn `ready`.
- **AC-19** 2 token khác workspace → chọn đúng theo host; không khớp → `no_token_for_workspace`.
- **AC-20** `xoxp-` không xuất hiện trong log, event, lỗi, response API, output MCP, prompt dịch (grep).
- **AC-21** Request avatar không có header `Authorization` (test `files.rs`).

**Dịch**
- **AC-22** Tin Nhật/Anh có bản tiếng Việt; code block/inline code giống byte-for-byte; mention hiển thị `@Tên` đúng ở cả hai bản.
- **AC-23** Unit `protect/restore` round-trip (code block + inline + `<@U1>` + `<https://x|y>` + `:smile:`); thiếu placeholder → block lỗi.
- **AC-24** Unit `parse_batch_output`: đảo thứ tự, text ngoài thẻ, thiếu block, id lạ → chỉ block hợp lệ được nhận; tin thiếu dịch lại riêng 1 lần; vẫn lỗi → `error`, thread `ready` + `translate_partial`.
- **AC-25** Unit `build_batches`: thread > 8000 ký tự → nhiều lô, mỗi lô ≤ 8000; tin 12 000 ký tự cắt và ghép đúng.
- **AC-26** Làm mới thread không đổi → 0 lô dịch (`phase:'translate'` `total=0`), `text_vi` giữ nguyên.
- **AC-27** Sửa 1 tin + thêm 1 reply trên Slack → Làm mới → chỉ 2 tin được dịch; tin sửa có "(đã chỉnh sửa)"; tin bị xoá trên Slack biến mất.
- **AC-28** Đang dịch thread vẫn dùng được popover dịch trong RunView (không phải chờ hết lô).
- **AC-29** Ngôn ngữ đích luôn tiếng Việt kể cả khi `translate_target_lang = en`.

**File**
- **AC-30** File tải vào `slack-threads/<id>/files/<slack_file_id>/`; ảnh thumbnail → lightbox → mở app mặc định; file khác mở app mặc định.
- **AC-31** Làm mới lần 2 không tải lại file không đổi (mtime không đổi, không có request tải).
- **AC-32** Mock `text/html` khi tải → `error` + warning `files_scope`; thẻ file có "Mở trên Slack"; tin vẫn đủ.
- **AC-33** File > 100 MB (mock) → `too_large`, không tải.
- **AC-34** Xoá thread (UI hoặc MCP) → xoá dòng thread, tin, file và thư mục `slack-threads/<id>/`; avatar cache còn nguyên.

**Trạng thái**
- **AC-35** Progress đúng thứ tự `auth → messages → users → files → translate → done`; sidebar spinner + banner `done/total`.
- **AC-36** Tắt app khi đang `translating` → mở lại tự chạy tiếp tới `ready`.
- **AC-37** Bấm Làm mới 3 lần nhanh → 1 job + tối đa 1 lần chạy lại; nút disable khi đang chạy.
- **AC-38** Xoá thread khi đang fetch → mọi dữ liệu + dir biến mất, job không tạo lại.

**Dữ liệu cũ**
- **AC-39** Bản sao DB thật, token đủ quyền → cả 10 dòng tự về `ready`, `legacy = 0`, file zip cũ bị xoá khỏi `files/`, file Slack mới được tải, title tự đặt (9) / project (4) / thứ tự giữ nguyên.
- **AC-40** Dòng legacy không có `thread_url` (QA tạo trước khi migrate) → `error/legacy_no_url`, UI hiện markdown + file cũ với banner "Bản lưu cũ".
- **AC-41** Hai dòng legacy cùng thread (QA tạo, một dòng URL có query `?thread_ts`/khác dạng để SQL không parse được khoá) → sau job còn 1 dòng, giữ title/project của dòng `updated_at` mới nhất, không có lỗi; thư mục dòng bị gộp bị xoá.

**UI kiểu Slack**
- **AC-42** Cửa sổ `slack-threads` mở từ nav sidebar App, menu `view.slackThreads`, notification; mở lần 2 chỉ focus cửa sổ cũ; ItemListWindow không còn tab Slack.
- **AC-43** Sidebar nhóm workspace → channel, thu gọn nhớ sau khi mở lại; tìm theo từ chỉ có trong bản tiếng Việt → ra thread; lọc project đúng (gồm "Không project").
- **AC-44** Badge tin mới: xem → có reply mới → Làm mới → badge = số reply mới; mở thread → mất badge.
- **AC-45** Khung thread: tin cha trên cùng, "N phản hồi" đúng, gộp tin liên tiếp ≤ 5 phút, avatar hoặc chữ cái đầu, tooltip ngày đủ, reaction đúng emoji + số.
- **AC-46** Renderer: từng dòng bảng §13.6 đúng; link mở trình duyệt ngoài; `&lt;script&gt;` không chạy.
- **AC-47** Toggle "Xem bản gốc" toàn thread nhớ sau khi mở lại; toggle từng tin chỉ đổi tin đó; "Sao chép" copy text đang hiển thị với `@Tên`.
- **AC-48** Dark/light đọc rõ, chỉ token màu có sẵn; footer không có nút Edit; ProjectSelect gán/bỏ project được; đổi tên bằng F2 giữ sau Làm mới.

**CapturePicker**
- **AC-49** Picker liệt kê thread Slack từ bảng mới (cả trạng thái chưa ready, có icon); chọn → composer nhận transcript tiếng Việt đúng định dạng §13.9 với `@Tên` và đường dẫn file tuyệt đối; bật "Kèm bản gốc" → mỗi tin có dòng `> Gốc:`; thread > 20 000 ký tự → bị cắt kèm dòng hướng dẫn `slack_threads_read`. Web/note/todo vẫn chèn reference 1 dòng như cũ.

**MCP**
- **AC-50** `slack_threads_list/read/search/delete/rename/set_project` chạy trên DB thật (smoke test); `read` mặc định tiếng Việt với `@Tên`; `original:"both"`/`"only"` đúng; đường dẫn file tồn tại; legacy chưa fetch trả markdown cũ.
- **AC-51** `tools/list` không còn `slack_threads_update/append/reorder/read_attachment`; 10 tool `web_pages_*` không đổi.
- **AC-52** `slack_threads_search` theo từ chỉ có trong bản gốc tiếng Nhật → ra thread + snippet gốc.

**Chất lượng**
- **AC-53** `cargo check`, `cargo test`, `cargo clippy` (không thêm warning), `pnpm typecheck`, `pnpm check:i18n`, `pnpm lint`, smoke test sidecar-mcp đều xanh.

## 18. Quyết định đã chốt cho câu hỏi mở v1

| Q | Quyết định của người dùng | Ở đâu trong spec |
|---|---|---|
| Q1 scope token / Q9 Enterprise Grid | BE kiểm scope lúc chạy qua header `x-oauth-scopes` của `auth.test`, hiện warning; không chặn | §7.1-3, §8.4, AC-18 |
| Q2 trùng bản ghi cũ | Gộp làm một, giữ title/project của bản cập nhật gần nhất, không trạng thái lỗi | D9, §5.1, §12, AC-01, AC-41 |
| Q3 `projectId` khi import lại | Chỉ gán khi thread chưa có project | §12, AC-12 |
| Q4 thông báo OS | Chỉ cho job từ Inbox API | §9.3, AC-14 |
| Q5 ngôn ngữ đích | Cố định tiếng Việt | §8.2, AC-29 |
| Q6 `slack_threads_refresh` MCP | Không làm v1 | §2.2 |
| Q7 thread dài | Không hỏi xác nhận, dịch hết | §8.2 |
| Q8 cache | Xoá thread xoá file của nó; không GC tự động, avatar cache được giữ | §5.2, AC-34 |

Câu hỏi mở còn lại: **không có**. Rủi ro kỹ thuật cần WP-BE xác nhận sớm (không phải quyết định sản phẩm): `CREATE TEMP TABLE … AS WITH … ROW_NUMBER()` trong transaction của `sqlx::migrate!` (§5.1 đã có phương án thay bằng bảng thường).
