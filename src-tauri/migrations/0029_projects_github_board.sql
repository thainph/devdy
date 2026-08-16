-- GitHub Project V2 board config per project (NULL = not linked).
-- board_url: the pasted board URL; field_mappings: JSON blob with
-- { boardId, ownerType, owner, number, startFieldId, deadlineFieldId, statusFieldId, notStartedStatuses }.
-- Read-only tracking; no secrets stored here (PAT stays account-scoped in Keychain).
ALTER TABLE projects ADD COLUMN github_project_board_url TEXT;
ALTER TABLE projects ADD COLUMN github_project_field_mappings TEXT;
