-- Remote Control redesign: single per-session link + OTP + session token.
--
-- The device registry (pair / approve / paired-devices) is removed. Sessions are
-- now bound one-at-a-time in memory + a single Keychain credential; there is no
-- durable per-device row to keep. The append-only audit log (remote_audit) is
-- retained as the historical record — its `device_id` column is left unused
-- (NULL) going forward.
DROP TABLE IF EXISTS remote_devices;
