# Plan: Quản lý MCP tập trung trên Devdy

## Context

Hiện tại Devdy **không** quản lý MCP server nào. Cả Claude (qua `sidecar/index.mjs` → Agent SDK `query()`) lẫn Codex (qua `sidecar-codex/index.mjs` → `codex app-server`) đều chạy không có MCP; Codex thậm chí auto-decline mọi MCP elicitation.

Mục tiêu: thêm một nơi tập trung trong Devdy để **định nghĩa** các MCP server (giống Skills/Rules), rồi **bật theo từng project**. Khi một run khởi chạy, các MCP server đã bật cho project đó được bơm vào engine:
- **Claude**: inject `options.mcpServers` vào Agent SDK.
- **Codex**: inject qua `-c mcp_servers.*` config overrides khi spawn `codex app-server`.

Hỗ trợ 2 transport: **stdio** (command/args/env) và **HTTP/SSE** (url/headers). MCP là config bơm lúc chạy — **không** cần sync file ra đĩa như skills/rules.

## Quyết định đã chốt (nguồn: BA review)
- **QĐ-1 (Codex + remote):** Codex nhận server **stdio** và **streamable HTTP**. Server **sse** bị **bỏ qua** khi run bằng Codex, và ghi **1 dòng note vào log run** để user biết (server nào bị bỏ, lý do "Codex chỉ hỗ trợ stdio/streamable HTTP").
- **QĐ-2 (Engine resolve):** Việc inject quyết định theo **engine thực tế lúc run** (đã tính `engine_override` per-run), không theo `default_engine`. Badge "Claude-only" trên UI **chỉ mang tính cảnh báo** dựa trên `default_engine` của project.
- **QĐ-3 (Secret):** Giá trị nhạy cảm trong `env` và `headers` **bắt buộc lưu macOS Keychain ngay từ v1** (giống PAT). SQLite **không** chứa `env`/`headers` plaintext.
- **QĐ-4 (Scope v1):** Ngoài CRUD + toggle + gán per-project, v1 gồm cả **import/export** cấu hình và **test-connection**.

## Điểm inject đã xác định
- Claude options build: `src-tauri/src/commands/runs.rs:288-301` → sidecar áp options tại `sidecar/index.mjs:193-218`.
- Codex spawn: `sidecar-codex/index.mjs:61` (`spawn(CODEX_BIN, ['app-server'], ...)`); env set tại `runs.rs:352-362`.
- Log note dùng cơ chế synthetic message ghi vào `log_buf` như "initial user message" hiện có (`runs.rs:306-315`).

## Data model (SQLite + Keychain)

Migration mới `src-tauri/migrations/0016_mcp_servers.sql`:

```sql
CREATE TABLE IF NOT EXISTS mcp_servers (
    id          TEXT PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,      -- MCP key hợp lệ (a-z0-9_-)
    description TEXT,
    transport   TEXT NOT NULL,             -- 'stdio' | 'http' | 'sse'
    command     TEXT,                      -- stdio (không nhạy cảm)
    args        TEXT,                      -- JSON array (stdio)
    url         TEXT,                      -- http/sse
    env_keys    TEXT,                      -- JSON array tên biến env (chỉ KEY, để hiển thị/edit; VALUE ở Keychain)
    header_keys TEXT,                      -- JSON array tên header (chỉ KEY; VALUE ở Keychain)
    enabled     INTEGER NOT NULL DEFAULT 1,-- công tắc tổng
    created_at  TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS project_mcp_servers (
    project_id TEXT NOT NULL,
    server_id  TEXT NOT NULL,
    enabled_at TEXT NOT NULL,
    PRIMARY KEY (project_id, server_id),
    FOREIGN KEY (project_id) REFERENCES projects(id) ON DELETE CASCADE,
    FOREIGN KEY (server_id)  REFERENCES mcp_servers(id) ON DELETE CASCADE
);
```

- **Keychain**: mỗi server lưu 1 entry (service `devdy-mcp`, account = `server_id`) chứa JSON `{"env":{KEY:VALUE...},"headers":{KEY:VALUE...}}`. SQLite chỉ giữ **tên** KEY (`env_keys`/`header_keys`) để render form, không giữ VALUE.
- Join `project_mcp_servers` bám pattern `project_skills` (`migrations/0001_init.sql:25`) / `project_rules` (`migrations/0005_rules.sql:12`), bỏ `synced_hash_*`/`target`.

