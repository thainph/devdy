-- Remote Control relay (C1) — Host-side device registry + append-only audit.
--
-- The per-device pre-shared key (psk) NEVER lives in SQLite; it is stored in the
-- macOS Keychain consolidated item (devdy_secret_store_v1), map `remote_devices`,
-- keyed by device id. SQLite only holds non-secret metadata (DATA-002/DATA-003).
-- The relay auth_token is likewise Keychain-only (DATA-001, SEC-001).

-- DATA-002 — paired Controller devices managed at the Host.
CREATE TABLE IF NOT EXISTS remote_devices (
    id          TEXT PRIMARY KEY,   -- device_id (UUID v4)
    label       TEXT NOT NULL,      -- display name shown to the Owner
    room_id     TEXT,               -- last/active relay room for this device (revoke target)
    status      TEXT NOT NULL,      -- 'pending' | 'active' | 'revoked'
    created_at  TEXT NOT NULL,      -- RFC3339
    last_seen   TEXT,               -- RFC3339, updated on activity
    revoked_at  TEXT                -- RFC3339, set on revoke (BR-012)
);

-- DATA-003 — append-only audit of every remote command (accepted or rejected).
-- Never updated or deleted by the app (BR-013 / SEC-007). No secret payload.
CREATE TABLE IF NOT EXISTS remote_audit (
    id          TEXT PRIMARY KEY,   -- UUID v4
    ts          TEXT NOT NULL,      -- RFC3339
    device_id   TEXT,               -- source Controller device (may be unknown)
    room_id     TEXT,               -- relay room the command arrived on
    run_id      TEXT,               -- affected run, when applicable
    action      TEXT NOT NULL,      -- e.g. respond_permission | start_run | <rejected action>
    result      TEXT NOT NULL,      -- 'accepted' | 'rejected'
    reason      TEXT                -- machine-readable reason (never a secret/plaintext)
);

CREATE INDEX IF NOT EXISTS idx_remote_audit_ts ON remote_audit (ts);
