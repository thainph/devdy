# Kế hoạch: Nhóm Skill & Rule + bật/tắt nhanh theo nhóm

## 1. Mục tiêu

- Gom skill thành **skill group**, rule thành **rule group** (hai hệ nhóm tách biệt).
- Tại `Project detail → tab Skills / tab Rules`, bật/tắt **cả nhóm** bằng một toggle thay vì bật từng item.
- Project **ghi nhớ** nhóm đang bật: sau này thêm skill mới vào nhóm thì skill đó tự lan sang mọi project đang bật nhóm.

## 2. Quyết định thiết kế (đã chốt)

| Câu hỏi | Chốt |
|---|---|
| Phạm vi nhóm | Nhóm skill riêng, nhóm rule riêng |
| Quan hệ item ↔ nhóm | N–N (một skill có thể thuộc nhiều nhóm) |
| Trạng thái | Lưu liên kết `project ↔ group`; toggle 3 trạng thái off / mixed / on |

## 3. Hiện trạng liên quan

- DB: `skills`, `project_skills(project_id, skill_id, target, synced_hash_claude, synced_hash_codex, applied_at)`; `rules`, `project_rules` cùng cấu trúc (`0005_rules.sql`, `0006_skills_codex.sql`).
- Backend: `commands/projects.rs` (`get_applied_skills`, `apply_skill`, `apply_skill_to_project`, `remove_skill_from_project`), `commands/rules.rs` (`get_applied_rules`, `apply_rule`, `apply_rule_to_project`, `remove_rule_from_project`), `commands/skills.rs::sync_skill_to_projects`.
- Frontend: `views/SkillsView.vue`, `views/RulesView.vue`, `views/ProjectDetailView.vue` (tab `skills` / `rules`, `skillItems` / `ruleItems` + toggle từng dòng), stores `stores/skills.ts`, `stores/rules.ts`, `stores/projects.ts`.
- i18n: `src/i18n/locales/{vi,en}/{skills,rules,projectDetail}.json`, kiểm tra bằng `pnpm check:i18n`.

## 4. Thiết kế dữ liệu

Migration mới `src-tauri/migrations/0034_skill_rule_groups.sql`:

```sql
CREATE TABLE IF NOT EXISTS skill_groups (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    description TEXT NOT NULL DEFAULT '',
    color TEXT,                       -- màu badge, nullable
    position INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS skill_group_members (
    group_id TEXT NOT NULL,
    skill_id TEXT NOT NULL,
    position INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (group_id, skill_id),
    FOREIGN KEY (group_id) REFERENCES skill_groups(id) ON DELETE CASCADE,
    FOREIGN KEY (skill_id) REFERENCES skills(id)       ON DELETE CASCADE
);
CREATE INDEX IF NOT EXISTS idx_skill_group_members_skill ON skill_group_members(skill_id);

CREATE TABLE IF NOT EXISTS project_skill_groups (
    project_id TEXT NOT NULL,
    group_id   TEXT NOT NULL,
    enabled_at TEXT NOT NULL,
    PRIMARY KEY (project_id, group_id),
    FOREIGN KEY (project_id) REFERENCES projects(id)     ON DELETE CASCADE,
    FOREIGN KEY (group_id)   REFERENCES skill_groups(id) ON DELETE CASCADE
);

-- 3 bảng tương ứng cho rule: rule_groups / rule_group_members / project_rule_groups

-- Phân biệt "bật thủ công" vs "bật do nhóm" để tắt nhóm không xoá nhầm lựa chọn tay.
-- Dữ liệu cũ đều là bật thủ công → default 1.
ALTER TABLE project_skills ADD COLUMN manual INTEGER NOT NULL DEFAULT 1;
ALTER TABLE project_rules  ADD COLUMN manual INTEGER NOT NULL DEFAULT 1;
```

