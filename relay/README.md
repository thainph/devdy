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
| `sync_hello{auth_token, group_id, device_id, device_token, create?, ticket?, known_epoch?}` | Device → Relay | First frame. `auth_token` = `RELAY_HOST_TOKEN`. `create=true` on an unknown group creates it and registers this device as its **creator**. A known device must match its pinned `sha256(device_token)` (`create=true` from a known device is an idempotent hello). An unknown device joins an existing group only with a one-time `ticket` registered by a member (`sync_invite`); `create=true` from it → `group_exists`. `known_epoch` = the store epoch the device last synced in (see *Store epochs*). → `sync_welcome{last_seq, creator_device, epoch, known_head}` (`known_head`: the head this store recorded for `known_epoch`, `null` when not asked or unknown) or `error{auth_failed\|group_not_found\|group_exists\|ticket_required\|ticket_invalid\|device_revoked\|sync_disabled}` and the connection closes. |
| `sync_invite{ticket_hash, ttl_secs}` | Device → Relay | A member registers a join ticket: `ticket_hash` = lowercase hex `sha256(ticket)`, `ttl_secs` 1..600 (default 600). The ticket itself travels device→device, never to the relay before the joiner's hello. One use; at most 16 open per group (expired/used ones are pruned). → `control{code:"ok"}`. Errors: `bad_request`, `quota_exceeded`. |
| `sync_push{base_seq, items:[{key, blob}]}` | Device → Relay | `base_seq` = the pusher's pull cursor (missing = 0). Relay assigns per-group monotonic `seq`s; an older op with the same `key` is deleted only if `seq <= base_seq` (the pusher had pulled it) or the pusher stored it itself — otherwise both are kept and devices resolve them by HLC. All or nothing. → `sync_ack{last_seq}`. Errors: `quota_exceeded`, `too_large`. |
| `sync_notify{last_seq}` | Relay → other devices | Sent to every other connected device of the group after a push. |
| `sync_pull{after_seq, limit}` | Device → Relay | → `sync_ops{items:[{seq, device_id, key, blob}], last_seq, more}`. At most 500 ops / ~8 MiB per page. Set the cursor to `last_seq` and pull again while `more`. |
| `blob_put{sha, data}` | Device → Relay | `data` = base64 of the sealed bytes, `sha` = lowercase hex sha256 of those bytes (verified). Idempotent. → `blob_ack{sha}`. Errors: `sha_mismatch`, `too_large`, `quota_exceeded`. |
| `blob_get{sha}` | Device → Relay | → `blob_data{sha, data}` or `error{blob_not_found}`. |
| `blob_release{shas}` | Device → Relay | The uploading device no longer references these blobs (at most 1000 per frame). Only blobs that device uploaded are marked; anything else is ignored. → `blob_released{released}` (how many were marked). |
| `sync_revoke{device_id, successor?}` | Device → Relay | A device may revoke itself; only the group's creator may revoke another device (else `forbidden`). The revoked device's live connections get `error{device_revoked}` and close, its hello fails from then on (with or without a ticket). When the **creator revokes itself** the role moves to `successor` if that is an active member, else to the oldest active member (registration time, ties by device id); no member left = no creator. → `control{code:"ok"}`. |
| `sync_group_info{creator_device}` | Relay → other devices | Sent to the group's other connected devices when the creator role moved; devices that were offline learn it from their next `sync_welcome`. |
| `sync_peers{}` | Device → Relay | On-demand sync (design `on-demand-sync` §1). → `sync_peers{devices:[device_id, ...]}`: the group's devices with a live sync connection, excluding the caller (deduplicated, sorted). |
| `sync_peer_send{to_device, data}` | Device → Relay | Forward `data` (base64 of a message sealed with the group key) to `to_device`'s most recent live connection in the **same group**, as `sync_peer_msg`. → `sync_peer_ack{}` once queued for the target. Errors: `peer_offline` (no live connection, revoked, not a member, the sender itself, or the target's queue did not drain within 60 s — that target is then disconnected), `too_large` (`data` longer than the base64 of `RELAY_SYNC_MAX_BLOB_BYTES`), `bad_request` (missing `to_device` / `data`). Never stored, never logged. |
| `sync_peer_msg{from_device, data}` | Relay → target device | A forwarded `sync_peer_send` (no `id`). |

