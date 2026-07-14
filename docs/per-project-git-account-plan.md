# Plan: Per-project GitHub/GitLab account cho Claude trong Devdy

> Trạng thái: ĐỀ XUẤT (chưa implement). Lập: 2026-07-14. Bản **v4** — chốt **một kiến
> trúc duy nhất: mọi thao tác đi qua `gh`/`glab`/`git` thật, bọc bởi một lớp
> proxy/shim + broker** (Phương án B). Thay thế v3 (hybrid), v2 (enumerate tool),
> v1 (tiêm PAT vào env).

## 1. Mục tiêu

Mỗi project gắn với một account GitHub/GitLab riêng. Khi Claude (Agent SDK sidecar)
làm việc trong project, nó gọi `gh`/`glab`/`git` như bình thường, nhưng **mọi lời gọi
đi qua một lớp proxy do Devdy kiểm soát** — proxy gắn đúng account, áp policy, và
**không bao giờ để token vào tầm với của agent**.

## 2. Nguyên tắc bảo mật cốt lõi

### 2.1 Token không bao giờ nằm trong env của agent

Claude có Bash tool và xử lý nội dung không đáng tin (issue/PR/comment, file repo, web).
Nếu token ở env, prompt-injection có thể dụ đọc (`echo $GH_TOKEN`) → exfiltrate, hoặc
lạm dụng. Trong kiến trúc này, token **chỉ sống trong broker và tiến trình gh/glab/git
thật do shim spawn**, không có trong env của sidecar/shell mà Claude điều khiển.

### 2.2 Fail-closed: máy chạy Devdy KHÔNG login GitHub/GitLab toàn cục

`gh` phân giải credential theo thứ tự: (1) env `GH_TOKEN`/`GITHUB_TOKEN` → (2)
`gh auth login` đã lưu. Nếu không có cả hai → `gh` fail mọi thao tác cần auth.

→ **Quy ước: máy chạy Devdy KHÔNG `gh auth login`, KHÔNG set token toàn cục; env của
sidecar KHÔNG chứa token.** Hệ quả:
- Cách duy nhất `gh`/`glab`/`git` auth được là **qua shim → broker**.
- Nếu Claude né shim (gọi gh thật bằng đường dẫn tuyệt đối, tự tải gh, hay `curl`) →
  **không có token → fail**. Hành vi fail-closed, an toàn theo mặc định.

### 2.3 Permission modal chỉ là cổng runtime — không thay thế broker

Đã xác minh: SDK gọi `canUseTool` (`sidecar/index.mjs:159-191`); `permissionMode`
quyết định có gate hay không (`sidecar.rs:31-38`), **`bypassPermissions` = chạy ngầm**;
"always allow" khiến lệnh khớp pattern tự duyệt. Vì modal dễ bị nới, **không dựa vào
modal để bảo vệ token** — broker là lớp thực thi policy chính; modal là lớp thứ hai
(xác nhận của người dùng cho thao tác ghi). Cấm `bypassPermissions` cho run có credential.

## 3. Bối cảnh hiện tại (đã xác minh trong code)

- Project gắn account qua `projects.github_account_id` → PAT lưu macOS Keychain
  (`src-tauri/src/secrets.rs`, service `vn.papay.devdy`).
- `src-tauri/src/github/mod.rs::client_for_project()` đã lấy PAT theo account (Octocrab)
  — broker sẽ tái dùng cơ chế lấy PAT/mint token này.
- Devdy chạy Claude qua **Agent SDK** trong Node sidecar (`sidecar/index.mjs::query`),
  có hook `canUseTool`. Bash tool bật mặc định.
- Sidecar spawn với `cwd = project_path` (`commands/runs.rs`, nhánh `claude`), hiện chỉ
  set env `DEVDY_*`.
- **GitLab chưa được mô hình hóa** (chỉ có `github_accounts`).

## 4. Quyết định đã chốt

| Vấn đề | Quyết định |
|---|---|
| Kiến trúc | **Proxy/shim + broker** cho `gh`/`glab`/`git` — mọi thao tác đi qua gh/glab thật |
| Trạng thái máy | **Logged-out global** (fail-closed); env sidecar không chứa token |
| Nguồn credential | Broker cấp; ưu tiên **token phù du** (GitHub App / project token), tối thiểu PAT fine-grained + expiry |
| Provider | GitHub + GitLab (ngang hàng) |
| Thao tác ghi | Broker áp policy + **permission modal**; cấm `bypassPermissions` |
| Quan hệ `devtools` | Devdy thay thế devtools cho luồng AI; không tích hợp |
| Cơ chế switch | Không switch global; token gắn per-run/per-project qua broker |

## 5. Kiến trúc proxy/shim + broker

