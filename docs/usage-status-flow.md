# Usage Status & Plan Limit Flow

Tài liệu này tóm tắt flow xử lý trạng thái usage/plan limit của Devdy, đặc biệt cho Claude subscription usage.

## Mục tiêu

- Luôn hiển thị trạng thái usage ở sidebar, không chỉ khi gần/vượt limit.
- Ưu tiên số liệu plan thật từ Claude `/usage` hoặc `rate_limit_event`.
- Fallback sang token budget nội bộ khi không có plan usage.
- Cập nhật realtime khi Claude run đang hoạt động.
- Hiển thị rõ khi đang dùng snapshot cache.

## Thành phần chính

- `sidecar/index.mjs`: Claude Agent SDK sidecar, nhận stream từ Claude và lấy `/usage`.
- `src-tauri/src/runs/sidecar.rs`: Rust drain task, parse event từ sidecar, lưu usage, emit event về frontend.
- `src-tauri/src/commands/stats.rs`: tính budget status, đọc/ghi `plan_usage`, cung cấp Tauri commands.
- `src/stores/budget.ts`: Pinia store giữ trạng thái usage/budget.
- `src/components/BudgetBadge.vue`: UI sidebar luôn hiển thị usage status.
- `src/views/SettingsView.vue`: bảng chi tiết plan usage trong Settings.

## Nguồn dữ liệu

### 1. Claude plan usage

Nguồn chính là dữ liệu plan/rate-limit của Claude:

- `_devdy_usage` từ sidecar, lấy bằng `query.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET()`.
- `system.init.rate_limits` nếu Claude gửi ngay lúc khởi tạo session.
- `rate_limit_event` khi Claude báo thay đổi trạng thái limit trong lúc run.

Dữ liệu được normalize và lưu vào `settings.plan_usage`.

### 2. Token budget nội bộ

Khi không có plan usage phù hợp, backend fallback sang ledger nội bộ:

- Bảng `run_usage`.
- Settings liên quan:
  - `token_budget_period`
  - `token_budget_limit`
  - `budget_warn_percent`

## Flow khi chạy Claude run

1. Frontend start/resume/send follow-up một run Claude.
2. Rust spawn `sidecar/index.mjs`.
3. Sidecar tạo Claude SDK `query`.
4. Khi nhận `system.init`:
   - Sidecar phát raw event về Rust.
   - Rust lưu `system.init.rate_limits` nếu có.
   - Sidecar gọi capture `/usage` best-effort.
5. Trong lúc run đang chạy:
   - Sidecar poll `/usage` mỗi 60 giây, chỉ khi ở chế độ `warning` (khi gần/vượt limit); run thường chỉ capture ở `init` và `result` để tiết kiệm token.
   - Nếu Claude gửi `rate_limit_event`, Rust merge ngay vào `settings.plan_usage`.
6. Khi nhận `result`:
   - Rust ghi row vào `run_usage`.
   - Rust emit `budget_status_updated`.
   - Sidecar capture `/usage` thêm lần cuối và flush trước khi đóng.
7. Khi `plan_usage` được cập nhật:
   - Rust emit `plan_usage_updated`.
   - Frontend refresh budget store ngay.

## Flow khi mở app/sidebar

1. `App.vue` luôn render `BudgetBadge`.
2. `BudgetBadge` gọi `budget.refreshPlanUsage()` khi mount.
3. Store gọi Tauri command `refresh_plan_usage`.
4. Backend spawn Claude sidecar ở chế độ `usage_probe`.
5. Probe cố lấy plan usage bằng:
   - `_devdy_usage` có `rate_limits`.
   - `system.init.rate_limits`.
   - `rate_limit_event`.
6. Nếu lấy được snapshot mới:
   - Backend lưu `settings.plan_usage`.
   - Emit `plan_usage_updated`.
7. Nếu probe không lấy được plan window:
   - Không ghi đè cache plan hợp lệ.
   - UI tiếp tục hiển thị snapshot gần nhất.
   - Badge ghi rõ dạng cached, ví dụ: `last captured 6m ago · updates during Claude runs`.

## Vì sao có trạng thái Cached

Idle probe của Claude SDK/CLI có thể trả:

```json
{
  "rate_limits_available": false,
  "rate_limits": null
}
```

Trường hợp này không phải snapshot plan thật, nên Devdy không dùng để ghi đè `settings.plan_usage`.

Realtime tuyệt đối chỉ có khi Claude cung cấp plan window qua một trong các event sau:

- `_devdy_usage` có `rate_limits`.
- `system.init.rate_limits`.
- `rate_limit_event`.

Nếu app đang idle và Claude không trả plan window, UI chỉ có thể hiển thị snapshot gần nhất.

## BudgetStatus backend trả về

`get_budget_status` trả một verdict chung cho UI và guardrail:

- `source`: `plan`, `tokens`, hoặc `disabled`.
- `percent`: phần trăm sử dụng.
- `is_warning`: đã qua ngưỡng warning.
- `is_over`: đã chạm/vượt limit.
- `reset`: thời điểm reset window.
- `captured_at`: thời điểm capture plan snapshot.
- `is_stale`: snapshot plan đã cũ hơn ngưỡng freshness.

## UI behavior

`BudgetBadge` luôn hiển thị:

- `Refreshing usage`: đang probe `/usage`.
- `% of plan limit`: có plan snapshot mới.
- `Cached: % of plan limit`: đang dùng snapshot cũ.
- `Plan limit reached`: plan usage >= 100%.
- `% of token budget`: fallback sang token budget nội bộ.
- `Usage status`: chưa có dữ liệu plan và chưa cấu hình token budget.

Countdown reset dùng clock reactive (`now`) nên tự đếm ngược, không bị đứng ở `reset in 1h`.

## Realtime events

Frontend đang nghe các event:

- `plan_usage_updated`: plan snapshot được cập nhật.
- `budget_status_updated`: usage ledger/token budget thay đổi sau turn.
- `run:budget_exceeded:<run_id>`: turn vừa làm vượt budget, UI có thể khóa composer/hiện cảnh báo.

## Guardrail khi gửi turn mới

Backend gọi `enforce_budget` tại các điểm tiêu token:

- `start_run`
- `resume_run`
- `send_user_message`

Nếu `is_over = true` và user không override, backend trả lỗi `BUDGET_EXCEEDED`.

## Ghi chú vận hành

- Thay đổi Tauri command/Rust cần rebuild/restart app, hot reload Vue không đủ.
- Nếu badge hiện cached nhưng `last captured` mới, app đang dùng snapshot hợp lệ gần nhất.
- Nếu muốn cập nhật plan usage thật ngay, cần có Claude run hoạt động để Claude phát `rate_limit_event` hoặc `/usage` có `rate_limits`.
