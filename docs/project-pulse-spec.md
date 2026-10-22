---
status: draft
version: v2
date: 2026-10-22
---

# Spec: Project Pulse v2 — theo dõi Slack thread & GitHub issue, tổng hợp tiến độ

> **Quyết định đã chốt với người dùng**
> - Mô hình **danh sách theo dõi (watch list)**: Pulse chỉ tổng hợp những Slack thread / GitHub issue mà người dùng **bật theo dõi** — không tự quét mọi thread / mọi issue.
> - Bật theo dõi bằng 2 cách: (1) nói với AI trong session ("theo dõi slack X, issue Y") → AI gọi MCP `devdy`; (2) bấm nút **"Theo dõi"** trên Slack thread / issue trong Devdy.
> - Nguồn chỉ gồm **Slack thread** và **GitHub issue**. Không Calendar, không todo, không mốc nhập tay.
> - Devdy **chỉ đọc** Slack / GitHub.
> - Quy tắc màu 🔴🟡🟢 **cố định**.
> - Có nút **Làm mới** để kéo dữ liệu mới và tổng hợp lại.
> - Slack token là user token; ngưỡng 24h tính giờ thực, kể cả cuối tuần.
> - Mở màn Pulse **không** tự refresh; **không** cho sửa tay kết quả AI.
> - Bản này **thay thế** bản v1 (commitment nhập tay + signal tự quét).

## 1. Tổng quan

- **Mục tiêu:** người dùng có nhiều dự án, mỗi dự án nhiều Slack thread và GitHub issue; lịch và việc được chốt rải rác ở cả hai nơi. Pulse gom các nguồn **đang theo dõi** theo dự án, dùng AI đọc từng nguồn để rút ra: trạng thái, việc tiếp theo, ai đang giữ bóng, hạn đã chốt, quyết định đã chốt — rồi hiển thị tổng quan sức khoẻ từng dự án.
- **Actor:** 1 người dùng (chủ máy Devdy) và AI trong session Devdy (qua MCP).
- **Trong phạm vi:**
  - Bảng `pulse_watches` (nguồn đang theo dõi) + `pulse_digests` (kết quả AI tổng hợp mỗi nguồn).
  - MCP tools `pulse_watch`, `pulse_unwatch`, `pulse_list`, `pulse_status`.
  - Nút "Theo dõi / Bỏ theo dõi" trên: SlackThreadsWindow (thread), Gantt (dòng issue), RunView (run tạo từ issue), và ô "+ Theo dõi" trong màn Pulse (dán link).
  - Màn **Pulse** (`/pulse`): thẻ theo dự án, danh sách nguồn, nút Làm mới (toàn bộ / từng dự án / từng nguồn).
- **Ngoài phạm vi:** ghi ngược ra Slack/GitHub; tự phát hiện nguồn mới; Calendar/todo; GitLab; thông báo đẩy / digest buổi sáng; tab "Tuần này" (để sau).

## 2. Thuật ngữ
- **Watch:** một nguồn đang được theo dõi (1 Slack thread hoặc 1 GitHub issue), thuộc 1 dự án.
- **Digest:** bản tổng hợp do AI tạo cho 1 watch tại một thời điểm (trạng thái, việc tiếp theo, hạn…), lưu kèm `content_hash` của nội dung nguồn lúc tổng hợp.
- **Bóng ở mình:** nguồn đang chờ người dùng phản hồi/hành động.

## 3. Luồng nghiệp vụ

### 3.1 Bật theo dõi qua chat với AI
1. Người dùng nói: "theo dõi thread https://malme.slack.com/archives/C…/p… và issue org/app#12".
2. AI gọi `pulse_watch({ url })` mỗi nguồn (hoặc `{ urls: [...] }`). `project` mặc định = `DEVDY_PROJECT_ID` của session; session không có project → tool trả lỗi `project_required`, AI hỏi lại người dùng.
3. MCP chuẩn hoá nguồn (BR-02), ghi `pulse_watches` với `resolve_status='pending'`, trả về `{ id, source_type, source_ref, project, already_watching }`. MCP **không** gọi mạng.
4. App nghe thay đổi (BR-11) → resolve nguồn (Slack: capture thread qua cơ chế `add_slack_thread` có sẵn nếu chưa có; GitHub: kiểm tra issue tồn tại) → chạy tổng hợp lần đầu cho watch đó.
5. Màn Pulse (nếu đang mở) cập nhật thẻ dự án.