```
Claude (agent, KHÔNG có token)
  │  chạy `gh pr create ...` / `glab mr create ...` / `git push`
  ▼
Shim `gh` / `glab` / git-credential-helper  (đứng ĐẦU PATH của sidecar)
  │  gửi argv + host + project_id qua Unix socket
  ▼
Broker Devdy (Rust)  ── áp policy (allow/deny) ── (tùy) bật permission modal
  │  nếu duyệt: mint/lấy token đúng account cho project (phù du)
  ▼
gh/glab/git THẬT do shim spawn, token chỉ trong tiến trình con này
  │  stdout/stderr
  ▼
trả về cho Claude (đã redact token nếu lỡ xuất hiện)
```

### 5.1 Broker (Devdy, Rust)
- Sở hữu credential: lấy PAT theo account từ Keychain (tái dùng `client_for_project`
  logic) hoặc **mint token phù du** (GitHub App installation token / GitLab project
  access token).
- Lắng nghe **Unix socket per-run** (đường dẫn truyền vào shim qua env, ví dụ
  `DEVDY_BROKER_SOCK`); chỉ shim của đúng run kết nối được.
- **Áp policy** trên `argv`:
  - Allowlist subcommand thao tác thường (pr, issue, mr, repo view, ci…).
  - **Denylist bắt buộc**: `gh auth token`, `gh auth status --show-token`,
    `gh auth setup-git`, `gh secret`, `gh ssh-key`, và tương tự cho `glab` — các lệnh
    in/ghi credential.
  - `gh api` / `glab api` (cửa hậu vạn năng): mặc định **chỉ cho qua permission modal**,
    hoặc giới hạn method/endpoint.
- Cho thao tác ghi → bắn `_devdy_permission_request` (dùng lại luồng modal hiện có).
- Ghi **audit log** mọi lời gọi (đã redact token).

### 5.2 Shim `gh` / `glab`
- Binary/script nhỏ tên `gh`/`glab`, đặt trong thư mục **đầu PATH** của sidecar
  (`augment_command_path` prepend). Ẩn gh/glab thật (đường dẫn riêng chỉ broker biết).
- Luồng: nhận argv → hỏi broker → nếu allow, `exec` gh/glab thật với token do broker
  cấp (set trong **env của tiến trình con này**, không phải env agent) → stream I/O.
- Không tự chứa token; xin broker theo từng lời gọi.

### 5.3 Git transport (push/fetch/clone)
- `git` không phải lệnh gh nên xử lý qua **git credential helper** trỏ về broker:
  cấu hình per-run `credential.helper` = helper của Devdy (qua env
  `GIT_CONFIG_COUNT`/`GIT_CONFIG_KEY_*` để không đụng config global).
- Helper hỏi broker token **theo host** → **host allowlist** (chỉ host của account đã
  gắn; từ chối remote lạ, chặn dụ `git remote add evil`).
- `GIT_AUTHOR/COMMITTER_NAME/EMAIL` set theo account để commit đúng danh tính.

### 5.4 Vì sao đủ an toàn
- Agent không có token; né shim → fail-closed.
- Chặn ở **thời điểm thực thi** (không parse chuỗi lệnh) → khó né bằng obfuscation
  (`g=gh; $g ...`, base64…).
- Token phù du + scope hẹp → blast radius nhỏ kể cả khi lộ.

### 5.5 Rủi ro tồn dư của mô hình proxy (ghi nhận thẳng)
- **Denylist có thể sót**: gh/glab nhiều lệnh; phải rà kỹ các lệnh in credential và
  cập nhật theo phiên bản. → Giữ allowlist "default-deny subcommand" thay vì chỉ blocklist
  khi có thể.
- **`gh api`/`glab api`** là cửa hậu: mặc định gate bằng modal / giới hạn.
- **Đọc env tiến trình con**: trong lúc gh thật chạy, `GH_TOKEN` nằm trong env của nó;
  agent chạy song song về lý thuyết có thể `ps eww`/đọc `/proc/<pid>/environ` (cùng uid).
  → Giảm thiểu: **token phù du ngắn hạn** (dù bắt được cũng hết hạn ~1h, scope hẹp);
  cân nhắc `GH_CONFIG_DIR` per-run thay cho env; window lộ chỉ trong thời gian lệnh chạy.

## 6. Kiến trúc theo giai đoạn

### Giai đoạn 1 — Mô hình dữ liệu account (GitHub + GitLab)
- Migration `gitlab_accounts`: `id, label, username, host, email, scopes, created_at`.
- `ALTER TABLE projects ADD COLUMN gitlab_account_id TEXT REFERENCES gitlab_accounts(id) ON DELETE SET NULL`.
- Thêm cột `email` cho `github_accounts` (git commit identity).
- `secrets.rs`: `set/get/has_gitlab_account_pat` (Keychain key `gitlab_account_<id>`).
- `commands/gitlab_accounts.rs`: mirror `commands/github_accounts.rs`.
- Một project gắn được **cả** GitHub và GitLab.

### Giai đoạn 2 — Broker + Unix socket + policy
- Module broker (Rust): socket per-run, lấy/mint token theo project, policy engine
  (allowlist/denylist), audit log, cầu nối tới permission modal.
- Spawn broker cùng lúc với sidecar trong `commands/runs.rs`; truyền `DEVDY_BROKER_SOCK`.

