# Playwright test report — Remote Control

- **Ngày chạy:** 2026-07-27
- **Controller:** `https://relay.example.com/controller.html`
- **Relay:** `relay.example.com`, service `devdy-relay` active
- **Host:** Devdy desktop đang chạy trên macOS
- **Run:** `5b38aa4d…`
- **Công cụ:** MCP Playwright, relay journal, Host SQLite audit
- **Nguồn test case:** `docs/remote-control-test-scenarios.md`

Không ghi controller link, master password, OTP, `link_secret` hoặc session
token vào báo cáo.

## 1. Kết luận

**Không đạt release gate P0.**

Happy path điều khiển từ browser hoạt động: deep-link, xác thực, history,
start/resume command, stream response, reload bằng session token, responsive
mobile và disconnect đều pass.

Phát hiện ba lỗi P0:

1. Hai controller có thể cùng authenticated và nhận history cho cùng session.
2. `Deny once` bị Host từ chối với `malformed`.
3. Command có thể mất khi session bị gián đoạn bởi controller/reconnect; UI từng
   hiển thị optimistic turn nhưng Host không có audit `start_run`, sau reconnect
   turn biến mất.

Ngoài ra, UI cho phép chọn `Bypass all`, xác nhận mâu thuẫn đã nêu giữa SRS và
code về permission mode.

## 2. Tổng hợp

| Trạng thái | Số case |
|---|---:|
| PASS | 20 |
| FAIL | 3 |
| BLOCKED BY PRODUCT DECISION | 1 |
| PARTIAL | 3 |
| NOT RUN | Các case còn lại cần fixture riêng, restart/chaos/flood/soak hoặc browser engine khác |

## 3. Case đã chạy

| ID | Kết quả | Evidence/Kết quả thực tế |
|---|---|---|
| HOST-01 | PASS | Host process đang chạy; relay active; Host đã đăng ký và tạo rendezvous |
| LINK-01 | PASS | Mở deep-link tự chuyển thẳng tới màn hình `Enter password or code` |
| LINK-02 | PASS | Paste link hợp lệ và Continue bắt đầu handshake |
| LINK-05 | PARTIAL | Đã test input rỗng và thiếu `link_secret`; lỗi đúng, chưa bỏ lần lượt cả 5 field |
| LINK-06 | PARTIAL | Fingerprint sai format bị chặn; chưa chạy handshake với fingerprint 64-hex hợp lệ nhưng sai giá trị |
| LINK-07 | PASS | Link có whitespace/newline đầu cuối vẫn parse và mở auth screen |
| AUTH-01 | PASS | Master password đúng → Connected, fingerprint đúng, history tải thành công |
| AUTH-03 | PASS | Một lần sai hiển thị lỗi, clear field; lần sau đúng kết nối thành công |
| SYNC-01 | PASS | Audit `request_history accepted`, `replayed=22`; transcript đúng |
| SYNC-02 | PASS | Marker prompt và response stream đúng thứ tự |
| SYNC-07 | PASS | Browser gửi marker; Host audit `start_run accepted`; browser nhận response |
| SYNC-09 | PASS | Usage Claude/Codex và context meter hiển thị |
| CMD-01 | PASS | Prompt marker tạo đúng một `start_run accepted` |
| CMD-05 | PARTIAL | Enter gửi thành công; chưa kiểm Shift+Enter multiline |
| META-04 | BLOCKED | Browser cho chọn `Ask via UI`, `Auto-accept edits`, `Plan only`, `Auto`, `Bypass all`; `set_run_meta accepted` |
| PERM-01 | PASS | Dialog có tool `Bash`, cwd và command preview |
| PERM-03 | **FAIL** | Click `Deny once` → toast `command rejected: malformed`; audit `respond_permission rejected malformed` |
| PERM-04 | PASS (UI) | Chỉ có `Allow once` và `Deny once`; không có remember action trên UI |
| NET-01 | PASS | Reload URL không hash tự Connected bằng token, không hỏi secret, marker không bị nhân đôi |
| NET-12 | **FAIL** | Sau xung đột controller, optimistic prompt biến mất; không có audit `start_run` tương ứng |
| SEC-04 | **FAIL** | Tab thứ hai dùng cùng token vẫn Connected và nhận đầy đủ history trong khi tab đầu còn Connected |
| SEC-09 | PASS | Literal `<img ... onerror=...>` render thành text; không tạo image, không execute handler |
| SEC-10 | PASS | Disconnect đóng connection, về Link Entry, xóa storage key |
| SEC-13 | PASS | Audit có accepted/rejected, run/action/result/reason và không chứa secret |
| UX-01 | PASS | 390×844 không horizontal overflow; header, composer và Run vẫn thao tác được |
| UX-02 | PASS | 768×1024 không horizontal overflow; composer visible |
| UX-07 | PASS | Error parse/auth không hiển thị full secret/token |
| General | PASS | Browser console: 0 error, 0 warning trong bài test |

## 4. Chi tiết lỗi

### BUG-RC-001 — Nhiều controller cùng active

- **Severity:** Critical / P0
- **Liên quan:** `SEC-04`, `BR-015`, `MAX_CONTROLLER=1`

**Steps**

1. Tab 1 authenticate và ở trạng thái Connected.
2. Mở tab 2 cùng origin, session token đang tồn tại.
3. Tab 2 tự reconnect.
4. Quan sát cả hai tab.

**Actual**

- Cả tab 1 và tab 2 đều hiển thị Connected.
- Cả hai nhận full history và có composer hoạt động.
- Relay tạo room mới cho controller thứ hai thay vì từ chối ở cấp bound session.

**Relay evidence**

```text
10:39:57 conn=190 join_ok room=rm_30f368… → room_active
10:41:08 conn=191 join_ok room=rm_16c3af… → room_active
```

