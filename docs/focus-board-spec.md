---
status: draft
version: v1
date: 2026-10-30
---

# Spec: Focus — bảng todo toàn cục, nhắc việc và AI phân tích

> **Quyết định đã chốt với người dùng**
> - **Todo là trung tâm**: người dùng tự quyết định đâu là việc (tạo todo khi đọc Slack / issue / PR / session); Devdy lo **sắp xếp, nhắc, phân tích**.
> - Deadline có **ngày** (bắt buộc nếu có hạn) và **giờ tuỳ chọn**.
> - **Có** dùng Google Calendar (giờ bận) để tính tải công việc trong ngày.
> - **Bỏ hẳn Project Pulse** (màn hình, MCP tools, bảng dữ liệu). Hạ tầng còn dùng được (chuẩn hoá link nguồn, fetch GitHub issue, gom nội dung Slack thread, gọi AI một lượt) được chuyển sang module mới.
> - AI **chỉ chạy khi người dùng bấm** — không tự estimate khi tạo, không tự phân tích nền.
> - Thứ tự ưu tiên tính bằng **công thức xác định** (không phải AI) để tức thì, ổn định, giải thích được.

## 1. Tổng quan

- **Mục tiêu:** một màn hình nhìn vào là biết nên làm gì trước, gom todo của mọi dự án; nhắc đúng lúc cần bắt đầu; AI hỗ trợ phân tích cách làm và estimate khi được yêu cầu.
- **Actor:** 1 người dùng (chủ máy Devdy), AI trong session Devdy (qua MCP `devdy`).
- **Trong phạm vi:**
  - Mở rộng bảng `todos` (giờ hạn, ưu tiên, estimate, trạng thái, nguồn, hoãn, nhắc, AI plan, thời gian thực tế).
  - Màn **Focus** (`/focus`, cửa sổ riêng) thay cho Pulse trong nav / tray.
  - Tạo todo từ nguồn: Slack thread, GitHub/GitLab issue (Gantt), PR (PR Inbox), RunView.
  - Bộ lập lịch nhắc việc (Rust) + thông báo macOS bấm được.
  - Tải công việc theo Google Calendar.
  - AI theo yêu cầu: gợi ý khi tạo, phân tích todo, lên kế hoạch hôm nay, "Bắt đầu làm" (mở session).
  - Hiệu chỉnh estimate theo thời gian thực tế.
  - Gỡ Pulse.
- **Ngoài phạm vi:** ghi ngược ra Slack/GitHub; tự quét nguồn sinh todo; todo lặp lại (recurring); subtask lồng nhau; chia sẻ todo cho người khác.

## 2. Thuật ngữ

- **Hạn (due):** `due_date` (`YYYY-MM-DD`, giờ địa phương) + `due_time` tuỳ chọn (`HH:MM`). Không có giờ → coi hạn là **cuối giờ làm việc** của ngày đó (BR-03).
- **Estimate hiệu dụng:** `estimate_min × hệ số hiệu chỉnh` (BR-10). Chưa có estimate → mặc định 60 phút và gắn cờ "chưa estimate".
- **Giờ làm việc:** khung giờ + ngày làm việc trong Settings (mặc định 09:00–18:00, T2–T6).
- **Thời gian dư (slack):** số phút **làm việc** còn lại tới hạn trừ estimate hiệu dụng. Âm = không kịp nếu chưa bắt đầu.
- **Giờ phải bắt đầu (start_by):** thời điểm muộn nhất nên bắt đầu để kịp hạn = hạn − estimate hiệu dụng − 15 phút đệm, tính lùi trên giờ làm việc.

## 3. Luồng nghiệp vụ

