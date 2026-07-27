# Test scenarios — Remote Control qua trình duyệt

## 1. Mục tiêu

Tài liệu này dùng để kiểm thử chức năng Remote Control theo luồng thật:

```text
Controller browser ↔ WSS Relay ↔ Devdy Host ↔ Run/Sidecar
```

Phạm vi gồm:

- Tạo và mở session link theo từng run.
- Xác thực bằng master password hoặc OTP.
- E2E handshake và kiểm tra host fingerprint.
- Đồng bộ lịch sử, stream, trạng thái run và composer.
- Gửi prompt, resume, cancel, permission và `AskUserQuestion`.
- Reconnect, session token, mất mạng và restart.
- Allow-list, rate limit, audit, cô lập run và bảo vệ secret.
- Khả năng sử dụng trên desktop/mobile browser.

## 2. Baseline theo code hiện tại

| Hạng mục | Giá trị hiện tại |
|---|---|
| Session scope | Một link chỉ điều khiển một `run_id` |
| OTP | 6 chữ số, TTL 120 giây |
| Số lần nhập OTP tối đa | 5 lần |
| Master password | Dùng thay OTP, chia sẻ cùng ngân sách 5 lần thử |
| Session token | Sliding idle 3 giờ, lưu ở browser `localStorage` |
| Reconnect window với link mới | 60 giây |
| Controller reconnect backoff | 500 ms, tăng dần tới 5 giây |
| Host reconnect backoff | 1 giây, tăng dần tới 30 giây |
| Keepalive browser | 15 giây |
| Zombie socket timeout | Khoảng 35 giây |
| Host command rate limit | 30 command/phút/controller |
| Relay coarse rate limit | Mặc định 60 frame/10 giây |
| Replay history | Tối đa 2.000 dòng |
| Controller/room | Tối đa 1 |
| Attachment | Tối đa 10 MB/file |
| Remote permission decision | Chỉ `allow_once`, `deny_once` |
| Command allow-list | 12 action, xem nhóm SEC bên dưới |

### Điểm đặc tả cần chốt trước release

`docs/srs-remote-control-relay.md` yêu cầu permission mode của remote luôn là
`default`. Tuy nhiên code hiện tại trong `remote/command.rs`,
`remote/handler.rs` và UI controller cho phép chọn, đồng bộ và truyền
`permission_mode`.

Các case `META-04` và `SEC-06` phải được giữ ở trạng thái **BLOCKED BY PRODUCT
DECISION** cho tới khi chốt một trong hai hành vi:

1. Khóa remote ở `default` đúng SRS; hoặc
2. Cho remote đổi permission mode và cập nhật SRS/security model.

## 3. Quy ước

### Mức ưu tiên

- **P0:** Luồng cốt lõi hoặc chốt bảo mật; fail thì không phát hành.
- **P1:** Tính ổn định và chức năng chính; phải pass trước release.
- **P2:** Edge case, UX hoặc tương thích; có thể xử lý theo risk acceptance.

### Loại kiểm thử

- **PW:** Playwright trên controller browser.
- **HOST:** Cần thao tác hoặc quan sát Devdy desktop.
- **INT:** Integration test ở protocol/relay/host.
- **CHAOS:** Cần ngắt mạng, restart process hoặc proxy lỗi.
- **SEC:** Cần gửi frame/payload đã chỉnh sửa ngoài UI bình thường.

### Điều kiện pass chung

- Không có uncaught exception hoặc console error trên browser.
- Không có plaintext prompt, attachment, OTP, master password, `link_secret`
  hoặc session token trong relay log.
- Không có command nào chạy sai `run_id`.
- Mỗi command accepted/rejected tạo đúng một audit record, trừ các frame
  handshake/keepalive không phải business command.
- UI cuối cùng phải hội tụ về trạng thái Host; không nhân đôi message sau replay
  hoặc reconnect.

## 4. Test data và môi trường

Chuẩn bị:

- Một Host bật Remote Control và kết nối relay thành công.
- `Run A` ở trạng thái idle/finished, đã có ít nhất 20 log entry.
- `Run B` khác project hoặc khác run để kiểm tra isolation.
- Một run có thể phát sinh permission cho `Bash`, `Edit`, `Write`.
- Một run phát sinh `AskUserQuestion` single-select và multi-select.
- Hai browser context độc lập để test controller thứ hai.
- File test: ảnh nhỏ, ảnh >10 MB, text nhỏ, file >10 MB.
- Khả năng restart relay và Host; khả năng chặn WebSocket bằng proxy/firewall.

Không dùng dữ liệu production hoặc prompt có secret trong môi trường test.

## 5. Ma trận test

### 5.1 Cấu hình Host và tạo session

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| HOST-01 | P0 | HOST/INT | Bật Remote Control với relay URL và token hợp lệ | Lưu config, bấm Enable | Host chuyển `running=true`, relay ghi `host_registered`, UI hiển thị Connected/Waiting phù hợp |
| HOST-02 | P0 | HOST/INT | Token Host sai | Lưu token sai, Enable | Relay từ chối; Host không báo Connected; lỗi rõ ràng, không retry dồn dập |
| HOST-03 | P1 | HOST | Relay URL sai scheme/host | Nhập HTTP, URL lỗi cú pháp hoặc domain không tồn tại | Không crash; validation hoặc lỗi kết nối rõ ràng; không lưu secret ra log |
| HOST-04 | P1 | HOST/INT | Disable khi đang có controller | Bấm Disable trong room ACTIVE | Controller bị ngắt; command mới không chạy; trạng thái Host về Off |
| HOST-05 | P0 | HOST/PW | Tạo link cho Run A | Mở Run A, bấm Remote, tạo link | Link có đủ 5 field hợp lệ, bind đúng Run A; không chứa OTP/master password |
| HOST-06 | P1 | HOST | Tạo link mới khi link cũ đang chờ | Tạo lại link trước khi join | Chỉ link/rendezvous mới có hiệu lực; link cũ bị từ chối hoặc revoke rõ ràng |
| HOST-07 | P0 | HOST/PW | End session đang ACTIVE | Host kết thúc session | Controller rời session ngay; token/link cũ không tái kết nối được |
| HOST-08 | P1 | HOST | Restart Devdy khi Remote Control đã Enable | Đóng/mở lại app | Agent tự đăng ký lại; config và trạng thái không làm lộ auth token |

### 5.2 Session link và màn hình entry

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| LINK-01 | P0 | PW | Mở deep-link đầy đủ | `page.goto(controllerUrlWithHash)` | Tự chuyển sang màn hình password/OTP, không cần paste thủ công |
| LINK-02 | P0 | PW | Paste URL đầy đủ | Paste link, bấm Continue | Parse thành công và bắt đầu handshake |
| LINK-03 | P1 | PW | Paste fragment `k=v&...` | Chỉ paste phần sau `#` | Parse thành công |
| LINK-04 | P1 | PW | Paste JSON hợp lệ | Paste object đủ 5 field | Parse thành công |
| LINK-05 | P0 | PW | Thiếu từng field bắt buộc | Lần lượt bỏ `relay_url`, fingerprint, rendezvous, run, secret | Không mở socket; hiển thị lỗi đúng field |
| LINK-06 | P0 | PW/SEC | Fingerprint/link secret sai định dạng | Dùng hex sai độ dài/ký tự | Bị từ chối trước hoặc trong handshake; không vào session |
| LINK-07 | P1 | PW | Link có khoảng trắng/newline khi copy | Thêm whitespace đầu/cuối | Trim và parse đúng; không thay đổi dữ liệu bên trong |
| LINK-08 | P1 | PW | Link đã revoke/hết hiệu lực | Mở link cũ | Báo link/session không còn hiệu lực; không reconnect vô hạn |
| LINK-09 | P1 | PW | Link Run B khi token Run A đang lưu | Đang có token A rồi mở link B | Link B được ưu tiên, token A bị clear, không vào nhầm Run A |
| LINK-10 | P2 | PW | Double click Continue | Click nhanh hai lần | Chỉ có một connection logic; không tạo hai room/socket hoạt động |

