# Remote E2E Crypto Spec — Devdy Remote Control (C1 / U2)

- **Mã:** RC-E2E-v1
- **Nguồn yêu cầu:** `docs/srs-remote-control-relay.md` — SEC-006 (PSK + ECDH, forward secrecy, thư viện chuẩn), SEC-002 (pairing PSK one-time), §5.1 (envelope `cipher`), CON-04 (relay không giải mã).
- **AC liên quan:** AC-19 (payload E2E-encrypt PSK+ECDH; relay log chỉ thấy ciphertext).
- **Tương thích chéo:** Rust (`remote-e2e/`, crate `dryoc`) ↔ TS (`src/remote/e2e/`, `libsodium-wrappers`). Cả hai dùng cùng primitive libsodium-compatible → cùng byte kết quả với cùng vector.

> Nguyên tắc BẮT BUỘC: **không tự viết primitive mật mã**. Mọi phép toán (X25519, BLAKE2b, XChaCha20-Poly1305) đều gọi qua thư viện chuẩn.

---

## 1. Mục tiêu bảo mật

| Mục tiêu | Cơ chế |
|---|---|
| Bảo mật payload (relay chỉ thấy ciphertext) | AEAD XChaCha20-Poly1305 |
| Chống MITM tại relay dù relay đọc được ECDH public | Trộn `psk` (out-of-band qua QR) vào KDF; không có `psk` thì không dựng được session key |
| Forward secrecy | Keypair X25519 **ephemeral** sinh mới mỗi phiên; lộ `psk` về sau không giải mã lại được traffic cũ vì cần thêm ephemeral secret đã hủy |
| Xác thực Host (SEC-006) | `host_fingerprint` = BLAKE2b(host ephemeral pub), Controller so khớp với giá trị đọc từ QR |
| Toàn vẹn + chống sửa version | Version byte đưa vào AEAD associated data |

Ngoài phạm vi U2: transport (WSS), pairing lifecycle, allow-list, audit — thuộc U1/U3.

---

## 2. Primitive & hằng số

| Thành phần | Thuật toán | Kích thước | libsodium name |
|---|---|---|---|
| KEX | X25519 (Curve25519 ECDH) | pub/secret 32B, shared 32B | `crypto_scalarmult`, `crypto_box_keypair` |
| KDF | BLAKE2b keyed (key = `psk`) | out 32B | `crypto_generichash` |
| AEAD | XChaCha20-Poly1305-IETF | key 32B, nonce 24B, tag 16B | `crypto_aead_xchacha20poly1305_ietf_encrypt/decrypt` |
| Fingerprint | BLAKE2b (không key) | out 32B → hex 64 ký tự | `crypto_generichash` |

Hằng số:
- `E2E_VERSION = 0x01`
- `CONTEXT = "devdy-remote-e2e/v1"` (ASCII, 19 byte) — domain separation cho KDF.
- `FP_CONTEXT = "devdy-host-fp/v1"` — domain separation cho fingerprint.
- `KEY_LEN = 32`, `NONCE_LEN = 24`, `TAG_LEN = 16`, `PUB_LEN = 32`.

---

## 3. Handshake (dựng session key)

Mỗi phiên, cả hai đầu tự sinh ephemeral keypair và trao **public key** cho nhau (qua khung điều phối của relay; public key không phải bí mật). `psk` KHÔNG bao giờ đi qua relay — nó tới Controller out-of-band qua QR (SEC-002/BR-003).

Ký hiệu: `A`, `B` là hai đầu; `pk_A`, `sk_A` là ephemeral pub/secret.

1. Mỗi đầu: `(pk, sk) = X25519_keypair()`.
2. Trao `pk` cho đầu kia.
3. Điểm DH chung: `dh = X25519(sk_mine, pk_theirs)` (32B). Curve25519 giao hoán nên hai đầu ra cùng `dh`.
4. **Sắp xếp** hai public key theo thứ tự byte tăng dần để cả hai đầu có cùng đầu vào bất kể vai trò:
   `pk_lo, pk_hi = sort_lex(pk_A, pk_B)`.
5. Nguyên liệu KDF: `material = CONTEXT || pk_lo || pk_hi || dh` (19 + 32 + 32 + 32 = 115 byte).
6. **Session key:** `K = BLAKE2b(key = psk, input = material, outlen = 32)`.

> Vì `psk` là **key** của BLAKE2b, kẻ tấn công đứng ở relay (biết `pk_A`, `pk_B`, có thể tự chen ephemeral của mình) vẫn không dựng được `K` nếu thiếu `psk` → chống MITM. Ephemeral keypair đảm bảo forward secrecy.

