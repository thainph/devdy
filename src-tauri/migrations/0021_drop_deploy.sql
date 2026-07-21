-- Pivot: remove GĐ3 deploy feature. Keep GĐ2 mapping (project_servers) intact.
-- Only drop the playbook table. Do NOT drop runs.server_id / runs.deploy_role
-- (SQLite DROP COLUMN is risky on old engines; the loose columns are harmless).
DROP TABLE IF EXISTS deploy_playbooks;
