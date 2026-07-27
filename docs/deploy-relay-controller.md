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
| Relay binary | `/usr/local/bin/devdy-relay` |
| Relay service | `devdy-relay.service` (systemd, `Restart=always`, user không đặc quyền) |
| Relay env | `/etc/devdy-relay.env` (chứa `RELAY_HOST_TOKEN` — **không commit**) |
| Controller web root | `/var/www/relay/` (mirror nguyên thư mục `dist/`) |

Nginx (tóm tắt): `root /var/www/relay; index controller.html;` — `location /ws`
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
npm run build            # vue-tsc --noEmit && vite build  ->  dist/
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

## 3. Deploy controller (web tĩnh, rủi ro thấp)

```bash
# 1) Backup web root hiện tại (đặt tên theo timestamp trên VPS)
TS=$(ssh -p <SSH_PORT> <user>@<VPS_IP> 'date +%Y%m%d-%H%M%S')
ssh -p <SSH_PORT> <user>@<VPS_IP> "cp -a /var/www/relay /var/www/relay.backup-$TS"

# 2) Đồng bộ dist/ -> web root (mirror; --delete dọn asset cũ theo hash)
rsync -az --delete -e 'ssh -p <SSH_PORT>' \
  /path/to/Tools/devdy/dist/ \
  <user>@<VPS_IP>:/var/www/relay/

# 3) (tuỳ chọn) chuẩn hoá owner về root (rsync -a giữ uid local)
ssh -p <SSH_PORT> <user>@<VPS_IP> 'chown -R root:root /var/www/relay'
```

Không cần reload nginx (chỉ đổi file tĩnh). Verify:
```bash
# hash asset trong controller.html phải khớp local, và file asset tồn tại
grep -oE '/assets/controller-[^"]+\.js' /path/to/Tools/devdy/dist/controller.html
ssh -p <SSH_PORT> <user>@<VPS_IP> '
  grep -oE "/assets/controller-[^\"]+\.js" /var/www/relay/controller.html
  ls -la /var/www/relay/assets/controller-*.js
  curl -sS -o /dev/null -w "controller.html %{http_code}\n" https://relay.example.com/controller.html
'
```

---

## 4. Deploy relay binary (có restart service)

```bash
BIN=/path/to/Tools/devdy/relay/target/x86_64-unknown-linux-musl/release/devdy-relay

# 1) Copy binary mới lên thư mục tạm
scp -P <SSH_PORT> "$BIN" <user>@<VPS_IP>:/tmp/devdy-relay.new

# 2) Backup binary cũ, thay, restart (thao tác nhanh để giảm downtime)
ssh -p <SSH_PORT> <user>@<VPS_IP> '
  set -e
  TS=$(date +%Y%m%d-%H%M%S)
  cp -a /usr/local/bin/devdy-relay /usr/local/bin/devdy-relay.backup-$TS
  systemctl stop devdy-relay
  install -m 0755 /tmp/devdy-relay.new /usr/local/bin/devdy-relay
  rm -f /tmp/devdy-relay.new
  systemctl start devdy-relay
  sleep 1
  systemctl is-active devdy-relay
  systemctl status devdy-relay --no-pager | head -8
'
```
> Lưu ý: `systemd Restart=always` — nếu binary lỗi khởi động, service sẽ loop
> restart. Xem log ngay sau khi start (mục 5) và rollback nếu cần (mục 6).

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

**Controller:**
```bash
ssh -p <SSH_PORT> <user>@<VPS_IP> '
  ls -dt /var/www/relay.backup-* | head        # chọn bản gần nhất
  # rm -rf /var/www/relay && cp -a /var/www/relay.backup-<TS> /var/www/relay
'
```

**Relay binary:**
```bash
ssh -p <SSH_PORT> <user>@<VPS_IP> '
  systemctl stop devdy-relay
  cp -a /usr/local/bin/devdy-relay.backup-<TS> /usr/local/bin/devdy-relay
  systemctl start devdy-relay && systemctl is-active devdy-relay
'
```

---

## 7. Ghi chú an toàn

- VPS là **production dùng chung** (còn web Node.js/PHP khác chiếm 80/443). Chỉ đụng
  đúng `devdy-relay.service`, `/usr/local/bin/devdy-relay`, `/var/www/relay/`,
  `/etc/nginx/sites-available/relay.example.com`. **Không** đụng service/site khác.
- **Không** commit `relay.env` / `RELAY_HOST_TOKEN` hay bất kỳ secret nào.
- Luôn backup trước khi ghi đè (mục 3.1, 4.2).
- Dọn backup cũ định kỳ để khỏi đầy đĩa: `ls -dt /var/www/relay.backup-*`,
  `ls -t /usr/local/bin/devdy-relay.backup-*`.
- Env relay (mặc định): `RELAY_BIND=127.0.0.1:8787`, `PAIR_TTL_SECS=60`,
  `RELAY_ROOM_IDLE_TIMEOUT_SECS=3600`. Chi tiết xem `relay/README.md`.
```