### 3.2 Bật theo dõi bằng nút
- **SlackThreadsWindow:** header thread có nút toggle "👁 Theo dõi" / "Đang theo dõi". Thread chưa gắn project → bấm sẽ mở chọn project (đồng thời `set_slack_thread_project`).
- **Gantt (`IssuesGanttView`):** menu của dòng issue → "Theo dõi trong Pulse" (project = project của Gantt).
- **RunView:** run được tạo từ issue (`fetch_issue`) → nút "Theo dõi issue" trên header.
- **Màn Pulse:** ô "+ Theo dõi" nhận link Slack thread hoặc link/`owner/repo#n` GitHub issue + chọn project.
- Bấm khi đã theo dõi → bỏ theo dõi (confirm nhẹ "Bỏ theo dõi nguồn này?").

### 3.3 Làm mới (Refresh)
1. Người dùng bấm "⟳ Làm mới" (toàn bộ) / icon refresh trên thẻ dự án / trên một nguồn.
2. Với mỗi watch `active=1` trong phạm vi:
   - a. **Lấy dữ liệu mới** — Slack: chạy `refresh_slack_thread` (job có sẵn, kèm dịch); GitHub: lấy issue + comments + state + assignees + milestone (tái dùng `build_issue_markdown` / octocrab với GitHub account của project).
   - b. Tính `content_hash` của nội dung chuẩn hoá (BR-05). **Trùng** hash của digest gần nhất → giữ digest cũ, chỉ cập nhật `checked_at`, **không** gọi AI.
   - c. Khác → gọi AI tổng hợp (BR-06), lưu digest mới.
3. Chạy tối đa **3 nguồn đồng thời**; tiến độ hiện "Đang làm mới 4/12".
4. Xong → tính lại sức khoẻ dự án (BR-08), cập nhật "Cập nhật lúc HH:mm".
5. Mở màn Pulse **không** tự refresh — hiển thị digest đã lưu ngay lập tức. Ngoại lệ: watch chưa từng có digest → tự tổng hợp.

### 3.4 Hỏi tiến độ qua chat
- Người dùng hỏi "dự án này đang thế nào?" → AI gọi `pulse_status({ project? })` → nhận digest đã lưu (không refresh) + sức khoẻ → trả lời. Muốn số liệu mới, AI gọi `pulse_status({ refresh: true })` → MCP ghi yêu cầu refresh (BR-11), chờ tối đa 90 giây để app xử lý, rồi trả kết quả (hết giờ → trả bản đang có kèm `stale: true`).

### 3.5 Nhánh rẽ
- Link không nhận dạng được → lỗi `invalid_source` ("Chỉ hỗ trợ link Slack thread hoặc GitHub issue").
- Nguồn đã được theo dõi → trả `already_watching: true`, không tạo bản ghi mới (nếu đang `active=0` → bật lại).
- Slack fetch lỗi / GitHub 401/404 → watch có `last_error`, giữ digest cũ, thẻ hiện chip lỗi; nguồn khác vẫn chạy.
- AI tổng hợp lỗi (sidecar không sẵn sàng, output sai schema 2 lần) → giữ digest cũ, `last_error='summarize_failed'`.
- Link là PR (không phải issue) → `invalid_source` (PR đã có PR Inbox riêng).

## 4. Quy tắc nghiệp vụ