### 3.1 Tạo todo
1. Từ nút **"+ Todo"** tại: header Slack thread (`SlackThreadHeader`), menu dòng issue trên Gantt (`GanttChart`), header RunView (run từ issue), thẻ PR Inbox (`PrInboxCard`), hoặc quick-capture toàn cục (`QuickCaptureForm`).
2. Form điền sẵn: project (từ ngữ cảnh), nguồn (`source_type`, `source_url`, `source_ref`), tiêu đề gợi ý (tiêu đề thread/issue/PR).
3. Người dùng nhập/sửa: tiêu đề, project, hạn (ngày + giờ tuỳ chọn), ưu tiên (P1/P2/P3, mặc định P2), estimate (tuỳ chọn; ô nhập kiểu `30m`, `2h`, `1h30`).
4. Nút **"✨ Gợi ý"** (chỉ khi có nguồn): gọi AI (§3.6a) trả tiêu đề, hạn trích từ nội dung, estimate, ưu tiên — **điền vào form**, người dùng vẫn phải bấm Lưu.
5. Lưu → `todos` có `status='todo'`. Phát `item:changed`.
6. Qua MCP: `todos_add` nhận thêm `due_time`, `priority`, `estimate_min`, `source_url` (§6).

### 3.2 Màn Focus

```
┌ Focus ─────────────────────── [Lên kế hoạch hôm nay ✨] [+ Todo] ┐
│ Hôm nay: 6h30 việc · 4h trống (Calendar) · ⚠ quá tải 2h30        │
├──────────────────────────────────────────────────────────────────┤
│ ▶ LÀM NGAY                                                       │
│  1 [Yahagi]   Fix lỗi export CSV    🔴 quá hạn 1 ngày  ~2h  #123↗ │
│  2 [Lunchbox] Trả lời KH về API     ⏰ 15:00 hôm nay   ~30m Slack↗│
│  3 [Devdy]    Review PR sync        bắt đầu trước 14:00 ~1h PR↗   │
├──────────────────────────────────────────────────────────────────┤
│ ▾ Đang làm (1)                                                   │
│ ▾ Quá hạn (2)   ▾ Hôm nay (4)   ▸ Ngày mai (2)   ▸ Tuần này (5)  │
│ ▸ Sau (3)   ▸ Không hạn (9)   ▸ Đang chờ người khác (3)          │
│ ▸ Đã hoãn (2)   ▸ Đã xong gần đây                                │
└──────────────────────────────────────────────────────────────────┘
 Xem: [Theo thời gian] [Theo dự án] [Tuần]   Lọc: dự án ▾ nguồn ▾
```

- **Làm ngay:** tối đa 3 todo điểm cao nhất (BR-05), không tính `waiting` / đang hoãn.
- **Nhóm theo thời gian** (BR-04), trong mỗi nhóm sắp theo điểm. Nhóm "Đang làm", "Quá hạn", "Hôm nay" mở sẵn.
- **Mỗi dòng:** chip màu dự án, tiêu đề, trạng thái hạn tương đối (quá hạn X / giờ hạn / "bắt đầu trước HH:mm" khi start_by trong hôm nay), estimate (`~2h`, cờ "?" nếu chưa estimate), ưu tiên (chỉ hiện P1), icon nguồn bấm mở link, số session liên kết.
- **Tooltip "Vì sao ở đây":** chuỗi giải thích từ BR-05 (vd. "Hạn 15:00 hôm nay · cần ~30m · còn 2h làm việc · P2").
- **Thao tác dòng:** ✓ Xong · ▶ Bắt đầu (status `doing`) · ⏸ Hoãn (1h / chiều nay 14:00 / mai 9:00 / tuần sau / chọn) · ⏳ Chờ người khác · ✨ Phân tích · 🚀 Bắt đầu với AI · sửa · xoá. Bấm dòng → mở drawer chi tiết (mọi trường + AI plan + session liên kết).
- **Xem theo dự án:** mỗi dự án một cột, todo sắp theo điểm; đầu cột hiện tổng estimate và số quá hạn.
- **Xem tuần:** 7 cột ngày (T2→CN), todo xếp vào ngày hạn; ngày quá tải (tổng estimate > giờ trống) tô cảnh báo.
- **Thanh tải công việc** (BR-08) ở đầu màn; Calendar chưa kết nối → "Kết nối Google Calendar để xem giờ trống".
- Màn tự tính lại mỗi 60 s (giờ tương đối) và khi nhận `item:changed` / `focus:changed`.

