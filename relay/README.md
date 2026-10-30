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

## Sync groups (the relay is a pipe)

Same binary, same `/ws` endpoint, but a **separate connection** from Remote
Control (design: `on-demand-sync` §P1). The relay **stores no sync data** — no
ops, no blobs, no history, no quota. It only authenticates the devices of a
sync group and forwards sealed messages between the two online devices of that
group; either device asks, the other answers. Messages are never stored and
never logged.

A group has **at most 2 active (non-revoked) devices**.

| `t` | Direction | Purpose |
|---|---|---|
| `sync_hello{auth_token, group_id, device_id, device_token, create?, ticket?}` | Device → Relay | First frame. `auth_token` = `RELAY_HOST_TOKEN`. `create=true` on an unknown group creates it and registers this device as its **creator**. A known device must match its pinned `sha256(device_token)` (`create=true` from a known device is an idempotent hello). An unknown device joins an existing group only with a one-time `ticket` registered by a member (`sync_invite`), and only while the group has fewer than 2 active devices; `create=true` from it → `group_exists`. → `sync_welcome{creator_device}` or `error{auth_failed\|group_not_found\|group_exists\|ticket_required\|ticket_invalid\|group_full\|device_revoked\|sync_disabled}` and the connection closes. `group_full` is only reported to a device holding a valid ticket, and the ticket stays unused. |
| `sync_invite{ticket_hash, ttl_secs}` | Device → Relay | A member registers a join ticket: `ticket_hash` = lowercase hex `sha256(ticket)`, `ttl_secs` 1..600 (default 600). The ticket itself travels device→device, never to the relay before the joiner's hello. One use; at most 16 open per group (expired/used ones are pruned). → `control{code:"ok"}`. Errors: `bad_request`, `quota_exceeded`. |
| `sync_revoke{device_id, successor?}` | Device → Relay | A device may revoke itself; only the group's creator may revoke another device (else `forbidden`). The revoked device's live connections get `error{device_revoked}` and close, its hello fails from then on (with or without a ticket), and its slot is free for a new device. When the **creator revokes itself** the role moves to `successor` if that is an active member, else to the oldest active member (registration time, ties by device id); no member left = the group is deleted (see *Abandoned groups*). → `control{code:"ok"}`. |
| `sync_group_info{creator_device}` | Relay → other devices | Sent to the group's other connected devices when the creator role moved; devices that were offline learn it from their next `sync_welcome`. |
| `sync_peers{}` | Device → Relay | → `sync_peers{devices:[device_id, ...]}`: the group's devices with a live sync connection, excluding the caller (deduplicated, sorted). |
| `sync_peer_send{to_device, data}` | Device → Relay | Forward `data` (base64 of a message sealed with the group key) to `to_device`'s most recent live connection in the **same group**, as `sync_peer_msg`. → `sync_peer_ack{}` once queued for the target. Errors: `peer_offline` (no live connection, revoked, not a member, the sender itself, or the target's queue did not drain within 60 s — that target is then disconnected), `too_large` (`data` longer than the base64 of `RELAY_SYNC_MAX_BLOB_BYTES`), `bad_request` (missing `to_device` / `data`). Never stored, never logged. |
| `sync_peer_msg{from_device, data}` | Relay → target device | A forwarded `sync_peer_send` (no `id`). |

Removed with the old relay store: `sync_push`, `sync_ack`, `sync_pull`,
`sync_ops`, `sync_notify`, `blob_put`, `blob_ack`, `blob_get`, `blob_data`,
`blob_release`, `blob_released`. Any of them → `error{code:"unsupported"}`
(the connection stays open).

After `sync_welcome` only these frames and `keepalive` are accepted on the
connection (anything else → `error{forbidden}`); sync frames on a Remote
Control connection are refused the same way. An optional `id` on any request
is echoed on its response or error. `sync_peer_send` is not rate limited (a
folder sync answers with many messages); every other sync frame counts as a
control frame. A target is only given another message once the frames still
queued for its socket are under 16 MiB (`MAX_QUEUED_REPLY_BYTES`); a target
that stops reading for 60 s is disconnected. While sync is enabled the
WebSocket message limit is raised, if needed, to fit one maximum-size
`sync_peer_send`.

**Storage.** SQLite file at `RELAY_SYNC_DB`, e.g. `/var/lib/devdy-relay/sync.db`
(keep it outside the release directory; the directory must be writable by the
relay's service user — `ReadWritePaths=` in the unit). It holds only what
authentication needs: `groups` (creator device), `devices` (pinned token
hashes, revoked flags — a revoked id can never register again) and `invites`
(join ticket hashes). No sync data ever reaches it. Older databases are
upgraded at startup: `creator_device` is added if missing (set to the group's
earliest active registered device), and the tables of the former op / blob
store (`ops`, `blobs`, `epochs`, `store_epoch`, plus the `last_seq` / `bytes`
columns of `groups`) are dropped, followed by one `VACUUM`. That runs once,
before the relay serves, and logs only the sizes,
`event="sync_store_migrated" before_bytes after_bytes` (a failed `VACUUM` is
logged as `sync_vacuum_failed` and the relay starts anyway; the freed pages are
reused). The file runs in WAL mode; losing it only means devices pair again.

**Abandoned groups.** When a `sync_revoke` leaves a group without any
non-revoked device (the last member revoked, or revoking itself), the group is
deleted in the same transaction: its devices, invites and the group row (a few
small rows). A later `sync_hello` for it gets `group_not_found` (with
`create=true` it starts a new group). Each relay start also purges every group
that has no active device left and logs only the count,
`event="sync_groups_purged" groups=N`.

**Server setup.** With the hardened unit in `deploy/relay.service`
(`ProtectSystem=strict`), the store's directory must be listed in
`ReadWritePaths=` and be writable by the service user, which creates `sync.db`
and its `-wal`/`-shm` files there. Then set `RELAY_SYNC_DB` in the env file,
restart the service and look for `event="sync_store" enabled=true` in the log.
Keep the env file and the store outside anything a release replaces.

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
| `RELAY_SYNC_DB` | no | — | SQLite file of the sync group store (groups, devices, join tickets; e.g. `/var/lib/devdy-relay/sync.db`). Unset = sync disabled (`error{sync_disabled}`). |
| `RELAY_SYNC_MAX_BLOB_BYTES` | no | `4194368` | Max peer message: largest sealed `sync_peer_send` message (4 MiB + 64); its base64 `data` may be at most the base64 size of this, else `too_large`. (Name kept from the former blob store.) |
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

This crate implements the relay (and sync group auth + forwarding) only: the WSS server, control frames, room
lifecycle, `room_id` routing of opaque envelopes, auth, pairing TTL,
`MAX_CONTROLLER=1`, revoke, coarse rate-limit, and structured logging.

E2E crypto (U2), the Host agent (U3), and the Controller web client (U4) live
elsewhere. The relay is deliberately payload-agnostic.
