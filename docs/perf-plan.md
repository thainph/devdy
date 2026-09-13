# Plan: Hiệu năng & độ mượt devdy

Trạng thái: chưa implement
Phạm vi: backend (`src-tauri/src/runs/`, `src-tauri/src/commands/runs.rs`) + frontend (`src/views/RunView.vue`, `src/stores/liveRuns.ts`, `src/components/StreamLog.vue`)

Tài liệu này là **plan tổng**. `docs/ui-smoothness-plan.md` là tài liệu tiền nhiệm, chỉ phủ trục render frontend; các hạng mục của nó đã được gộp vào bảng §7 dưới đây. Giữ lại vì phần phân tích §3–§5 của nó chi tiết hơn phần tóm tắt ở đây.

---

## 0. Số liệu đo được

```
run log:  876 file   tổng 818.2 MB   avg 0.93 MB   max 28.66 MB
top 5:    28.66 / 21.63 / 21.16 / 18.61 / 11.70 MB
```

Mọi thao tác chạm "cả file log" đều phải đọc theo con số 28.66 MB, không phải con số trung bình 0.93 MB. Phân bố đuôi dài là lý do vấn đề chỉ lộ ra ở một số run.

---

## 1. F1 — Ghi lại toàn bộ file log mỗi 500ms, trong lúc giữ mutex

**Vị trí:** `src-tauri/src/runs/sidecar.rs:397-419` (flush định kỳ) và `:655-659` (flush cuối).

```rust
let existing_prefix = if merge_existing_log {
    std::fs::read_to_string(&log_path).unwrap_or_default()   // ① thường trú trong RAM cả run
} else { String::new() };

let flush_to_disk = |buf: &str| {
    if merge_existing_log {
        let mut merged = String::with_capacity(existing_prefix.len() + buf.len());
        merged.push_str(&existing_prefix);
        merged.push_str(buf);
        let _ = std::fs::write(&log_path, merged);   // ② ghi lại toàn bộ + alloc mới mỗi tick
    } else {
        let _ = std::fs::write(&log_path, buf);      // ② ghi lại toàn bộ
    }
};

loop {
    tokio::select! {
        _ = flush_tick.tick() => {
            let buf = log_buf.lock().await;   // ③ guard sống suốt lần ghi
            flush_to_disk(&buf);              // ④ blocking I/O trong select!
        }
        line = stdout_reader.next_line() => { ... }
    }
}
```

Bốn vấn đề chồng nhau:

1. **O(n²) theo độ dài run.** Run 28 MB ghi lại 28 MB mỗi 500ms (~57 MB/s) chỉ để thêm vài KB. Run càng dài càng chậm.
2. **Blocking I/O nằm cùng `select!` với nhánh đọc stdout.** Trong lúc ghi, không dòng nào được đọc khỏi sidecar. Đây không phải vấn đề threading — cùng một `select!` thì không thể chạy song song. Nguyên nhân trực tiếp của khựng khi stream.
3. **Giữ `log_buf.lock()` suốt lần ghi** → `append_log` (`:341`) bị chặn theo.
4. **Resume là trường hợp tệ nhất**, không phải trường hợp phụ: `existing_prefix` giữ nguyên file cũ trong RAM suốt run, cộng thêm một String `prefix + buf` mới mỗi tick.

### 1.1 Cách sửa

**Không** dùng `spawn_blocking(...).await` ngay trong nhánh `select!`. Tuy không chặn Tokio worker, vòng đọc stdout/stderr vẫn phải chờ job ghi hoàn tất — chưa giải quyết được (2).

Thiết kế: **một writer task tuần tự riêng**, giao tiếp qua bounded channel.

- Writer mở file **append một lần** (`OpenOptions::new().create(true).append(true)`) và giữ handle suốt run.
- Drain task mỗi tick chỉ: lock → `let tail = buf[flushed_len..].to_string()` → **nhả lock ngay** → `flushed_len += tail.len()` → enqueue.
- Writer nhận và `write_all`. Thứ tự được bảo toàn, không tranh chấp write, không chặn đọc sidecar.

Với resume: mở append từ đầu, **bỏ hẳn `existing_prefix`** — không cần đọc file cũ vào RAM.

### 1.2 Chính sách backlog (phải quyết tường minh)

Bounded channel mà `send().await` thì backpressure dội ngược vào vòng đọc stdout — đúng thứ ta đang bỏ.