### 3.3 Trạng thái todo

```
todo ──▶ doing ──▶ done
  │  ▲     │
  ▼  │     ▼
 waiting  (todo)        hoãn (snoozed_until) là thuộc tính, áp lên todo/waiting
```
- `▶ Bắt đầu` → `doing`, set `started_at` nếu chưa có.
- `✓ Xong` → `done=1`, `completed_at=now`. Nếu có `started_at` và estimate → hiện ô nhỏ "Thực tế mất ~Xh?" (điền sẵn đề xuất BR-11, bỏ qua được) → lưu `actual_min`.
- Bỏ đánh dấu xong → quay về `todo`, xoá `completed_at`.
- `done` vẫn là cột cũ (giữ tương thích MCP / sync / ItemListWindow); `status` chỉ mang `todo|doing|waiting`.

### 3.4 Nhắc việc

Bộ lập lịch Rust (`focus::scheduler`) tick mỗi 60 s khi app chạy (kể cả không mở Focus), dùng lại `show_clickable` của `commands/notifications.rs`. Bấm thông báo → mở cửa sổ Focus và highlight todo (hoặc mở Focus ở trạng thái bình thường với tóm tắt sáng).

| Loại | Khi nào | Nội dung |
|---|---|---|
| `morning` | Giờ tóm tắt sáng (mặc định 09:00) ngày làm việc, nếu có todo mở | "Hôm nay N việc · M quá hạn · bắt đầu với: <top 1>" |
| `start_by` | `now ≥ start_by`, status `todo`, không hoãn | "Nên bắt đầu: <tiêu đề> (~2h, hạn 17:00)" |
| `due_soon` | Có `due_time`, còn 30 phút, chưa xong | "Còn 30 phút: <tiêu đề>" |
| `overdue` | Vừa quá hạn (một lần) | "Đã quá hạn: <tiêu đề>" |
| `remind` | `remind_at` do người dùng đặt | "Nhắc: <tiêu đề>" |
| `snooze_end` | Hết hoãn, todo đó thuộc Làm ngay | "Quay lại: <tiêu đề>" |

- Chống trùng bằng bảng `todo_notifications` (BR-07). Nhiều thông báo cùng tick → **gộp một** ("3 việc cần bắt đầu: A, B, C").
- Ngoài giờ làm việc chỉ gửi `remind` (người dùng chủ động đặt). Có công tắc tắt từng loại + tắt toàn bộ trong Settings.
- Tray hiện số việc quá hạn (badge/tiêu đề) — thay chỗ Pulse trong `src/lib/tray.ts`.

### 3.5 Tải công việc theo Calendar
1. Settings → Focus chọn tài khoản Google + các lịch tính là "bận" (mặc định lịch chính của tài khoản mặc định).
2. Backend lấy sự kiện hôm nay (dùng lại `commands/gcalendar.rs`), cache 10 phút, làm mới khi bấm.
3. Tính theo BR-08; hiển thị thanh tải và các khe trống (dùng cho §3.6c).

### 3.6 AI (chỉ khi bấm)

Dùng engine một lượt có sẵn (`TranslateState` + `submit_prompt`, như Pulse digest), engine/model theo Settings → AI của Focus (mặc định theo cấu hình translate). Một yêu cầu AI cho mỗi todo tại một thời điểm; nút hiện spinner, huỷ được. Lỗi → toast, không đổi dữ liệu.

