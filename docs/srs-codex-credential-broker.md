# Software Requirements Specification — Cấp Credential Broker + Git Integration cho Codex (ngang bằng Claude Code)

## 1. Thông tin tài liệu
- **Phiên bản:** 1.0 (draft)
- **Ngày cập nhật:** 2026-07-15
- **Trạng thái:** Draft để review
- **Người chuẩn bị:** BA (skill ba-srs-analyst)
- **Nguồn requirement:** Yêu cầu người dùng "muốn Codex cũng giống Claude" + kết quả điều tra code Devdy và thực nghiệm sandbox Codex CLI 0.140.0 trên macOS 26.4 (arm64)

> Ghi chú: SRS này mô tả một tính năng nội bộ của ứng dụng Devdy (Tauri + Vue + Rust). "Người dùng" ở đây là người dùng cuối của Devdy (developer), không phải end-user của một hệ thống web thương mại.

---

## 2. Mục tiêu

### 2.1 Bối cảnh
Devdy tích hợp hai engine AI CLI: **Claude Code** (qua Agent SDK) và **Codex** (qua `codex app-server`). Hiện tại chỉ Claude runs được "wire" cơ chế **Credential Broker + Git Integration** (đặt tên nội bộ GĐ3/GĐ6/GĐ7):
- `wire_broker()` cấp một Unix socket broker per-run, một git credential helper (`git-credential-devdy`) và shim cho `gh`/`glab`.
- Nhờ đó, khi agent chạy `git`/`gh`/`glab`, credential được broker cấp theo policy per-project, không lộ token ra env.

Codex hiện **không** được wire (`broker_run: None`, comment "Codex is untouched by GĐ3"). Khi chạy Codex, thao tác git chỉ dựa vào credential toàn cục của máy (hoặc treo/fail nếu máy trống credential), và không có kiểm soát per-project.

### 2.2 Vấn đề cần giải quyết
- Codex không tận dụng được credential broker → không có git per-project account, không fail-closed, không audit như Claude.
- Trải nghiệm hai engine không đồng nhất, gây nhầm lẫn khi handoff Claude ↔ Codex.

### 2.3 Mục tiêu nghiệp vụ
Cho phép Codex runs sử dụng đúng cơ chế Credential Broker + Git Integration như Claude, để `git`/`gh`/`glab` trong Codex run:
1. Lấy được credential từ broker theo policy per-project.
2. Thực hiện được thao tác git remote (clone/fetch/pull/push HTTPS).
3. Giữ mô hình bảo mật fail-closed và audit tương đương Claude ở mức tối đa mà Codex CLI cho phép.

### 2.4 Tiêu chí thành công
- Một Codex run có thể `git push`/`pull` tới private repo qua HTTPS, dùng token do broker cấp (không dùng credential toàn cục máy).
- `gh`/`glab` trong Codex run được cấp `GH_TOKEN`/`GITLAB_TOKEN` qua shim của broker.
- Hành vi Claude hiện tại **không đổi**.
- Khi cấu hình không cho phép (ví dụ giữ sandbox `workspace-write` trên macOS), hệ thống báo rõ giới hạn thay vì treo âm thầm.

---

## 3. Phạm vi