### 5.3 Handshake và xác thực

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| AUTH-01 | P0 | PW/HOST | Master password đúng | Nhập password đúng, Connect | `Connected`, fingerprint đúng, nhận session token và history |
| AUTH-02 | P0 | PW/HOST | OTP đúng | Không đặt master password; nhập OTP đang hiển thị | Kết nối thành công; OTP không xuất hiện trong storage/log |
| AUTH-03 | P0 | PW/HOST | Password/OTP sai một lần rồi đúng | Nhập sai, sau đó đúng | Lỗi inline; field được clear; attempts giảm; lần đúng vẫn vào được |
| AUTH-04 | P0 | PW/HOST | Sai đủ 5 lần | Gửi 5 giá trị sai | Session/link bị burn hoặc room đóng; lần thứ 6 không được xử lý |
| AUTH-05 | P0 | PW/HOST | OTP hết hạn 120 giây | Chờ quá TTL rồi nhập đúng OTP cũ | Bị từ chối; yêu cầu tạo/lấy OTP mới; không mint token |
| AUTH-06 | P0 | PW/SEC | Host fingerprint không khớp | Sửa fingerprint trong link | Handshake dừng, không gửi business command, hiển thị lỗi MITM/fingerprint |
| AUTH-07 | P0 | PW/SEC | `link_secret` sai | Sửa một nibble của secret | Không giải mã được auth gate; không vào session; không có oracle tiết lộ secret nào sai |
| AUTH-08 | P1 | PW | Bấm Connect liên tục | Điền đúng rồi click/Enter nhiều lần | Chỉ một auth attempt hợp lệ; không tiêu hao attempts bất thường |
| AUTH-09 | P1 | PW | Reload trong màn hình OTP | Reload trước khi xác thực | Không tự coi là authenticated; có thể tiếp tục an toàn hoặc báo link cần mở lại |
| AUTH-10 | P1 | PW | `localStorage` bị disable/full | Chặn storage rồi đăng nhập | Lần đầu vẫn hoạt động; reload yêu cầu xác thực lại, không crash |

### 5.4 Đồng bộ session, history và stream

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| SYNC-01 | P0 | PW | Join run đã có lịch sử | Kết nối Run A | Hiển thị đủ history theo thứ tự trước khi tiếp tục live |
| SYNC-02 | P0 | PW/HOST | Stream realtime | Host phát nhiều text/tool event | Browser cập nhật đúng thứ tự, không mất hoặc nhân đôi |
| SYNC-03 | P0 | PW | Replay rồi nhận live cùng lúc | Join khi run đang stream | Boundary history/live không trùng và không đảo thứ tự |
| SYNC-04 | P1 | PW/INT | History >2.000 dòng | Dùng log 2.500+ dòng | Chỉ replay 2.000 dòng cuối; UI vẫn responsive; có hành vi truncation xác định |
| SYNC-05 | P1 | PW | Unicode/Markdown/code/mermaid | Stream tiếng Việt, emoji, code block, bảng | Render đúng, không XSS, không hỏng layout |
| SYNC-06 | P1 | PW | Tool event dài | Stream input/output lớn | Có thể scroll/expand; không khóa main thread đáng kể |
| SYNC-07 | P0 | PW/HOST | Host và browser cùng quan sát | Gửi một prompt từ browser | Cả hai phía cùng thấy một user turn và một response |
| SYNC-08 | P0 | PW/SEC | Frame cipher lỗi | Inject cipher hỏng | Frame bị bỏ, `decodeErrors` tăng/báo lỗi; session không thực thi dữ liệu rác |
| SYNC-09 | P1 | PW | Usage/status badges | Kết nối và chờ refresh | Claude/Codex usage, context meter và trạng thái run khớp Host |

