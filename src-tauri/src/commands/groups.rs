//! Skill / rule groups: bundle items together so a project can switch a whole bundle on or off in
//! one action, instead of flipping items one by one.
//!
//! Skill groups and rule groups live in separate tables but behave identically, so the logic here
//! is written once against `GroupKind`, which maps a kind to its table/column names and to the
//! per-item apply/remove routines in `projects.rs` / `rules.rs`.
//!
//! Membership is many-to-many, and a project remembers which groups it enabled. Two consequences
//! drive the rules below:
//!   * adding an item to a group propagates it to every project already running that group;
//!   * disabling a group only removes items it brought in (`manual = 0`) that no other enabled
//!     group still claims.

use crate::commands::projects::{apply_skill_to_project, remove_skill_from_project_inner};
use crate::commands::rules::{
    apply_rule_to_project, remove_rule_from_project_inner, ApplyAllOutcome, ApplyFailure,
};
use crate::db::Db;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupKind {
    Skill,
    Rule,
}

impl GroupKind {
    fn parse(kind: &str) -> Result<Self, String> {
        match kind {
            "skill" => Ok(GroupKind::Skill),
            "rule" => Ok(GroupKind::Rule),
            _ => Err("kind must be one of: skill, rule".to_string()),
        }
    }

    /// Table names are picked from this enum, never from caller input, so interpolating them into
    /// SQL below is safe.
    fn groups_table(self) -> &'static str {
        match self {
            GroupKind::Skill => "skill_groups",
            GroupKind::Rule => "rule_groups",
        }
    }

    fn members_table(self) -> &'static str {
        match self {
            GroupKind::Skill => "skill_group_members",
            GroupKind::Rule => "rule_group_members",
        }
    }

    fn project_groups_table(self) -> &'static str {
        match self {
            GroupKind::Skill => "project_skill_groups",
            GroupKind::Rule => "project_rule_groups",
        }
    }

    fn items_table(self) -> &'static str {
        match self {
            GroupKind::Skill => "skills",
            GroupKind::Rule => "rules",
        }
    }

    fn project_items_table(self) -> &'static str {
        match self {
            GroupKind::Skill => "project_skills",
            GroupKind::Rule => "project_rules",
        }
    }

    fn item_col(self) -> &'static str {
        match self {
            GroupKind::Skill => "skill_id",
            GroupKind::Rule => "rule_id",
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: String,
    pub color: Option<String>,
    pub position: i64,
    pub member_count: i64,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateGroupPayload {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateGroupPayload {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub color: Option<String>,
}

/// One group as seen from a project: how many of its members are applied, and whether the project
/// has the group switched on.
#[derive(Debug, Serialize)]
pub struct ProjectGroupState {
    pub group_id: String,
    pub name: String,
    pub description: String,
    pub color: Option<String>,
    pub member_count: i64,
    pub applied_count: i64,
    pub linked: bool,
    /// "on" (every member applied), "mixed" (some applied, or linked but incomplete), "off".
    pub state: String,
}

#[derive(Debug, Serialize, Default)]
pub struct GroupItemFailure {
    pub item_id: String,
    pub item_name: String,
    pub error: String,
}

#[derive(Debug, Serialize, Default)]
pub struct GroupApplyOutcome {
    pub applied: usize,
    pub failures: Vec<GroupItemFailure>,
}

#[derive(Debug, Serialize, Default)]
pub struct GroupDisableOutcome {
    pub removed: usize,
    /// Kept because the user had switched them on by hand.
    pub kept_manual: usize,
    /// Kept because another enabled group still contains them.
    pub kept_other_group: usize,
    pub failures: Vec<GroupItemFailure>,
}

// ---- Shared helpers -----------------------------------------------------------

async fn apply_item(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    item_id: &str,
    manual: bool,
) -> Result<(), String> {
    match kind {
        GroupKind::Skill => apply_skill_to_project(db, project_id, item_id, manual).await,
        GroupKind::Rule => apply_rule_to_project(db, project_id, item_id, manual).await,
    }
}

async fn remove_item(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    item_id: &str,
) -> Result<(), String> {
    match kind {
        GroupKind::Skill => remove_skill_from_project_inner(db, project_id, item_id).await,
        GroupKind::Rule => remove_rule_from_project_inner(db, project_id, item_id).await,
    }
}

async fn item_name(db: &Db, kind: GroupKind, item_id: &str) -> String {
    sqlx::query(&format!(
        "SELECT name FROM {} WHERE id = ?",
        kind.items_table()
    ))
    .bind(item_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .map(|row| row.get::<String, _>("name"))
    .unwrap_or_else(|| item_id.to_string())
}

async fn member_ids(db: &Db, kind: GroupKind, group_id: &str) -> Result<Vec<String>, String> {
    let rows = sqlx::query(&format!(
        "SELECT m.{item} as item_id FROM {members} m
         JOIN {items} i ON i.id = m.{item}
         WHERE m.group_id = ?
         ORDER BY m.position, i.name",
        item = kind.item_col(),
        members = kind.members_table(),
        items = kind.items_table(),
    ))
    .bind(group_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| r.get::<String, _>("item_id")).collect())
}

/// Projects that currently have this group switched on.
async fn projects_with_group(db: &Db, kind: GroupKind, group_id: &str) -> Result<Vec<String>, String> {
    let rows = sqlx::query(&format!(
        "SELECT project_id FROM {} WHERE group_id = ?",
        kind.project_groups_table()
    ))
    .bind(group_id)
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| r.get::<String, _>("project_id")).collect())
}