Hai room trên cùng active trong khoảng khoảng 30 giây. Relay giữ
`MAX_CONTROLLER=1` theo từng room, nhưng Host liên tục tạo standing rendezvous
mới nên invariant một controller cho một bound session bị vượt qua.

**Ảnh hưởng**

- Hai browser có thể đồng thời xem và điều khiển một run.
- Token/session secret mới có thể thay thế token của controller cũ.
- Khi tab 2 đóng, tab 1 có thể bị reconnect/auth failure.

### BUG-RC-002 — Deny once bị malformed

- **Severity:** Critical / P0
- **Liên quan:** `PERM-03`, `FR-007`

**Steps**

1. Chọn permission mode `Ask via UI`.
2. Gửi prompt làm phát sinh Bash permission.
3. Dialog hiển thị đúng tool/cwd/command.
4. Click `Deny once`.

**Actual**

```text
command rejected: malformed
```

Host audit:

```text
10:47:07 respond_permission rejected malformed
```

**Expected**

- Host nhận `request_id`, `run_id`, `decision=deny_once`.
- Tool không chạy.
- Permission được resolve và audit accepted.

**Khả năng nguyên nhân cần kiểm tra**

- Controller store không giữ đúng `request_id` sau replay/reconnect.
- Mapping payload permission giữa stream parser và `respondPermissionCmd`.
- Permission event bị resolve/replace trước khi click nhưng UI còn stale.

### BUG-RC-003 — Mất command khi reconnect/xung đột controller

- **Severity:** Critical / P0
- **Liên quan:** `CMD-12`, `NET-12`

**Steps**

1. Tab 1 Connected.
2. Tab 2 cùng token Connected rồi đóng.
3. Tab 1 gửi prompt trong lúc connection chuyển trạng thái.
4. Prompt xuất hiện optimistic với trạng thái `thinking`.
5. Tab 1 vào `Joining…`, sau đó Connected lại.

**Actual**

- Prompt biến mất khỏi transcript sau replay.
- Host audit không có `start_run` cho prompt đó.
- Audit có `authenticate rejected bad_otp` trong thời điểm reconnect:

```text
10:42:54 authenticate rejected bad_otp
10:43:27 authenticate accepted
```

**Expected**

- Composer không cho gửi khi command channel chưa ACTIVE; hoặc
- Command được giữ/retry có idempotency key; hoặc
- UI báo send failed và giữ draft, không hiển thị turn như đã accepted.

## 5. Phát hiện bổ sung

### Permission mode chưa thống nhất

Controller hiện expose:

```text
Default (from settings)
Ask via UI (default)
Auto-accept edits
Plan only (read-only)
Auto (classifier)
Bypass all
```

Host audit xác nhận `set_run_meta accepted`. Điều này trái với SRS hiện tại nói
remote không được đổi permission mode và đặc biệt không được nâng lên bypass.

Đây là quyết định sản phẩm/security phải chốt trước khi đánh pass/fail cuối cùng:

- Nếu theo SRS: đây là lỗi P0.
- Nếu cho phép parity desktop: cập nhật SRS, threat model và acceptance criteria.

### Permission có dấu hiệu hiển thị output trước khi resolve

Trong lần test `Ask via UI`, transcript đã hiển thị command `pwd` và output path
trong lúc dialog Permission vẫn đang mở. Cần test bằng fixture có side effect
quan sát được để xác định tool đã thực thi trước permission hay đây chỉ là lỗi
group/render event. Chưa kết luận vì bài test không tạo side effect.

### Internal Codex cache error xuất hiện trong transcript

Controller hiển thị:

```text
codex_models_manager::cache: failed to load models cache:
missing field `supports_reasoning_summaries`
```

Không phải lỗi transport Remote Control, nhưng làm bẩn transcript và có thể ảnh
hưởng model selector.

## 6. Case chưa chạy và lý do

- `AUTH-04`, `AUTH-05`: có thể burn link/OTP; cần disposable session fixture.
- `HOST-04`, `HOST-07`, `HOST-08`, `NET-05`, `NET-06`: cần disable/restart/end
  Host hoặc relay; chưa tác động môi trường đang dùng.
- `NET-02`..`NET-04`, `NET-09`..`NET-11`: cần network proxy/chaos harness.
- `SEC-01`..`SEC-03`, `SEC-05`..`SEC-08`, `SEC-11`, `SEC-12`: cần protocol
  injection harness hoặc flood test, không thể tạo chỉ bằng UI Playwright.
- Attachment/slash/mention/AskUserQuestion: cần fixture data phù hợp.
- Cross-browser Safari/Firefox: MCP session hiện tại chạy Chromium.
- `PERF-01`..`PERF-05`: cần load/soak environment; `PERF-03` yêu cầu 8 giờ.

## 7. Evidence

- `remote-control-test-run-20260727.png`
- `remote-control-mobile-390x844.png`
- Relay journal từ `10:35` đến `10:49` UTC.
- Host DB: `remote_audit`, truy vấn theo cùng time window.

## 8. Đề xuất thứ tự sửa và retest

1. Sửa bound-session exclusivity để controller mới bị từ chối hoặc có cơ chế
   takeover rõ ràng, atomic và revoke controller cũ.
2. Sửa payload/state của `Deny once`; thêm Playwright fixture bắt buộc cho cả
   Allow/Deny và double-response.
3. Thêm command delivery state: pending/ack/idempotency và giữ draft khi send
   chưa được Host accept.
4. Chốt permission-mode policy; không release khi `Bypass all` còn mơ hồ.
5. Sau khi sửa, chạy lại toàn bộ P0 smoke rồi mới mở rộng chaos/security/soak.
