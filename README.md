# Devdy

> Ứng dụng desktop dành cho lập trình viên — quản lý AI Skills/Rules và tự động hóa phân tích GitHub Issue / review Pull Request, chạy trực tiếp trên subscription CLI sẵn có của bạn.

## Devdy là gì?

**Devdy** là ứng dụng desktop (ưu tiên macOS) xây trên **Tauri 2 + Vue 3 + Rust**. Nó quản lý tập trung **AI Skills / Rules** và tự động hóa các tác vụ phát triển bằng cách điều khiển hai engine AI: `claude` và `codex`.

Điểm khác biệt cốt lõi:

- ✅ **Không cần API key** — chạy dựa trên đăng nhập/subscription CLI sẵn có (`claude` đăng nhập subscription, `codex` đăng nhập ChatGPT).
- ✅ **Local-first** — toàn bộ dữ liệu lưu trong SQLite trên máy, không đồng bộ cloud.
- ✅ **Bảo mật** — GitHub PAT chỉ lưu trong OS Keychain, không bao giờ ghi ra disk hay log.

## Các chức năng chính

### 1. Chạy AI engine (the "run")
Trái tim của ứng dụng. Mỗi **run** là một lần thực thi AI trên một prompt, với 3 dạng:

- 🔍 **Phân tích GitHub Issue**
- 👀 **Review Pull Request**
- 💬 **Phiên làm việc tự do** (free session)

Hỗ trợ:
- **Multi-turn** — hội thoại nhiều lượt trong cùng một phiên.
- **Resume** — tiếp tục phiên đã kết thúc.
- **Streaming đồng thời** — nhiều run chạy và stream output cùng lúc; output được giữ nguyên khi chuyển màn hình.

### 2. Hai engine thay thế lẫn nhau
- `claude` — qua **Claude Agent SDK**.
- `codex` — qua **codex app-server** (JSON-RPC).
- **Handoff** — chuyển toàn bộ ngữ cảnh từ engine này sang engine kia.

Cả hai engine nói chung một giao thức nội bộ, nên giao diện hiển thị, modal xin quyền... dùng chung mà không cần sửa đổi.

### 3. Quản lý Skills & Rules
- Soạn thảo ngay trong app bằng **CodeMirror 6**.
- **Apply** vào cây thư mục của project, target cho `claude`, `codex` hoặc cả hai.
- Theo dõi đồng bộ bằng **hash**; khi bản copy ở project lệch với nguồn → tạo **sync conflict** để xử lý có kiểm soát.

### 4. Quản lý tài khoản & bảo mật
- GitHub PAT chỉ lưu trong **OS Keychain** (qua `keyring`).
- DB chỉ lưu tham chiếu khóa, không lưu secret.
- Engine tự xác thực qua login CLI sẵn có — app không quản lý API key nào.

### 5. Theo dõi sử dụng & chi phí
- Hiển thị **mức sử dụng rate-limit** thật của gói claude.ai (dữ liệu `/usage`).
- Ghi lại **token & chi phí** mỗi run: ưu tiên cost thật từ Claude SDK, ước lượng cho Codex.
- Bản ghi usage lưu tự chứa (self-contained) nên vẫn tồn tại kể cả khi xóa run hoặc project.

### 6. Session mirroring
Tự động phát hiện và phản chiếu các transcript Claude dùng chung (từ CLI / VSCode) qua file watcher.

## Kiến trúc tổng quan

Luồng dữ liệu của một **run** chạy qua bốn lớp:

```
Vue (liveRuns store) ──invoke──▶ Rust commands ──spawn──▶ Node sidecar ──stdio──▶ claude/codex CLI
       ▲                              │                         │
       └────── Tauri events ──────────┴── NDJSON drain ─────────┘
```

1. **Frontend** (Vue 3) gọi `start_run` / `resume_run` / `send_user_message` qua Tauri `invoke`, lắng nghe sự kiện theo từng run.
2. **Rust commands** resolve engine + model + paths, spawn **Node sidecar**, đăng ký vào `RunRegistry` và chạy task `drain_sidecar`.
3. **Sidecars** dịch giữa broker và engine thật:
   - `sidecar/` — host Claude Agent SDK.
   - `sidecar-codex/` — điều khiển `codex app-server` và **dịch output sang định dạng stream-json giống Claude**.
4. **`drain_sidecar`** đọc stdout của sidecar theo dòng, lưu log stream-json, ghi nhận usage, và re-emit về frontend.

## Tech stack

| Lớp | Công nghệ |
|-----|-----------|
| Frontend | Vue 3, Pinia, TanStack Query, Tailwind v4, CodeMirror 6 |
| Backend | Rust, Tauri 2, sqlx (SQLite) |
| Sidecar | Node.js (≥ 22), `@anthropic-ai/claude-agent-sdk`, codex app-server |
| Lưu trữ | SQLite local + file log stream-json |

## Yêu cầu môi trường

- **Node ≥ 22**, **pnpm** (qua corepack)
- **Rust stable** + Tauri 2 toolchain
- `claude` CLI đã đăng nhập subscription
- `codex` CLI đã đăng nhập ChatGPT

## Bắt đầu nhanh

```bash
pnpm install                  # frontend deps
npm --prefix sidecar install  # Claude Agent SDK sidecar deps

pnpm tauri dev                # chạy app (Vite + Tauri, hot reload)
pnpm tauri build              # build bản production
```

---

*Xem `SPEC.md` để biết spec chức năng đầy đủ, và `docs/` cho ghi chú từng tính năng.*
