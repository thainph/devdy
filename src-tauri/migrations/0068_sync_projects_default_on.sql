-- Projects arriving from another device are taken by default, folder sync
-- included (on-demand sync: one press of Sync brings everything); the user
-- turns single projects off. `prefs_synced_project_enabled` = '0' restores the
-- old opt-in default (tests).
DROP TRIGGER IF EXISTS sync_prefs_project_ins;
CREATE TRIGGER sync_prefs_project_ins AFTER INSERT ON projects BEGIN
    INSERT OR IGNORE INTO sync_project_prefs (project_id, enabled, folder)
    SELECT NEW.id, v, v FROM (
        SELECT CASE WHEN (SELECT active FROM sync_apply_guard WHERE id = 1) = 1
                    THEN COALESCE((SELECT CAST(value AS INTEGER) FROM sync_state WHERE key = 'prefs_synced_project_enabled'), 1)
                    ELSE NULL END AS v
    ) WHERE v IS NOT NULL;
    -- Created here: selected, no folder sync (this machine is its source).
    INSERT OR IGNORE INTO sync_project_prefs (project_id, enabled, folder) VALUES (NEW.id, 1, 0);
END;

-- Projects that arrived under the old opt-in default: take them now, and
-- re-pull so their rows come in (the same reset `sync::prefs` does).
INSERT INTO sync_state (key, value)
    SELECT 'pull_cursor', '0' WHERE EXISTS (SELECT 1 FROM sync_project_prefs WHERE enabled = 0)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value;
INSERT INTO sync_state (key, value)
    SELECT 'pull_cursor_reset', '1' WHERE EXISTS (SELECT 1 FROM sync_project_prefs WHERE enabled = 0)
    ON CONFLICT(key) DO UPDATE SET value = excluded.value;
UPDATE sync_project_prefs SET enabled = 1, folder = 1 WHERE enabled = 0;
