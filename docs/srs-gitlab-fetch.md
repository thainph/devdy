# SRS — Bổ sung hỗ trợ GitLab cho chức năng Fetch Issue/MR (DevDy)

> Phiên bản: 1.0 (final) — mọi quyết định sản phẩm/kỹ thuật đã đóng băng.
> Nguồn code đối chiếu: `src-tauri/src/commands/github.rs`, `commands/gitlab_accounts.rs`,
> `github/mod.rs`, `runs/broker/token.rs`, `migrations/0002_repos.sql`, `migrations/0014_gitlab_accounts.sql`.

## 1. Giới thiệu

### 1.1 Mục tiêu
Mở rộng chức năng "fetch issue/PR" của DevDy — hiện chỉ chạy với GitHub — để hỗ trợ **GitLab**
(GitLab.com và GitLab self-hosted). Sau khi hoàn thành, người dùng có thể fetch **GitLab Issue**
và **GitLab Merge Request (MR)** về `.devdy/tasks/...` dưới dạng markdown, tạo run và dùng lại
toàn bộ luồng AI/refetch như GitHub.

### 1.2 Phạm vi
- **Trong phạm vi:** Fetch cả Issue và MR từ GitLab; tái dùng run type `analyze_issue`/`review_pr`;
  MR bắt buộc có linked issue; định danh provider qua cột `provider` trên bảng `repos`;
  namespacing path task theo repo để chống trùng số.
- **Ngoài phạm vi:** Tạo/sửa/comment issue-MR; OAuth GitLab (đã có PAT account); auto-detect
  provider từ git remote; ephemeral/project access token cho GitLab (đang ở mức scaffold trong `token.rs`).

### 1.3 Hiện trạng (dữ kiện từ code)
| Thành phần | Trạng thái | Nguồn |
|---|---|---|
| Fetch GitHub issue/PR | Đã có đầy đủ | `commands/github.rs:127,411,496` |
| GitHub API client (PAT) | Đã có | `github/mod.rs:8` |
| GitLab account CRUD + validate PAT | Đã có | `commands/gitlab_accounts.rs` |
| GitLab PAT trong Keychain | Đã có | `secrets::*_gitlab_account_pat` |
| GitLab token resolver (broker) | Đã có | `runs/broker/token.rs:347` |
| `projects.gitlab_account_id` | Đã có (migration 0014) | `migrations/0014_gitlab_accounts.sql` |
| Fetch GitLab issue/MR | **CHƯA CÓ** | — |
| Cột định danh GitLab trong `repos` | **CHƯA CÓ** (chỉ `github_owner/github_repo`) | `migrations/0002_repos.sql` |

### 1.4 Thuật ngữ
| Từ | Nghĩa |
|---|---|
| MR | Merge Request (tương đương PR của GitHub) |
| IID | Internal ID của issue/MR trong 1 project GitLab (số hiển thị trên UI, khác `id` toàn cục) |
| PAT | Personal Access Token |
| Provider | Nhà cung cấp git host: `github` \| `gitlab` |
| Linked issue | Issue được MR đóng khi merge (`closes_issues`) |
| host | GitLab base URL, ví dụ `https://gitlab.com` hoặc self-hosted |
| repo_slug | Chuỗi định danh repo dùng cho tên thư mục task (BR-008) |

## 2. Actor & Permission

| Actor | Mô tả |
|---|---|
| Người dùng DevDy | Chủ máy, đã cấu hình GitLab account (PAT) và link account vào project |

- **SEC-001:** Fetch GitLab chỉ dùng PAT của account GitLab đã link vào project (`projects.gitlab_account_id`).
  Không có account/PAT → không được fetch.
- **SEC-002:** PAT chỉ lấy từ Keychain (`secrets::get_gitlab_account_pat`) tại thời điểm gọi API;
  không log, không ghi vào markdown, không đưa vào error message.
- **SEC-003:** Gọi API qua header `PRIVATE-TOKEN` (không phải `Bearer`), đúng như
  `gitlab_accounts.rs::validate_token`.

## 3. Yêu cầu chức năng

### FR-001 — Định danh provider cho repo
- **Actor:** Người dùng DevDy.
- **Trigger:** Cấu hình/link một repo.
- **Hành vi:** Mỗi repo có trường `provider` (`github` | `gitlab`). Với `gitlab`, repo lưu cả
  **path** `namespace/project` (để hiển thị) và **numeric project id** (để gọi API); host lấy từ
  account GitLab đã link.