- **BR-01 — Một nguồn, một watch:** UNIQUE `(source_type, source_ref)` trên toàn app. Một nguồn chỉ thuộc **1 dự án**; watch lại với project khác → lỗi `watched_in_other_project` (kèm tên dự án), người dùng có thể chuyển dự án qua `pulse_watch({ url, project, move: true })` hoặc menu "Chuyển dự án".
- **BR-02 — Chuẩn hoá nguồn:**
  - Slack: parse bằng `slack_threads/link.rs` → `source_ref = <channel_id>:<thread_ts>`. Link tới một reply (`?thread_ts=`) → lấy thread gốc. Nhận cả `slack_thread_id` nội bộ của Devdy.
  - GitHub: `https://github.com/<o>/<r>/issues/<n>` hoặc `<o>/<r>#<n>` → `source_ref = <o>/<r>#<n>` (lowercase owner/repo). Dạng `#<n>` trần chỉ hợp lệ khi project có đúng 1 repo GitHub.
- **BR-03 — Project của watch:** ưu tiên tham số `project` (id hoặc tên, khớp không phân biệt hoa thường) → `DEVDY_PROJECT_ID` → (nút bấm) project của ngữ cảnh. Với Slack, nếu thread đã có `project_id` khác project chỉ định → cập nhật thread theo project của watch.
- **BR-04 — Bỏ theo dõi:** đặt `active=0` (giữ lịch sử digest); không xoá Slack thread đã capture. Watch `active=0` không hiện trên Pulse, không refresh.
- **BR-05 — Nội dung để hash & tổng hợp:**
  - Slack: toàn bộ tin nhắn (`seq` tăng dần) dạng `[<ISO time>] <author>(<me|other>): <text_vi ?? text_raw>`; giới hạn **60.000 ký tự** — vượt thì giữ tin nhắn gốc + các tin nhắn mới nhất vừa đủ, ghi `truncated=true`.
  - GitHub: title, state, labels, assignees, milestone (+ due), board Start/Deadline/Status nếu có (từ cache `list_milestone_board`), body, comments theo thời gian; cùng giới hạn 60.000 ký tự.
  - `content_hash = sha256` của chuỗi trên.
- **BR-06 — AI tổng hợp:** gọi engine sidecar ấm sẵn (cùng engine với dịch Slack, `commands/translate.rs`), prompt yêu cầu **chỉ trả JSON** theo schema §5.2, ngôn ngữ **tiếng Việt**, kèm ngày hiện tại và "người dùng là `<me>`" để AI xác định bóng. Validate JSON; sai schema → retry 1 lần với lỗi đính kèm; vẫn sai → lỗi `summarize_failed`. Mỗi `deadline`/`decision` phải kèm `evidence` (trích ≤ 200 ký tự từ nguồn) — thiếu evidence thì bỏ mục đó.
- **BR-07 — Sự thật cứng ghi đè AI** (không phụ thuộc AI):
  - Issue `state=closed` → `status='done'` bất kể AI.
  - Slack "bóng ở mình" kiểm tra bằng luật: tin nhắn có nghĩa cuối cùng (bỏ bot, join/leave, chỉ-emoji) của người khác **và** không mention người khác ngoài mình → `ball='me'`, `ball_since = ts` của tin nhắn đó. Ngược lại dùng `ball` của AI. `my_slack_user_id` từ `auth.test` (lưu `slack_identities`).
  - Hạn từ board GitHub (`deadline ?? milestoneDueOn`) **ưu tiên** hơn hạn AI trích cho issue đó.
- **BR-08 — Sức khoẻ từng watch:**
  - 🔴 khi: `status='blocked'`; hoặc có hạn `due_at < now` mà `status ∉ {done}`; hoặc `ball='me'` và `now − ball_since > 24h` (giờ thực).
  - 🟡 khi (không đỏ): hạn trong 72h tới; `status='at_risk'`; `ball='customer'` và `now − ball_since ≥ 5 ngày`; có `last_error`.
  - ✅ khi `status='done'`.
  - 🟢 còn lại.