### Giai đoạn 3 — Shim gh/glab + PATH isolation
- Viết shim `gh`/`glab`; đặt đầu PATH của sidecar (mở rộng `augment_command_path`),
  ẩn binary thật.
- Env sidecar **không** chứa `GH_TOKEN`/`GITLAB_TOKEN`.

### Giai đoạn 4 — Git credential helper + host allowlist
- Helper trỏ broker qua `GIT_CONFIG_*` per-run; host allowlist; commit identity.

### Giai đoạn 5 — Token phù du
- GitHub App installation token (~1h, scope repo) / GitLab project access token; tối
  thiểu PAT fine-grained + expiry. Broker chịu trách nhiệm mint/refresh.

### Giai đoạn 6 — Context cho Claude + UI
- `appendSystemPrompt`: "Project dùng account `<label>` (@username) trên `<host>`. Cứ
  dùng `gh`/`glab`/`git` bình thường — đã tự gắn đúng account; không cần login/token.
  Một số lệnh chạm credential sẽ bị chặn."
- Project settings: gắn **GitLab account** + email commit. RunView: badge account active.
- Onboarding/Settings: kiểm tra & nhắc máy **chưa login gh/glab global** (fail-closed).

## 7. Rủi ro & giảm thiểu

| Rủi ro | Mức | Giảm thiểu |
|---|---|---|
| Prompt injection → exfiltrate token | Cao → thấp | Token không vào env agent; đi qua broker; token phù du |
| Lệnh gh/glab in token (`auth token`…) | Trung bình | Denylist ở broker; default-deny subcommand |
| `gh api`/`glab api` cửa hậu | Trung bình | Gate bằng modal / giới hạn method+endpoint |
| Đọc env tiến trình gh con (`ps`/`/proc`) | Thấp–TB | Token phù du ngắn hạn; cân nhắc `GH_CONFIG_DIR` per-run |
| Token gửi sai host (git) | Trung bình | Host allowlist cứng; từ chối remote lạ |
| Né shim (gh thật / curl) | Thấp | Fail-closed: không token global; env không token |
| Token rò vào log/transcript | Trung bình | Redaction; không token-in-URL; `.devdy/` gitignore |
| Lạm dụng năng lực (push mã độc, PR rác) | Trung bình | Permission modal; scope token hẹp; cấm `bypassPermissions` |
| Commit spoofing danh tính | Thấp | Đánh dấu commit do AI / email noreply riêng |

### Checklist bắt buộc (must-do)
1. Env sidecar **không** chứa token; token chỉ trong broker + tiến trình gh/glab/git con.
2. Máy chạy Devdy **logged-out global** (không `gh auth login`, không token toàn cục).
3. Broker **default-deny subcommand** + denylist lệnh in/ghi credential; gate `api`.
4. **Host allowlist** cho git; từ chối host không khớp account.
5. **Token phù du hoặc PAT fine-grained + expiry**; không dùng classic `repo` scope.
6. **Redaction** token khỏi log/transcript/audit; `.devdy/` đã gitignore.
7. Giữ **permission modal**; **cấm `bypassPermissions`** cho run có thao tác ghi.

## 8. Đánh đổi
- Ưu điểm: **tái dùng toàn bộ tính năng gh/glab/git** — Claude làm việc tự nhiên, ít
  phải viết tool riêng; token luôn ngoài tầm agent.
- Nhược điểm: mô hình **denylist/allowlist subcommand** cần rà kỹ và bảo trì theo phiên
  bản gh/glab; `gh api` cần chính sách; có window nhỏ token ở env tiến trình con. Chấp
  nhận được với công cụ cá nhân + token phù du.

## 9. Thứ tự triển khai đề xuất
GĐ1 (data) → GĐ2 (broker + policy) → GĐ3 (shim gh/glab) → GĐ4 (git credential helper) →
GĐ5 (token phù du) → GĐ6 (context + UI + kiểm tra fail-closed).

Ship dần: GĐ1–3 (+ logged-out global) đã cho phép Claude dùng gh/glab đúng account an
toàn; GĐ4–5 bổ sung git transport và token phù du.

## 10. File dự kiến chạm
- `src-tauri/migrations/00XX_gitlab_accounts.sql`, `00XX_account_email.sql` (mới)
- `src-tauri/src/secrets.rs`
- `src-tauri/src/commands/gitlab_accounts.rs` (mới)
- `src-tauri/src/github/mod.rs`, `src-tauri/src/gitlab/mod.rs` (mint/lấy token cho broker)
- `src-tauri/src/runs/broker.rs` (mới — socket + policy + audit)
- `src-tauri/src/commands/runs.rs` (spawn broker, truyền `DEVDY_BROKER_SOCK`, PATH shim, GIT_CONFIG_*)
- Shim: `sidecar-proxy/gh`, `sidecar-proxy/glab`, git credential helper (mới)
- `sidecar/index.mjs` (đọc appendSystemPrompt; đảm bảo env không token)
- Frontend: Project settings (gắn GitLab account) + RunView badge + nhắc fail-closed (Vue)