## Secrets — `src-tauri/src/secrets.rs` (mở rộng)
Mirror API PAT hiện có (dùng ở `runs/broker/token.rs`):
- `set_mcp_secrets(server_id, env_map, headers_map)` → ghi Keychain.
- `get_mcp_secrets(server_id) -> {env, headers}` → đọc Keychain (fail-closed: trả rỗng nếu không có).
- `delete_mcp_secrets(server_id)` → xóa khi xóa server.
Không bao giờ log VALUE; không đưa VALUE ra stream ngoài đúng lúc dựng config cho engine.

## Backend (Rust) — `src-tauri/src/commands/mcp.rs` (file mới)
Theo khuôn `commands/skills.rs`:
- `list_mcp_servers`, `get_mcp_server` (kèm env/header keys; **không** trả VALUE), `create_mcp_server`, `update_mcp_server`, `delete_mcp_server` (xóa cả Keychain + rows join qua CASCADE).
- `list_project_mcp_servers(project_id)` → danh sách server + cờ đã-bật-cho-project.
- `set_project_mcp_servers(project_id, server_ids)` → ghi đè tập bật cho project.
- `test_mcp_connection(server_id)` → thử **MCP initialize handshake** với timeout ngắn: stdio thì spawn command+args+env rồi initialize; http/sse thì gọi URL kèm headers; trả `{ok, message}`. Kill/đóng ngay sau khi có kết quả.
- `export_mcp_server(server_id, path)` / `import_mcp_server(path)` → JSON. **Export có kèm secret** (hành động chủ động của user tới file tự chọn) và hiển thị **cảnh báo file chứa secret**; import đọc file, tạo server + ghi secret vào Keychain.
- **Helper dùng chung** `resolve_project_mcp_servers(db, project_id, engine) -> (serde_json::Value, Vec<String> skipped)`:
  - Lấy server `enabled=1` AND thuộc `project_mcp_servers`; nạp VALUE env/headers từ Keychain.
  - **Claude**: build map `{ name: {type:'stdio', command, args, env} | {type:'http'|'sse', url, headers} }`.
  - **Codex**: lấy stdio + http; các server sse cho vào `skipped` (phục vụ QĐ-1).

Đăng ký `mod mcp;` + `use` + entries vào `tauri::generate_handler![...]` trong `src-tauri/src/lib.rs` (block quanh `lib.rs:9,78-84`).

Validate create/update: `name` khớp `^[a-zA-Z0-9_-]+$` + unique; stdio bắt buộc `command`; http/sse bắt buộc `url`; keys env/headers không rỗng/không trùng.

## Bơm vào run — `src-tauri/src/commands/runs.rs`
- Nhánh Claude (sau `runs.rs:288`): 
  ```rust
  let (mcp, _skipped) = resolve_project_mcp_servers(&db_pool, &project_id, "claude").await;
  if !mcp.is_null() { options["mcpServers"] = mcp; }
  ```
  MCP tool đi qua `canUseTool` sẵn có (`sidecar/index.mjs:193`) → permission modal chạy nguyên vẹn, **không** đụng `allowedTools`.
- Nhánh Codex (quanh `runs.rs:352-362`): `resolve_project_mcp_servers(.., "codex")`, set env `DEVDY_CODEX_MCP` = JSON server stdio/http. Nếu `skipped` không rỗng → ghi 1 dòng note vào `log_buf` (QĐ-1), ví dụ: *"Bỏ qua N MCP server SSE vì Codex chỉ hỗ trợ stdio/streamable HTTP: <tên...>"*.

## Sidecar
- `sidecar/index.mjs` (quanh dòng 202): thêm `if (opts.mcpServers) options.mcpServers = opts.mcpServers`.
- `sidecar-codex/index.mjs` (quanh spawn dòng 61): đọc `process.env.DEVDY_CODEX_MCP`, với mỗi server stdio/http dựng args `-c` **đặt trước** `'app-server'`: `mcp_servers.<name>.command="..."`, `-c 'mcp_servers.<name>.args=[...]'`, `-c 'mcp_servers.<name>.env={...}'`, hoặc `mcp_servers.<name>.url="..."` + `env_http_headers`. Cần **escape TOML** cẩn thận (xem Risks).