- **BR-09 — Sức khoẻ dự án:** đỏ nếu có ≥1 watch đỏ; vàng nếu có ≥1 watch vàng; xanh nếu còn watch chưa xong; ⚪ nếu không có watch active nào hoặc tất cả ✅. Thứ tự thẻ: 🔴 → 🟡 → 🟢 → ⚪, cùng màu → hạn gần nhất tăng dần → tên A→Z.
- **BR-10 — Mốc kế tiếp của dự án:** hạn nhỏ nhất `≥ now` trong mọi digest của dự án (watch chưa done).
- **BR-11 — Giao tiếp MCP → app:** MCP chỉ ghi SQLite (giống các tool hiện có). App phát hiện thay đổi bằng cách đọc `pulse_watches.updated_at` / bảng `pulse_requests(id, kind: 'resolve'|'refresh', watch_id|project_id|NULL, created_at, done_at)`; app poll bảng này mỗi **3 giây** chỉ khi có session đang chạy hoặc màn Pulse đang mở (rẻ: 1 query index). Xử lý xong ghi `done_at`.
- **BR-12 — Watch tự kết thúc:** issue closed **hoặc** AI `status='done'` liên tục 7 ngày → chuyển vào nhóm "Đã xong" (thu gọn), vẫn `active=1` tới khi người dùng bỏ theo dõi. Không tự bỏ theo dõi.

## 5. Mô hình dữ liệu (migration `0050_project_pulse.sql`)

### 5.1 `pulse_watches`
| Trường | Kiểu | Bắt buộc | Ghi chú |
|---|---|---|---|
| id | TEXT UUID | Có | PK |
| project_id | TEXT | Có | FK `projects(id)` ON DELETE CASCADE |
| source_type | TEXT | Có | CHECK `slack\|github_issue` |
| source_ref | TEXT | Có | BR-02; UNIQUE `(source_type, source_ref)` |
| source_url | TEXT | Có | Link gốc để mở |
| slack_thread_id | TEXT | Không | FK `slack_threads(id)` ON DELETE SET NULL; set sau khi resolve |
| title | TEXT | Không | Thread title / issue title, cập nhật mỗi lần refresh |
| active | INTEGER | Có | 1/0, mặc định 1 |
| added_via | TEXT | Có | CHECK `mcp\|button\|paste` |
| added_run_id | TEXT | Không | Run đã gọi MCP (từ env `DEVDY_RUN_ID` nếu có) |
| resolve_status | TEXT | Có | `pending\|ready\|error` |
| last_error | TEXT | Không | Mã lỗi gần nhất (`slack_fetch_failed`, `github_not_found`, `github_auth`, `summarize_failed`…) |
| checked_at | TEXT | Không | Lần refresh gần nhất (kể cả không đổi) |
| created_at / updated_at | TEXT | Có | RFC3339 |

### 5.2 `pulse_digests` (giữ **5 bản gần nhất** mỗi watch)
| Trường | Kiểu | Ghi chú |
|---|---|---|
| id | TEXT UUID | PK |
| watch_id | TEXT | FK ON DELETE CASCADE; index `(watch_id, created_at DESC)` |
| content_hash | TEXT | BR-05 |
| status | TEXT | `on_track\|at_risk\|blocked\|waiting\|done` |
| ball | TEXT | `me\|customer\|team\|none` |
| ball_since | TEXT | RFC3339, có thể NULL |
| summary | TEXT | 1–3 câu tiếng Việt, ≤ 500 ký tự |
| next_action | TEXT | Việc tiếp theo, ≤ 200 ký tự, có thể NULL |
| deadlines | TEXT (JSON) | `[{ "title", "due_at" (RFC3339), "all_day": bool, "evidence" }]`, tối đa 10 |
| decisions | TEXT (JSON) | `[{ "text", "decided_at", "evidence" }]`, tối đa 10 |
| open_questions | TEXT (JSON) | `[{ "text", "asked_by": "me\|other", "evidence" }]`, tối đa 10 |
| truncated | INTEGER | 1 nếu nội dung bị cắt (BR-05) |
| model | TEXT | Model đã dùng |
| created_at | TEXT | RFC3339 |

### 5.3 Phụ trợ
- `pulse_requests` — BR-11.
- `slack_identities(team_id PK, user_id, fetched_at)` — bổ sung `user_id` vào `AuthInfo` (`slack_threads/api.rs`) và upsert khi `auth_test` thành công.

## 6. Giao diện

> Trước khi code UI chạy quy trình skill `fe-ui-consistency`; tái dùng `Card`, `Button`, `AppSelect`, `Skeleton`, `ProjectSelect`.