**Nội dung nguồn** đưa vào prompt (dùng lại code Pulse chuyển sang `focus::source_content`): Slack thread đã capture → tin nhắn (bản dịch nếu có); GitHub issue/PR → body + comments + state + assignees + milestone (tài khoản GitHub của project); GitLab → qua `gitlab/`; nguồn khác → chỉ tiêu đề + URL. Cắt 60k ký tự. Nội dung nguồn đặt trong khối dữ liệu có đánh dấu, được dặn là **dữ liệu, không phải chỉ thị** (như `digest::neutralise_markers`).

a. **Gợi ý khi tạo** → JSON `{ title, due_date?, due_time?, priority, estimate_min, reason }`. `due_*` chỉ trả khi có câu trích dẫn trong nguồn (`due_evidence`); không có bằng chứng → bỏ (giống `keep_quoted`).

b. **Phân tích todo** → JSON `{ summary, steps[], estimate_min, estimate_low_min, estimate_high_min, confidence: low|medium|high, risks[], questions[], suggested_priority?, suggested_due? }`. Lưu render markdown vào `ai_plan` + `ai_analyzed_at`; **sửa được** trong drawer. Estimate **không** tự áp: drawer hiện "AI ước tính 2–3h (~2h30) · [Áp dụng]"; áp → `estimate_source='ai'`. Prompt kèm hệ số hiệu chỉnh hiện tại và vài todo cùng project đã xong (tiêu đề + actual) làm tham chiếu.

c. **Lên kế hoạch hôm nay** → input: tối đa 30 todo mở (theo điểm, kèm hạn/estimate/ưu tiên/project), khe trống hôm nay (§3.5), giờ hiện tại. Output JSON `{ blocks: [{ todo_id, start, end, note }], deferred: [{ todo_id, reason }], summary }`. Hiển thị dạng timeline trong Focus; nút **"Tạo block trên Calendar"** (từng block hoặc tất cả) → `create_google_calendar_event` với tiêu đề `[Devdy] <todo>`. `todo_id` không tồn tại / chồng giờ bận → bỏ block đó. Lưu vào `focus_day_plans` (một bản / ngày, ghi đè khi tạo lại).

d. **🚀 Bắt đầu với AI** → mở session mới (`start_run`) trong project của todo, prompt điền sẵn: tiêu đề, link nguồn, ai_plan (nếu có), dặn dùng `mcp__devdy__todos_*` để cập nhật. Link session qua `todo_runs` (source `created_from_item`), status → `doing`. Todo không có project → chọn project trước.

### 3.7 Gỡ Pulse
- Xoá: `src-tauri/src/pulse/` (sau khi chuyển phần dùng lại), `commands/pulse.rs`, route/màn/cửa sổ/store/composable/component/i18n Pulse, MCP `pulse_*` + `sidecar-mcp/lib/pulse-*`, mục Pulse trong tray, nav, background processes, settings backup, sync schema.
- Migration xoá bảng `pulse_watches`, `pulse_digests`, `pulse_requests`.
- Nút "Theo dõi" ở Slack/Gantt/RunView được thay bằng "+ Todo".
- `docs/project-pulse-spec.md` → `status: superseded` (trỏ sang spec này).

## 4. Quy tắc nghiệp vụ (BR)

- **BR-01 Validate:** `text` không rỗng (≤ 500 ký tự hiển thị tiêu đề; phần dư vẫn lưu). `due_time` chỉ có khi có `due_date`; định dạng `HH:MM` 24h. `priority ∈ {1,2,3}`. `estimate_min ∈ [5, 2400]`. `status ∈ {todo, doing, waiting}`.
- **BR-02 Nguồn:** chuẩn hoá link như Pulse (`source::normalize`): Slack thread → `slack:<team>/<channel>/<thread_ts>`; GitHub issue/PR → `github:owner/repo#n`; GitLab → `gitlab:<path>#n` / `!n`; Devdy run → `run:<id>`; còn lại `url`. Một nguồn có thể có nhiều todo (không unique).
- **BR-03 Hạn hiệu dụng:** `due_date + due_time`, hoặc `due_date + giờ kết thúc làm việc` nếu không có giờ. Hạn rơi vào ngày nghỉ vẫn giữ nguyên.
- **BR-04 Nhóm thời gian** (theo hạn hiệu dụng, giờ địa phương, ưu tiên trên xuống):
  1. `done` → Đã xong (7 ngày gần nhất).
  2. `snoozed_until > now` → Đã hoãn.
  3. `waiting` → Đang chờ người khác.
  4. `doing` → Đang làm.
  5. hạn < now → Quá hạn; cùng ngày hôm nay → Hôm nay; ngày mai → Ngày mai; ≤ CN tuần này → Tuần này; xa hơn → Sau; không hạn → Không hạn.