/// Is the item pulled in by a group this project has enabled? `excluding_group` skips one group,
/// for the "is anything *else* still holding it?" question asked while disabling that group.
async fn claimed_by_group(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    item_id: &str,
    excluding_group: Option<&str>,
) -> Result<bool, String> {
    let row = sqlx::query(&format!(
        "SELECT 1 as hit FROM {members} m
         JOIN {project_groups} pg ON pg.group_id = m.group_id
         WHERE m.{item} = ? AND pg.project_id = ? AND m.group_id <> ?
         LIMIT 1",
        members = kind.members_table(),
        project_groups = kind.project_groups_table(),
        item = kind.item_col(),
    ))
    .bind(item_id)
    .bind(project_id)
    // No group to exclude: compare against a sentinel no real id matches.
    .bind(excluding_group.unwrap_or(""))
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.is_some())
}

async fn claimed_by_other_group(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    item_id: &str,
    excluding_group: &str,
) -> Result<bool, String> {
    claimed_by_group(db, kind, project_id, item_id, Some(excluding_group)).await
}

/// Public counterpart used by the per-item toggles: the direct link and the group link are
/// independent, so switching the item off by hand must not remove what a group still provides.
pub async fn claimed_by_enabled_group(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    item_id: &str,
) -> Result<bool, String> {
    claimed_by_group(db, kind, project_id, item_id, None).await
}