- **Postcondition:** Từ `repo_id` suy ra được provider và đủ thông tin gọi đúng API.
- **Rule liên quan:** BR-001, BR-002.
- **Nguồn:** SRC-002.

```gherkin
Scenario: Repo GitLab được nhận diện đúng provider
  Given một repo có provider = "gitlab" và định danh project hợp lệ
  When hệ thống cần fetch issue/MR cho repo đó
  Then hệ thống chọn nhánh xử lý GitLab (không gọi GitHub API)

Scenario: Repo cũ mặc định là GitHub
  Given một repo đã tồn tại trước khi thêm cột provider
  When đọc provider của repo
  Then provider = "github" (default, không phá vỡ luồng GitHub hiện tại)
```

### FR-002 — Fetch GitLab Issue
- **Actor:** Người dùng DevDy.
- **Trigger:** Yêu cầu fetch một Issue với `issue_iid` từ repo GitLab.
- **Precondition:** Repo `provider = gitlab`, định danh project hợp lệ; project đã link GitLab account có PAT.
- **Main flow:**
  1. Resolve host + PAT từ GitLab account đã link.
  2. `GET {host}/api/v4/projects/:id/issues/:iid` — lấy title, author, created, labels, description.
  3. `GET .../issues/:iid/notes` — lấy comment; **bỏ qua** system notes (`system == true`) và bot (BR-003).
  4. Render markdown theo cấu trúc frontmatter GitHub (BR-004); ghi `.devdy/tasks/<repo_slug>/issue-<iid>/issue.md`.
  5. Tạo run type `analyze_issue`, status `fetched`, `ref_number = iid`, engine = default,
     `input_path = output_path` = file vừa ghi.
- **Postcondition:** Có 1 run `analyze_issue`; file markdown tồn tại.
- **Exception:** BR-005.
- **Rule liên quan:** BR-003, BR-004, BR-005; DATA-001, DATA-002; INT-001.
- **Nguồn:** SRC-001; đối chiếu `github.rs::fetch_issue`.

```gherkin
Scenario: Fetch issue GitLab thành công
  Given repo GitLab đã link account có PAT hợp lệ
  When người dùng fetch issue có IID = 42
  Then file ".devdy/tasks/<repo_slug>/issue-42/issue.md" được tạo với frontmatter chứa issue: 42, title, author, created, labels
  And các comment của người thật được nối vào, comment hệ thống/bot bị loại
  And một run type "analyze_issue", status "fetched", ref_number = 42 được tạo

Scenario: Chưa link account GitLab
  Given repo GitLab chưa link account nào (hoặc account không có PAT)
  When người dùng fetch issue
  Then hệ thống trả lỗi rõ ràng "chưa cấu hình GitLab account cho project"
  And không tạo run, không ghi file
```

### FR-003 — Fetch GitLab Merge Request
- **Actor:** Người dùng DevDy.
- **Trigger:** Yêu cầu fetch một MR với `mr_iid` (kèm `linked_issue` tùy chọn).
- **Precondition:** như FR-002.
- **Main flow:**
  1. Resolve host + PAT.
  2. `GET .../merge_requests/:iid` — title, author, source_branch, target_branch, created, description.
  3. **Resolve linked issue:** ưu tiên tham số `linked_issue`; nếu không có →
     `GET .../merge_requests/:iid/closes_issues`, lấy IID nhỏ nhất; vẫn không có → lỗi `NO_LINKED_ISSUE` (BR-006).
  4. `GET .../merge_requests/:iid/changes` — danh sách file + diff; render `## Files Changed` và `## Diffs`.
  5. `GET .../merge_requests/:iid/notes` — comment + inline diff notes; lọc bot/system (BR-003);
     render `## Comments` và `## Inline Review Comments`.
  6. `GET .../merge_requests/:iid/approvals` — render `## Reviews` liệt kê người đã approve (BR-010).
  7. Ghi `.devdy/tasks/<repo_slug>/issue-<linked_issue>/mr-<mr_iid>.md`; tạo run type `review_pr`,
     `ref_number = mr_iid`.