- **BR-05 Điểm & giải thích** (số càng nhỏ càng ưu tiên, đơn vị phút):
  - `slack = phút_làm_việc(now → hạn) − estimate_hiệu_dụng`; quá hạn: `slack = −(phút_làm_việc(hạn → now)) − estimate_hiệu_dụng`.
  - Không hạn: `slack = 5 ngày làm việc`.
  - `score = slack + priority_adj − aging`, `priority_adj`: P1 −480, P2 0, P3 +480; `aging = min(phút từ khi tạo / 4, 480)` (cũ 4 ngày ≈ thêm 1 ngày làm việc).
  - `doing` luôn đứng đầu Làm ngay (sau đó theo score). Hoà → hạn sớm hơn → `position`.
  - Hàm thuần, trả kèm danh sách lý do để hiển thị tooltip.
- **BR-06 Start_by:** lùi `estimate_hiệu_dụng + 15` phút làm việc từ hạn hiệu dụng. Không hạn → không có start_by.
- **BR-07 Chống trùng thông báo:** khoá `(todo_id, kind, fire_key)`, `fire_key` = giá trị khiến thông báo phát sinh (hạn hiệu dụng / `remind_at` / `snoozed_until` / ngày cho `morning`). Đổi hạn → `fire_key` mới → có thể nhắc lại. Bảng chỉ local (không sync) — mỗi máy tự nhắc; máy đang ngủ bỏ qua thông báo đã lỡ quá 2 giờ (trừ `morning` trong ngày).
- **BR-08 Tải công việc hôm nay:** `cần = Σ estimate_hiệu_dụng(todo ∈ Đang làm ∪ Quá hạn ∪ Hôm nay)` (todo `doing` có `started_at` hôm nay vẫn tính đủ — đơn giản, không trừ phần đã làm). `trống = giờ làm việc còn lại hôm nay − sự kiện bận` (bỏ sự kiện cả ngày, `transparency=transparent`, đã từ chối; hợp khoảng chồng nhau). `cần > trống` → quá tải.
- **BR-09 AI không tự ghi đè:** kết quả AI chỉ ghi `ai_plan`, `ai_analyzed_at`, `focus_day_plans`; các trường hạn/estimate/ưu tiên chỉ đổi khi người dùng bấm Áp dụng.
- **BR-10 Hệ số hiệu chỉnh:** trung vị `actual_min / estimate_min` của 20 todo xong gần nhất có cả hai giá trị; cần ≥ 5 mẫu, nếu không = 1.0; kẹp [0.5, 3.0]. Hiển thị trong Focus ("Bạn thường làm lâu hơn ước tính 1.4×"), tắt được trong Settings.
- **BR-11 Đề xuất thời gian thực tế:** nếu có session liên kết → tổng thời lượng các run liên kết; nếu không → `completed_at − started_at` tính trên giờ làm việc; > 3× estimate → không đề xuất (để trống).

## 5. Dữ liệu

### 5.1 Migration `0067_todo_focus.sql`