async fn is_manual(db: &Db, kind: GroupKind, project_id: &str, item_id: &str) -> Result<bool, String> {
    let row = sqlx::query(&format!(
        "SELECT manual FROM {} WHERE project_id = ? AND {} = ?",
        kind.project_items_table(),
        kind.item_col()
    ))
    .bind(project_id)
    .bind(item_id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(row.map(|r| r.get::<i64, _>("manual") != 0).unwrap_or(false))
}

/// Apply every member of a group to one project, collecting per-item failures instead of aborting:
/// one broken skill shouldn't stop the rest of the bundle.
async fn apply_members(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    members: &[String],
    manual: bool,
) -> GroupApplyOutcome {
    let mut outcome = GroupApplyOutcome::default();
    for item_id in members {
        match apply_item(db, kind, project_id, item_id, manual).await {
            Ok(()) => outcome.applied += 1,
            Err(error) => outcome.failures.push(GroupItemFailure {
                item_id: item_id.clone(),
                item_name: item_name(db, kind, item_id).await,
                error,
            }),
        }
    }
    outcome
}

// ---- Group CRUD ---------------------------------------------------------------

#[tauri::command]
pub async fn list_groups(db: State<'_, Db>, kind: String) -> Result<Vec<Group>, String> {
    let kind = GroupKind::parse(&kind)?;
    let rows = sqlx::query(&format!(
        "SELECT g.id, g.name, g.description, g.color, g.position, g.updated_at,
                (SELECT COUNT(*) FROM {members} m WHERE m.group_id = g.id) as member_count
         FROM {groups} g
         ORDER BY g.position, g.name",
        members = kind.members_table(),
        groups = kind.groups_table(),
    ))
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|row| Group {
            id: row.get("id"),
            name: row.get("name"),
            description: row.get("description"),
            color: row.get("color"),
            position: row.get("position"),
            member_count: row.get("member_count"),
            updated_at: row.get("updated_at"),
        })
        .collect())
}

fn validate_group_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("Group name cannot be empty".to_string());
    }
    Ok(())
}

#[tauri::command]
pub async fn create_group(
    db: State<'_, Db>,
    kind: String,
    payload: CreateGroupPayload,
) -> Result<Group, String> {
    let kind = GroupKind::parse(&kind)?;
    let name = payload.name.trim().to_string();
    validate_group_name(&name)?;

    let next_position: i64 = sqlx::query(&format!(
        "SELECT COALESCE(MAX(position), -1) + 1 as next FROM {}",
        kind.groups_table()
    ))
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .get("next");

    let id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(&format!(
        "INSERT INTO {} (id, name, description, color, position, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        kind.groups_table()
    ))
    .bind(&id)
    .bind(&name)
    .bind(&payload.description)
    .bind(&payload.color)
    .bind(next_position)
    .bind(&now)
    .bind(&now)
    .execute(db.inner())
    .await
    .map_err(|e| format!("Could not create group: {}", e))?;

    Ok(Group {
        id,
        name,
        description: payload.description,
        color: payload.color,
        position: next_position,
        member_count: 0,
        updated_at: now,
    })
}

#[tauri::command]
pub async fn update_group(
    db: State<'_, Db>,
    kind: String,
    payload: UpdateGroupPayload,
) -> Result<Group, String> {
    let kind = GroupKind::parse(&kind)?;
    let name = payload.name.trim().to_string();
    validate_group_name(&name)?;

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(&format!(
        "UPDATE {} SET name = ?, description = ?, color = ?, updated_at = ? WHERE id = ?",
        kind.groups_table()
    ))
    .bind(&name)
    .bind(&payload.description)
    .bind(&payload.color)
    .bind(&now)
    .bind(&payload.id)
    .execute(db.inner())
    .await
    .map_err(|e| format!("Could not update group: {}", e))?;

    let member_count: i64 = sqlx::query(&format!(
        "SELECT COUNT(*) as c FROM {} WHERE group_id = ?",
        kind.members_table()
    ))
    .bind(&payload.id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .get("c");

    let position: i64 = sqlx::query(&format!(
        "SELECT position FROM {} WHERE id = ?",
        kind.groups_table()
    ))
    .bind(&payload.id)
    .fetch_one(db.inner())
    .await
    .map_err(|e| e.to_string())?
    .get("position");

    Ok(Group {
        id: payload.id,
        name,
        description: payload.description,
        color: payload.color,
        position,
        member_count,
        updated_at: now,
    })
}

/// Delete the group itself. Members and project links go with it (ON DELETE CASCADE); the skills
/// and rules themselves — and whatever is already applied to projects — are left untouched.
#[tauri::command]
pub async fn delete_group(db: State<'_, Db>, kind: String, group_id: String) -> Result<(), String> {
    let kind = GroupKind::parse(&kind)?;
    sqlx::query(&format!("DELETE FROM {} WHERE id = ?", kind.groups_table()))
        .bind(&group_id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn get_group_members(
    db: State<'_, Db>,
    kind: String,
    group_id: String,
) -> Result<Vec<String>, String> {
    let kind = GroupKind::parse(&kind)?;
    member_ids(db.inner(), kind, &group_id).await
}

/// All group ids each item belongs to, keyed by item id — lets list views show group badges
/// without a round trip per item.
#[tauri::command]
pub async fn get_item_groups(
    db: State<'_, Db>,
    kind: String,
) -> Result<Vec<(String, String)>, String> {
    let kind = GroupKind::parse(&kind)?;
    let rows = sqlx::query(&format!(
        "SELECT {item} as item_id, group_id FROM {members}",
        item = kind.item_col(),
        members = kind.members_table(),
    ))
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<String, _>("item_id"), r.get::<String, _>("group_id")))
        .collect())
}