**Quyết định: `await`, cap ~1024 delta.** Append một delta không thể thua tốc độ sinh token, nên hàng đợi thực tế không bao giờ đầy; nếu đầy thì chặn là đúng hơn mất dữ liệu. Đây là quyết định có chủ đích, không phải mặc định.

`src-tauri/src/remote/outbound.rs` vừa giải đúng bài toán coalescing + backpressure này cho remote link — **tái dùng pattern, không dựng mới**.

### 1.3 Khoảng flush

Sau khi mỗi tick tốn O(delta) thay vì O(total), 500ms không còn là giới hạn chi phí mà chỉ còn vai trò **gộp syscall**. Hạ xuống ~150ms gần như miễn phí và co cửa sổ mất dữ liệu khi crash từ 500ms còn 150ms.

### 1.4 Defect kèm theo: resume marker thiếu newline dẫn

`src-tauri/src/commands/runs.rs:2288`:

```rust
buf.push_str(&format!("[stderr] --- resumed session {} at {} ---\n", session_id, started_at));
```

Marker mở đầu bằng `[stderr]`, **không có `\n` dẫn**. Nếu log cũ không kết thúc bằng newline, marker bị dán vào cuối dòng cuối — làm hỏng cả dòng đó lẫn chính marker.

Đây là **bug có sẵn** ở cơ chế `prefix + buf` hiện tại, không phải rủi ro mới do append mode. Sửa: writer kiểm tra byte cuối của file khi mở ở chế độ resume, ghi một `\n` trước marker nếu thiếu.

### 1.5 Trade-off phải ghi rõ

Crash giữa một `write_all` có thể để lại dòng cuối dở dang. `parseStreamLog` (`streamEvents.ts:480`) đã `try/catch` từng dòng nên **an toàn về hiển thị**. Cửa sổ mất output tối đa = khoảng flush (500ms hiện tại → 150ms sau §1.3).

### 1.6 Test bắt buộc

- Resume run: nối đúng, không nhân đôi prefix, marker nằm trên dòng riêng.
- Resume sau crash: log cũ kết thúc bằng dòng JSON dở → resume vẫn chạy, parser vẫn render.
- Flush cuối (`:655`) **chờ writer drain xong** trước khi task kết thúc và trước khi `UPDATE runs SET status`.
- Đo lượng ghi đĩa trước/sau bằng `fs_usage` trên một run dài.

---

## 2. F2a — Focus refresh đọc + parse lại toàn bộ log

`RunView.vue:936` đăng ký `window.addEventListener('focus', onAppFocus)`. Mỗi lần alt-tab quay lại app, chuỗi sau chạy lại toàn bộ:

| Bước | Chi phí với log 28 MB |
|---|---|
| `fs::read_to_string` | đọc 28 MB |
| Tauri serialize → JSON | escape 28 MB |
| JS parse phía IPC | dựng lại 28 MB string |
| `content === viewingLog.value` (`RunView.vue:2352`) | memcmp 28 MB |
| `parseStreamLog` (`streamEvents.ts:442`) | `split()` ~200k dòng + `JSON.parse` từng dòng |

Guard ở `:2352` chỉ tránh được phần *re-render*, không tránh được phần đọc/parse — vốn mới là phần đắt.

### 2.1 Vì sao `stat(size, mtime)` đơn thuần KHÔNG đủ

`get_run_log` (`commands/runs.rs:1166-1200`) **không chỉ đọc file**. Nó gọi `upsert_claude_session` / `upsert_codex_session_file` trước, để log được làm mới nếu session đã được tiếp tục **bên ngoài Devdy** (Claude CLI, VS Code extension). Một `stat` chỉ trên `.devdy/runs/<id>.log` sẽ **bỏ sót transcript gốc thay đổi bên ngoài** → user thấy log cũ.

### 2.2 API đề xuất: `get_run_log_revision`

Trả về fingerprint đủ phủ cả hai nguồn:

```rust
struct RunLogRevision {
    source: LogSource,          // Conventional | Transcript | OutputPath
    path: Option<String>,
    size: u64,
    mtime: i64,                 // độ phân giải cao (nanos), không phải giây
    // Chỉ với run mirror:
    transcript_size: Option<u64>,
    transcript_mtime: Option<i64>,
}
```

Frontend chỉ gọi đường `get_run_log` nặng khi fingerprint đổi. Cơ chế đồng bộ session ngoài app được giữ nguyên, không phá.

`mtime` phải ở độ phân giải cao: hai lần ghi trong cùng một giây là chuyện thường khi stream.

