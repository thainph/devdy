-- Two-way multi-device sync was removed (remote desktop "Control another
-- Devdy" replaces it). Drop its bookkeeping tables, triggers and settings.
--
-- The change-capture triggers sync installed at runtime (`sync_cap_*`,
-- `sync_past_owner_*`, `sync_prefs_ref_*`) have dynamic names, so `db::init_db`
-- drops every `sync_*` trigger before the migrations run. The static ones the
-- migrations created are dropped here too, so a DB migrated without that step
-- (sidecar-mcp tests) is clean as well.
--
-- Unrelated and kept: the skill/rule-to-project sync tables (`sync_conflicts`,
-- `rule_sync_conflicts`) and the sync columns on `runs`.

DROP TRIGGER IF EXISTS sync_prefs_project_ins;
DROP TRIGGER IF EXISTS sync_prefs_project_rekey;
DROP TRIGGER IF EXISTS sync_prefs_project_del;
DROP TRIGGER IF EXISTS sync_feed_rows_ins;
DROP TRIGGER IF EXISTS sync_feed_rows_upd;
DROP TRIGGER IF EXISTS sync_feed_rows_del;
DROP TRIGGER IF EXISTS sync_past_owner_ins;
DROP TRIGGER IF EXISTS sync_past_owner_upd;

DROP TABLE IF EXISTS sync_device;
DROP TABLE IF EXISTS sync_state;
DROP TABLE IF EXISTS sync_outbox;
DROP TABLE IF EXISTS sync_rows;
DROP TABLE IF EXISTS sync_deferred;
DROP TABLE IF EXISTS sync_apply_guard;
DROP TABLE IF EXISTS sync_files;
DROP TABLE IF EXISTS sync_blobs;
DROP TABLE IF EXISTS sync_extra_deferred;
DROP TABLE IF EXISTS sync_blob_release;
DROP TABLE IF EXISTS sync_parked;
DROP TABLE IF EXISTS sync_project_prefs;
DROP TABLE IF EXISTS sync_skipped_rows;
DROP TABLE IF EXISTS sync_stripped_refs;
DROP TABLE IF EXISTS sync_feed;

DELETE FROM settings
WHERE key IN (
    'sync_paused',
    'sync_secrets_enabled',
    'sync_categories_off',
    'sync_source_device',
    'sync_leader_device',
    'sync_group_creator'
)
   OR key LIKE 'sync_device_name:%';
