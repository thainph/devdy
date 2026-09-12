# Plan: Thiết kế lại màn hình Duo Session — gộp 2 stream thành 1 hội thoại

Trạng thái: đã implement (P1–P4), trừ hai mục ghi rõ bên dưới (5.6 nút detach, và phần định tuyến click notification ở 5.7)
Phạm vi: frontend (`src/components/Duo*`, `src/stores/orchestrator.ts`, i18n `duo.json`)

---

## 1. Vấn đề hiện tại

`DuoWorkspace.vue` (593 dòng) đang render 2 cột `StreamLog` độc lập:

- Người đọc phải tự ghép lượt A ↔ lượt B, 2 cột cuộn riêng nên mất trục thời gian.
- Mỗi cột chỉ rộng ~50%, code block / diff / tool output bị bó hẹp.
- Form cấu hình (goal, engine, vai trò, permission…) chiếm gần nửa chiều cao khi mở.
- Permission prompt phủ overlay lên cả cột, che output đang đọc.
- `orch.state.turns` — dữ liệu hội thoại thật sự — hiện chỉ dùng cho orchestration, không hề được hiển thị.

## 2. Mục tiêu

Một **timeline hội thoại 1 cột**, đọc từ trên xuống như chat giữa 2 agent:

```
┌────────────────────────────────────────────────┐
│ Duo · Đang chạy · vòng 2/6        [Dừng] [⚙]   │
├────────────────────────────────────────────────┤
│ chip: claude ↔ codex · acceptEdits · goal…     │  ← khi đang chạy
├────────────────────────────────────────────────┤
│  ▸ Mục tiêu: thiết kế kiến trúc X              │  ← topic card
│                                                 │
│  ──────────── Vòng 1 ────────────               │
│ ┃D│ Designer · claude · 14:02      [Chi tiết]   │  ┃ = accent primary
│ ┃ │ markdown câu trả lời…                       │
│                                                 │
│ ┃R│ Reviewer · codex · 14:05       [Chi tiết]   │  ┃ = accent violet
│ ┃ │ markdown câu trả lời…                       │
│                                                 │
│  ──────────── Vòng 2 ────────────               │
│ ┃D│ Designer · claude · đang chạy ⟳             │  ← bubble live, full stream
│ ┃ │ [StreamLog: tool calls, thinking, text…]    │
│                                                 │
│ ┌ Permission: Bash(rm -rf …)  [Cho phép][Từ chối] ┐ ← inline, màu theo bên
└────────────────────────────────────────────────┘
                                    [↓ Mới nhất]
```

Quyết định đã chốt:

| Hạng mục | Chốt |
|---|---|
| Bố cục | 1 cột full-width, accent màu + avatar theo bên |
| Split 2 cột | **Bỏ hẳn** (đã có màn Session để xem chi tiết từng phiên) |
| Chi tiết lượt | Lượt xong: markdown text; nút "Chi tiết" mới mount StreamLog. Lượt đang chạy: full stream |
| Cấu hình | Panel setup khi idle → thu thành thanh chip tóm tắt khi đang chạy |

## 3. Mô hình dữ liệu

### 3.1 Nguồn

- `orch.state.turns: DuoTurn[]` — đã **đúng thứ tự xen kẽ** (orchestrator chạy tuần tự A→B→A→B), mỗi phần tử có `role / engine / round / text`. Đây là xương sống của timeline, không cần merge-sort gì thêm.
- `live.get(runId).entries` — stream chi tiết (tool, thinking, result) của từng phiên, dùng cho nút "Chi tiết" và bubble đang chạy.

### 3.2 Ghép entries vào từng lượt — segmentation căn từ đuôi

Mỗi lượt bắt đầu bằng đúng 1 `live.pushUser(...)` (xem `startTurn` / `continueTurn` trong `orchestrator.ts`), nên **cắt mảng `entries` tại các entry `kind === 'user'`** là ra ranh giới lượt. Slice của một lượt = `(user_i, user_{i+1})`, tức **không chứa prompt relay** — đúng điều ta muốn (nếu không, nội dung bên kia sẽ bị lặp lại 2 lần trong timeline).

Căn **từ đuôi lên**, không dùng index tuyệt đối:

```
cho mỗi bên S:
  segs    = segmentsOf(entries(runId_S))          // cắt tại kind==='user'
  C       = số lượt đã hoàn tất của S trong state.turns
  liveSeg = (live.get(runId_S)?.status === 'running') ? segs.at(-1) : null
  pool    = liveSeg ? segs.slice(0, -1) : segs
  → C lượt hoàn tất lấy C phần tử CUỐI của pool, theo thứ tự
  → thiếu phần tử ⇒ gán null ⇒ lượt đó render text-only (degrade an toàn)
```

Vì sao căn từ đuôi: ở chế độ `existing`, run đã có sẵn N lượt hội thoại cũ trước khi vào duo → index đầu lệch; căn đuôi tự khử offset. Đồng thời sau khi restart app (`hydrateStreamsFromDisk` parse lại log từ đĩa) số entry có thể lệch nhẹ so với lúc live — căn đuôi vẫn đúng, còn nếu lưu index cứng vào localStorage thì sẽ cắt sai.

Điều kiện bubble live dùng `live.status === 'running'` (chính xác, tránh nhân đôi ở khoảnh khắc lượt vừa xong nhưng `state.waiting` chưa đổi).

**Hạn chế đã biết:** nếu user tự gửi message tay vào 1 trong 2 session từ tab Session trong lúc duo chạy, sẽ sinh thêm segment và lệch mapping → lượt cũ nhất rơi vào nhánh `null` (text-only). Chấp nhận được, ghi chú trong code.

### 3.3 Model hiển thị

```ts
// src/lib/duoConversation.ts (thuần hàm, không phụ thuộc Vue)
export type DuoMessage =
  | { kind: 'topic';  goal: string; aLabel: string; bLabel: string }
  | { kind: 'round';  round: number }                       // divider
  | { kind: 'turn';   id: string; role: DuoRole; round: number; engine: string;
                      label: string; text: string; at?: string;
                      entries: StreamEntry[] | null; runId: string | null }
  | { kind: 'live';   role: DuoRole; round: number; engine: string;
                      label: string; entries: StreamEntry[]; runId: string }
  | { kind: 'notice'; phase: DuoPhase; reason: string | null }

export function buildDuoConversation(state, getEntries, getStatus): DuoMessage[]
```

Hàm thuần → test được bằng unit test, và giữ `DuoConversation.vue` mỏng.

### 3.4 Thay đổi store (tối thiểu)

`src/stores/orchestrator.ts`:

- `DuoTurn` thêm `at?: string` (ISO, gán lúc push turn trong `onDesignerDone` / `onReviewerDone`) để hiện giờ trên header mỗi lượt. Optional ⇒ history cũ trong localStorage vẫn load được, chỉ là không có timestamp.
- Không cần thêm gì khác: `waiting`, `round`, `phase`, `stopReason` đã đủ.

## 4. Thay đổi UI

### 4.1 Cấu trúc file

```
src/components/duo/
  DuoWorkspace.vue      (chuyển từ components/, còn ~180 dòng: shell + actions)
  DuoHistoryList.vue    (chuyển từ components/, không đổi nội dung)
  DuoSetupPanel.vue     (mới — toàn bộ form cấu hình hiện tại, emit `start` / `reset`)
  DuoSummaryBar.vue     (mới — thanh chip tóm tắt khi đang chạy, click để mở lại setup)
  DuoConversation.vue   (mới — scroller + danh sách message + auto-scroll)
  DuoMessage.vue        (mới — 1 block hội thoại)
src/lib/duoConversation.ts (mới)
```

Precedent đã có subdir `components/calendar/`, `components/remote/`. RunView chỉ phải sửa 2 dòng import; prop `projectId` và emit `openSession` giữ nguyên → không ảnh hưởng logic RunView.

### 4.2 `DuoWorkspace.vue` (shell)

- Header: tiêu đề, phase + icon (giữ nguyên logic `phaseLabel`), bộ đếm vòng, nút Dừng / Tiếp tục, nút ⚙ mở-đóng setup.
- Thân: `v-if` idle/chưa chạy → `DuoSetupPanel`; đang chạy/đã chạy → `DuoSummaryBar` (bấm mở lại panel dạng overlay trượt xuống).
- Dưới: `DuoConversation` chiếm `flex-1`.
- Xoá: `designerScrollEl` / `reviewerScrollEl`, 2 watcher scroll, khối `<section class="grid … md:grid-cols-2">`, 2 overlay `PermissionPrompt`.
- Giữ: toàn bộ `hydrateFromState` / `resetForm` / `start` / `stop` / `continueRun` (state form chuyển xuống `DuoSetupPanel` qua `v-model` hoặc emit config lúc start — đơn giản nhất: panel tự giữ form state, emit `start(config)`).

