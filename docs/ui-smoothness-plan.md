# Plan: Tăng độ mượt UI — scroll stream dài & chuyển màn hình

> **Đã gộp vào `docs/perf-plan.md`.** Tài liệu đó là plan tổng (backend I/O, IPC, bộ nhớ + trục render này) và giữ bảng ưu tiên duy nhất — làm theo bảng §7 ở đó, không theo §7 dưới đây.
>
> Tài liệu này giữ lại vì phần phân tích §3 (`content-visibility`), §4 (`KeepAlive`, kèm bảng phân loại 12 view theo side effect) và §5 (markdown cache) chi tiết hơn phần tóm tắt trong plan tổng.

Trạng thái: chưa implement
Phạm vi: frontend (`src/components/StreamLog.vue`, `src/App.vue`, `src/lib/markdown.ts`, các view trong `src/views/`)

---

## 0. Bối cảnh & phương án đã loại

Xuất phát từ câu hỏi "có nên viết lại native Swift cho macOS để app mượt hơn". Đã khảo sát và **loại phương án đó**: ~45.200 dòng Vue phải viết lại, Node sidecar vẫn bắt buộc giữ (Claude Agent SDK không có bản Swift), Mermaid không có tương đương native nên vẫn phải nhúng WKWebView, và `src/controller/` vẫn phải là web. Ước lượng 7–10 tháng cho 1 dev. Chi tiết phân tích nằm trong lịch sử thảo luận, không lặp lại ở đây.

Cũng đã **loại lazy-load route**. App chạy local nên thời gian fetch asset ~0; code-splitting chỉ cắt được phần parse/compile lúc khởi động, không giúp gì cho scroll, và làm **chậm hơn** lần đầu vào mỗi route. Nếu sau này muốn tối ưu thời gian mở app thì làm riêng, không thuộc plan này.

Hai điều đã được xử lý tốt từ trước, **không đụng vào**:

- `StreamLog.vue:552` đã có `v-memo` với dependency list đầy đủ → re-render không phải thủ phạm.
- `markdown.ts` đã có cache cho `renderText` → không còn parse lại markdown mỗi lần render.

---

## 1. Vấn đề

### 1.1 Scroll nội dung dài (StreamLog)

`grep` cho virtualization trả về **0 kết quả** trong toàn bộ `src/`. `StreamLog` render **toàn bộ** entries ra DOM.

`v-memo` ngăn Vue tính lại vnode, nhưng node DOM vẫn tồn tại — WebKit vẫn phải layout + paint chúng mỗi frame khi scroll. Một session dài với diff view, markdown block và mermaid SVG dễ lên hàng chục nghìn node.

**Đây là chi phí layout/paint, không phải chi phí re-render.** Nên `v-memo` không giải quyết được, và đó là lý do vấn đề vẫn còn dù `v-memo` đã có.

### 1.2 Chuyển màn hình

`App.vue:328` — `<RouterView :key="routeKey" />`, và **không có `<KeepAlive>` ở bất kỳ đâu** (grep ra đúng 1 kết quả, là một comment ghi nhận chính điều này tại `QuickCapturePanel.vue:6`).

Mỗi lần đổi route, app làm lại từ số không:

1. Destroy component cũ hoàn toàn
2. Create component mới — chạy lại `setup()`, dựng lại toàn bộ reactivity graph
3. `onMounted` chạy `await invoke(...)` — IPC sang Rust, chờ SQLite
4. Dựng lại toàn bộ DOM subtree

Quay lại màn hình vừa xem 5 giây trước vẫn trả đủ 4 bước. `SettingsView.vue:971-973` gọi `get_settings` **vô điều kiện** mỗi lần mount — không có guard cache như `ProjectDetailView.vue:513` hay `StatsView.vue:110`.

### 1.3 Khựng theo chu kỳ khi stream dài

`markdown.ts:36` — khi cache đạt 800 entry thì `_mdCache.clear()` xoá sạch.