- **Postcondition:** Có run `review_pr`; file markdown chứa metadata + diff + comment + approvals.
- **Rule liên quan:** BR-003, BR-004, BR-005, BR-006, BR-010; DATA-001, DATA-002; INT-002.
- **Nguồn:** SRC-001; đối chiếu `github.rs::fetch_pr` + `build_pr_markdown`.

```gherkin
Scenario: Fetch MR có linked issue
  Given repo GitLab đã link account có PAT hợp lệ
  And MR IID = 10 có closes_issues = [7]
  When người dùng fetch MR 10 không truyền linked_issue
  Then file ".devdy/tasks/<repo_slug>/issue-7/mr-10.md" được tạo
  And frontmatter chứa pr: 10, linked_issue: 7, base (target branch), head (source branch)
  And phần Files Changed và Diffs được render từ MR changes
  And phần Reviews liệt kê người đã approve (nếu có)
  And một run type "review_pr", ref_number = 10 được tạo

Scenario: MR không có linked issue và không truyền tham số
  Given MR IID = 11 không có closes_issues
  When người dùng fetch MR 11 không truyền linked_issue
  Then hệ thống trả lỗi "NO_LINKED_ISSUE"
  And không tạo run, không ghi file

Scenario: MR truyền linked_issue thủ công
  Given MR IID = 11 không có closes_issues
  When người dùng fetch MR 11 với linked_issue = 7
  Then file ".devdy/tasks/<repo_slug>/issue-7/mr-11.md" được tạo bình thường
```

### FR-004 — Refetch run GitLab (làm mới nội dung)
- **Actor:** Người dùng DevDy.
- **Trigger:** Bấm refetch trên một run GitLab đã có.
- **Hành vi:** Từ `run_id` đọc `repo_id` → suy ra provider. Nếu `gitlab`, chạy lại luồng fetch tương ứng
  (`analyze_issue` → FR-002, `review_pr` → FR-003) và **ghi đè** file `input_path` tại chỗ. Run record,
  AI output, session **giữ nguyên**. Với MR, đọc lại `linked_issue` từ frontmatter file cũ để giữ nhất
  quán (không ném `NO_LINKED_ISSUE`).
- **Postcondition:** File markdown cập nhật; run record không đổi.
- **Rule liên quan:** BR-007.
- **Nguồn:** SRC-001; đối chiếu `github.rs::refetch_run`.

```gherkin
Scenario: Refetch một run issue GitLab
  Given một run "analyze_issue" thuộc repo provider = "gitlab"
  When người dùng refetch run đó
  Then nội dung file input được ghi đè bằng dữ liệu mới nhất từ GitLab
  And run record, output AI và session không thay đổi

Scenario: Refetch MR giữ nguyên linked issue
  Given một run "review_pr" GitLab với frontmatter linked_issue = 7
  When người dùng refetch run đó
  Then hệ thống dùng lại linked_issue = 7, không gọi lại closes_issues
  And không phát sinh lỗi NO_LINKED_ISSUE
```

### FR-005 — Chọn nhánh provider ở tầng command
- **Actor:** Hệ thống.
- **Hành vi:** `fetch_issue`, `fetch_pr`, `refetch_run` đọc `provider` của repo và điều hướng sang nhánh
  GitHub (giữ nguyên) hoặc GitLab (mới). **Run type không đổi** (`analyze_issue`/`review_pr`) — UI/History
  suy ra nhãn như hiện tại.
- **Rule liên quan:** BR-001.
- **Nguồn:** SRC-003.

```gherkin
Scenario: Cùng một command phục vụ 2 provider
  Given hai repo: một provider "github", một "gitlab"
  When gọi fetch_issue cho từng repo
  Then repo github đi nhánh Octocrab, repo gitlab đi nhánh GitLab REST
  And cả hai đều tạo run type "analyze_issue" với cùng cấu trúc RunRecord
```

### FR-006 — Namespacing path task theo repo
- **Actor:** Hệ thống.
- **Trigger:** Bất kỳ lần fetch issue/MR (FR-002, FR-003).
- **Hành vi:** File task được ghi dưới thư mục con định danh theo repo:
  - Issue: `<project>/.devdy/tasks/<repo_slug>/issue-<iid>/issue.md`
  - MR: `<project>/.devdy/tasks/<repo_slug>/issue-<linked>/mr-<n>.md`
