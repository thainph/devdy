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
| `create_pair`    | Host → Relay     | Register a one-time `pair_code` (TTL = `PAIR_TTL`); relay returns the assigned `room_id`. |
| `join`           | Controller → Relay | Enter a room by `pair_code`. |
| `handshake_done` | Host/Controller → Relay | Signal E2E handshake half complete; when both signal → room `ACTIVE`. |
| `room_ready`     | Relay → both     | Room paired, carries `room_id`. |
| `peer_left`      | Relay → survivor | The other peer left / was cut. |
| `revoke`         | Host → Relay     | Close the room immediately. |
| `error`          | Relay → sender   | `{ code, message }` — auth/routing error. |

Room state machine (relay side, SRS §6.2):

```
PENDING_PAIR --join valid--> HANDSHAKING --both handshake_done--> ACTIVE
PENDING_PAIR --TTL elapsed--> EXPIRED (terminal)
HANDSHAKING  --fail--------> CLOSED  (terminal)
ACTIVE       --revoke/both left/idle--> CLOSED (terminal)
```

Guarantees: `MAX_CONTROLLER = 1` (a second `join` is refused); `pair_code` is
one-time and expires after `PAIR_TTL`.

## Configuration (environment)

| Variable | Required | Default | Meaning |
|---|---|---|---|
| `RELAY_HOST_TOKEN` | **yes** | — | Shared secret for `register_host`. Relay refuses to start if unset. |
| `RELAY_BIND` | no | `127.0.0.1:8787` | Bind address (keep it localhost). |
| `PAIR_TTL_SECS` | no | `60` | Pair-code TTL. |
| `RELAY_RATE_LIMIT_MAX` | no | `60` | Coarse anti-flood: frames per window. |
| `RELAY_RATE_LIMIT_WINDOW_SECS` | no | `10` | Rate-limit window. |
| `RELAY_ROOM_IDLE_TIMEOUT_SECS` | no | `3600` | Close an idle ACTIVE room. |
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

(The default glibc build also works if the VPS glibc matches; musl removes that
coupling. The crate has no C runtime dependencies of its own.)

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

This crate implements the relay only: the WSS server, control frames, room
lifecycle, `room_id` routing of opaque envelopes, auth, pairing TTL,
`MAX_CONTROLLER=1`, revoke, coarse rate-limit, and structured logging.

E2E crypto (U2), the Host agent (U3), and the Controller web client (U4) live
elsewhere. The relay is deliberately payload-agnostic.
