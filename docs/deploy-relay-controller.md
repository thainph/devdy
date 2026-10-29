# Deploy Relay + Controller lên VPS PROD (runbook)

Hướng dẫn tái sử dụng để build lại và deploy **relay** (WebSocket bridge, Rust) và
**controller web** (`controller.html`, U4) lên VPS production.

> Quy ước: relay chạy `localhost` sau nginx (Topology B), controller là web tĩnh
> cùng domain. Relay **không** giải mã payload — chỉ forward opaque cipher, nên
> đổi logic engine/permission mode **không** cần build lại relay; chỉ cần build lại
> relay khi sửa mã trong `relay/src`.

---

## 1. Thông tin hạ tầng (đã xác minh)

| Hạng mục | Giá trị |
|---|---|
| VPS | `<VPS_IP>`, SSH port **<SSH_PORT>**, user **<user>** (key đã cấu hình) |
| Domain controller/relay | `relay.example.com` (TLS do Certbot quản lý) |
| Nginx site | `/etc/nginx/sites-available/relay.example.com` |
| Relay bind | `127.0.0.1:8787` (WSS proxy tại `location /ws`) |
| Thư mục dự án | `/projects/relay` (owner `deploy`): `releases/<TS>/` (controller web + `bin/devdy-relay`), `current` → release đang chạy, `shared/` |
| Relay service | `devdy-relay.service` (systemd, user `devdyrelay`), drop-in `projects.conf`: `ExecStart=/projects/relay/current/bin/devdy-relay` |
| Relay env | `/etc/devdy-relay.env` (chứa `RELAY_HOST_TOKEN` — **không commit**) |
| Quyền `deploy` | ghi `/projects/relay`; `sudo` không mật khẩu chỉ cho `systemctl start/stop/restart devdy-relay`, `systemctl --no-pager status devdy-relay`, `journalctl --no-pager -u devdy-relay -n 200` |

Nginx (tóm tắt): `root /projects/relay/current; index controller.html;` — `location /ws`
proxy sang `127.0.0.1:8787` (có header `Upgrade/Connection`, `proxy_read_timeout 3600s`);
`location /` dùng `try_files $uri /controller.html`.

SSH nhanh:
```bash
ssh -p <SSH_PORT> <user>@<VPS_IP>
```

---

## 2. Build artifact (trên máy local)

### 2.1. Controller web (luôn cần khi sửa FE/controller)
```bash
cd /path/to/Tools/devdy
npm run build:vite       # vue-tsc --noEmit && vite build  ->  dist/
```
Output `dist/` gồm `controller.html`, `index.html`, `assets/…` (tên file có hash).
`controller.html` tham chiếu asset theo đường dẫn tuyệt đối `/assets/...` nên phục
vụ tại web root là chạy.

### 2.2. Relay binary Linux (chỉ khi sửa `relay/src`)
Máy macOS không có musl linker → build binary tĩnh Linux bằng Docker
(`messense/rust-musl-cross`). Cần Docker daemon đang chạy.
```bash
cd /path/to/Tools/devdy/relay
docker run --rm -v "$PWD":/home/rust/src \
  messense/rust-musl-cross:x86_64-musl \
  cargo build --release
# Output: relay/target/x86_64-unknown-linux-musl/release/devdy-relay
```
Lần đầu pull image (~2.4GB) là chậm; image đã được cache sẵn trên máy này nên các
lần sau chỉ compile incremental (~vài giây). Kiểm tra image đã có:
```bash
docker images | grep rust-musl-cross     # messense/rust-musl-cross:x86_64-musl
```

> ⚠️ Chỉ chậm ở lần pull image đầu tiên, KHÔNG phải do cargo. Nếu lỡ đang pull mà
> muốn hủy: `pkill -f rust-musl-cross`, đợi pull xong (image vẫn được cache) rồi
> chạy lại lệnh build — lúc này sẽ nhanh.

**Phương án thay thế (không dùng Docker):** cài cross-toolchain prebuilt qua Homebrew
rồi build native — cũng nhanh:
```bash
brew install messense/macos-cross-toolchains/x86_64-unknown-linux-musl
cd relay
CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-unknown-linux-musl-gcc \
CC_x86_64_unknown_linux_musl=x86_64-unknown-linux-musl-gcc \
  cargo build --release --target x86_64-unknown-linux-musl
```