After `sync_welcome` only these frames and `keepalive` are accepted on the
connection (anything else → `error{forbidden}`); sync frames on a Remote
Control connection are refused the same way. An optional `id` on any request
is echoed on its response or error. Push/pull/blob frames (including
`blob_release`) and `sync_peer_send` are not rate limited
(an initial upload, or a folder sync's replies, send many);
`sync_hello`/`sync_invite`/`sync_revoke`/`sync_peers` count as control frames. A connection's requests are handled one at a time, and a
`sync_pull` / `blob_get` is only served once the replies still queued for that
socket are under 16 MiB (`MAX_QUEUED_REPLY_BYTES`); a client that stops reading
for 60 s is disconnected. While sync is enabled the WebSocket message limit is raised, if
needed, to fit one maximum-size `blob_put`.

**Blob GC.** Ops are sealed, so the relay cannot see which blobs an op
references; the devices tell it. File sync re-uploads the last (partial)
chunk of a growing log on every append, which leaves the previous tail blob
unused once the new manifest op has superseded the old one (same key). The
device that uploaded it sends `blob_release` for it after the new manifest
was pushed (and only when no other file of that device still uses the chunk).
The relay records the release and deletes the blob 24 h later
(`BLOB_RELEASE_GRACE_SECS`), lazily on that group's next `blob_release` or
`sync_hello` — no background task. Until then the blob can still be fetched,
so a device that pulled the superseded op a moment earlier still finishes its
download (it gets the newer op on its next pull anyway). A `blob_put` of a
released sha cancels the release. Blobs stored before GC existed have no
recorded uploader and can be released by any device of the group. Freed bytes
are taken off the group's quota usage.

**Storage.** SQLite file at `RELAY_SYNC_DB`, e.g. `/var/lib/devdy-relay/sync.db`
(keep it outside the release directory; the directory must be writable by the
relay's service user — `ReadWritePaths=` in the unit). Tables:
`groups` (head seq + byte usage + creator device), `devices`, `ops`, `blobs`
(with uploader and release time), `invites` (ticket hashes), `store_epoch`
(the current epoch) and `epochs` (each group's head per epoch). Older databases are
upgraded at startup: missing columns are added, `creator_device` is set to the
group's earliest active registered device, and `ops` is rebuilt without its old
`UNIQUE(group_id, key)` constraint. It runs in WAL
mode, so a copy of the `.db` file alone is not a consistent backup — back up
with `sqlite3 "$RELAY_SYNC_DB" ".backup '/path/sync-$(date +%F).db'"`
(safe while the relay runs). The content is end-to-end encrypted, but the file
is the only server-side copy of every group's history; devices keep full local
copies, so losing it means re-uploading, not data loss.

## Operations (sync store)

**Backup.** `sync.db` runs in WAL mode, so copying the file alone can give a
torn copy. Use SQLite's online backup, which is safe while the relay runs:

```bash
sqlite3 "$RELAY_SYNC_DB" ".backup '<BACKUP_DIR>/sync-$(date +%F).db'"
```

(e.g. from a daily cron, keeping a few days). The content
is end-to-end encrypted, so backups are as safe to store as the live file; the
devices hold full local copies, so losing the store costs a re-upload, not data.

**Store epochs.** Every time the relay opens the store (each start) it picks a
new random *epoch* and records, per group, the head `seq` reached in each epoch
(updated in the same transaction as every push). A device remembers the epoch it
last synced in and the highest `seq` it has seen there (pulled, or acked for its
own push), presents the epoch as `known_epoch` on hello and compares
`known_head` with what it saw. A plain restart keeps the history, so the old
epoch's head is still there and the device moves on to the new epoch.

**Restoring an older backup.** The restored store's head `seq` is lower than
what devices already saw, the ops pushed after the backup are gone, and the
next pushes reuse their seqs. A device detects this on its next `sync_welcome`
— the restored store does not know the epoch the device synced in, or recorded
a lower head for it; a head below the device's own cursor shows it too — even
after other devices have pushed the head past that cursor again. It then stops
syncing and asks the user to merge again (re-read the group and re-upload what
the relay lost) instead of pulling from a cursor that now points at different
ops. Stop the relay before replacing the database file, and restore only when
the store is lost or corrupt; the creator recorded in the backup applies again
(devices are told on their next welcome).

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
| `RELAY_SYNC_DB` | no | — | SQLite file of the sync store (e.g. `/var/lib/devdy-relay/sync.db`). Unset = sync disabled (`error{sync_disabled}`). |
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