```sql
ALTER TABLE todos ADD COLUMN due_time TEXT;              -- HH:MM, NULL = cả ngày
ALTER TABLE todos ADD COLUMN priority INTEGER NOT NULL DEFAULT 2;
ALTER TABLE todos ADD COLUMN estimate_min INTEGER;
ALTER TABLE todos ADD COLUMN estimate_source TEXT;       -- user | ai
ALTER TABLE todos ADD COLUMN status TEXT NOT NULL DEFAULT 'todo';  -- todo|doing|waiting
ALTER TABLE todos ADD COLUMN snoozed_until TEXT;         -- RFC3339
ALTER TABLE todos ADD COLUMN remind_at TEXT;             -- RFC3339
ALTER TABLE todos ADD COLUMN source_type TEXT;           -- slack|github|gitlab|run|url
ALTER TABLE todos ADD COLUMN source_ref TEXT;
ALTER TABLE todos ADD COLUMN source_url TEXT;
ALTER TABLE todos ADD COLUMN started_at TEXT;
ALTER TABLE todos ADD COLUMN completed_at TEXT;
ALTER TABLE todos ADD COLUMN actual_min INTEGER;
ALTER TABLE todos ADD COLUMN ai_plan TEXT;
ALTER TABLE todos ADD COLUMN ai_analyzed_at TEXT;
ALTER TABLE todos ADD COLUMN updated_at TEXT;

CREATE INDEX IF NOT EXISTS idx_todos_due ON todos(done, due_date);

-- Local only (excluded from sync): which reminders already fired (BR-07).
CREATE TABLE IF NOT EXISTS todo_notifications (
    todo_id  TEXT NOT NULL,
    kind     TEXT NOT NULL,
    fire_key TEXT NOT NULL,
    fired_at TEXT NOT NULL,
    PRIMARY KEY (todo_id, kind, fire_key)
);

CREATE TABLE IF NOT EXISTS focus_day_plans (
    date       TEXT PRIMARY KEY,   -- YYYY-MM-DD
    plan_json  TEXT NOT NULL,
    created_at TEXT NOT NULL
);

DROP TABLE IF EXISTS pulse_requests;
DROP TABLE IF EXISTS pulse_digests;
DROP TABLE IF EXISTS pulse_watches;
```

- `todos` đã nằm trong sync schema → cột mới tự sync. `todo_notifications` thêm vào `EXCLUDED_TABLES`; `focus_day_plans` sync (TableSpec theo `date`). Gỡ `pulse_*` khỏi `TABLES` / `EXCLUDED_TABLES`.
- Cần kiểm tra trước khi merge: máy chạy bản cũ gửi bản ghi `pulse_*` / máy mới gửi cột lạ cho máy cũ → sync apply phải bỏ qua bảng/cột không biết (xem `sync/apply.rs`), không được lỗi.
- Thời điểm lưu RFC3339; `done`/`completed_at` giữ nhất quán: set cùng nhau trong cùng câu lệnh.

### 5.2 Settings (bảng settings hiện có, key `focus.*`)
`work_start` (09:00), `work_end` (18:00), `work_days` (1–5), `morning_time` (09:00), `notify_enabled` + công tắc từng loại, `calendar_account_id`, `calendar_ids[]`, `calibration_enabled`, `ai_engine` / `ai_model` (rỗng = theo translate).

## 6. Giao diện lập trình

### 6.1 Tauri commands (`commands/todos.rs` mở rộng + `commands/focus.rs` mới)
- `list_todos` trả thêm mọi cột mới (giữ tương thích ItemListWindow).
- `add_todo(text, project_id?, run_id?, due_date?, due_time?, priority?, estimate_min?, source_url?)` — nguồn chuẩn hoá theo BR-02.
- `update_todo_fields(id, patch)` — patch một phần: text, project, due, priority, estimate (+ `estimate_source`), status, snoozed_until, remind_at, ai_plan, actual_min. Giữ các command cũ (`set_todo_due_date`, `toggle_todo`, …) cho caller hiện có; `toggle_todo` cập nhật `completed_at`.
- `focus_board()` → `{ now, items: [{ todo, bucket, score, reasons[], start_by?, effective_due?, effective_estimate_min, estimate_missing }], top: [id], capacity?: { need_min, free_min, busy: [...] }, calibration: { factor, samples } }` — **toàn bộ tính toán trong Rust** (`focus::score`, hàm thuần, có test), frontend chỉ render.
- `focus_settings_get / focus_settings_set`.
- `focus_ai_suggest(source_url, project_id?)`, `focus_ai_analyze(todo_id)`, `focus_ai_plan_today()`, `focus_ai_cancel(request_id)`.
- `focus_plan_to_calendar(date, block_ids?)`.
- `focus_start_with_ai(todo_id)` → `run_id` (gọi `start_run_inner`).
- Sự kiện: `focus:changed` (sau mỗi ghi), `focus:open-todo` (bấm thông báo). (Không có `focus:ai-progress`: tiến trình AI theo lời gọi command; huỷ → lỗi `cancelled`.)