---

## 3. F2b — Pagination thật, không phải "tail API"

`RunView.vue:423` đã window DOM ở 80 entry (`HISTORY_WINDOW_INITIAL`), nhưng **vẫn đọc và parse toàn bộ file** để lấy 80 entry đó, rồi giữ cả `viewingLog` (raw 28 MB) lẫn `historyEntries` (parsed) trong heap.

### 3.1 Đọc phải theo ranh giới record, không theo byte cố định

Log là JSONL. Đọc "256 KB cuối" sẽ cắt giữa một dòng. Cần:

- Tìm newline đầu tiên trong vùng đọc, hoặc mở rộng ngược cho đủ N record.
- **Một record đơn lẻ có thể lớn hơn 256 KB**: user message mang `ImageAttachment.data` là base64 thô (`streamEvents.ts:5`) — ảnh dán vào dễ vượt mức này. Không được giả định kích thước byte cố định.

API cần **cursor byte + `has_more` + giới hạn an toàn**, không phải một tham số `tail_bytes`.

```rust
struct RunLogPage {
    records: Vec<String>,   // các dòng nguyên vẹn
    cursor: u64,            // byte offset để xin trang cũ hơn
    has_more: bool,
}
```

### 3.2 Phải tách các consumer đang phụ thuộc mảng đầy đủ

Đây là điều kiện tiên quyết, không phải việc phụ. Consumer đã xác minh:

- **`MentionedFiles`** (`RunView.vue:3091`) nhận `displayedEntries` = **toàn bộ** `historyEntries`, duyệt hết trong computed (`MentionedFiles.vue:93`).
- Export plain-text (comment tại `RunView.vue:426` có nhắc; cần rà lại vì `displayedEntries` chỉ xuất hiện ở 3091 — comment có thể đã cũ).

Hai tác vụ này chỉ nên **tải full log theo yêu cầu** (khi user bấm), không giữ reactive heap. Đây chính là lý do F2b phải là pagination thật: chừng nào còn một consumer cần mảng đầy đủ thường trực, việc bỏ `viewingLog` khỏi heap không có ý nghĩa.

Kết quả: bỏ hẳn `viewingLog` khỏi reactive state, thay bằng `RunLogRevision` làm dirty-marker.

---

## 4. F3 — Bộ nhớ: ba bản cho mỗi run, không bao giờ giải phóng

| Nơi | Cái gì | Vòng đời hiện tại |
|---|---|---|
| Rust | `log_buf: Arc<Mutex<String>>` (`runs/mod.rs:24`) | toàn bộ output của run trong RAM suốt run |
| Rust | `existing_prefix` (`sidecar.rs:397`) | toàn bộ log cũ trong RAM suốt run (chỉ khi resume) |
| JS | `viewingLog` + `historyEntries` | run đang xem — 2 bản |
| JS | `liveRuns.sessions[*].entries` + `.outputLines` | **không giới hạn, không evict** |

`discard()` (`liveRuns.ts:491`) chỉ được gọi từ `RunView.vue:2228` và `:2254` — thao tác tường minh của user. **Không evict khi run kết thúc, không LRU.** Mở 10 run trong một phiên → 10 session nằm nguyên trong heap tới khi đóng app. `outputLines` còn lưu song song với `entries` (`:243-246` push cùng một dòng vào cả hai).

### 4.1 Cách sửa

- **Rust:** sau F1, `log_buf` chỉ cần giữ phần *chưa flush* → từ O(run) xuống O(150ms output). `existing_prefix` biến mất. Đây là lợi ích đi kèm miễn phí của F1 nhưng lớn, nên theo dõi riêng khi đo.
- **`outputLines`:** ring buffer cap ~2000 dòng (chỉ phục vụ raw-output view).
- **LRU session:** cap theo **cả số session lẫn tổng entries/bytes ước lượng** — một session 50k entry nguy hiểm hơn 10 session nhỏ.

### 4.2 Điều kiện KHÔNG được evict

LRU chỉ áp dụng cho session **terminal và đã xem**. Giữ lại vô điều kiện:

1. Session đang chạy (`status === 'running'`).
2. Session còn `permissionQueue` chưa xử lý — kể cả khi trạng thái đã terminal (trạng thái bất thường vẫn phải giữ).
3. Session còn `notifyDone` chưa `markSeen`.
4. **Session còn `doneCallbacks`** (`liveRuns.ts:100`). Duo orchestrator giữ callback qua nhiều lượt có chủ đích — evict là cắt đứt chuỗi hội thoại giữa hai agent.