### 6.1 Màn `/pulse`
- **Header:** "Pulse", ô "+ Theo dõi" (dán link + chọn project), nút "⟳ Làm mới", "Cập nhật lúc HH:mm", chip tiến độ "Đang làm mới 4/12".
- **Bộ lọc:** tìm tên dự án; toggle "Chỉ dự án cần chú ý" (🔴+🟡).
- **Thẻ dự án** (grid 1–3 cột): chấm màu + tên + icon refresh dự án; "Mốc kế: <title> — dd/MM (còn N ngày | hôm nay | trễ N ngày)"; chip đếm: Cần mình xử lý · Chờ khách · Trễ · Đang theo dõi N; danh sách watch (tối đa 5, ưu tiên đỏ → vàng → xanh, "+N nữa"):
  - Dòng watch: icon nguồn (Slack/GitHub) · màu · title · `summary` (1 dòng, tooltip đầy đủ) · nhãn bóng ("Bạn cần trả lời · 30h", "Chờ khách · 6 ngày") · hạn gần nhất.
  - Click dòng → mở **panel chi tiết** (drawer): summary, next_action, deadlines/decisions/open_questions kèm evidence (click evidence → mở nguồn), "Tổng hợp lúc…", nút Mở nguồn, Làm mới nguồn này, Chuyển dự án, Bỏ theo dõi.
- **Empty:** chưa theo dõi gì → "Chưa theo dõi nguồn nào. Bấm 'Theo dõi' trên Slack thread / issue, hoặc nói với AI: *theo dõi thread <link>*." 
- **Loading:** lần đầu → `Skeleton`; sau đó giữ dữ liệu cũ, chỉ hiện chip tiến độ.
- **Lỗi nguồn:** chip "⚠ lỗi" trên dòng watch (tooltip = mã lỗi dịch i18n); lỗi toàn cục → `Card` lỗi + "Thử lại".

### 6.2 Điểm vào khác
- Sidebar: mục "Pulse" (icon `Activity`) + badge số dự án 🔴.
- Nút Theo dõi ở SlackThreadsWindow / Gantt / RunView (§3.2); trạng thái nút đồng bộ qua event `pulse:changed`.
- i18n `pulse.*` cho vi, en.

## 7. Validation & lỗi
| Điều kiện | Mã lỗi | Thông báo (vi) |
|---|---|---|
| Link không phải Slack thread / GitHub issue | `invalid_source` | "Chỉ hỗ trợ link Slack thread hoặc GitHub issue" |
| Không xác định được project | `project_required` | "Chọn dự án cho nguồn này" |
| Project không tồn tại / tên trùng nhiều project | `project_not_found` / `project_ambiguous` | "Không tìm thấy dự án" / "Có nhiều dự án trùng tên, hãy dùng id" |
| `#n` trần mà project có 0 hoặc > 1 repo GitHub | `repo_ambiguous` | "Ghi rõ owner/repo#số" |
| Nguồn đang theo dõi ở dự án khác | `watched_in_other_project` | "Nguồn này đang theo dõi ở dự án <tên>" |
| Repo không thuộc GitHub account của project (404/403) | `github_not_found` / `github_auth` | "Không truy cập được issue" |
| Slack token thiếu / sai workspace | dùng mã lỗi có sẵn của slack_threads | (giữ thông báo hiện có) |

## 8. API

### 8.1 Tauri commands (`src-tauri/src/commands/pulse.rs`)
| Command | Input | Output |
|---|---|---|
| `pulse_watch` | `{ url, projectId, via: 'button'\|'paste' }` | `Watch` |
| `pulse_unwatch` | `{ watchId }` | `()` |
| `pulse_move_watch` | `{ watchId, projectId }` | `Watch` |
| `pulse_is_watched` | `{ sourceType, sourceRef }` | `Watch \| null` |
| `pulse_overview` | `{ projectIds? }` | `ProjectPulse[]` (watch + digest mới nhất + health) — không gọi mạng |
| `pulse_refresh` | `{ projectId?, watchId? }` (trống = tất cả) | `{ queued }`; tiến độ qua event `pulse:refresh-progress { done, total }`, xong `pulse:changed` |