- **Postcondition:** Hai repo khác nhau (kể cả khác provider) trùng số issue/MR không ghi đè lên nhau.
- **Rule liên quan:** BR-008, BR-009.
- **Nguồn:** SRC-006.

```gherkin
Scenario: Hai repo cùng project trùng số issue không đè nhau
  Given project P có repo A (github) và repo B (gitlab), cả hai có issue #5
  When người dùng fetch issue #5 của A rồi #5 của B
  Then A ghi vào ".devdy/tasks/github-<ownerA>-<repoA>-<idA>/issue-5/issue.md"
  And B ghi vào ".devdy/tasks/gitlab-<nsB>-<projB>-<idB>/issue-5/issue.md"
  And hai file tồn tại độc lập, không file nào bị ghi đè
```

## 4. Business Rules

| ID | Rule |
|---|---|
| BR-001 | Provider xác định bởi `repos.provider`; mặc định `github` cho dữ liệu cũ. Không auto-detect từ git remote. |
| BR-002 | Host GitLab lấy từ `gitlab_accounts.host` của account đã link (chuẩn hóa bỏ `/` cuối); trống → `https://gitlab.com`. API base = `{host}/api/v4`. (Đồng bộ `gitlab_accounts.rs::normalize_host`.) |
| BR-003 | Loại khỏi markdown: (a) GitLab **system notes** (`system == true`); (b) user là bot — tái dùng danh sách bot & quy tắc `[bot]`/tên phổ biến trong `github.rs::is_bot_user`. |
| BR-004 | Định dạng markdown GitLab **giống hệt** GitHub để tương thích UI/parse: issue dùng key `issue/title/author/created/labels`; MR dùng key `pr/linked_issue/title/author/base/head/created`; các section `## Files Changed`, `## Diffs`, `## Comments`, `## Reviews`, `## Inline Review Comments`. |
| BR-005 | Mapping lỗi API: 401 → thông báo kiểm tra token/scope/host (tái dùng message trong `gitlab_accounts.rs`); 404 → "Không tìm thấy issue/MR"; lỗi mạng → message ngắn gọn, không lộ PAT. |
| BR-006 | MR **bắt buộc** có linked issue: ưu tiên tham số truyền vào, kế đến `closes_issues` (lấy IID nhỏ nhất); không có → lỗi `NO_LINKED_ISSUE`. |
| BR-007 | Refetch chỉ áp dụng cho run type `analyze_issue`/`review_pr`; type khác → lỗi "Cannot re-fetch". Provider suy ra từ repo của run. |
| BR-008 | `repo_slug` = `<provider>-<owner_hoặc_namespace>-<repo>-<repo_id[:6]>`. Chuẩn hóa: lowercase; thay mọi ký tự không phải `[a-z0-9]` (gồm `/` của namespace lồng, `.`, khoảng trắng) thành `-`; gộp nhiều `-` liên tiếp thành một. Hậu tố `repo_id` 6 ký tự đầu để chống trùng slug tuyệt đối. |
| BR-009 | **Không migrate** file/đường dẫn cũ. Run cũ đọc theo `input_path`/`output_path` đã lưu trong DB → vẫn hoạt động. Nhánh fallback dựng path trong `runs.rs:943-956, 1014-1027` **giữ nguyên scheme cũ** (`issue-<n>/issue.md`) vì chỉ dùng cho run legacy có `input_path = NULL`; fetch mới luôn set `input_path`. |
| BR-010 | Render `## Reviews` từ GitLab MR **approvals** (`.../approvals`), liệt kê người đã approve. Ánh xạ về cùng section `## Reviews` mà UI GitHub đang dùng (BR-004). |

## 5. Yêu cầu dữ liệu

| ID | Yêu cầu |
|---|---|
| DATA-001 | Bảng `repos` thêm: `provider TEXT NOT NULL DEFAULT 'github'`; `gitlab_project_path TEXT` (`namespace/project`); `gitlab_project_id INTEGER` (numeric id để gọi API). Giữ nguyên `github_owner/github_repo`. Migration mới (vd 0017), backfill `provider='github'` cho bản ghi cũ. |
| DATA-002 | File output: issue → `.devdy/tasks/<repo_slug>/issue-<iid>/issue.md`; MR → `.devdy/tasks/<repo_slug>/issue-<linked>/mr-<n>.md`. `input_path`/`output_path` trong `runs` lưu đường dẫn đầy đủ mới. |
| DATA-003 | `runs.ref_number` lưu **IID** (không phải id toàn cục). `runs.repo_id` phải trỏ về repo GitLab để refetch suy được provider. |