Kiểm tra binary có mới hơn source không (nếu source `relay/src/*.rs` mới hơn binary
thì phải build lại):
```bash
stat -f '%m %N' relay/target/x86_64-unknown-linux-musl/release/devdy-relay
stat -f '%m %N' relay/src/*.rs relay/Cargo.toml | sort -rn | head
```

---

## 3–4. Deploy (controller + relay cùng một release)

Relay và controller nằm chung một release, đổi cùng lúc bằng symlink `current`.
Không cần root: user `deploy` có sẵn quyền restart service.

```bash
SSH='ssh -p <SSH_PORT>'; H=deploy@<VPS_IP>
TS=$(date -u +%Y%m%d-%H%M%S); R=/projects/relay/releases/$TS
BIN=relay/target/x86_64-unknown-linux-musl/release/devdy-relay

# 1) Upload release mới (chưa ảnh hưởng bản đang chạy)
rsync -az -e "$SSH" dist/ $H:$R/
rsync -az -e "$SSH" "$BIN" $H:$R/bin/devdy-relay

# 2) Trỏ current sang release mới (atomic) + restart relay
$SSH $H "set -e
  chmod 0755 $R/bin/devdy-relay
  ln -sfn $R /projects/relay/current.tmp && mv -T /projects/relay/current.tmp /projects/relay/current
  sudo -n /usr/bin/systemctl restart devdy-relay
  sleep 2; sudo -n /usr/bin/systemctl --no-pager status devdy-relay | head -5"
```

Chỉ sửa controller (không sửa `relay/src`)? Vẫn tạo release mới, copy
`bin/devdy-relay` từ release hiện tại sang (`cp -a /projects/relay/current/bin $R/`).
Lúc này không bắt buộc restart service, vì binary không đổi.

---

## 5. Verify sau deploy

```bash
ssh -p <SSH_PORT> <user>@<VPS_IP> '
  echo "--- service ---"; systemctl is-active devdy-relay
  echo "--- log gần nhất ---"; journalctl -u devdy-relay -n 20 --no-pager
'
# Health web + WSS
curl -sS -o /dev/null -w "controller %{http_code}\n" https://relay.example.com/controller.html
```
Test end-to-end: mở app desktop → tạo remote link → mở link trên controller web →
kiểm tra engine/permission mode hiển thị đúng và đồng bộ 2 chiều realtime.

---

## 6. Rollback

Trỏ `current` về release trước rồi restart:
```bash
ssh -p <SSH_PORT> deploy@<VPS_IP> '
  ls -t /projects/relay/releases | head      # chọn release trước đó
  # ln -sfn /projects/relay/releases/<TS_CU> /projects/relay/current.tmp && mv -T /projects/relay/current.tmp /projects/relay/current
  # sudo -n /usr/bin/systemctl restart devdy-relay
'
```

---

## 7. Ghi chú an toàn

- VPS là **production dùng chung** (còn web Node.js/PHP khác chiếm 80/443). Chỉ đụng
  đúng `devdy-relay.service`, `/projects/relay/`,
  `/etc/nginx/sites-available/relay.example.com`. **Không** đụng service/site khác.
- **Không** commit `relay.env` / `RELAY_HOST_TOKEN` hay bất kỳ secret nào.
- Mỗi lần deploy là một release mới, nên release cũ chính là bản backup. Dọn bớt
  release cũ định kỳ (giữ vài bản gần nhất): `ls -t /projects/relay/releases`.
- Env relay (mặc định): `RELAY_BIND=127.0.0.1:8787`, `RELAY_PING_INTERVAL_SECS=15`,
  `RELAY_PEER_LIVENESS_TIMEOUT_SECS=45`, `RELAY_MAX_ROOMS=256`,
  `RELAY_MAX_MESSAGE_BYTES=33554432` (32 MiB — phải lớn hơn một lượt có ảnh
  đính kèm: ảnh 10 MiB qua base64 hai lần ≈ 18 MiB). Chi tiết xem
  `relay/README.md`. Các biến cũ `PAIR_TTL_SECS`, `RELAY_ROOM_IDLE_TIMEOUT_SECS`,
  `RELAY_RECONNECT_WINDOW_SECS` không còn dùng (để lại trong env cũng không sao).
- Relay, controller web và app desktop dùng chung một giao thức kết nối lại:
  khi nâng cấp, deploy **relay + controller trước**, rồi mới cập nhật app desktop.
  App desktop cũ vẫn chạy được với relay mới, nhưng app mới **không** chạy với
  relay cũ (relay cũ chỉ route khi đủ `handshake_done`, và không đánh dấu ack
  `create_pair` bằng `code:"armed"`).
```