---

## 5. F4 — Live entries không có windowing

`RunView.vue:3138` truyền `windowedHistoryEntries` (cap 80) — tốt.
`RunView.vue:3167` truyền `liveEntries` — **không cap**. Run live dài đẩy DOM lên vô hạn.

`content-visibility` (§6) gánh phần lớn chi phí này. **Đo sau §6 rồi mới quyết** có window hoá live hay không — việc đó đụng auto-follow (`onOutputScroll`, `:650`) và scroll-anchoring (`:662-676`), rủi ro cao hơn nhiều.

---

## 6. Trục render frontend (từ `ui-smoothness-plan.md`)

Giữ nguyên, không lặp lại chi tiết:

- **`content-visibility: auto` + `contain-intrinsic-size`** cho `.stream-entry` (`StreamLog.vue:552`). Rủi ro gần 0, không đụng logic. **Phải test scroll-anchoring** (`RunView.vue:662-676`) — xem §6.1 của tài liệu cũ.
- **`KeepAlive` nhóm A** (7 view không timer/listener/cleanup) + `defineOptions({ name })` cho từng view. **Không cache RunView, không đụng `routeKey`** (`App.vue:71-73`) — ràng buộc cứng.
- **Markdown cache: FIFO/LRU thay cho `clear()`** (`markdown.ts:36`), và **không cache entry đang stream** — mỗi chunk sinh một key mới, rác rất nhanh, là nguyên nhân khựng theo chu kỳ.

---

## 7. Bảng ưu tiên

| Ưu tiên | Hạng mục | Công | Phụ thuộc | Trạng thái |
|---|---|---|---|---|
| **P0** | F1 — writer task riêng + bounded backlog + fix newline marker + test resume/crash/final drain | 1–1.5d | — | **chưa làm** |
| **P0** | F2a — `get_run_log_revision`, phủ cả transcript mirror | 4h | — | ✅ đã làm |
| **P1** | `content-visibility` + đo scroll/anchoring | 1h | — | ✅ code xong, **chưa đo** |
| **P1** | F3 — cap `outputLines` + LRU terminal session (4 điều kiện giữ lại) | 4h | — | ✅ đã làm |
| **P1** | Markdown cache FIFO/LRU, bỏ cache entry đang stream | 1h | — | ✅ đã làm |
| **P2** | `KeepAlive` nhóm A | 2h | — | ✅ đã làm |
| **P2** | F2b — pagination theo record/cursor; tách `MentionedFiles`; bỏ raw log khỏi reactive | 1.5–2d | P0, F2a | ✅ đã làm |
| **P3** | Storage-management UX (§8) | 1–1.5d | — | chưa làm |
| **P3** | `KeepAlive` nhóm B (5 view, chuyển lifecycle sang `onActivated`/`onDeactivated`) | 3h | P2 | chưa làm |
| **P3** | **Spike** index DB (§9) — chỉ triển khai khi số đo chứng minh cần | spike 0.5d | P2 | chưa làm |
| **P4** | Virtualize live entries | 3–5d | chỉ khi P1 không đạt mục tiêu | chưa làm |

**F2a được kéo lên trước dù thuộc P0**: nó là điều kiện tiên quyết cứng của F2b (P2), không thể giao P2 mà bỏ nó.

**F1 vẫn chưa làm.** Đây là hạng mục P0 nặng nhất còn lại — mỗi 500ms vẫn ghi lại toàn bộ file log trong lúc giữ mutex và chặn vòng đọc sidecar (§1).

### 7.2 Cách F2b được triển khai (khác chi tiết so với §3)

- `get_run_log_page(run_id, before, limit)` đọc ngược theo ranh giới record, trả `{records, cursor, has_more, preamble, revision}`. Có 10 unit test phủ: phân trang không trùng/sót, không trả record dở, record lớn hơn byte-cap, file không kết thúc bằng newline, file rỗng.
- `preamble` giải quyết thứ một cửa sổ đuôi về mặt cấu trúc không thể thấy: `system.init` nằm ở đầu file và mang model id (quyết định context-window limit).
- `parseStreamLogWindow` tách khỏi `parseStreamLog`: không kiểm tra head, không trả null, nhận `seedModel`.
- **`MentionedFiles`**: giải bằng `get_run_tool_records` — Rust stream qua log, chỉ trả các record `tool_use`/`tool_result` đã **cắt bớt** (bỏ thân file trong `input.content`, bỏ nội dung `tool_result`, chỉ giữ `is_error`). JS chạy nguyên logic `writeActionOf`/`fileTargets` hiện có nên **không có rủi ro lệch logic**. An toàn vì `sidecar-codex/index.mjs:543` đã chuẩn hoá `fileChange` thành `tool_use` name `Edit` trước khi ghi log — hai engine giống hệt nhau ở tầng này.
- `get_run_log_path` trước đây `read_to_string` cả file chỉ để kiểm tra rỗng (~29MB) → đổi sang `metadata().len()`.