### 5.5 Composer và command

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| CMD-01 | P0 | PW/HOST | Start run từ idle | Nhập prompt, bấm Run | Host nhận đúng một `start_run`; response stream về browser |
| CMD-02 | P0 | PW/HOST | Resume run đã có session | Nhập prompt, bấm Resume | Reattach đúng session rồi gửi turn; không tạo run/session ngoài ý muốn |
| CMD-03 | P0 | PW/HOST | Follow-up khi run đang chạy | Gửi prompt khi running | Dùng `send_chat_message`; không restart run |
| CMD-04 | P0 | PW/HOST | Cancel | Bấm Cancel khi đang stream | Run dừng; UI về trạng thái có thể Run/Resume; audit accepted |
| CMD-05 | P1 | PW | Enter và Shift+Enter | Gõ multiline, thử phím | Enter gửi theo convention; Shift+Enter xuống dòng; không gửi rỗng |
| CMD-06 | P1 | PW/HOST | Slash command | Chọn command từ palette | Đúng slash command được gửi và xử lý như desktop |
| CMD-07 | P1 | PW/HOST | `@` mention | Chọn file project từ autocomplete | Path đúng project/run được gửi; file ngoài scope không xuất hiện |
| CMD-08 | P1 | PW/HOST | Attachment ảnh/file <10 MB | Attach rồi gửi | Preview đúng; Host nhận đúng name/MIME/base64; bỏ attachment sau send |
| CMD-09 | P0 | PW | Attachment >10 MB | Chọn file lớn | Browser từ chối trước khi gửi, thông báo giới hạn; không tăng memory kéo dài |
| CMD-10 | P1 | PW | Xóa attachment trước send | Attach rồi remove | Payload không còn attachment đã xóa |
| CMD-11 | P0 | PW/HOST | Double click Run/Resume | Click nhanh nhiều lần | Chỉ một turn được tạo; nút disabled trong thời gian sending |
| CMD-12 | P0 | PW/CHAOS | Mất mạng ngay sau bấm Run | Drop socket sau click | Sau reconnect, command không bị chạy hai lần; UI không kẹt giả Connected |
| CMD-13 | P0 | PW/SEC | Command mang `run_id` của Run B | Sửa payload qua test harness | Host từ chối `wrong_run`, audit rejected, Run B không thay đổi |
| CMD-14 | P1 | PW/HOST | Engine/model đổi từ browser | Chọn engine/model mới | Host composer phản chiếu đúng; model invalid được reset |

### 5.6 Metadata và parity hai chiều

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| META-01 | P1 | PW/HOST | Host đổi engine/model | Đổi trên desktop | Browser cập nhật không cần reload |
| META-02 | P1 | PW/HOST | Browser đổi engine/model | Đổi trên controller | Host cập nhật; không tạo vòng lặp/bounce command |
| META-03 | P1 | PW/HOST | Hai phía đổi gần đồng thời | Thay giá trị trong khoảng <500 ms | Hội tụ theo rule đã chốt; không nhấp nháy vô hạn |
| META-04 | P0 | PW/HOST/SEC | Browser đổi permission mode | Chọn mode khác `default`, gửi run | **BLOCKED BY PRODUCT DECISION**; expected phải theo quyết định ở §2 |
| META-05 | P1 | PW/HOST | Refresh model/slash/project files | Mở selector/palette/mention | Dữ liệu khớp Host và đúng run/project |

### 5.7 Permission và câu hỏi tương tác

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| PERM-01 | P0 | PW/HOST | Permission Bash/Edit/Write hiển thị | Trigger tool cần duyệt | Hiện tool, cwd/path, command hoặc diff đủ để quyết định |
| PERM-02 | P0 | PW/HOST | Allow once | Bấm Allow once | Tool chạy đúng một lần; prompt biến mất ở cả Host/browser |
| PERM-03 | P0 | PW/HOST | Deny once | Bấm Deny once | Tool không chạy; run nhận denial; prompt được resolve |
| PERM-04 | P0 | PW/SEC | Không có Allow always/Deny always | Kiểm UI và gửi payload giả | UI không có nút; Host từ chối `allow_always`/`deny_always` |
| PERM-05 | P0 | PW/HOST/SEC | Double response | Browser gửi lại cùng `request_id` hoặc Host trả lời trước | Chỉ response đầu áp dụng; response sau `duplicate_request` |
| PERM-06 | P0 | PW/CHAOS | Disconnect khi permission đang chờ | Drop mạng, reconnect | Permission vẫn hiện và có thể trả lời một lần sau reconnect |
| PERM-07 | P1 | PW/HOST | AskUserQuestion single-select | Chọn một option, submit | Answer map đúng question → answer |
| PERM-08 | P1 | PW/HOST | AskUserQuestion multi-select + Other | Chọn nhiều option và nhập Other | Chuỗi answer đúng thứ tự/quy ước, không gửi khi câu nào còn trống |
| PERM-09 | P1 | PW | Diff/input rất dài hoặc chứa HTML | Trigger payload đặc biệt | Render escaped, scroll được, không thực thi script |