### 8.2 MCP tools (`sidecar-mcp`, server `devdy`, file mới `lib/pulse-tools.mjs`)
| Tool | Input | Output / hành vi |
|---|---|---|
| `pulse_watch` | `{ url?: string, urls?: string[], project?: string, move?: bool }` | Mỗi nguồn: `{ id, source_type, source_ref, project, already_watching, error? }`. Ghi `pulse_watches` + `pulse_requests(kind='resolve')`. |
| `pulse_unwatch` | `{ url? \| id? }` | `{ ok }` |
| `pulse_list` | `{ project?: string, all?: bool }` | Danh sách watch (mặc định project hiện tại; `all` = mọi dự án) kèm health + summary 1 dòng |
| `pulse_status` | `{ project?: string, all?: bool, refresh?: bool }` | Markdown tiến độ: sức khoẻ, mốc kế, việc cần mình làm, đang chờ khách, quyết định gần đây; `refresh=true` theo §3.4 |

Mô tả tool phải nói rõ: "Dùng khi người dùng bảo theo dõi / bỏ theo dõi một Slack thread hoặc GitHub issue, hoặc hỏi tiến độ dự án."

## 9. Trường hợp biên
- Watch Slack thread chưa capture → resolve sẽ capture (fetch + dịch) như thêm thread bình thường; capture lỗi → `resolve_status='error'`, nút "Thử lại".
- Slack thread bị xoá khỏi Devdy → `slack_thread_id` NULL; lần refresh sau capture lại từ `source_url`.
- Project bị xoá → CASCADE watch + digest.
- Issue chuyển repo (GitHub redirect) → cập nhật `source_ref` theo repo mới nếu không trùng watch khác.
- Nội dung không đổi giữa các lần refresh → không gọi AI (BR-05) — tiết kiệm chi phí.
- AI trích hạn không có năm → giả định năm gần nhất ≥ ngày của tin nhắn chứa nó (ghi trong prompt).
- Nhiều hạn mâu thuẫn trong cùng nguồn → AI giữ hạn **mới nhất theo thời gian tin nhắn**, các hạn cũ đưa vào `decisions` dạng "Đã dời từ X sang Y".
- Refresh khi đang refresh → bỏ qua yêu cầu trùng watch đang chạy.
- 2 session AI cùng watch một nguồn → UNIQUE đảm bảo 1 bản ghi; lần sau trả `already_watching`.

## 10. Phi chức năng
- `pulse_overview` < 150 ms với 30 dự án / 300 watch.
- Refresh: tối đa 3 nguồn đồng thời; chỉ gọi AI khi `content_hash` đổi; nội dung gửi AI ≤ 60.000 ký tự/nguồn.
- Chỉ đọc: không gọi endpoint ghi nào của Slack/GitHub.
- Nội dung Slack/issue chỉ gửi tới engine AI mà Devdy đã dùng cho dịch — không gửi thêm dịch vụ nào khác.
- Test: Rust — chuẩn hoá link (BR-02), luật bóng (BR-07), health (BR-08/09), dedupe hash; MCP — test `pulse-tools` như `test-slack-threads.mjs`; vitest cho format/sort phía FE.

## 11. Tiêu chí chấp nhận

**AC1 — Theo dõi qua chat**
- Giả sử đang trong session của dự án A
- Khi nói "theo dõi thread <link Slack> và issue org/app#12"
- Thì AI gọi `pulse_watch` 2 lần thành công; trong ≤ 1 phút màn Pulse hiện 2 nguồn dưới dự án A, mỗi nguồn có summary.

**AC2 — Theo dõi bằng nút**
- Khi bấm "Theo dõi" trên một thread trong SlackThreadsWindow (thread đã gắn dự án B)
- Thì nút đổi thành "Đang theo dõi", Pulse hiện thread dưới dự án B; bấm lại → bỏ theo dõi, thread biến mất khỏi Pulse.