### 3.1 In-scope
- Wire `wire_broker()` cho nhánh Codex trong `start_run()` và `resume_run()` (`src-tauri/src/commands/runs.rs`).
- Truyền và **không bị lọc mất** các biến môi trường broker (`DEVDY_BROKER_SOCK`, `DEVDY_PROJECT_ID`, `DEVDY_RUN_ID`, `GIT_CONFIG_*`) tới tiến trình lệnh mà Codex thực thi (cấu hình `shell_environment_policy` của Codex).
- Cấu hình sandbox Codex sao cho `git` truy cập được: Unix socket của broker + network egress.
- Thông báo/cảnh báo cho người dùng khi cấu hình hiện hành không thể bật network trên nền tảng đang chạy (đặc biệt macOS — bug #10390).
- Tùy chọn bật/tắt tính năng (feature flag) để không ảnh hưởng hành vi hiện tại theo mặc định.

### 3.2 Out-of-scope
- Sửa lỗi upstream của Codex CLI (bug macOS seatbelt #10390).
- Thay đổi cơ chế broker lõi (policy engine, DB token) — tái sử dụng nguyên trạng của Claude.
- True session resume của Codex (vẫn dùng cơ chế hiện tại).
- Hỗ trợ SSH-based git remote (SRS này tập trung HTTPS + credential helper; SSH xem Open Issue).
- Nền tảng Windows (chưa nằm trong phạm vi kiểm chứng; xem Open Issue).

### 3.3 Dependency
- `DEP-01`: Cơ chế broker sẵn có của Claude (`src-tauri/src/runs/broker/`, `sidecar-proxy/git-credential-devdy.mjs`, shim `gh`/`glab`).
- `DEP-02`: Codex CLI ≥ 0.140.0 với subcommand `app-server`, hỗ trợ config `shell_environment_policy`, `sandbox_mode`, và cấu hình unix socket (`permissions.*.network.unix_sockets` hoặc `features.network_proxy.dangerously_allow_all_unix_sockets`).
- `DEP-03`: Account/token per-project đã liên kết trong DB Devdy (giống điều kiện Claude hiện nay).

### 3.4 Assumptions
| ID | Giả định | Ảnh hưởng nếu sai |
|---|---|---|
| AS-01 | `wire_broker()` là OS-level, không phụ thuộc Agent SDK, dùng lại được cho Codex | Nếu sai, phải refactor broker; tăng đáng kể khối lượng |
| AS-02 | Trên macOS, chỉ `sandbox = danger-full-access` mới có network (đã kiểm chứng bug #10390) | Nếu Codex vá bug, có thể giữ `workspace-write` + `network_access=true`, an toàn hơn |
| AS-03 | Codex CLI honor cấu hình `-c` truyền lúc spawn `app-server` (env policy, sandbox, unix socket) | Nếu không honor, phải dùng file `config.toml`/`CODEX_HOME` riêng cho run |
| AS-04 | `shell_environment_policy.inherit` mặc định là `core` và sẽ lọc `DEVDY_*`/`GIT_CONFIG_*` | Nếu thực tế không lọc, bỏ được FR-002 (giảm việc) |
| AS-05 | Người dùng chấp nhận đánh đổi bảo mật khi bật full-access để có network trên macOS | Nếu không, tính năng git-remote không khả dụng trên macOS; chỉ git local |

### 3.5 Constraints
- `CON-01`: **Bug macOS seatbelt #10390 (chưa có bản vá):** ở mọi sandbox mode có seatbelt (`read-only`, `workspace-write`), Codex ép `CODEX_SANDBOX_NETWORK_DISABLED=1`; `network_access=true` bị bỏ qua. Chỉ `danger-full-access` mới có network. (Kiểm chứng trực tiếp trên máy.)
- `CON-02`: `codex sandbox` (subcommand debug) bỏ qua `sandbox_mode` trong config → không dùng để cấu hình runtime; runtime thật đi qua `app-server`.
- `CON-03`: Mặc định `shell_environment_policy.inherit = "core"` lọc biến lạ và chặn biến chứa `KEY/SECRET/TOKEN`.
- `CON-04`: Trên macOS seatbelt, AF_UNIX bị chặn mặc định; phải khai báo cho phép socket path của broker (đã kiểm chứng `--allow-unix-socket` mở được).
- `CON-05`: Không được thay đổi hành vi Claude hiện tại.

---

## 4. Thuật ngữ và định nghĩa
| Thuật ngữ | Định nghĩa |
|---|---|
| Broker | Tiến trình cấp credential per-run của Devdy qua Unix socket, áp policy và resolve token từ DB |
| Shim | Script chặn `gh`/`glab` được prepend vào PATH để inject token qua broker |
| git-credential-devdy | Git credential helper của Devdy, nối tới broker socket để lấy username/password (token) |
| Engine | Loại AI CLI của một run: `claude` hoặc `codex` |
| app-server | Chế độ chạy của Codex CLI mà Devdy điều khiển qua JSON-RPC (`thread/start`) |
| sandbox_mode | Chế độ sandbox OS của Codex: `read-only` / `workspace-write` / `danger-full-access` |
| shell_environment_policy | Chính sách lọc biến môi trường khi Codex chạy lệnh shell (tool call) |
| Fail-closed | Nếu broker từ chối/không có token thì thao tác git thất bại, không fallback sang credential máy |
| Bug #10390 | Lỗi upstream: macOS seatbelt bỏ qua `network_access`, luôn tắt network trừ full-access |

---

## 5. Actor và phân quyền
| Actor | Mô tả | Phạm vi dữ liệu | Quyền chính |
|---|---|---|---|
| Người dùng Devdy (Developer) | Người tạo/chạy run, phê duyệt permission | Project của mình | Bật/tắt tính năng, chọn engine, chọn permission mode, approve/deny git/gh |
| Codex run (agent) | Tiến trình Codex thực thi lệnh trong run | Theo policy broker per-project | Yêu cầu credential/thực thi git dưới sự kiểm soát broker + approval |
| Broker (System) | Thành phần cấp credential | Token của account liên kết project | Resolve/deny token theo policy, ghi audit |
| Devdy backend (System) | Rust core spawn sidecar + wiring | Toàn bộ run | Wire broker, set env/sandbox, quản registry run |

---

## 6. Tổng quan nghiệp vụ

### 6.1 Context
Khi người dùng khởi chạy một Codex run có bật tính năng, Devdy sẽ: spawn `sidecar-codex` → wire broker (env + shim + git config) → cấu hình Codex (env policy + sandbox) → Codex thực thi lệnh git/gh → git credential helper/shim gọi broker socket → broker áp policy, cấp token → thao tác git remote hoàn tất.

### 6.2 Luồng tổng quát
1. Người dùng bật tính năng "Broker cho Codex" và chọn permission mode.
2. Devdy spawn Codex sidecar, gọi `wire_broker()` (giống Claude): tạo socket, set `DEVDY_BROKER_SOCK/PROJECT_ID/RUN_ID`, prepend shim vào PATH, set `GIT_CONFIG_*` + `git-credential-devdy`.
3. Devdy cấu hình Codex để (a) không lọc mất các biến broker khi chạy lệnh; (b) cho phép AF_UNIX tới socket path; (c) bật network (chọn sandbox phù hợp nền tảng).
4. Codex chạy `git`/`gh`/`glab` như một tool call.
5. Helper/shim nối broker socket kèm `run_id` → broker resolve theo policy → trả token hoặc deny.
6. Nếu cần phê duyệt, broker phát "Ask" tới đúng modal của run (`run_id`); người dùng approve/deny.
7. Thao tác git remote thành công (hoặc fail-closed nếu bị từ chối / thiếu điều kiện).

### 6.3 State transition (theo góc độ khả năng git remote của một Codex run)
| Trạng thái hiện tại | Sự kiện | Điều kiện | Trạng thái tiếp theo | Actor/System |
|---|---|---|---|---|
| FeatureOff | Người dùng bật tính năng | — | Wiring | Người dùng |
| Wiring | `wire_broker()` OK | Socket + env + shim sẵn sàng | BrokerReady | System |
| Wiring | Wire thất bại | Lỗi tạo socket/helper | RunFailed | System |
| BrokerReady | Cấu hình sandbox/env | Nền tảng cho phép network | NetworkEnabled | System |
| BrokerReady | Cấu hình sandbox/env | macOS + không dùng full-access | NetworkBlocked(Warned) | System |
| NetworkEnabled | Codex chạy git remote | Broker cấp token | GitRemoteOK | Codex/Broker |
| NetworkEnabled | Codex chạy git remote | Broker deny | GitRemoteDenied(fail-closed) | Broker |
| NetworkBlocked(Warned) | Codex chạy git remote | Network bị sandbox chặn | GitRemoteBlocked | Codex sandbox |

---

## 7. Functional requirements

### FR-001 — Wire Credential Broker cho Codex run (start + resume)
- **Mục tiêu:** Codex run có socket broker, git credential helper và shim `gh`/`glab` như Claude.
- **Actor:** Devdy backend (System).
- **Trigger:** Bắt đầu một Codex run (hoặc resume) khi tính năng bật.
- **Preconditions:** Engine = `codex`; account/token liên kết project sẵn sàng; tính năng bật (SEC-flag).
- **Main flow:**
  1. Spawn Codex sidecar (`resolve_codex_sidecar`).
  2. Gọi `wire_broker(app, db, cmd, run_id, project_id, project_path)` như nhánh Claude.
  3. Prepend thư mục shim vào PATH; set `DEVDY_BROKER_SOCK`, `DEVDY_PROJECT_ID`, `DEVDY_RUN_ID`.
  4. Set `GIT_CONFIG_*` (reset helper hệ thống → `git-credential-devdy`, `useHttpPath=false`, `GIT_TERMINAL_PROMPT=0`).
  5. Lưu `RunHandles { broker_run: Some(...) }` vào registry.
- **Alternative flows:** Resume run: áp dụng đúng wiring trên trong `resume_run()`.
- **Error/exception flows:** Nếu `wire_broker()` lỗi → run fail với thông báo rõ; không spawn Codex ở trạng thái nửa vời.
- **Postconditions:** Codex sidecar chạy với broker gắn theo `run_id`; `broker_run: Some`.
- **Business rules liên quan:** BR-001, BR-002
- **Permission liên quan:** SEC-001, SEC-002
- **Nguồn:** SRC-001, SRC-002

```gherkin
Scenario: Codex run được gắn broker khi bật tính năng
  Given tính năng "Broker cho Codex" đang bật và project đã liên kết account
  When người dùng khởi chạy một Codex run
  Then process Codex nhận biến DEVDY_BROKER_SOCK, DEVDY_PROJECT_ID, DEVDY_RUN_ID
  And PATH có thư mục shim đứng trước gh/glab thật
  And run được lưu với broker_run = Some

Scenario: Resume Codex run cũng được gắn broker
  Given một Codex run trước đó và tính năng đang bật
  When người dùng resume run đó
  Then broker được wire lại giống lúc start
```

### FR-002 — Không lọc mất biến môi trường broker khi Codex chạy lệnh
- **Mục tiêu:** Bảo đảm `DEVDY_BROKER_SOCK`, `DEVDY_RUN_ID`, `DEVDY_PROJECT_ID`, `GIT_CONFIG_*` đến được tiến trình `git`/`gh`/`glab` mà Codex spawn.
- **Actor:** Devdy backend (System).
- **Trigger:** Cấu hình Codex `app-server` lúc spawn.
- **Preconditions:** FR-001 đã set các biến trên cho tiến trình sidecar.
- **Main flow:**
  1. Devdy set `shell_environment_policy` cho Codex sao cho các biến broker được giữ (ví dụ `inherit=all`, hoặc `include_only`/`set` whitelist đúng các biến broker).
  2. Xác minh biến tồn tại trong môi trường lệnh mà Codex thực thi.
- **Alternative flows:** Nếu không truyền được qua `-c` lúc spawn, dùng `CODEX_HOME`/`config.toml` tạm riêng cho run.
- **Error/exception flows:** Nếu biến bị lọc → git credential helper không được cấu hình/không tìm thấy socket → git remote fail-closed; ghi log chẩn đoán rõ nguyên nhân.
- **Postconditions:** Lệnh git do Codex chạy nhìn thấy đủ biến broker.
- **Business rules liên quan:** BR-002
- **Permission liên quan:** SEC-003
- **Nguồn:** derived (từ CON-03)

```gherkin
Scenario: Biến broker không bị shell_environment_policy lọc
  Given Codex chạy với shell_environment_policy đã cấu hình cho tính năng
  When Codex thực thi lệnh "env"
  Then output chứa DEVDY_BROKER_SOCK và GIT_CONFIG_COUNT

Scenario: Cảnh báo khi biến bị lọc
  Given cấu hình env policy sai khiến DEVDY_BROKER_SOCK bị lọc
  When Codex chạy một lệnh git remote
  Then thao tác thất bại fail-closed
  And log ghi rõ "thiếu DEVDY_BROKER_SOCK trong môi trường lệnh"
```

### FR-003 — Cho phép truy cập Unix socket của broker trong sandbox Codex
- **Mục tiêu:** `git-credential-devdy` nối được tới socket broker dù Codex chạy trong sandbox.
- **Actor:** Devdy backend (System).
- **Trigger:** Cấu hình sandbox Codex lúc spawn.
- **Preconditions:** Đường dẫn socket broker xác định (per-run, dưới thư mục broker).
- **Main flow:**
  1. Nếu sandbox = `danger-full-access`: không cần cấu hình thêm (không có seatbelt).
  2. Nếu còn sandbox (seatbelt): khai báo cho phép AF_UNIX tại socket path của broker (qua cấu hình unix socket của Codex tương ứng phiên bản).
- **Alternative flows:** —
- **Error/exception flows:** Nếu không khai báo → connect socket bị "Operation not permitted"; helper fail → git fail-closed; log rõ.
- **Postconditions:** Helper nối socket thành công (đã kiểm chứng bằng thực nghiệm với `--allow-unix-socket`).
- **Business rules liên quan:** BR-003
- **Permission liên quan:** SEC-003
- **Nguồn:** derived (từ CON-04)

```gherkin
Scenario: Helper nối được broker socket trong sandbox
  Given Codex chạy trong sandbox và socket broker được cho phép
  When git gọi git-credential-devdy
  Then helper kết nối socket thành công và nhận được token
```

### FR-004 — Bật network egress cho thao tác git remote theo nền tảng
- **Mục tiêu:** `git clone/fetch/pull/push` HTTPS thực hiện được.
- **Actor:** Devdy backend (System) + Người dùng (chọn mode).
- **Trigger:** Người dùng chạy Codex run cần git remote.
- **Preconditions:** FR-001..FR-003 thỏa; account/token hợp lệ.
- **Main flow (macOS):**
  1. Để có network, đặt sandbox Codex = `danger-full-access` cho run (tương ứng mode `bypassPermissions`).
  2. Codex chạy git remote → network hoạt động → broker cấp token → thành công.
- **Main flow (Linux — kỳ vọng):**
  1. Có thể giữ `workspace-write` + `network_access=true` (AF_UNIX được seccomp miễn trừ) → cần kiểm chứng (Open Issue OI-02).
- **Alternative flows:** Người dùng chọn giữ sandbox có bảo vệ và chấp nhận **chỉ git local** (không remote) trên macOS.
- **Error/exception flows:** Trên macOS nếu không dùng full-access → network bị chặn (bug #10390) → git remote fail; hệ thống phải **cảnh báo trước** (FR-005), không để treo.
- **Postconditions:** Git remote thành công khi network bật; hoặc fail có thông báo khi bị chặn.
- **Business rules liên quan:** BR-004, BR-005
- **Permission liên quan:** SEC-004
- **Nguồn:** SRC-003, derived (CON-01)

```gherkin
Scenario: Git push thành công trên macOS với full-access
  Given Codex run đặt sandbox danger-full-access và broker đã wire
  And project liên kết account có quyền push repo private
  When Codex chạy "git push" tới repo private qua HTTPS
  Then thao tác thành công dùng token do broker cấp
  And không dùng credential toàn cục của máy

Scenario: Git remote bị chặn network trên macOS với workspace-write
  Given Codex run giữ sandbox workspace-write trên macOS
  When Codex chạy "git push"
  Then thao tác thất bại do network bị sandbox chặn
  And hệ thống đã cảnh báo trước khi chạy (xem FR-005)
```

### FR-005 — Cảnh báo giới hạn nền tảng và cấu hình
- **Mục tiêu:** Người dùng hiểu rõ khi cấu hình hiện tại không thể git remote (tránh treo/nhầm lẫn).
- **Actor:** Devdy backend + UI.
- **Trigger:** Khởi chạy Codex run có bật tính năng nhưng cấu hình không đủ điều kiện network.
- **Preconditions:** Nền tảng macOS + sandbox ≠ full-access (hoặc điều kiện tương đương chặn network).
- **Main flow:**
  1. Trước/đầu run, phát cảnh báo (một dòng, tiếng Việt) nêu: network sẽ bị chặn, cần chuyển mode để git remote.
  2. Nêu cách khắc phục (dùng mode full-access / bypassPermissions cho run này).
- **Alternative flows:** Nếu cấu hình đủ điều kiện → không cảnh báo.
- **Error/exception flows:** —
- **Postconditions:** Cảnh báo hiển thị trong log/stream run.
- **Business rules liên quan:** BR-005
- **Permission liên quan:** —
- **Nguồn:** derived (CON-01)

```gherkin
Scenario: Cảnh báo network bị chặn
  Given macOS và Codex run giữ sandbox có seatbelt
  When run bắt đầu với tính năng bật
  Then log/stream hiển thị cảnh báo rằng thao tác git remote sẽ bị chặn network
  And gợi ý chuyển sang mode full-access nếu cần push/pull
```

### FR-006 — Feature flag và không hồi quy hành vi Claude
- **Mục tiêu:** Tính năng bật/tắt được; mặc định không thay đổi hành vi hiện tại của cả Claude lẫn Codex.
- **Actor:** Người dùng + System.
- **Trigger:** Cấu hình settings.
- **Preconditions:** —
- **Main flow:**
  1. Có cờ bật/tắt "Broker cho Codex" (mặc định tắt).
  2. Khi tắt: nhánh Codex giữ nguyên `broker_run: None`.
  3. Khi bật: áp dụng FR-001..FR-005.
- **Alternative flows:** —
- **Error/exception flows:** —
- **Postconditions:** Claude runs không bị ảnh hưởng ở mọi trạng thái cờ.
- **Business rules liên quan:** BR-006
- **Permission liên quan:** —
- **Nguồn:** derived (CON-05)

```gherkin
Scenario: Mặc định không đổi hành vi
  Given cờ "Broker cho Codex" đang tắt
  When chạy một Codex run
  Then run hoạt động y như hiện tại (broker_run = None)

Scenario: Claude không hồi quy
  Given cờ ở bất kỳ trạng thái nào
  When chạy một Claude run
  Then hành vi broker/git của Claude không thay đổi
```

---

## 8. Business rules
| ID | Rule | Phạm vi áp dụng | Exception | Nguồn |
|---|---|---|---|---|
| BR-001 | Broker định danh và route theo `run_id`; thiếu `run_id` → fail-closed deny mọi "Ask" | Mọi run có broker | — | SRC-002 |
| BR-002 | Credential không được lộ qua env; chỉ truyền qua socket cho tiến trình con | Codex + Claude | — | SRC-001 |
| BR-003 | Git chỉ dùng `git-credential-devdy`; helper hệ thống bị reset (fail-closed) | Run có broker | — | SRC-001 |
| BR-004 | Codex ưu tiên credential broker theo project, không dùng credential toàn cục máy | Codex có broker | Khi tính năng tắt → dùng hành vi cũ | SRC-002 |
| BR-005 | Trên macOS, git remote chỉ khả dụng khi sandbox = danger-full-access | Codex/macOS | Khi Codex vá #10390 | CON-01 |
| BR-006 | Mặc định tính năng tắt; hành vi Claude bất biến | Toàn hệ thống | — | CON-05 |

---

## 9. Data requirements
| ID | Entity/Field | Type/Format | Required | Validation | Source of truth | Retention/Notes |
|---|---|---|---|---|---|---|
| DATA-001 | `DEVDY_BROKER_SOCK` | Đường dẫn Unix socket | Có | Tồn tại, quyền 0600 | Broker (per-run) | Xóa khi run kết thúc |
| DATA-002 | `DEVDY_RUN_ID` | String | Có | Khớp run trong registry | Devdy backend | — |
| DATA-003 | `DEVDY_PROJECT_ID` | String | Có | Khớp project | Devdy backend | — |
| DATA-004 | `GIT_CONFIG_*` | Env theo chuẩn git | Có | COUNT khớp số cặp KEY/VALUE | Devdy backend | Chỉ trong phạm vi run |
| DATA-005 | Token account (git) | Secret | Có | Do broker resolve | DB Devdy | Không ghi ra log/transcript |
| DATA-006 | Cấu hình engine/mode/flag | Settings | Có | Enum hợp lệ | AppSettings | — |

---

## 10. UI/UX requirements

### 10.1 Màn hình
- Settings: thêm toggle "Cho phép Codex dùng Credential Broker (thử nghiệm)".
- RunView: khi chọn engine Codex + tính năng bật, hiển thị trạng thái broker và mode sandbox đang dùng.

### 10.2 Trạng thái hiển thị
- Badge/nhãn: "Broker: bật/tắt", "Sandbox: workspace-write / full-access".

### 10.3 Empty/loading/error state
- Error: hiển thị lý do fail-closed (thiếu env, socket bị chặn, network bị chặn) bằng thông điệp rõ ràng.

### 10.4 Responsive/device/accessibility
- Tuân theo hệ design hiện có của Devdy (dark-first, UI primitives).

### 10.5 Nội dung thông báo
- Cảnh báo network (FR-005) bằng tiếng Việt, ngắn gọn, kèm hành động khắc phục.

---

## 11. Integration requirements
| ID | Hệ thống | Direction | Trigger | Payload chính | Auth | Timeout/Retry | Idempotency | Error handling |
|---|---|---|---|---|---|---|---|---|
| INT-001 | Broker (Unix socket) | git/gh → broker | git cần credential | request kèm `run_id`, host | Socket cục bộ (0600) | Theo helper | Không cần | Deny → fail-closed |
| INT-002 | Codex CLI `app-server` | Devdy → Codex | Spawn run | env policy, sandbox, mcp, model | Subscription Codex | — | — | Lỗi spawn → run fail |
| INT-003 | Git remote (GitHub/GitLab...) | Codex → remote | git remote op | HTTPS git protocol | Token do broker cấp | Theo git | Theo git | Network chặn → báo lỗi |

---

## 12. Security và permission
| ID | Yêu cầu | Actor/Scope | Cách kiểm soát | Audit |
|---|---|---|---|---|
| SEC-001 | Broker chỉ cấp token theo policy per-project | Broker | Policy engine + `run_id` | Ghi log cấp/deny |
| SEC-002 | "Ask" route đúng modal của run | Broker/UI | `ApproverResolver.resolve(run_id)` | Log quyết định approve/deny |
| SEC-003 | Không lộ token qua env/log/transcript | System | Truyền qua socket; env chỉ chứa `SOCK`/`RUN_ID` | Kiểm tra log không chứa token |
| SEC-004 | Cảnh báo rõ khi bật full-access (mất sandbox OS của Codex) | Người dùng | UI xác nhận + cảnh báo | Ghi nhận lựa chọn mode |
| SEC-005 | Fail-closed khi thiếu điều kiện (env/socket/policy) | System | Không fallback credential máy | Log lý do fail |

> Lưu ý bảo mật trọng yếu: bật `danger-full-access` để có network đồng nghĩa **tắt sandbox OS của Codex**. Khi đó lớp bảo vệ còn lại là approval policy + policy broker (tương tự cách Claude vận hành vốn không có seatbelt). Cần nêu rõ đánh đổi này cho người dùng.

---

## 13. Non-functional requirements

### 13.1 Performance
- NFR-001: Wiring broker cho Codex không làm tăng thời gian khởi động run quá mức đáng kể so với Claude (mục tiêu chênh lệch < 300ms cho bước wiring).

### 13.2 Availability và recovery
- NFR-002: Nếu broker socket lỗi giữa chừng, git thao tác kế tiếp fail-closed và log rõ, không treo vô hạn (`GIT_TERMINAL_PROMPT=0`).

### 13.3 Scalability
- NFR-003: Hỗ trợ nhiều Codex run song song, mỗi run một socket/`run_id` riêng (không rò rỉ chéo run).

### 13.4 Security/privacy/compliance
- NFR-004: Token không xuất hiện trong stream/transcript/log; xem SEC-003.

### 13.5 Logging/monitoring/audit
- NFR-005: Ghi audit sự kiện cấp/deny credential, quyết định approval, và cảnh báo network.

### 13.6 Compatibility
- NFR-006: Tương thích Codex CLI ≥ 0.140.0; nếu API config khác phiên bản, có cơ chế phát hiện và cảnh báo.

### 13.7 Localization/timezone/currency
- NFR-007: Thông báo người dùng bằng tiếng Việt; không liên quan tiền tệ/timezone.

---

## 14. Báo cáo và vận hành
- Log chẩn đoán per-run: trạng thái wiring, env policy áp dụng, sandbox mode, kết quả kết nối socket.
- Manual operation: người dùng có thể chuyển mode sandbox cho run cần git remote.
- Alert: cảnh báo network-blocked (FR-005).
- Data correction: không áp dụng (không lưu trạng thái nghiệp vụ ngoài run).

---

## 15. Migration và rollout
- Existing data: không có migration dữ liệu.
- Backward compatibility: cờ mặc định tắt → không đổi hành vi (FR-006).
- Feature flag: "Broker cho Codex (thử nghiệm)".
- Rollback: tắt cờ → Codex trở lại `broker_run: None`.
- Release phase: (1) sau flag, macOS full-access; (2) kiểm chứng Linux workspace-write+network; (3) bật mặc định khi ổn định.

---

## 16. Open issues
| ID | Nội dung | Owner | Deadline | Ảnh hưởng |
|---|---|---|---|---|
| OI-01 | Xác nhận cách Devdy truyền cấu hình (env policy/sandbox/unix socket) vào `app-server`: qua `-c` lúc spawn hay `CODEX_HOME`/`config.toml` riêng | Dev | Trước khi implement | Quyết định cách wiring FR-002/003/004 |
| OI-02 | Kiểm chứng Linux: `workspace-write` + `network_access=true` có cho git remote không (AF_UNIX được seccomp miễn trừ) | Dev/QA | Rollout phase 2 | Có thể giữ sandbox an toàn hơn trên Linux |
| OI-03 | Hỗ trợ SSH-based git remote cho Codex (ngoài HTTPS) | BA/Dev | Sau MVP | Mở rộng phạm vi |
| OI-04 | Hành vi trên Windows (sandbox/credential) | Dev | Sau MVP | Phạm vi nền tảng |
| OI-05 | Theo dõi bản vá upstream #10390 để bỏ ràng buộc full-access trên macOS | Dev | Liên tục | Cải thiện bảo mật |
| OI-06 | Chốt UX xác nhận khi bật full-access (mức cảnh báo, có cần double-confirm không) | BA/UX | Trước rollout | Bảo mật/UX |

---

## 17. Traceability matrix
| Source ID | Requirement ID | Loại | Trạng thái | Ghi chú |
|---|---|---|---|---|
| SRC-001 | FR-001, FR-002, FR-003, BR-002, BR-003, SEC-001, SEC-003 | Broker OS-level | Đã xác minh code | `runs/broker/`, `sidecar-proxy/` |
| SRC-002 | FR-001, BR-001, BR-004, SEC-002 | Wiring per-run + Ask routing | Đã xác minh code | `wire_broker()`, `ApproverResolver` |
| SRC-003 | FR-004, BR-005 | Ràng buộc network macOS | Đã kiểm chứng thực nghiệm + #10390 | Chỉ full-access có network |
| SRC-004 (yêu cầu người dùng) | FR-006 | Ngang bằng Claude, không hồi quy | Chốt scope | "muốn Codex giống Claude" |
| derived | FR-002 (CON-03), FR-003 (CON-04), FR-005 (CON-01) | Ràng buộc kỹ thuật | Suy ra hợp lý | Từ config reference + thực nghiệm |

---

## 18. Phụ lục

### 18.1 Kết quả thực nghiệm (macOS 26.4 arm64, Codex CLI 0.140.0)
| Thí nghiệm | Kết quả |
|---|---|
| Baseline không sandbox: unix socket + TCP + DNS | OK cả 3 |
| `codex sandbox` mặc định | Chặn socket, TCP, DNS, ghi file |
| `codex sandbox --allow-unix-socket <path>` | Socket OK; TCP/DNS vẫn chặn |
| `-c ...network_access=true` (seatbelt) | Network vẫn chặn (đúng bug #10390) |
| env `DEVDY_BROKER_SOCK`/`GIT_CONFIG_*` qua wrapper | Giữ nguyên (nhưng runtime agent dùng `shell_environment_policy=core` sẽ lọc → cần FR-002) |

### 18.2 Điểm sửa code dự kiến (tham khảo, không phải cam kết thiết kế)
- `src-tauri/src/commands/runs.rs`: nhánh Codex `start_run()` gọi `wire_broker()` + `broker_run: Some(...)`; `resume_run()` tương tự.
- Cấu hình Codex lúc spawn: `shell_environment_policy` (giữ biến broker), sandbox mode, cho phép unix socket.
- `sidecar-codex/`: truyền cấu hình xuống `app-server` (theo kết luận OI-01).
- Settings/UI: feature flag + cảnh báo network.

### 18.3 Decision log
- Chọn `danger-full-access` làm điều kiện có network trên macOS vì bug #10390 chưa có bản vá (đã kiểm chứng). Đánh đổi: mất sandbox OS của Codex; bù lại bằng approval policy + policy broker.
