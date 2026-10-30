# Devdy Remote Control Relay

WebSocket bridge for Devdy's Remote Control feature (SRS-RC-C1, **Unit U1**).

The relay bridges a **Host** (Devdy on the dev machine) and a single
**Controller** (phone/other PC) by `room_id`. It **never** decrypts, stores, or
logs the business payload — it forwards the opaque `cipher` field verbatim
(SEC-006 / CON-04 / NFR-005). Both peers connect *outbound* to the relay, so
NAT/firewalls are never an obstacle.

This is a standalone Rust crate. It is **not** part of the Tauri Cargo
workspace and has no dependency on `src-tauri/`.

## Topology B (the only supported deployment)

```
[Devdy Host] --outbound WSS--> nginx/apache (TLS :443) --proxy--> relay 127.0.0.1:8787 <--outbound WSS-- [Controller]
```

The relay binds **localhost only**. TLS is terminated by the reverse proxy
already running on the VPS (CON-01, SEC-009). The relay never holds a
certificate and never opens a public port.

## Protocol (summary — full spec: `docs/srs-remote-control-relay.md` §5)

Outermost envelope the relay can see:

```jsonc
{ "t": "stream|permission_request|cmd|join|register_host|control|...",
  "room_id": "rm_...",   // relay routes on this only
  "seq": 1234,           // loss/dup detection (metadata; never blocking)
  "cipher": "<base64>"   // opaque E2E payload — relay forwards, never reads
}
```

Control frames handled by the relay:

| `t`              | Direction        | Purpose |
|------------------|------------------|---------|
| `register_host`  | Host → Relay     | Authenticate with `auth_token`; hold long-lived. |
| `create_pair`    | Host → Relay     | Listen on a rendezvous `pair_code` until this connection closes (a new code replaces the old one in the same `slot`). Acked with `room_ready{code:"armed"}`. Optional `slot`: absent = the session, `"login"` = password login (well-known code `devdy-login-v1`). |
| `join`           | Controller → Relay | Join the Host listening on `pair_code`. A newer join replaces the previous Controller (`error{superseded}` to the old one). |
| `room_ready`     | Relay → both     | Room opened, carries `room_id`. |
| `peer_left`      | Relay → survivor | The other peer left / was cut. Re-join. |
| `revoke`         | Host → Relay     | Close a room; empty `room_id` = stop listening in `slot` (default: the session) and close its room. |
| `keepalive`      | either → Relay   | Echoed back; keeps idle proxies from reaping the socket. |
| `error`          | Relay → sender   | `{ code, message }` — auth/routing error. |

Model (deliberately minimal — reconnect = re-join):

- Rooms route as soon as they exist; authentication is end-to-end (OTP /
  session token between Host and Controller), not a relay state.
- One room per code, newest join wins — a phone that reconnects on a fresh
  socket while its old one is half-open is never refused.
- Liveness is per **connection** and is the relay's only timeout: it pings
  every `RELAY_PING_INTERVAL_SECS` (browsers answer natively, even in a
  background tab) and closes any connection that delivers no bytes for
  `RELAY_PEER_LIVENESS_TIMEOUT_SECS`. Closing a connection drops its listen and
  room, so nothing outlives a dead peer.
- Legacy `resume` → `error{resume_invalid}`; legacy `handshake_done` → ignored.
- Password login: while a master password is set, the Host also listens in the
  `login` slot. The Controller joins it with only the password; the E2E key is
  derived from the password (PBKDF2 → BLAKE2b), the Host rate limits wrong
  attempts and answers a correct one with a full session link + token. The
  relay just routes; `room_ready` to the Host carries `slot` so it can tell.

## Two-way sync store

Same binary, same `/ws` endpoint, but a **separate connection** from Remote
Control (design: `two-way-sync` §3 / §9.2). Devices of a sync group push sealed
ops and pull them back by cursor, so they never need to be online at the same
time. The relay only stores ciphertext and never logs op blobs or blob data.

| `t` | Direction | Purpose |
|---|---|---|
| `sync_hello{auth_token, group_id, device_id, device_token, create?}` | Device → Relay | First frame. `auth_token` = `RELAY_HOST_TOKEN`. `create=true` creates the group (an existing group is just a hello). An unknown device is registered on first hello (TOFU on `sha256(device_token)`); a known one must match. → `sync_welcome{last_seq}` or `error{auth_failed\|group_not_found\|device_revoked\|sync_disabled}` and the connection closes. |
| `sync_push{items:[{key, blob}]}` | Device → Relay | Relay assigns per-group monotonic `seq`s, deletes older ops with the same `key`, all or nothing. → `sync_ack{last_seq}`. Errors: `quota_exceeded`, `too_large`. |
| `sync_notify{last_seq}` | Relay → other devices | Sent to every other connected device of the group after a push. |
| `sync_pull{after_seq, limit}` | Device → Relay | → `sync_ops{items:[{seq, device_id, blob}], last_seq, more}`. At most 500 ops / ~8 MiB per page. Set the cursor to `last_seq` and pull again while `more`. |
| `blob_put{sha, data}` | Device → Relay | `data` = base64 of the sealed bytes, `sha` = lowercase hex sha256 of those bytes (verified). Idempotent. → `blob_ack{sha}`. Errors: `sha_mismatch`, `too_large`, `quota_exceeded`. |
| `blob_get{sha}` | Device → Relay | → `blob_data{sha, data}` or `error{blob_not_found}`. |
| `sync_revoke{device_id}` | Device → Relay | Any device of the group revokes a device (itself included); its live connections get `error{device_revoked}` and close, its hello fails from then on. → `control{code:"ok"}`. |