### 4.3 `DuoMessage.vue`

Header row: avatar tròn chữ cái đầu của label, tên vai trò (semibold), chip engine (font-mono, muted), chip `vòng n`, giờ, và bên phải: nút Copy, nút "Chi tiết" (chỉ hiện khi `entries` khác null), nút mở session (`ExternalLink` → emit `openSession(runId)` như hiện tại).

Body:
- Mặc định: `v-html="renderText(text)"` bọc trong `CollapsibleMessage` (`:max-height="480"`, `fade="hsl(var(--card))"`) — component sẵn có, tránh 1 lượt dài chiếm cả màn hình.
- Bấm "Chi tiết" → mount `StreamLog :entries="entries"` (lazy, `v-if`), có `renderText`, `fileMatcher` để giữ tính năng click mở file như màn Session.
- `kind === 'live'`: luôn render `StreamLog :entries :running="true"`, không clamp.

Màu theo bên (dùng token + palette sẵn có trong repo):

| | Accent trái | Avatar |
|---|---|---|
| A (designer) | `border-l-2 border-primary/60` | `bg-primary/10 text-primary` |
| B (reviewer) | `border-l-2 border-violet-500/60` | `bg-violet-500/10 text-violet-600 dark:text-violet-400` |

Nền block `bg-card/40`, bo `rounded-lg`, padding `px-4 py-3`, cách nhau `space-y-3`.

### 4.4 `DuoConversation.vue`

- Vùng cuộn duy nhất, `absolute inset-0 overflow-y-auto`.
- Divider `Vòng n` giữa các round (chèn bởi builder).
- Auto-scroll theo đúng pattern đã có ở `RunView.vue`: `stickToBottom` + `isNearBottom` + tạm dừng follow khi pointer đang giữ (drag chọn text) + nút nổi "↓ Mới nhất" khi `!stickToBottom`.
- Permission / câu hỏi xác nhận: xem mục 5 riêng bên dưới.
- Toolbar nhỏ phía trên timeline: lọc `Tất cả | A | B`, nút "Sao chép hội thoại" (xuất markdown `## {label} · vòng n` + text).
- Empty state khi chưa có lượt nào: icon + `duo.conversation.empty`.

### 4.5 i18n

Thêm nhánh `conversation` vào `src/i18n/locales/{en,vi}/duo.json`:
`topic`, `empty`, `round`, `detail`, `hideDetail`, `streaming`, `copyTranscript`, `copied`, `jumpLatest`, `filterAll`, `filterA`, `filterB`, `openSession`, `endedConsensus`, `endedMaxRounds`, `endedManual`, `endedError`.
Bỏ `emptyDesigner` / `emptyReviewer` (không còn 2 cột).

## 5. Câu hỏi cần xác nhận (permission / AskUserQuestion / plan)

**Nguyên tắc: giữ nguyên giao diện và vị trí như màn Session thường.** Không vẽ card inline trong timeline, không thanh ghim, không đổi vị trí. Việc duy nhất phải làm cho Duo là **gộp hàng đợi của cả 2 session** để không bỏ sót câu hỏi từ bên nào.

### 5.1 Các dạng interrupt

Tất cả tới qua `live.get(runId).permissionQueue` và đều do `PermissionPrompt.vue` (644 dòng, sẵn có) render — không viết lại gì:

| Dạng | Giao diện | Handler |
|---|---|---|
| Tool permission (Bash, Edit…) | Cho phép / Từ chối + "nhớ lựa chọn" | `handleDecide` |
| `AskUserQuestion` | Wizard nhiều bước, chọn option, có ô "Other" | `handleAnswer` |
| `ExitPlanMode` | Duyệt plan (render markdown) | `handleDecide` |
| MCP | Như tool permission, không có "nhớ lựa chọn" | `handleDecide` |

### 5.2 Giao diện: bê nguyên drawer của màn Session

Copy y nguyên khối ở `RunView.vue:3163-3203` sang `DuoWorkspace`, đặt trong container của vùng hội thoại:

- Overlay trượt từ phải: `absolute inset-y-0 right-0 z-20 bg-card border-l border-border` + shadow, rộng `questionWidthPct%` (mặc định 42).
- Tay nắm resize ở mép trái (`startQuestionResize`, kẹp 20–80%) — copy 3 hàm ở `RunView.vue:593-606`.
- Nổi **đè lên** hội thoại (absolute, không nằm trong flex row) để timeline giữ nguyên bề rộng, không reflow và không mất vị trí đang đọc khi prompt hiện ra — đúng lý do đã ghi trong comment của RunView.
- Bên trong: `<PermissionPrompt :key="head.request_id" :request="head" :allowed-tools :render-text @decide @answer />`.

So với hiện tại (`DuoWorkspace.vue:542`): overlay `absolute inset-0` phủ kín nguyên một cột → bỏ, thay bằng drawer bên phải như session.

### 5.3 Detect câu hỏi từ **cả 2** session ← phần việc thật sự

Hiện `DuoWorkspace` có 2 computed tách rời (`designerPermission`, `reviewerPermission`), mỗi cái đổ vào overlay của cột tương ứng. Gộp thành **một hàng đợi duy nhất**:

```ts
// Ưu tiên bên đang chạy; bên kia có thể còn request treo từ lượt trước
// (vd lượt bị huỷ giữa chừng) — vẫn phải gom vào, không được bỏ sót.
const duoPermissions = computed(() => {
  const sides: Array<{ role: DuoRole; runId: string | null }> = [
    { role: 'designer', runId: orch.state.designerRunId },
    { role: 'reviewer', runId: orch.state.reviewerRunId },
  ]
  if (orch.state.waiting === 'reviewer') sides.reverse()
  return sides.flatMap(({ role, runId }) => {
    const req = runId ? live.get(runId)?.permissionQueue[0] : undefined
    return req ? [{ role, runId: runId!, request: req }] : []
  })
})
const headPermission = computed(() => duoPermissions.value[0] ?? null)
```

Lưu ý khi trả lời: dùng `req.run_id` sẵn có trong request cho `respondPermission`, và `live.shiftPermission(runId)` đúng run đó. Hai handler hiện tại đã nhận `runId` làm tham số nên **giữ nguyên, chỉ đổi chỗ gọi** — truyền `headPermission.role/runId` thay vì cứng theo cột.

Thêm đúng **một dòng nhãn** trên đầu drawer cho biết bên nào đang hỏi (`Designer · claude`) — ở màn session không cần vì chỉ có một run, còn ở duo không có nó thì không biết trả lời cho ai. Nếu thấy thừa thì bỏ được, không ảnh hưởng gì khác.

### 5.4 Chỉ báo trong hội thoại — dùng lại đúng marker của session

Trên header lượt đang chạy của bên đang hỏi, gắn marker y hệt History list ở `RunView.vue:2773-2790`: `ShieldQuestion` (permission) hoặc `MessageCircleQuestion` (`AskUserQuestion`), bọc `animate-ping` màu primary, title `run.waitingForPermission` / `run.waitingForAnswer`. Không thêm style mới, chỉ tái dùng convention đã có.

### 5.5 Tình huống biên

- **Hàng đợi nhiều yêu cầu**: vẫn chỉ render phần tử đầu như session, trả lời xong queue tự đẩy cái kế tiếp lên.
- **Cả 2 bên cùng pending**: `duoPermissions` giữ cả hai, hiển thị lần lượt (bên đang chạy trước). Không mất request nào.
- **Dấu vết sau khi trả lời**: không cần vẽ thêm — `StreamLog` render tool call / nhánh `isAskUserTool` kèm câu trả lời trong phần "Chi tiết" của lượt đó.
- **Rời tab Duo**: `DuoWorkspace` mount bằng `v-if` theo `leftTab === 'duo'` nên drawer bị unmount, nhưng request vẫn nằm trong `permissionQueue`; quay lại tab là thấy nguyên.

### 5.6 Nút detach ra cửa sổ riêng — để P4

Màn session có nút detach (`openPermissionPopout`, `RunView.vue:2002`) nhưng nó kéo theo cả cầu nối event (`permission:sync` / `permission:decide` / `permission:answer` / `window-ready`, ~60 dòng ở `RunView.vue:1996-2050`) và cờ `poppedOut`. Đưa vào P4 nếu cần, P2 làm drawer trước cho gọn.