/// Overwrite a group's membership, then reconcile every project running the group: newly added
/// items get applied, dropped items get removed unless they're hand-picked or claimed elsewhere.
#[tauri::command]
pub async fn set_group_members(
    db: State<'_, Db>,
    kind: String,
    group_id: String,
    item_ids: Vec<String>,
) -> Result<GroupApplyOutcome, String> {
    let kind = GroupKind::parse(&kind)?;
    let db = db.inner();

    let previous = member_ids(db, kind, &group_id).await?;
    let added: Vec<String> = item_ids
        .iter()
        .filter(|id| !previous.contains(id))
        .cloned()
        .collect();
    let dropped: Vec<String> = previous
        .iter()
        .filter(|id| !item_ids.contains(id))
        .cloned()
        .collect();

    sqlx::query(&format!(
        "DELETE FROM {} WHERE group_id = ?",
        kind.members_table()
    ))
    .bind(&group_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    for (index, item_id) in item_ids.iter().enumerate() {
        sqlx::query(&format!(
            "INSERT INTO {members} (group_id, {item}, position) VALUES (?, ?, ?)",
            members = kind.members_table(),
            item = kind.item_col(),
        ))
        .bind(&group_id)
        .bind(item_id)
        .bind(index as i64)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    }

    // Propagate to projects already running this group.
    let mut outcome = GroupApplyOutcome::default();
    for project_id in projects_with_group(db, kind, &group_id).await? {
        let applied = apply_members(db, kind, &project_id, &added, false).await;
        outcome.applied += applied.applied;
        outcome.failures.extend(applied.failures);

        for item_id in &dropped {
            if is_manual(db, kind, &project_id, item_id).await?
                || claimed_by_other_group(db, kind, &project_id, item_id, &group_id).await?
            {
                continue;
            }
            if let Err(error) = remove_item(db, kind, &project_id, item_id).await {
                outcome.failures.push(GroupItemFailure {
                    item_id: item_id.clone(),
                    item_name: item_name(db, kind, item_id).await,
                    error,
                });
            }
        }
    }

    Ok(outcome)
}

// ---- Per-project enable / disable ---------------------------------------------

#[tauri::command]
pub async fn get_project_groups(
    db: State<'_, Db>,
    kind: String,
    project_id: String,
) -> Result<Vec<ProjectGroupState>, String> {
    let kind = GroupKind::parse(&kind)?;
    let rows = sqlx::query(&format!(
        "SELECT g.id, g.name, g.description, g.color,
                (SELECT COUNT(*) FROM {members} m WHERE m.group_id = g.id) as member_count,
                (SELECT COUNT(*) FROM {members} m
                   JOIN {project_items} pi ON pi.{item} = m.{item} AND pi.project_id = ?
                 WHERE m.group_id = g.id) as applied_count,
                (SELECT COUNT(*) FROM {project_groups} pg
                 WHERE pg.group_id = g.id AND pg.project_id = ?) as linked
         FROM {groups} g
         ORDER BY g.position, g.name",
        members = kind.members_table(),
        project_items = kind.project_items_table(),
        item = kind.item_col(),
        project_groups = kind.project_groups_table(),
        groups = kind.groups_table(),
    ))
    .bind(&project_id)
    .bind(&project_id)
    .fetch_all(db.inner())
    .await
    .map_err(|e| e.to_string())?;

    Ok(rows
        .iter()
        .map(|row| {
            let member_count: i64 = row.get("member_count");
            let applied_count: i64 = row.get("applied_count");
            let linked: i64 = row.get("linked");
            let linked = linked > 0;
            // The group link is its own thing: a project that never enabled the group reads "off"
            // even if some members happen to be switched on directly. A linked group missing some
            // members (someone deleted one by hand) reads "mixed" so the UI can offer a re-apply.
            let state = if !linked {
                "off"
            } else if member_count > 0 && applied_count == member_count {
                "on"
            } else {
                "mixed"
            };
            ProjectGroupState {
                group_id: row.get("id"),
                name: row.get("name"),
                description: row.get("description"),
                color: row.get("color"),
                member_count,
                applied_count,
                linked,
                state: state.to_string(),
            }
        })
        .collect())
}

#[tauri::command]
pub async fn enable_group_for_project(
    db: State<'_, Db>,
    kind: String,
    project_id: String,
    group_id: String,
) -> Result<GroupApplyOutcome, String> {
    let kind = GroupKind::parse(&kind)?;
    let db = db.inner();
    enable_group(db, kind, &project_id, &group_id).await
}

async fn enable_group(
    db: &Db,
    kind: GroupKind,
    project_id: &str,
    group_id: &str,
) -> Result<GroupApplyOutcome, String> {
    let members = member_ids(db, kind, group_id).await?;
    let outcome = apply_members(db, kind, project_id, &members, false).await;

    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query(&format!(
        "INSERT INTO {} (project_id, group_id, enabled_at) VALUES (?, ?, ?)
         ON CONFLICT(project_id, group_id) DO UPDATE SET enabled_at = excluded.enabled_at",
        kind.project_groups_table()
    ))
    .bind(project_id)
    .bind(group_id)
    .bind(&now)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    Ok(outcome)
}

#[tauri::command]
pub async fn disable_group_for_project(
    db: State<'_, Db>,
    kind: String,
    project_id: String,
    group_id: String,
) -> Result<GroupDisableOutcome, String> {
    let kind = GroupKind::parse(&kind)?;
    let db = db.inner();

    sqlx::query(&format!(
        "DELETE FROM {} WHERE project_id = ? AND group_id = ?",
        kind.project_groups_table()
    ))
    .bind(&project_id)
    .bind(&group_id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;

    let mut outcome = GroupDisableOutcome::default();
    for item_id in member_ids(db, kind, &group_id).await? {
        if is_manual(db, kind, &project_id, &item_id).await? {
            outcome.kept_manual += 1;
            continue;
        }
        if claimed_by_other_group(db, kind, &project_id, &item_id, &group_id).await? {
            outcome.kept_other_group += 1;
            continue;
        }
        match remove_item(db, kind, &project_id, &item_id).await {
            Ok(()) => outcome.removed += 1,
            Err(error) => outcome.failures.push(GroupItemFailure {
                item_id: item_id.clone(),
                item_name: item_name(db, kind, &item_id).await,
                error,
            }),
        }
    }

    Ok(outcome)
}

/// Switch a group on for every project at once — the group-level counterpart of
/// `apply_skill_to_all_projects`.
#[tauri::command]
pub async fn apply_group_to_all_projects(
    db: State<'_, Db>,
    kind: String,
    group_id: String,
) -> Result<ApplyAllOutcome, String> {
    let kind = GroupKind::parse(&kind)?;
    let db = db.inner();

    let rows = sqlx::query("SELECT id, name FROM projects ORDER BY name")
        .fetch_all(db)
        .await
        .map_err(|e| e.to_string())?;

    let mut applied = 0;
    let mut failures = Vec::new();
    for row in &rows {
        let project_id: String = row.get("id");
        let project_name: String = row.get("name");
        match enable_group(db, kind, &project_id, &group_id).await {
            Ok(outcome) if outcome.failures.is_empty() => applied += 1,
            Ok(outcome) => failures.push(ApplyFailure {
                project_id,
                project_name,
                error: outcome
                    .failures
                    .iter()
                    .map(|f| format!("{}: {}", f.item_name, f.error))
                    .collect::<Vec<_>>()
                    .join("; "),
            }),
            Err(error) => failures.push(ApplyFailure {
                project_id,
                project_name,
                error,
            }),
        }
    }
    Ok(ApplyAllOutcome { applied, failures })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use std::path::PathBuf;

    /// Throwaway project dir for one test, so rule artifacts land somewhere harmless.
    fn temp_project(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("devdy-groups-{}-{}", label, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// In-memory DB with the real migrations applied, plus one project pointing at `dir`.
    async fn setup(dir: &PathBuf) -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'proj', ?, '2026-01-01')")
            .bind(dir.to_string_lossy().to_string())
            .execute(&pool)
            .await
            .unwrap();
        pool
    }

    async fn add_rule(db: &Db, id: &str, dir: &PathBuf) {
        let source = dir.join(format!("{}.md", id));
        std::fs::write(&source, format!("# {}\n", id)).unwrap();
        sqlx::query(
            "INSERT INTO rules (id, name, description, target, source_path, updated_at)
             VALUES (?, ?, '', 'claude', ?, '2026-01-01')",
        )
        .bind(id)
        .bind(id)
        .bind(source.to_string_lossy().to_string())
        .execute(db)
        .await
        .unwrap();
    }

    async fn new_group(db: &Db, id: &str) {
        sqlx::query(
            "INSERT INTO rule_groups (id, name, description, position, created_at, updated_at)
             VALUES (?, ?, '', 0, '2026-01-01', '2026-01-01')",
        )
        .bind(id)
        .bind(id)
        .execute(db)
        .await
        .unwrap();
    }

    async fn set_members(db: &Db, group_id: &str, ids: &[&str]) {
        for (i, id) in ids.iter().enumerate() {
            sqlx::query("INSERT INTO rule_group_members (group_id, rule_id, position) VALUES (?, ?, ?)")
                .bind(group_id)
                .bind(id)
                .bind(i as i64)
                .execute(db)
                .await
                .unwrap();
        }
    }

    async fn applied_ids(db: &Db) -> Vec<String> {
        sqlx::query("SELECT rule_id FROM project_rules ORDER BY rule_id")
            .fetch_all(db)
            .await
            .unwrap()
            .iter()
            .map(|r| r.get::<String, _>("rule_id"))
            .collect()
    }

    #[tokio::test]
    async fn enable_applies_members_and_links_project() {
        let path = temp_project("enable");
        let db = setup(&path).await;
        add_rule(&db, "a", &path).await;
        add_rule(&db, "b", &path).await;
        new_group(&db, "g1").await;
        set_members(&db, "g1", &["a", "b"]).await;

        let outcome = enable_group(&db, GroupKind::Rule, "p1", "g1").await.unwrap();
        assert_eq!(outcome.applied, 2);
        assert!(outcome.failures.is_empty());
        assert_eq!(applied_ids(&db).await, vec!["a", "b"]);

        let linked: i64 = sqlx::query("SELECT COUNT(*) as c FROM project_rule_groups WHERE project_id = 'p1'")
            .fetch_one(&db)
            .await
            .unwrap()
            .get("c");
        assert_eq!(linked, 1);
    }

    #[tokio::test]
    async fn group_applied_items_are_not_manual() {
        let path = temp_project("manual-flag");
        let db = setup(&path).await;
        add_rule(&db, "a", &path).await;
        new_group(&db, "g1").await;
        set_members(&db, "g1", &["a"]).await;

        enable_group(&db, GroupKind::Rule, "p1", "g1").await.unwrap();
        assert!(!is_manual(&db, GroupKind::Rule, "p1", "a").await.unwrap());

        // Flipping it on by hand afterwards upgrades the row to manual.
        apply_rule_to_project(&db, "p1", "a", true).await.unwrap();
        assert!(is_manual(&db, GroupKind::Rule, "p1", "a").await.unwrap());
    }

    #[tokio::test]
    async fn item_in_two_enabled_groups_survives_one_being_disabled() {
        let path = temp_project("two-groups");
        let db = setup(&path).await;
        add_rule(&db, "shared", &path).await;
        add_rule(&db, "only-g1", &path).await;
        new_group(&db, "g1").await;
        new_group(&db, "g2").await;
        set_members(&db, "g1", &["shared", "only-g1"]).await;
        set_members(&db, "g2", &["shared"]).await;

        enable_group(&db, GroupKind::Rule, "p1", "g1").await.unwrap();
        enable_group(&db, GroupKind::Rule, "p1", "g2").await.unwrap();

        // Disabling g1 drops its exclusive member but keeps the one g2 still claims.
        assert!(claimed_by_other_group(&db, GroupKind::Rule, "p1", "shared", "g1")
            .await
            .unwrap());
        assert!(!claimed_by_other_group(&db, GroupKind::Rule, "p1", "only-g1", "g1")
            .await
            .unwrap());
    }

    /// The direct link and the group link are independent: switching the item off by hand while a
    /// group still provides it only clears `manual` — the artifacts stay.
    #[tokio::test]
    async fn manual_off_keeps_item_a_group_still_provides() {
        let path = temp_project("unlink-kept");
        let db = setup(&path).await;
        add_rule(&db, "a", &path).await;
        new_group(&db, "g1").await;
        set_members(&db, "g1", &["a"]).await;

        apply_rule_to_project(&db, "p1", "a", true).await.unwrap();
        enable_group(&db, GroupKind::Rule, "p1", "g1").await.unwrap();

        crate::commands::rules::unlink_rule_from_project(&db, "p1", "a")
            .await
            .unwrap();

        assert_eq!(applied_ids(&db).await, vec!["a"], "group still provides it");
        assert!(!is_manual(&db, GroupKind::Rule, "p1", "a").await.unwrap());
    }

    /// With no group behind it, the same action removes the item outright.
    #[tokio::test]
    async fn manual_off_removes_item_no_group_provides() {
        let path = temp_project("unlink-removed");
        let db = setup(&path).await;
        add_rule(&db, "a", &path).await;

        apply_rule_to_project(&db, "p1", "a", true).await.unwrap();
        crate::commands::rules::unlink_rule_from_project(&db, "p1", "a")
            .await
            .unwrap();

        assert!(applied_ids(&db).await.is_empty());
    }

    #[tokio::test]
    async fn manual_items_survive_group_disable() {
        let path = temp_project("manual-survives");
        let db = setup(&path).await;
        add_rule(&db, "hand-picked", &path).await;
        new_group(&db, "g1").await;
        set_members(&db, "g1", &["hand-picked"]).await;

        // User switched it on by hand first, then enabled a group that also contains it.
        apply_rule_to_project(&db, "p1", "hand-picked", true).await.unwrap();
        enable_group(&db, GroupKind::Rule, "p1", "g1").await.unwrap();

        assert!(is_manual(&db, GroupKind::Rule, "p1", "hand-picked").await.unwrap());
        assert_eq!(applied_ids(&db).await, vec!["hand-picked"]);
    }
}