After `sync_welcome` only these frames and `keepalive` are accepted on the
connection (anything else → `error{forbidden}`); sync frames on a Remote
Control connection are refused the same way. An optional `id` on any request
is echoed on its response or error. Push/pull/blob frames are not rate limited
(an initial upload sends many); `sync_hello`/`sync_revoke` count as control
frames. While sync is enabled the WebSocket message limit is raised, if
needed, to fit one maximum-size `blob_put`.

**Storage.** SQLite file at `RELAY_SYNC_DB`; in production
`/projects/relay/shared/sync.db` (survives releases; the directory must be
writable by the `devdyrelay` user — `ReadWritePaths=` in the unit). Tables:
`groups` (head seq + byte usage), `devices`, `ops`, `blobs`. It runs in WAL
mode, so a copy of the `.db` file alone is not a consistent backup — back up
with `sqlite3 /projects/relay/shared/sync.db ".backup '/path/sync-$(date +%F).db'"`
(safe while the relay runs). The content is end-to-end encrypted, but the file
is the only server-side copy of every group's history; devices keep full local
copies, so losing it means re-uploading, not data loss.

## Configuration (environment)

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `RELAY_HOST_TOKEN` | **yes** | — | Shared secret for `register_host`. Relay refuses to start if unset. |
| `RELAY_BIND` | no | `127.0.0.1:8787` | Bind address (keep it localhost). |
| `RELAY_RATE_LIMIT_MAX` | no | `60` | Anti-flood for control frames (register/create_pair/join/…) per window. Data frames and keepalives are not limited. |
| `RELAY_RATE_LIMIT_WINDOW_SECS` | no | `10` | Rate-limit window. |
| `RELAY_PING_INTERVAL_SECS` | no | `15` | WebSocket ping cadence to every connection. |
| `RELAY_PEER_LIVENESS_TIMEOUT_SECS` | no | `45` | Close a connection that sent no bytes (frames, pongs) for this long. |
| `RELAY_MAX_ROOMS` | no | `256` | Ceiling on listeners + rooms. Everything dies with its connection; this only guards against a misbehaving Host. |
| `RELAY_MAX_MESSAGE_BYTES` | no | `33554432` | Largest WebSocket message accepted (32 MiB). Must stay above the worst legitimate frame: a 10 MiB image base64'd into the payload and then again into the ciphertext lands near 18 MiB. |
| `RELAY_SYNC_DB` | no | — | SQLite file of the sync store (production: `/projects/relay/shared/sync.db`). Unset = sync disabled (`error{sync_disabled}`). |
| `RELAY_SYNC_MAX_GROUP_BYTES` | no | `2147483648` | Storage cap per sync group (op blobs + blobs). Over it → `quota_exceeded`. |
| `RELAY_SYNC_MAX_BLOB_BYTES` | no | `4194368` | Largest blob (sealed bytes, 4 MiB + 64); an op blob may be at most the base64 size of this. |
| `RUST_LOG` | no | `info` | Log level (metadata only; payloads never logged). |

No secret is ever hardcoded or committed (SEC-001).

## Build

```bash
cargo build --release        # -> target/release/devdy-relay (single static-friendly binary)
cargo test                   # unit + real in-process WebSocket integration tests
cargo clippy --all-targets -- -D warnings
```

### Fully static binary (musl) for the VPS (NFR-008)

Deploy needs only **one binary + a systemd unit** — no runtime installed on the
VPS. For a self-contained, portable Linux binary, build against musl:

```bash
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
# -> target/x86_64-unknown-linux-musl/release/devdy-relay
```

Or without a local musl toolchain:

```bash
docker run --rm -v "$PWD":/home/rust/src messense/rust-musl-cross:x86_64-musl cargo build --release
```

(The default glibc build also works if the VPS glibc matches; musl removes that
coupling. SQLite is compiled in via `rusqlite`'s `bundled` feature, so no
system libsqlite is needed.)

## Deploy (Topology B)

Files in `deploy/`:

- `relay.service` — systemd unit: unprivileged user, `Restart=always`
  (recover <=10s, NFR-006), `MemoryMax=128M`, and sandboxing (SEC-010).
- `relay.env.example` — copy to `/etc/devdy-relay.env`, set `RELAY_HOST_TOKEN`,
  `chmod 0640`. **Never commit the real file.**
- `nginx-relay.conf.example` — WSS reverse proxy with `Upgrade`/`Connection`
  headers and long read timeout (INT-002).

```bash
# On the VPS:
sudo useradd --system --no-create-home --shell /usr/sbin/nologin devdyrelay
sudo install -m 0755 devdy-relay /usr/local/bin/devdy-relay
sudo install -m 0640 -o devdyrelay -g devdyrelay relay.env /etc/devdy-relay.env  # edit token first
sudo cp deploy/relay.service /etc/systemd/system/devdy-relay.service
sudo systemctl daemon-reload && sudo systemctl enable --now devdy-relay
# then wire the nginx subdomain (see nginx-relay.conf.example) and reload nginx
```

## Scope note (Unit U1)

This crate implements the relay (and the two-way sync store) only: the WSS server, control frames, room
lifecycle, `room_id` routing of opaque envelopes, auth, pairing TTL,
`MAX_CONTROLLER=1`, revoke, coarse rate-limit, and structured logging.

E2E crypto (U2), the Host agent (U3), and the Controller web client (U4) live
elsewhere. The relay is deliberately payload-agnostic.