### 5.7 Thông báo OS — 2 lỗ hổng đã phát hiện

`PermissionNotifier.vue` (app-wide, mount ở `App.vue:304`) loại trừ run đang xem bằng `route.params.runId` (`PermissionNotifier.vue:41`). Ở tab Duo, 2 run của duo không phải runId trên route, nên:

1. Đang nhìn thẳng vào màn Duo vẫn bị bắn notification OS → nhiễu.
2. Bấm notification thì `navigateToRun` đưa về **màn Session** của run đó, không quay lại tab Duo.

**Đã xử lý (1):** orchestrator expose `visibleRunIds` + `setVisibleRunIds`; `DuoWorkspace` publish 2 run của duo khi mounted và xoá lúc unmount; `PermissionNotifier` loại chúng khỏi danh sách cần bắn noti. (Notifier không gate theo focus cửa sổ, nên đây là lỗi thật chứ không phải chỉ về lý thuyết.)

**Không làm (2):** `leftTab` là state cục bộ của `RunView`, không nằm trong store, nên đưa click về đúng tab Duo cần lift state lên — to hơn nhiều so với lợi ích, vì màn Session của run đó **vẫn render đúng prompt** và trả lời được bình thường. Ngoài ra tự động nhảy sang tab Duo sẽ đá nhau với nút "Mở session" (`openDuoSession` cố tình chuyển sang tab Session).

## 6. Hiệu năng

- Lượt đã xong chỉ render markdown (1 `v-html`), không mount `StreamLog` → nhẹ hơn hiện tại rất nhiều với hội thoại nhiều vòng.
- `StreamLog` chỉ mount khi bấm "Chi tiết" hoặc cho lượt đang chạy.
- `CollapsibleMessage` clamp 480px cho từng lượt.
- Nếu về sau >40 lượt thấy chậm: thêm windowing "Xem các vòng trước" giống `historyWindow` của RunView (để phase sau, chưa làm ngay).

## 7. Các bước thực hiện

1. **P1 — Nền dữ liệu**: tạo `src/lib/duoConversation.ts` (segmentation + builder); thêm `at` vào `DuoTurn` và gán ở 2 chỗ push turn.
2. **P2 — Hội thoại + câu hỏi**: tạo `DuoConversation.vue` + `DuoMessage.vue`, cắm vào `DuoWorkspace` thay cho khối 2 cột; gộp queue 2 session thành `duoPermissions` (5.3) và bê drawer từ `RunView` sang (5.2), thêm marker chờ trả lời (5.4). *(Tới đây đã dùng được.)*
3. **P3 — Cấu hình**: tách `DuoSetupPanel.vue`, thêm `DuoSummaryBar.vue`, gọn lại header.
4. **P4 — Hoàn thiện**: divider vòng, lọc A/B, copy transcript, nút "↓ Mới nhất", i18n en/vi, dời file vào `components/duo/` + sửa import ở `RunView.vue`, vá `PermissionNotifier` (5.7), cân nhắc nút detach (5.6).
5. **Kiểm chứng**: `pnpm typecheck` + `pnpm lint`; chạy app thử 4 luồng:
   - duo mới (`new`),
   - duo từ session có sẵn (`existing`),
   - restore 1 duo cũ từ history sau khi khởi động lại app (mapping entries căn-từ-đuôi còn đúng không),
   - **duo dính permission + `AskUserQuestion`**: trả lời khi đang ở đáy, và khi đã cuộn lên giữa hội thoại (thanh ghim phải hiện, bấm phải nhảy đúng card).

## 8. Rủi ro

| Rủi ro | Xử lý |
|---|---|
| Mapping entries lệch (user gửi tay vào session, log parse khác live) | Căn từ đuôi + `null` ⇒ tự động degrade sang text-only, không vỡ UI |
| Bỏ sót câu hỏi của bên không chạy (request treo từ lượt bị huỷ) | Gộp queue cả 2 session vào `duoPermissions`, không lọc theo `state.waiting` (5.3) |
| History cũ trong localStorage không có `at` | Field optional, ẩn timestamp |
| Mất khả năng xem song song 2 stream | Đã có màn Session riêng; nút "Mở session" trên mỗi lượt vẫn dẫn sang đó |
| Prompt relay không còn nhìn thấy | Có thể thêm hàng "Prompt đã gửi" thu gọn trong "Chi tiết" (phase sau nếu cần) |