### 6.2 MCP `devdy` (sidecar-mcp)
- `todos_add`: thêm `due_time`, `priority` (1–3 hoặc `"P1"`), `estimate` (chuỗi `"2h"` / phút), `source_url`.
- `todos_set_due`: thêm `due_time`.
- `todos_update`: thêm `priority`, `estimate`, `status`, `snooze_until`.
- `todos_list`: trả thêm các cột mới; tham số `sort: "due" | "position"` (mặc định `due` cho todo mở). Không tính score trong sidecar (logic nằm ở Rust); mô tả tool nói rõ "mở Focus để xem thứ tự đề xuất".
- Xoá `pulse_watch`, `pulse_unwatch`, `pulse_list`, `pulse_status`.

## 7. Phân pha triển khai

| Pha | Nội dung | Kết quả dùng được |
|---|---|---|
| **P1 — Lõi** | Migration; Rust `focus::score` + `focus_board`; commands; màn Focus (3 kiểu xem, drawer, thao tác); form "+ Todo" ở các nguồn; MCP mở rộng; **gỡ Pulse** | Bảng todo toàn cục xếp hạng được |
| **P2 — Nhắc & lịch** | `focus::scheduler` + thông báo + tray; Settings Focus; tải công việc theo Calendar | Được nhắc đúng lúc, biết hôm nay có quá tải không |
| **P3 — AI** | Gợi ý khi tạo; phân tích; kế hoạch hôm nay + tạo block Calendar; Bắt đầu với AI; hiệu chỉnh estimate | AI hỗ trợ khi bấm |

## 8. Kiểm thử

- Rust unit: `focus::score` (nhóm, slack qua đêm/cuối tuần, quá hạn, không hạn, ưu tiên, aging, doing), `start_by`, BR-07 chống trùng + gộp, BR-08 hợp khoảng bận, BR-10/11, parse JSON AI (thiếu trường, bằng chứng hạn không có trong nguồn, `todo_id` lạ).
- Migration: DB có dữ liệu Pulse cũ → migrate sạch; todo cũ có `status='todo'`, `priority=2`.
- Sync: `schema.rs` test bao phủ bảng; bản ghi `pulse_*` từ peer cũ bị bỏ qua.
- `sidecar-mcp`: cập nhật `test-*.mjs` cho tham số todo mới; xoá `test-pulse.mjs`.
- Frontend: định dạng estimate (`30m`/`2h`/`1h30` ⇄ phút) trong `src/lib/focus.ts` + `__tests__/focus.test.mjs`.

## 9. Câu hỏi mở / rủi ro

- `ItemListWindow` (tab Todo) vẫn giữ làm danh sách nhanh; có thể gộp vào Focus ở bản sau.
- Thông báo khi app đóng: không có (app phải chạy). Có thể cân nhắc launch-at-login sau.
- Nhiều máy sync: mỗi máy tự nhắc → có thể nhận trùng trên 2 máy (chấp nhận ở v1).

## 10. Ghi chú triển khai P1 (backend)