## 6. Tích hợp (GitLab REST API v4)

| ID | Endpoint | Dùng cho |
|---|---|---|
| INT-001 | `GET /projects/:id/issues/:iid` + `.../notes` | FR-002 |
| INT-002 | `GET /projects/:id/merge_requests/:iid` + `/changes` + `/notes` + `/closes_issues` + `/approvals` | FR-003 |
| INT-003 | Auth: header `PRIVATE-TOKEN: <pat>`, `User-Agent: devdy/0.1` | Tất cả |
| INT-004 | `:id` = **numeric project id** (`gitlab_project_id`) — ưu tiên; hoặc `namespace/project` đã URL-encode | Tất cả |
| INT-005 | Phân trang notes/changes: đọc hết trang (header `X-Next-Page`/`link`), tương đương `all_pages` bên GitHub | FR-002, FR-003 |

## 7. Yêu cầu phi chức năng

| ID | Yêu cầu |
|---|---|
| NFR-001 | Không phá vỡ luồng GitHub hiện tại: mọi repo cũ (`provider` mặc định `github`) hành xử y như trước. |
| NFR-002 | Bảo mật: tuân thủ SEC-001..003; PAT không xuất hiện ở log/markdown/error. |
| NFR-003 | Hỗ trợ GitLab self-hosted qua `host` cấu hình ở account (không hardcode gitlab.com). |
| NFR-004 | Thông báo lỗi bằng tiếng Việt, nêu được nguyên nhân hành động. |

## 8. Traceability

| Source ID | Nội dung nguồn | Requirement | Loại |
|---|---|---|---|
| SRC-001 | Yêu cầu gốc: bổ sung GitLab cho fetch issue/PR | FR-002, FR-003, FR-004 | Chức năng |
| SRC-002 | Q2: cột provider tổng quát | FR-001, DATA-001, BR-001 | Dữ liệu |
| SRC-003 | Q3: tái dùng run type | FR-005, BR-007 | Kỹ thuật |
| SRC-004 | Q4: MR bắt buộc linked issue | BR-006, FR-003 | Nghiệp vụ |
| SRC-005 | Q1: scope cả Issue + MR | FR-002, FR-003 | Phạm vi |
| SRC-006 | Rủi ro trùng số issue/MR giữa nhiều repo | FR-006, BR-008, DATA-002 | Chức năng |
| SRC-007 | Chọn giữ nguyên file cũ | BR-009 | Kỹ thuật |
| SRC-008 | OQ-001: lưu cả path + numeric id | DATA-001, INT-004 | Dữ liệu |
| SRC-009 | OQ-002: render approvals | BR-010, INT-002 | Sản phẩm |
| SRC-010 | OQ-003: tên file mr-<n>.md | DATA-002, FR-003 | Sản phẩm |
| SRC-011 | OQ-004: nối hậu tố repo_id vào slug | BR-008 | Kỹ thuật |

## 9. Assumptions

- `A-01` — Tái dùng `is_bot_user` của GitHub cho GitLab là chấp nhận được; bổ sung lọc `system == true` cho notes.
- `A-02` — Diff MR lấy qua `/changes` là đủ (không cần raw diff riêng).
- `A-03` — Mọi open issue (OQ-001..004) đã được chốt; không còn điểm chặn để implement.

## 10. Phạm vi sửa code (định hướng implement)

| File | Thay đổi |
|---|---|
| `migrations/00xx_repos_provider.sql` | Thêm `provider`, `gitlab_project_path`, `gitlab_project_id`; backfill `provider='github'` |
| `commands/github.rs` (fetch_issue, fetch_pr, refetch_run) | Rẽ nhánh theo `provider`; chèn `repo_slug` vào path |
| `gitlab/mod.rs` (mới) | GitLab REST client (PRIVATE-TOKEN), build_issue_markdown/build_mr_markdown, detect_linked_issue |
| Hàm `repo_slug()` (mới) | Sinh slug theo BR-008 |
| `runs.rs:943-956, 1014-1027` | **Không đổi** (giữ fallback legacy — BR-009) |
| Frontend (store/UI) | Cho phép nhập/chọn repo GitLab và số issue/MR (nếu cần) |