`host_fingerprint = hex( BLAKE2b(key = none, input = FP_CONTEXT || pk_host, outlen = 32) )`.
Controller so khớp fingerprint tính từ `pk_host` nhận qua relay với giá trị đọc từ QR; lệch → hủy phiên (SEC-006, FR-003 error flow).

---

## 4. Định dạng khung message (`cipher`)

Sau khi có `K`, mỗi message được seal thành một khung nhị phân rồi base64 (chuẩn, có padding) đặt vào trường `cipher` của envelope (§5.1 SRS).

```
frame = version(1B) || nonce(24B) || aead_ct
        aead_ct    = XChaCha20Poly1305_encrypt(key=K, nonce, plaintext, ad = version(1B))
        cipher     = base64_standard(frame)
```

- `version = 0x01`. Receiver đọc byte đầu; khác `0x01` → lỗi `unsupported version` (chừa đường nâng cấp).
- `nonce` = 24 byte **ngẫu nhiên** mỗi message (`randombytes`). Không gian 192-bit → xác suất trùng bỏ qua được; không tái sử dụng.
- `ad = [version]` được xác thực bởi Poly1305 → chống hạ cấp version.
- `aead_ct` gồm ciphertext + tag 16B (định dạng combined chuẩn libsodium: `ct || tag`).

Độ dài tối thiểu khung hợp lệ: `1 + 24 + 16 = 41` byte. Ngắn hơn → lỗi định dạng.

> Ephemeral public key KHÔNG nằm trong mỗi khung message: nó chỉ trao một lần ở handshake (§3). Điều này giữ khung message nhỏ và tách handshake khỏi message như spec cho phép ("điều chỉnh hợp lý nếu handshake tách khỏi message; ghi rõ trong spec").

---

## 5. API tối thiểu (gương Rust ↔ TS)

| Chức năng | Rust (`remote-e2e`) | TS (`src/remote/e2e`) |
|---|---|---|
| Sinh keypair | `Keypair::generate()` | `generateKeypair()` |
| Handshake | `Session::new(&psk, &my_kp, &their_pub)` | `deriveSession(psk, myKeypair, theirPub)` |
| Fingerprint | `host_fingerprint(&pub)` | `hostFingerprint(pub)` |
| Seal | `session.seal(plaintext) -> String(base64)` | `session.seal(plaintext) -> string` |
| Open | `session.open(cipher_b64) -> Vec<u8>` | `session.open(cipherB64) -> Uint8Array` |

Seal/open ánh xạ trực tiếp lên trường `cipher` opaque của envelope. Nonce do seal tự sinh (random) và nhúng trong khung.

---

## 6. Test vector dùng chung

`remote-e2e/vectors/*.json` là **dữ liệu test** (đánh dấu rõ `"_note": "TEST VECTOR — DO NOT USE IN PRODUCTION"`), không phải secret vận hành. Vector cố định:

- `psk` (hex 32B), `pk_a`/`sk_a`, `pk_b`/`sk_b` (hex 32B), `nonce` (hex 24B) → để khung ciphertext tất định.
- `plaintext` (utf-8) và `cipher` (base64) kỳ vọng.
- `session_key` (hex 32B) và `host_fingerprint` (hex) kỳ vọng.

Vì nonce trong vector cố định, seal(plaintext, nonce cố định) phải cho đúng `cipher`. Đây là điểm chốt interop: **Rust và TS cùng nạp vector, cùng ra một `cipher`, và open `cipher` của nhau ra đúng `plaintext`.**

Interop được chứng minh 2 chiều:
1. Rust seal(vector) == `cipher` kỳ vọng; TS open(`cipher`) == `plaintext`.
2. TS seal(vector) == `cipher` kỳ vọng; Rust open(`cipher`) == `plaintext`.

Do cùng byte `cipher`, (1)+(2) tương đương "Rust seal → TS open" và "TS seal → Rust open".

---

## 7. Deviation & ghi chú

- Spec SRS gợi ý khung `version | ephemeral_pub | nonce | aead_ct`. U2 **tách** `ephemeral_pub` ra handshake (§3) thay vì lặp trong mỗi khung — spec cho phép điều chỉnh và yêu cầu ghi rõ; lợi ích: khung nhỏ, một session key ổn định cho nhiều message, forward secrecy vẫn giữ nhờ ephemeral hủy sau phiên.
- KDF chọn BLAKE2b keyed (đúng gợi ý SRS) thay vì HKDF để cả `dryoc` và `libsodium-wrappers` có sẵn cùng hàm `crypto_generichash`, đảm bảo cùng byte.
