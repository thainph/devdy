-- Remote Control Web Push: browsers (the phone controller PWA) that asked the
-- Host to notify them. `p256dh` / `auth` are the subscription's public key and
-- auth secret (base64url) the payload is encrypted to; the VAPID private key
-- lives in the Keychain, never here. `events` is the comma list of event kinds
-- this device receives (pending, done, failed).
CREATE TABLE IF NOT EXISTS remote_push_subscriptions (
    id          TEXT PRIMARY KEY,
    endpoint    TEXT NOT NULL UNIQUE,
    p256dh      TEXT NOT NULL,
    auth        TEXT NOT NULL,
    label       TEXT NOT NULL DEFAULT '',
    events      TEXT NOT NULL DEFAULT 'pending,done,failed',
    created_at  TEXT NOT NULL,
    last_ok_at  TEXT
);