## Frontend (Vue)
- Store `src/stores/mcpServers.ts` (khuôn `stores/skills.ts`): `items/loading/error`, CRUD, `listForProject`, `setForProject`, `testConnection`, `exportServer`, `importServer`.
- List `src/views/McpServersView.vue` (mirror `views/SkillsView.vue`): header (title + count + **Import** + **New**), grid card. Card: tên (mono), `Badge` transport, badge cảnh báo "Claude-only" cho server SSE khi `project.default_engine==='codex'` (chỉ cảnh báo — QĐ-2), mô tả, nút edit/delete/**export**/toggle `enabled`. `useConfirm()` cho delete.
- Editor `src/views/McpServerEditorView.vue` (mirror `views/SkillEditorView.vue`): name, description, `AppSelect` transport; field theo transport (stdio: command + args list + env key-value rows; http/sse: url + headers key-value rows); nút **Test connection** hiển thị kết quả `{ok,message}`; toggle enabled. Dùng `Button/Input/AppSelect/Card`. VALUE secret nhập mới hoặc để trống = giữ nguyên (không hiển thị lại VALUE cũ đọc từ Keychain — chỉ báo "đã có").
- Router `src/router/index.ts`: `/mcp`, `/mcp/new`, `/mcp/:id/edit`.
- Nav: thêm "MCP" vào sidebar `src/App.vue` (sau Rules).
- Gán per-project: section "MCP Servers" trong `src/views/ProjectDetailView.vue` với checkbox bật/tắt (tái dùng pattern áp skills/rules per-project), gọi `set_project_mcp_servers`.

## Hành vi & ràng buộc bổ sung (nguồn: BA review)
- Sửa/bật/tắt server chỉ có hiệu lực **từ run kế tiếp** (inject lúc launch), không ảnh hưởng run đang chạy.
- Hai tầng bật/tắt: `enabled=0` toàn cục → tắt ở **mọi** project kể cả đã gán; UI cần nói rõ.
- `args` nhập mỗi phần tử một dòng; env/headers là key-value rows; trim key, bỏ dòng key rỗng.

## Risks / lưu ý
- **Escape TOML cho Codex `-c`**: `args`/`env` chứa dấu nháy, khoảng trắng, ký tự đặc biệt dễ vỡ cú pháp → cần hàm escape + test case riêng.
- **Export chứa secret**: file export có VALUE secret → phải cảnh báo rõ, khuyến nghị không commit; import ghi thẳng vào Keychain.
- **Codex SSE MCP**: không hỗ trợ ở v1 (đã theo QĐ-1); HTTP dùng streamable HTTP.
- Tên MCP hợp lệ + unique → validate cả FE lẫn BE.

## Verification (end-to-end)
1. `cargo build` OK; migration `0016` áp thành công; tạo server → Keychain có entry, SQLite không chứa VALUE secret.
2. Tạo stdio server (`npx -y @modelcontextprotocol/server-filesystem <path>`), env có 1 secret → **Test connection** trả `ok`. Bật cho 1 project.
3. Chạy session **Claude** → tool MCP xuất hiện & gọi được (permission modal đúng). Tắt server → chạy lại → tool biến mất.
4. Chạy **Codex** (stdio/http) → xác nhận `-c mcp_servers.*` được truyền (log codex/tracing).
5. Bật thêm server **sse** rồi chạy **Codex** → server SSE bị bỏ qua + **log run có dòng note** liệt kê server bị bỏ (QĐ-1). Chạy **Claude** → server SSE kết nối được.
6. **Engine override**: project `default_engine=codex` nhưng run override sang Claude → server remote vẫn được inject (QĐ-2).
7. **Error path**: server sai command/URL → Test connection trả lỗi rõ; nếu vẫn bật và chạy → run không crash, log hiện lỗi MCP từ engine.
8. **Import/export**: export 1 server ra JSON (thấy cảnh báo chứa secret) → xóa → import lại → server + secret khôi phục, Test connection `ok`.
9. CRUD FE + toggle + gán/bỏ gán project; reload app dữ liệu vẫn đúng; xóa server → Keychain entry bị xóa.
