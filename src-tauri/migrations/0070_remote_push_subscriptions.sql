-- Remote Control Web Push: browsers (the phone controller PWA) that asked the
-- Host to notify them. `p256dh` / `auth` are the subscription's public key and
-- auth secret (base64url) the payload is encrypted to; the VAPID private key
-- lives in the Keychain, never here. `events` is the comma list of event kinds
-- this device receives (pending, done, failed).
--
-- `vapid_key`: the Host public key the browser subscribed with — a row made
--   for another key can never be delivered to and is dropped.
-- `session_tag`: which remote session subscribed it (a hash, never the
--   secret); ending or kicking that session drops its devices.
-- `removed_at`: removed on the desktop. Kept as a tombstone so the phone's
--   automatic re-subscribe does not undo it; turning notifications on again
--   on the phone clears it.
-- `auth_failures`: consecutive 401/403 answers; a few in a row mean the push
--   service no longer accepts this subscription.
CREATE TABLE IF NOT EXISTS remote_push_subscriptions (
    id            TEXT PRIMARY KEY,
    endpoint      TEXT NOT NULL UNIQUE,
    p256dh        TEXT NOT NULL,
    auth          TEXT NOT NULL,
    label         TEXT NOT NULL DEFAULT '',
    events        TEXT NOT NULL DEFAULT 'pending,done,failed',
    vapid_key     TEXT NOT NULL DEFAULT '',
    session_tag   TEXT NOT NULL DEFAULT '',
    created_at    TEXT NOT NULL,
    last_ok_at    TEXT,
    removed_at    TEXT,
    auth_failures INTEGER NOT NULL DEFAULT 0
);