### 5.8 Reconnect, lifecycle và resilience

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| NET-01 | P0 | PW | Reload sau xác thực | Reload URL không hash | Tự reconnect bằng token, không hỏi password/OTP, history không trùng |
| NET-02 | P0 | PW/CHAOS | Mất mạng <60 giây | Chặn WSS 10–30 giây rồi mở | UI hiện Reconnecting; tự về Connected; state hội tụ |
| NET-03 | P0 | PW/CHAOS | Half-open socket | Drop traffic không gửi FIN/RST | Watchdog phát hiện khoảng 35 giây và thay socket |
| NET-04 | P0 | PW/CHAOS | Mất mạng >60 giây với session token | Chặn >60 giây rồi mở | Reconnect durable bằng token nếu còn trong 3 giờ |
| NET-05 | P0 | PW/CHAOS | Restart relay | Restart service khi ACTIVE | Host/controller reconnect với backoff; không cần OTP nếu token còn hạn |
| NET-06 | P0 | PW/CHAOS | Restart Host | Restart Devdy khi session token còn hạn | Host rehydrate bound session; browser reconnect đúng Run A |
| NET-07 | P0 | PW | Token idle >3 giờ | Giả lập clock/storage quá hạn | Token bị clear; browser không tự vào session; yêu cầu xác thực lại |
| NET-08 | P0 | PW/HOST | End session rồi reload | Host end/revoke, browser reload | Token/link cũ bị từ chối; không tự tạo session mới |
| NET-09 | P1 | PW/CHAOS | Browser background/foreground mobile | Background >1 phút rồi foreground | Wake listener force reconnect nếu cần; transcript cập nhật đầy đủ |
| NET-10 | P1 | PW | Nhiều lần online/offline liên tục | Toggle mạng 10 lần | Tối đa một socket active; không reconnect storm, không leak timer |
| NET-11 | P1 | PW/INT | Idle nhưng vẫn connected | Không có run event >2 phút | Keepalive giữ room; UI vẫn Connected |
| NET-12 | P0 | PW/CHAOS | Host mất trong lúc command đang chờ response | Ngắt Host ngay sau send | Silent-host watchdog reconnect; command không chạy lặp ngoài idempotency rule |

### 5.9 Security, isolation, audit và abuse

Allow-list hiện tại:

```text
respond_permission, start_run, request_history, cancel_run,
send_chat_message, set_run_meta, list_runs, list_projects,
list_slash_commands, list_engine_models, list_project_files,
list_plan_usage
```

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| SEC-01 | P0 | SEC/INT | Action ngoài allow-list | Gửi `read_file`, `open_terminal`, action rỗng/case sai | Host từ chối `action_not_allowed`; đúng một audit rejected |
| SEC-02 | P0 | SEC/INT | Payload thiếu field bắt buộc | Thiếu/blank run, request, decision, text theo action | Host từ chối `malformed`; không gọi sidecar |
| SEC-03 | P0 | SEC/INT | Replay cipher/sequence cũ | Gửi lại frame command đã accepted | Không chạy business action lần hai; audit/idempotency đúng |
| SEC-04 | P0 | PW/SEC | Controller thứ hai | Context 2 mở cùng link khi context 1 ACTIVE | Context 2 bị từ chối; context 1 không bị chiếm session |
| SEC-05 | P0 | SEC/INT | Vượt 30 command/phút | Gửi 31+ command hợp lệ | Command vượt trần bị `rate_limited`, được audit, Host ổn định |
| SEC-06 | P0 | SEC/HOST | Nâng permission mode | Gửi `bypassPermissions`/`acceptEdits`/`plan` | **BLOCKED BY PRODUCT DECISION**; không release khi expected chưa chốt |
| SEC-07 | P0 | SEC/INT | Truy cập Run B qua link Run A | Gửi history/cancel/start/meta với Run B | Mọi action bị `wrong_run`; không rò history/metadata Run B |
| SEC-08 | P0 | SEC/OPS | Kiểm tra relay log | Chạy prompt chứa marker độc nhất | Log chỉ có metadata/cipher; không có marker, password, token, attachment |
| SEC-09 | P0 | PW/SEC | XSS trong stream/tool/file name | Gửi `<script>`, event handler, URL nguy hiểm | Chỉ render text; không execute; CSP không bị phá |
| SEC-10 | P0 | PW | Disconnect | Bấm Disconnect | Socket đóng, token localStorage bị xóa, về Link Entry |
| SEC-11 | P1 | SEC/OPS | TLS/WSS bắt buộc | Thử downgrade/mixed content/cert sai | Browser từ chối; không fallback plaintext |
| SEC-12 | P1 | SEC/INT | Frame quá lớn/flood | Gửi frame vượt giới hạn hoặc burst >relay limit | Relay chặn/đóng kết nối có kiểm soát; không OOM; Host khác không ảnh hưởng |
| SEC-13 | P0 | HOST/INT | Audit accepted/rejected | Thực hiện mỗi loại command và lỗi | Đủ time, device/room/run/action/result/reason; không có secret |

### 5.10 UX, mobile và compatibility

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| UX-01 | P1 | PW | Viewport điện thoại 390×844 | Chạy full smoke | Không tràn ngang; composer/permission dùng được; keyboard không che nút chính |
| UX-02 | P1 | PW | Tablet và desktop | 768×1024, 1440×900 | Layout thích nghi, transcript và composer không che nhau |
| UX-03 | P1 | PW | Chrome/Safari/Firefox hiện hành | Chạy P0 smoke | WebCrypto, WebSocket, storage và upload hoạt động nhất quán |
| UX-04 | P1 | PW | Mạng chậm/độ trễ 500–2.000 ms | Throttle network | Có trạng thái joining/handshaking/reconnecting; không tạo thao tác trùng |
| UX-05 | P2 | PW | Accessibility keyboard | Tab qua entry, Connect, composer, permission | Focus order hợp lý, Enter/Space hoạt động, dialog có accessible name |
| UX-06 | P2 | PW | Contrast/zoom 200% | Zoom và theme sáng/tối | Nội dung đọc được, control không bị mất |
| UX-07 | P1 | PW | Error message không lộ secret | Gây auth/parse/decode/network error | Chỉ báo nguyên nhân an toàn; không render full token/link secret |

### 5.11 Performance và soak

| ID | P | Type | Kịch bản | Bước chính | Kết quả mong đợi |
|---|---:|---|---|---|---|
| PERF-01 | P1 | PW/INT | Stream burst | Phát 1.000 event trong thời gian ngắn | Không mất thứ tự; browser vẫn tương tác; không crash |
| PERF-02 | P1 | PW | Transcript dài | Replay 2.000 dòng và tiếp tục 500 live event | Scroll/composer usable; memory không tăng vô hạn |
| PERF-03 | P1 | PW/OPS | Soak 8 giờ | Giữ idle/active xen kẽ, gửi command định kỳ | Không rớt room ngoài dự kiến, không leak socket/timer/memory |
| PERF-04 | P2 | INT/OPS | Nhiều Host độc lập | Kết nối nhiều Host/room test | Không route chéo room; relay CPU/memory trong giới hạn vận hành |
| PERF-05 | P1 | CHAOS/OPS | Relay restart lặp lại | Restart 10 lần cách nhau ngẫu nhiên | Tất cả Host/controller hồi phục; không room zombie/reconnect storm |

## 6. Bộ smoke bắt buộc cho mỗi build

Chạy theo thứ tự:

1. `HOST-01` — Host đăng ký relay.
2. `HOST-05` — tạo link cho Run A.
3. `LINK-01` — mở deep-link trực tiếp.
4. `AUTH-01` hoặc `AUTH-02` — xác thực thành công.
5. `SYNC-01` — history tải đúng.
6. `CMD-01` — gửi prompt và nhận marker response.
7. `PERM-01` + `PERM-02` — nhận và Allow once một tool.
8. `NET-01` — reload và reconnect không cần nhập lại password/OTP.
9. `SEC-07` — xác minh không điều khiển được Run B.
10. `SEC-10` — disconnect và xóa token.

Nếu bất kỳ case nào fail, build không đủ điều kiện phát hành.

## 7. Kịch bản Playwright P0 mẫu

### RC-E2E-P0-01 — Login, điều khiển và reconnect

**Precondition**

- Host đang online.
- Link bind Run A còn hiệu lực.
- Có master password hoặc OTP hợp lệ.

**Steps**

1. Tạo browser context sạch.
2. Mở deep-link.
3. Assert màn hình password/OTP xuất hiện.
4. Nhập secret hợp lệ và Connect.
5. Assert `Connected` và fingerprint rút gọn hiển thị.
6. Assert transcript chứa marker đã có trên Host.
7. Gửi prompt có correlation marker duy nhất:
   `REMOTE_E2E_<timestamp>`.
8. Chờ user turn và assistant response tương ứng.
9. Assert composer trở lại trạng thái Run/Resume.
10. Reload bằng URL `controller.html` không có hash.
11. Assert tự Connected, không hiện màn hình password/OTP.
12. Assert marker chỉ xuất hiện một lần trong transcript.
13. Assert browser console không có error.
14. Bấm Disconnect.
15. Assert về Link Entry và storage key session không còn.

**Expected**

- Toàn bộ assert pass.
- Relay có `join_ok`/`room_active` cho login và reconnect.
- Audit có đúng một accepted command cho prompt.

### RC-E2E-P0-02 — Sai password/OTP và giới hạn attempts

1. Mở link mới trong context sạch.
2. Gửi secret sai bốn lần; mỗi lần assert lỗi retryable và attempts giảm.
3. Lần thứ năm gửi sai.
4. Assert room/session bị đóng hoặc invalidated.
5. Gửi secret đúng cũ.
6. Assert không thể authenticate và không có session token.
7. Tạo link/OTP mới.
8. Assert authenticate thành công với secret mới.

### RC-E2E-P0-03 — Permission once-only

1. Kết nối Run A.
2. Gửi prompt làm phát sinh một tool permission an toàn trong fixture.
3. Assert browser hiển thị tool name, cwd/path và preview.
4. Assert không tồn tại nút Allow always/Deny always.
5. Bấm Allow once.
6. Assert tool chạy và prompt biến mất.
7. Replay cùng `request_id` qua security harness.
8. Assert tool không chạy lần hai và audit ghi `duplicate_request`.

### RC-E2E-P0-04 — Mất mạng và hội tụ trạng thái

1. Kết nối, gửi prompt tạo stream kéo dài.
2. Chặn WebSocket nhưng giữ page mở.
3. Assert UI chuyển Reconnecting hoặc watchdog thay socket trong ngưỡng.
4. Bỏ chặn trước 60 giây.
5. Assert Connected trở lại.
6. Assert history + live không mất/trùng message.
7. Gửi marker mới để chứng minh command channel hoạt động sau recovery.

## 8. Evidence cần lưu

Mỗi run test nên lưu:

- Playwright trace, screenshot khi fail và console log đã redact.
- Correlation marker của command, không lưu password/OTP/link secret.
- Relay log trong đúng time window.
- Host audit rows tương ứng.
- Trạng thái cuối của Run A và Run B.
- Browser/version, viewport, network profile.

Không đưa full controller link, master password, OTP, session token hoặc
`localStorage` raw value vào artifact/CI log.

## 9. Release gate đề xuất

- 100% P0 pass.
- Ít nhất 95% P1 pass; case fail phải có risk acceptance.
- Không còn case security bị blocked.
- `META-04`/`SEC-06` đã có product decision và SRS/code thống nhất.
- Soak tối thiểu 8 giờ không có reconnect storm, memory leak hoặc message
  duplication.
- Deep-link `LINK-01` phải pass vì đây là đường vào chính từ QR/copy link.