Trong lúc stream, `entry.text` dài dần từng chunk, **mỗi chunk là một cache key mới**. Cache đầy rất nhanh → clear toàn bộ → lần render kế tiếp phải parse lại markdown của *cả cuộc hội thoại*. Kết quả là một cú khựng lặp lại theo chu kỳ.

---

## 2. Mục tiêu

- Scroll trong run dài giữ được 60fps, không phụ thuộc số entry đã tích luỹ.
- Quay lại một màn hình đã xem là tức thì, không remount và không IPC lại.
- Không còn khựng chu kỳ khi stream chạy lâu.
- Không thay đổi hành vi hiện có của RunView và cơ chế `routeKey`.

---

## 3. P1 — `content-visibility` cho StreamLog entry

**Ưu tiên cao nhất. Rủi ro gần như bằng 0, không đụng một dòng logic nào.**

### 3.1 Thay đổi

`src/components/StreamLog.vue:552` — wrapper `v-memo` hiện chưa có class:

```vue
<div v-memo="[entry, toolResult(entry), expanded[i], expandedCompacts.has(i), copiedCmd[i], expandedMessageIndices.has(i)]">
```

Thêm class `stream-entry`:

```vue
<div class="stream-entry" v-memo="[...giữ nguyên...]">
```

`src/assets/main.css` — thêm vào `@layer base` (block thứ hai, từ dòng 483):

```css
.stream-entry {
  content-visibility: auto;
  contain-intrinsic-size: auto 200px;
}
```

### 3.2 Vì sao hiệu quả

WebKit bỏ qua **hoàn toàn** layout + paint cho entry nằm ngoài viewport. Chi phí scroll trở nên tỉ lệ với phần đang nhìn thấy, không phải tổng số entry.

### 3.3 Điểm cần lưu ý

- `contain-intrinsic-size` là **bắt buộc**. Thiếu nó, chiều cao entry ngoài viewport bị coi là 0 → `scrollHeight` sai → thanh scroll nhảy.
- Giá trị `auto 200px`: `auto` cho phép browser nhớ chiều cao thật sau lần render đầu; `200px` là ước lượng ban đầu. Nếu entry trung bình cao hơn nhiều thì chỉnh lại con số này.
- **Rủi ro cần test kỹ:** logic scroll-anchoring tại `RunView.vue:662-676` neo theo element để không nhảy khi resize panel. `content-visibility` thay đổi cách `scrollHeight` được tính trong lúc entry chưa render. Phải kiểm tra kịch bản 6.1 bên dưới.
- Không áp `content-visibility` cho entry **cuối cùng** khi đang stream — nó luôn trong viewport nên không lợi gì, mà còn có thể gây nhấp nháy. Nếu gặp, thêm điều kiện `:class="{ 'stream-entry': i < entries.length - 1 }"`.

### 3.4 Đo trước/sau

Bắt buộc đo, vì P2 chỉ đáng làm nếu P1 chưa đủ:

- Mở một run có ≥ 300 entry.
- Safari Web Inspector → Timelines → Rendering Frames, scroll từ đầu tới cuối.
- Ghi lại: frame rate trung bình, số frame > 16ms, tổng thời gian Layout + Paint.

---

## 4. P2 — `KeepAlive` cho non-run routes

### 4.1 Ràng buộc phải tôn trọng

`routeKey` tại `App.vue:71-73` **cố tình** ép RunView remount khi đổi project, comment tại dòng 67-70 giải thích rõ lý do (stream state nặng nằm ở `liveRuns` store; đổi run trong cùng project đã xử lý in-place bằng watcher `activeRunId` tại `RunView.vue:896`).

→ **Không đưa RunView vào KeepAlive.** Không sửa `routeKey`.

### 4.2 Phân loại view theo side effect

Đã rà toàn bộ view ứng viên:

| View | timers | listeners | cleanup hooks | Nhóm |
|---|---|---|---|---|
| SkillsView | 0 | 0 | 0 | **A — sạch** |
| RulesView | 0 | 0 | 0 | **A — sạch** |
| McpServersView | 0 | 0 | 0 | **A — sạch** |
| CalendarView | 0 | 0 | 0 | **A — sạch** |
| WorkDigestView | 0 | 0 | 0 | **A — sạch** |
| PrInboxView | 0 | 0 | 0 | **A — sạch** |
| ServersView | 0 | 0 | 0 | **A — sạch** |
| ProjectsView | 0 | 0 | 2 | **B — cần rà** |
| TodosView | 0 | 1 | 3 | **B — cần rà** |
| NotesView | 0 | 1 | 3 | **B — cần rà** |
| SettingsView | 0 | 2 | 2 | **B — cần rà** |
| StatsView | **2** | 1 | 2 | **B — cần rà** |

### 4.3 P2a — cache nhóm A trước (7 view, rủi ro thấp)

Không có timer, không có listener, không có cleanup hook → cache được ngay, không cần sửa lifecycle.

`src/App.vue:328`:

```vue
<RouterView v-slot="{ Component }" :key="routeKey">
  <KeepAlive :include="CACHED_VIEWS">
    <component :is="Component" />
  </KeepAlive>
</RouterView>
```

```ts
const CACHED_VIEWS = [
  'SkillsView', 'RulesView', 'McpServersView',
  'CalendarView', 'WorkDigestView', 'PrInboxView', 'ServersView',
]
```

**Quan trọng:** `KeepAlive :include` khớp theo *tên component*. Toàn bộ view đang dùng `<script setup>` và **không có** `defineOptions({ name })` (đã grep — chỉ có `inheritAttrs` trong `components/ui/`). Vue suy ra tên từ tên file, nhưng cơ chế này phụ thuộc build và dễ vỡ khi minify.

→ Thêm `defineOptions({ name: 'SkillsView' })` vào **từng view** được cache. Không dựa vào tên suy ra.

### 4.4 P2b — nhóm B (5 view, cần sửa lifecycle)

Chỉ làm sau khi P2a chạy ổn định.

Với KeepAlive, component **không bị destroy** khi rời route — nó chỉ deactivate. Nghĩa là:

- `onMounted` chỉ chạy **một lần duy nhất**, không chạy lại khi quay lại màn hình.
- `onUnmounted` / `onBeforeUnmount` **không chạy** khi rời route.

→ Mọi side effect có vòng đời gắn với "đang xem màn hình này" phải chuyển sang `onActivated` / `onDeactivated`.

Việc cụ thể từng view:

- **StatsView** — nặng nhất. `StatsView.vue:112` có `setInterval` 30s cập nhật `now`, cùng một timer nữa và 1 listener; cleanup tại dòng 123. Nếu không chuyển, timer chạy ngầm vĩnh viễn kể cả khi không xem Stats. Chuyển cả 2 timer + listener sang `onActivated`/`onDeactivated`.
- **SettingsView** — 2 listener, cleanup tại dòng 993. Ngoài ra `onMounted` (dòng 971) gọi `get_settings` vô điều kiện; sau khi cache, cần thêm refresh trong `onActivated` để không hiển thị settings cũ sau khi sửa ở nơi khác.
- **TodosView / NotesView** — mỗi view 1 listener (`quickCreateUnlisten`, xem `TodosView.vue:39-42`). Chuyển sang `onActivated`/`onDeactivated`.
- **ProjectsView** — 2 cleanup hook, không timer/listener. Đọc kỹ xem cleanup đang dọn gì rồi quyết định.

### 4.5 Giới hạn bộ nhớ

`KeepAlive` giữ instance sống → tốn RAM. Thêm `:max="8"` để LRU tự loại instance cũ nhất. Với 12 view thì con số này đủ rộng mà vẫn có trần.

---

## 5. P3 — LRU cho markdown cache

`src/lib/markdown.ts:36`:

```ts
if (_mdCache.size >= MD_CACHE_MAX) _mdCache.clear()
```

Đổi sang loại **một** entry cũ nhất thay vì xoá sạch. `Map` trong JS giữ thứ tự chèn nên không cần thư viện:

```ts
if (_mdCache.size >= MD_CACHE_MAX) {
  const oldest = _mdCache.keys().next().value
  if (oldest !== undefined) _mdCache.delete(oldest)
}
```

Muốn đúng LRU (ưu tiên giữ entry hay đọc) thì khi cache hit, `delete` rồi `set` lại để đẩy key xuống cuối. Cân nhắc: thêm chi phí nhỏ mỗi lần hit, đổi lấy hit rate cao hơn. Với pattern append-only của stream, **FIFO ở trên là đủ** — entry cũ hiếm khi được đọc lại trừ khi người dùng scroll ngược.

Cải tiến kèm theo, đáng làm: entry **đang stream** tạo key mới mỗi chunk và rác rất nhanh. Cân nhắc bỏ qua cache cho entry cuối khi `running === true`, chỉ cache khi entry đã hoàn tất.

---

## 6. Kiểm thử

### 6.1 Scroll (sau P1)

- Run ≥ 300 entry, có lẫn diff view, code block và mermaid.
- Scroll nhanh từ đầu tới cuối rồi ngược lại → không giật, không nhảy thanh scroll.
- **Scroll-anchoring:** đang đọc giữa chừng, mở/đóng question panel và kéo thanh chia đôi → nội dung đang đọc phải đứng yên (đây là hợp đồng mà `RunView.vue:662-676` bảo đảm).
- **Auto-follow:** đang ở đáy khi stream chạy → phải tiếp tục dính đáy (`onOutputScroll`, `RunView.vue:650`).
- Drag-select text qua nhiều entry → không được tự bật lại auto-follow (`pointerDownInOutput`).
- Mermaid ngoài viewport, scroll tới → phải render đúng, không mất diagram.

### 6.2 Chuyển màn hình (sau P2)

- Đi qua tất cả view nhóm A rồi quay lại → vị trí scroll và state form còn nguyên, không có nháy loading.
- Safari Web Inspector → Memory: đi vòng qua các view 20 lần, heap phải ổn định (phát hiện rò rỉ instance).
- Sau P2b: để app chạy 30 phút, xác nhận không còn timer nào của StatsView chạy khi đang ở màn hình khác.
- Đổi project trong run route → RunView vẫn remount đúng như trước (hành vi `routeKey` không đổi).

### 6.3 Stream dài (sau P3)

- Chạy một run sinh > 800 block markdown, theo dõi có còn khựng chu kỳ không.

---

## 7. Thứ tự thực hiện

| # | Việc | Công sức | Nhắm vào | Phụ thuộc |
|---|---|---|---|---|
| P1 | `content-visibility` cho StreamLog entry | ~1 giờ | Scroll | — |
| P2a | `KeepAlive` cho 7 view nhóm A + `defineOptions` | ~2 giờ | Chuyển màn hình | — |
| P3 | LRU cho markdown cache | ~1 giờ | Khựng chu kỳ | — |
| P2b | Rà lifecycle 5 view nhóm B | ~3 giờ | Chuyển màn hình | P2a |
| P4 | Virtual list cho StreamLog | 3–5 ngày | Scroll | **Chỉ khi P1 chưa đủ** |

P1 + P2a + P3 ≈ **một ngày công**, và phủ hết những gì đang gây khó chịu.

**Đo lại sau P1 trước khi quyết định có làm P4 hay không.** P4 đắt vì phải viết lại toàn bộ scroll-anchoring tại `RunView.vue:662-676` — chỉ chạm vào nếu số liệu ở mục 3.4 chứng minh là cần.

---

## 8. Ghi chú

- Không đụng `routeKey` (`App.vue:71-73`) và không cache RunView. Đây là ràng buộc cứng.
- Không đụng `v-memo` tại `StreamLog.vue:552` — dependency list đang đúng, sửa vào dễ sinh bug stale render.
- Nếu P2b phát sinh rắc rối ngoài dự kiến, dừng ở P2a cũng đã lấy được phần lớn lợi ích: 7 view nhóm A là nhóm người dùng qua lại nhiều nhất.