- **Diễn giải công thức** (`focus::score`):
  - `aging = min(phút_làm_việc(created_at → now) / 4, 480)` — dùng phút **làm việc** để khớp chú thích "cũ 4 ngày ≈ thêm 1 ngày làm việc".
  - Không hạn: `slack = 5 × độ dài ngày làm việc` (không trừ estimate, đúng như BR-05).
  - Todo `done` có `score = 0`, `reasons = [{kind: "done"}]`, không có `start_by`. Todo done không có `completed_at` (dữ liệu cũ) không hiện trên bảng.
- **Lý do (`reasons`)** là dữ liệu có cấu trúc `{ kind, value }` để UI tự dịch: `doing`, `waiting`, `snoozed` (value `YYYY-MM-DDTHH:MM` giờ địa phương), `overdue` (phút đồng hồ đã quá hạn), `due` (`YYYY-MM-DDTHH:MM` giờ địa phương), `work_left` (phút làm việc còn lại), `late_by` (phút thiếu nếu bắt đầu ngay), `no_due`, `estimate` / `estimate_missing` (phút hiệu dụng), `priority` (chỉ khi ≠ 2), `age_days` (khi ≥ 2 ngày), `done`.
- **Nguồn (BR-02)**: Slack team = phần host trước `.slack.com`; link DM Slack được lưu như `url`; PR GitHub dùng chung dạng `github:o/r#n` với issue. Vector dùng chung Rust/MCP: `src-tauri/src/focus/source_vectors.json`.
- **Sync**: bản ghi `pulse_*` từ máy bản cũ bị bỏ qua (không đưa vào `sync_deferred`); cột lạ bị bỏ qua sẵn trong `codec::upsert_row`.
- **Settings**: các key `focus.*` (trừ `focus.calendar_*`) nằm trong `PORTABLE_SETTING_KEYS` (backup + sync). Backup cài đặt mang theo các cột Focus của todo.

## 11. Ghi chú triển khai P2

- **Nhắc việc** (`focus::scheduler`): tick đầu sau 10 s, rồi mỗi 60 s. `morning` chỉ gửi trong giờ làm việc của ngày làm việc, từ `morning_time` (bỏ lỡ vẫn gửi muộn trong ngày). `due_soon` / `overdue` / `start_by` không gửi khi todo đang hoãn; `start_by` chỉ với status `todo`. `fire_key` của các loại theo hạn = hạn hiệu dụng `YYYY-MM-DDTHH:MM` (local). Ghi `todo_notifications` **trước** khi hiện thông báo; xoá bản ghi > 30 ngày. Nội dung thông báo theo ngôn ngữ app (`settings.language`).
- **Bấm thông báo**: không tự nâng cửa sổ chính. Cửa sổ `focus` đang mở → nâng lên + phát `focus:open-todo { todo_id }` cho mọi cửa sổ. Chưa mở → phát `focus:open-window` cho cửa sổ chính (frontend tạo cửa sổ bằng `openFocusWindow()`, cùng URL / tuỳ chọn), cửa sổ mới lấy todo bằng `focus_take_open_todo` khi mount (id chờ chỉ lấy được một lần).
- **Tray**: dòng `tray:focus` ("Focus — N việc quá hạn") ở phần do cửa sổ chính sở hữu; tiêu đề menu bar = số phiên đang chạy + `⏰N` quá hạn. Số quá hạn lấy từ board của cửa sổ chính (không có sự kiện / command riêng).
- **Calendar** (`focus::calendar`): tài khoản = `calendar_account_id` hoặc tài khoản Google mặc định; lịch = `calendar_ids` hoặc `primary`. Cache 10 phút theo (tài khoản, lịch, ngày), kể cả lỗi; `focus_refresh_calendar` xoá cache. `focus_board` chờ Google tối đa 3 s, quá thì dùng kết quả cũ cùng ngày (nếu có) và phát `focus:changed` khi tải xong. `capacity_error`: `no_account`, `loading`, hoặc thông điệp lỗi (không chứa token). Lỗi một lịch bất kỳ → cả capacity lỗi.