**AC3 — Không trùng lặp**
- Giả sử issue org/app#12 đã theo dõi ở A
- Khi AI gọi lại `pulse_watch` với link issue đó cho A
- Thì trả `already_watching: true`, không có bản ghi mới; gọi cho dự án C → lỗi `watched_in_other_project`.

**AC4 — Bạn cần trả lời**
- Giả sử thread đang theo dõi có tin nhắn cuối của khách (không mention ai), cách đây 30 giờ
- Khi bấm Làm mới
- Thì dòng thread hiện "Bạn cần trả lời · 30h", màu 🔴, dự án 🔴.

**AC5 — Hạn chốt trên Slack**
- Giả sử thread có tin "Demo chốt thứ 6 ngày 30/10"
- Khi Làm mới
- Thì digest có deadline 30/10 kèm evidence trích đúng câu đó; dự án hiện "Mốc kế: Demo — 30/10".

**AC6 — Issue đóng**
- Giả sử issue đang theo dõi bị close
- Khi Làm mới
- Thì nguồn chuyển ✅ và vào nhóm "Đã xong", không làm dự án đỏ/vàng.

**AC7 — Không gọi AI khi không đổi**
- Giả sử nguồn không có thay đổi từ lần refresh trước
- Khi Làm mới
- Thì không có lời gọi AI cho nguồn đó, `checked_at` cập nhật, digest giữ nguyên.

**AC8 — Hỏi tiến độ**
- Khi hỏi AI "dự án A đang thế nào?"
- Thì AI gọi `pulse_status` và trả lời đúng sức khoẻ, mốc kế, việc cần làm theo dữ liệu Pulse.

**AC9 — Lỗi một nguồn**
- Giả sử một issue trả 404
- Khi Làm mới
- Thì nguồn đó hiện chip lỗi và màu 🟡, các nguồn khác vẫn được cập nhật.

**AC10 — Chỉ đọc**
- Khi chạy toàn bộ luồng (watch, refresh, unwatch) với log request mạng bật
- Thì không có request ghi nào tới Slack / GitHub.

## 12. Giả định
- G1: Ngưỡng: bóng ở mình trễ sau 24h (giờ thực), hạn sắp tới 72h, chờ khách lâu 5 ngày, poll yêu cầu MCP 3 giây, giữ 5 digest/watch.
- G2: Mở màn Pulse không tự refresh (chỉ nút Làm mới), trừ nguồn chưa có digest.
- G3: AI tổng hợp dùng engine đang dùng cho dịch Slack; chi phí chấp nhận được nhờ dedupe hash.
- G4: Không cho sửa tay kết quả AI ở bản này; sai thì Làm mới hoặc bỏ theo dõi.
- G5: Không có tab "Tuần này" ở bản này.

## 13. Câu hỏi mở
Không còn. Đã chốt: mở màn Pulse **không** tự refresh (chỉ nút Làm mới — G2); **không** cho sửa tay kết quả AI (G4).

## 14. Phụ thuộc
- Slack: `slack_threads/{link.rs,job.rs,api.rs,token.rs}`, `add_slack_thread`, `refresh_slack_thread`, SlackThreadsWindow.
- GitHub: `build_issue_markdown`, `list_milestone_board` (`commands/github.rs`), IssuesGanttView, RunView.
- AI: engine sidecar của `commands/translate.rs`.
- MCP: `sidecar-mcp/index.mjs`, `lib/store.mjs`, file mới `lib/pulse-tools.mjs`.

## 15. Chia task
| # | Task |
|---|---|
| T1 | Migration 0050 + chuẩn hoá link (BR-02) + commands watch/unwatch/move/is_watched + unit test |
| T2 | MCP `pulse_watch/unwatch/list/status` + `pulse_requests` + app xử lý request (BR-11) + test MCP |
| T3 | Refresh pipeline: fetch Slack/GitHub → hash → AI digest (BR-05/06) + luật cứng (BR-07) + `slack_identities` |
| T4 | Health (BR-08/09/10) + `pulse_overview` + màn `/pulse` + drawer chi tiết + sidebar |
| T5 | Nút Theo dõi ở SlackThreadsWindow / Gantt / RunView + i18n vi/en |