**P0 + P1 ≈ 2.5 ngày công.** Đây là phần tin là xử lý được phần lớn cảm giác khó chịu hiện tại.

### 7.1 Cổng đo (bắt buộc, trước khi cam kết P3/P4)

Sau P0–P2b, đo và ghi lại:

- **Latency mở log: p95 và p99** (không phải trung bình — phân bố đuôi dài ở §0 khiến trung bình vô nghĩa).
- **Heap sau khi mở 10 run** rồi quay lại màn hình khác.
- **Lượng ghi đĩa** trên một run dài (`fs_usage`), trước/sau P0.
- **Frame rate khi scroll** một run ≥ 300 entry (Safari Web Inspector → Timelines → Rendering Frames).

P3-index và P4-virtualize chỉ được mở khi số đo không đạt mục tiêu. Cả hai đều đắt, không quyết bằng cảm giác.

---

## 8. Retention — không auto-xoá mặc định

818 MB nằm trong `.devdy/runs/` của từng project. Đây là **artifact người dùng có thể đang tham chiếu** (`get_run_log_path` cho phép copy path và mở trong FileViewer). Không được âm thầm can thiệp.

Đề xuất **"Storage management"** trong Settings:

- Breakdown theo project / run, sắp theo dung lượng.
- Preview trước khi hành động, chọn thủ công.
- Hành động: **delete** hoặc **gzip** log của run đã terminal.
- Auto-prune **chỉ là opt-in policy tường minh**, có số ngày / ngưỡng dung lượng, và **dry-run preview** trước lần chạy đầu.

---

## 9. Câu hỏi "có nên chuyển run log sang DB?" — kết luận

**Không lưu nội dung log vào SQLite. Nếu cần, chỉ đưa *index* vào DB — và chỉ sau khi số đo chứng minh là cần.**

### 9.1 Vì sao không lưu blob

- Update một cột TEXT 28 MB = **ghi lại cả row + khuếch đại WAL** → đúng bài toán O(n²) của F1, chỉ chuyển chỗ.
- File `.log` là artifact người dùng thấy được (`get_run_log_path`, `runs.rs:1231`). Và devdy vốn **mirror** transcript `.jsonl` của Claude/Codex — thứ nó không sở hữu. Đưa vào DB là **tách đôi nguồn sự thật** với CLI.
- 818 MB nhồi vào `devdy.db` khiến mọi query không liên quan (projects, todos, notes) trả giá: file lớn hơn, backup nặng hơn, VACUUM lâu hơn.

### 9.2 Index: hấp dẫn nhưng bị đánh giá thấp chi phí

Ý tưởng: `run_log_index(run_id, seq, byte_offset, byte_len, kind)` → mở run = query 80 row cuối → đọc đúng byte range → parse 80 dòng. Có nền cho virtual list thật và filter server-side.

Nhưng **log có thể bị `upsert_claude_session` / `upsert_codex_session_file` ghi lại từ transcript ngoài app**, nên index không phải append-only thuần. Phải có thêm:

- **file revision** gắn vào index; transcript đổi → index stale.
- **invalidation + rebuild** khi revision lệch.
- **migration** cho 876 log đã tồn tại.
- Dọn index khi **xoá run / xoá project**.
- **Fallback** khi index lỗi hoặc thiếu (đọc file trực tiếp).
- **Batch insert theo trang**, không ghi SQLite mỗi event.

Đây là lý do **không cam kết 3–4 ngày**. Xếp thành **spike sau P2b**:

1. Đo latency mở log p95/p99 và heap sau P0–P2b (§7.1).
2. Chỉ triển khai index nếu pagination theo byte **vẫn** không đạt mục tiêu, **hoặc** khi có nhu cầu filter/tìm kiếm server-side (nhu cầu này hiện chưa tồn tại).
3. Nếu làm: index theo file revision, batch insert, có đường fallback.