**Vì sao cần cột `manual`:** với quan hệ N–N, khi tắt một nhóm ta phải biết item nào được gỡ. Quy tắc gỡ: item chỉ bị gỡ khi `manual = 0` **và** không còn thuộc nhóm nào khác đang bật cho project đó.

## 5. Backend (Rust)

Module mới `src-tauri/src/commands/groups.rs`, viết generic theo `enum GroupKind { Skill, Rule }` (map sang tên bảng + hàm apply/remove tương ứng) để không nhân đôi ~300 dòng logic.

Tauri commands (đăng ký thêm trong `lib.rs`):

| Command | Mô tả |
|---|---|
| `list_groups(kind)` | Trả `Group { id, name, description, color, position, member_count }` |
| `create_group(kind, payload)` / `update_group` / `delete_group` | CRUD nhóm; xoá nhóm **không** xoá skill/rule, chỉ xoá membership + link project |
| `get_group_members(kind, group_id)` | Danh sách id thành viên |
| `set_group_members(kind, group_id, ids[])` | Ghi đè thành viên; **lan truyền** (mục 5.1) |
| `get_project_groups(kind, project_id)` | `ProjectGroupState { group_id, name, member_count, applied_count, linked, state: on\|mixed\|off }` |
| `enable_group_for_project(kind, project_id, group_id)` | Apply lần lượt member (tái dùng `apply_skill_to_project` / `apply_rule_to_project`, ghi `manual = 0` nếu record chưa có), ghi link, trả `GroupApplyOutcome { applied, skipped, failures[] }` |
| `disable_group_for_project(kind, project_id, group_id)` | Xoá link, gỡ member theo quy tắc mục 4, trả `{ removed, kept_manual, kept_other_group }` |
| `apply_group_to_all_projects(kind, group_id)` | Tuỳ chọn — phase 3, dùng lại `ApplyAllOutcome` sẵn có |

### 5.1 Lan truyền khi sửa thành viên nhóm

- Thêm skill vào nhóm → với mọi project có link nhóm đó: `apply_skill_to_project`, insert `project_skills` với `manual = 0` (nếu đã tồn tại thì giữ nguyên `manual`).
- Bỏ skill khỏi nhóm → với mọi project có link: gỡ nếu `manual = 0` và không thuộc nhóm bật khác.
- `sync_skill_to_projects` / `sync_rule_to_projects` **không đổi** — chúng đã chạy theo `project_skills`, nên mọi thứ nhóm ghi vào đều được sync tiếp như thường.

### 5.2 Tương tác với toggle từng item (giữ nguyên)

- Bật tay một item → `manual = 1` (kể cả item đó đang thuộc nhóm đang bật).
- Tắt tay một item đang thuộc nhóm đang bật → vẫn cho phép; link nhóm giữ nguyên, nhóm chuyển sang trạng thái **mixed**, UI có nút "Áp dụng lại đủ nhóm".

## 6. Frontend

### 6.1 Store

`src/stores/groups.ts`: một factory `createGroupsStore(kind)` → export `useSkillGroupsStore` và `useRuleGroupsStore` (theo pattern hiện có của `stores/skills.ts` / `stores/rules.ts`). `stores/projects.ts` thêm `getProjectGroups / enableGroup / disableGroup`.

### 6.2 Màn quản lý nhóm

`src/components/GroupManagerModal.vue` (props `kind: 'skill' | 'rule'`), mở từ nút "Nhóm" trên header của `SkillsView.vue` và `RulesView.vue`:
- Cột trái: danh sách nhóm (tạo / đổi tên / xoá / chọn màu).
- Cột phải: checkbox list toàn bộ skill (hoặc rule) để chọn thành viên → `set_group_members`.
- Trên card skill/rule ở list view: hiển thị badge tên nhóm; header có chip filter theo nhóm.

### 6.3 Project detail — điểm chạm chính

Trong `ProjectDetailView.vue`, tab `skills` và tab `rules` thêm section "Nhóm" nằm **trên** danh sách item:

```
┌ Nhóm ─────────────────────────────────┐
│ ● Frontend            3/3   [ ON  ]   │
│ ● Review              1/4   [mixed]   │   ← nửa sáng + nút "Áp dụng lại"
│ ● Delivery            0/5   [ OFF ]   │
└───────────────────────────────────────┘
┌ Skills ───────────────────────────────┐
│ (danh sách item hiện tại, giữ nguyên) │
└───────────────────────────────────────┘
```

- Toggle nhóm dùng lại đúng markup switch hiện có (`h-6 w-11`, `Loader2` khi đang chạy), thêm biến thể mixed.
- Sau khi bật/tắt nhóm: `loadAppliedSkills()` + `loadProjectGroups()` để đồng bộ cả hai khối.
- Toast báo kết quả, kèm chi tiết `failures` như `handleApplyToAll` đang làm.

### 6.4 i18n

Thêm key vào `skills.json`, `rules.json`, `projectDetail.json` cho cả `vi` và `en`; chạy `pnpm check:i18n`.

## 7. Phân đoạn triển khai

| Phase | Nội dung | Ước lượng |
|---|---|---|
| 1 | Migration + `commands/groups.rs` (CRUD nhóm, thành viên, enable/disable cho project) + đăng ký `lib.rs` | ~1 ngày |
| 2 | Store + `GroupManagerModal.vue` + badge/filter ở SkillsView & RulesView | ~1 ngày |
| 3 | Section nhóm trong ProjectDetailView (toggle 3 trạng thái, apply lại, toast) + i18n | ~0.5–1 ngày |
| 4 | Lan truyền khi sửa thành viên nhóm + "Áp dụng nhóm cho tất cả dự án" | ~0.5 ngày |
| 5 | (Tuỳ chọn) Mở rộng MCP `skills_list` / `rules_list` trả thêm nhóm; thêm tool `groups_list` | ~0.5 ngày |

## 8. Rủi ro & edge case

- **Item thuộc nhiều nhóm đang bật:** tắt một nhóm không được gỡ item còn thuộc nhóm bật khác — có unit test riêng cho nhánh này.
- **Item bật tay rồi mới bật nhóm:** `manual = 1` được giữ, tắt nhóm không gỡ.
- **Nhóm rỗng:** toggle disable, hiển thị `0/0` và hint "Nhóm chưa có thành viên".
- **Xoá skill/rule:** `ON DELETE CASCADE` dọn membership; project vẫn giữ link nhóm.
- **Apply thất bại một phần** (project path không tồn tại, lỗi ghi file): vẫn ghi link nhóm, nhóm ở trạng thái mixed, toast liệt kê lỗi.
- **Conflict sync:** không đổi cơ chế; nhóm chỉ là lớp điều phối phía trên `apply_*_to_project`.

## 9. Trạng thái triển khai (2026-09-17)

- Phase 1–4: **đã code**. Migration `0034_skill_rule_groups.sql`, `src-tauri/src/commands/groups.rs`
  (+ 4 unit test), `src/stores/groups.ts`, `src/components/GroupManagerModal.vue`,
  `src/components/ProjectGroupToggles.vue`, i18n namespace `groups` (vi + en), tích hợp vào
  `SkillsView` / `RulesView` / `ProjectDetailView`.
- Phase 5 (mở rộng MCP `skills_list` / `rules_list` trả thêm nhóm): **chưa làm** — vẫn là tuỳ chọn.

## 10. Kiểm thử

- Rust `#[cfg(test)]` trong `commands/groups.rs` (DB in-memory, theo pattern `vps_servers.rs`): enable/disable nhóm, item đa nhóm, `manual` flag, lan truyền khi thêm/bớt thành viên.
- Thủ công: tạo 2 nhóm chồng thành viên → bật cả hai → tắt một → kiểm tra `.claude/skills/`, `.codex/skills/`, `AGENTS.md` còn đúng phần cần giữ.
- `pnpm check:i18n`, `pnpm typecheck`, `cargo test` trong `src-tauri`.
