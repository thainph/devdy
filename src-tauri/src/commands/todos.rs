use crate::db::Db;
use crate::focus::{self, source};
use crate::item_links::{self, ItemKind};
use serde::Serialize;
use serde_json::{Map, Value};
use sqlx::Row;
use tauri::{AppHandle, State};
use uuid::Uuid;

/// A todo. `position` orders the quick list (ascending: lower value sits
/// higher). Drag-and-drop reorder rewrites positions.
///
/// `project_id` is the capture context (nullable, unenforced). Sessions (runs)
/// are linked through `todo_runs` (see `item_links`); `linked_run_count`
/// counts those whose run still exists. `due_date` is a local calendar date
/// `YYYY-MM-DD`, `due_time` an optional local `HH:MM` on it. The Focus fields
/// are described in docs/focus-board-spec.md §5.1; `done` / `completed_at`
/// always change together, and `status` only carries `todo|doing|waiting`.
#[derive(Debug, Serialize, Clone, PartialEq)]
pub struct Todo {
    pub id: String,
    pub text: String,
    pub done: bool,
    pub position: i64,
    pub created_at: String,
    pub project_id: Option<String>,
    pub due_date: Option<String>,
    pub due_time: Option<String>,
    pub priority: i64,
    pub estimate_min: Option<i64>,
    pub estimate_source: Option<String>,
    pub status: String,
    pub snoozed_until: Option<String>,
    pub remind_at: Option<String>,
    pub source_type: Option<String>,
    pub source_ref: Option<String>,
    pub source_url: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub actual_min: Option<i64>,
    pub ai_plan: Option<String>,
    pub ai_analyzed_at: Option<String>,
    pub updated_at: Option<String>,
    pub linked_run_count: i64,
}

const TODO_COLS: &str = "t.id, t.text, t.done, t.position, t.created_at, t.project_id, t.due_date, \
     t.due_time, t.priority, t.estimate_min, t.estimate_source, t.status, t.snoozed_until, \
     t.remind_at, t.source_type, t.source_ref, t.source_url, t.started_at, t.completed_at, \
     t.actual_min, t.ai_plan, t.ai_analyzed_at, t.updated_at";

fn row_to_todo(row: &sqlx::sqlite::SqliteRow) -> Todo {
    Todo {
        id: row.get("id"),
        text: row.get("text"),
        done: row.get("done"),
        position: row.get("position"),
        created_at: row.get("created_at"),
        project_id: row.get("project_id"),
        due_date: row.get("due_date"),
        due_time: row.get("due_time"),
        priority: row.get("priority"),
        estimate_min: row.get("estimate_min"),
        estimate_source: row.get("estimate_source"),
        status: row.get("status"),
        snoozed_until: row.get("snoozed_until"),
        remind_at: row.get("remind_at"),
        source_type: row.get("source_type"),
        source_ref: row.get("source_ref"),
        source_url: row.get("source_url"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        actual_min: row.get("actual_min"),
        ai_plan: row.get("ai_plan"),
        ai_analyzed_at: row.get("ai_analyzed_at"),
        updated_at: row.get("updated_at"),
        linked_run_count: row.get("linked_run_count"),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Every todo, quick-list order.
pub async fn load_todos(db: &Db) -> Result<Vec<Todo>, String> {
    let rows = sqlx::query(&format!(
        "SELECT {TODO_COLS}, {} FROM todos t ORDER BY t.position ASC, t.created_at DESC",
        item_links::linked_count_sql(ItemKind::Todo, "t")
    ))
    .fetch_all(db)
    .await
    .map_err(|e| e.to_string())?;
    Ok(rows.iter().map(row_to_todo).collect())
}

pub async fn get_todo(db: &Db, id: &str) -> Result<Todo, String> {
    let row = sqlx::query(&format!(
        "SELECT {TODO_COLS}, {} FROM todos t WHERE t.id = ?",
        item_links::linked_count_sql(ItemKind::Todo, "t")
    ))
    .bind(id)
    .fetch_optional(db)
    .await
    .map_err(|e| e.to_string())?
    .ok_or_else(|| format!("todo not found: {id}"))?;
    Ok(row_to_todo(&row))
}

#[tauri::command]
pub async fn list_todos(db: State<'_, Db>) -> Result<Vec<Todo>, String> {
    load_todos(db.inner()).await
}

#[allow(clippy::too_many_arguments)]
#[tauri::command]
pub async fn add_todo(
    app: AppHandle,
    db: State<'_, Db>,
    text: String,
    project_id: Option<String>,
    run_id: Option<String>,
    due_date: Option<String>,
    due_time: Option<String>,
    priority: Option<i64>,
    estimate_min: Option<i64>,
    source_url: Option<String>,
) -> Result<Todo, String> {
    let text = focus::validate::text(&text)?;
    let due_date = focus::validate::due_date(due_date.as_deref())?;
    let due_time = focus::validate::due_time(due_time.as_deref())?;
    if due_time.is_some() && due_date.is_none() {
        return Err("due_time needs a due_date".into());
    }
    let priority = focus::validate::priority(priority.unwrap_or(2))?;
    let estimate_min = estimate_min.map(focus::validate::estimate).transpose()?;
    let source = match source_url.as_deref() {
        Some(u) => source::normalize(u)?,
        None => None,
    };
    let db = db.inner();
    let id = Uuid::new_v4().to_string();
    let created_at = now();
    // New todos go to the top by taking a position below the current minimum.
    let min_pos: Option<i64> = sqlx::query("SELECT MIN(position) AS min_pos FROM todos")
        .fetch_one(db)
        .await
        .map_err(|e| e.to_string())?
        .get("min_pos");
    let position = min_pos.unwrap_or(0) - 1;

    sqlx::query(
        "INSERT INTO todos (id, text, done, position, created_at, project_id, due_date, due_time, \
         priority, estimate_min, estimate_source, status, source_type, source_ref, source_url, \
         updated_at) VALUES (?, ?, 0, ?, ?, ?, ?, ?, ?, ?, ?, 'todo', ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(&text)
    .bind(position)
    .bind(&created_at)
    .bind(&project_id)
    .bind(&due_date)
    .bind(&due_time)
    .bind(priority)
    .bind(estimate_min)
    .bind(estimate_min.map(|_| "user"))
    .bind(source.as_ref().map(|s| s.source_type))
    .bind(source.as_ref().and_then(|s| s.source_ref.clone()))
    .bind(source.as_ref().map(|s| s.source_url.clone()))
    .bind(&created_at)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    // The run it was captured from becomes a `captured` session link (a
    // project-less capture takes the run's project with it).
    item_links::link_captured(db, ItemKind::Todo, &id, run_id.as_deref()).await;
    let todo = get_todo(db, &id).await?;
    focus::emit_todos_changed(&app);
    Ok(todo)
}

/// Link or unlink a todo from a project. Pass `None` to clear the link. Session
/// links are independent of it (a session may live in another project).
#[tauri::command]
pub async fn set_todo_project(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    project_id: Option<String>,
) -> Result<(), String> {
    sqlx::query("UPDATE todos SET project_id = ?, updated_at = ? WHERE id = ?")
        .bind(&project_id)
        .bind(now())
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

/// Set or clear (`None`) a todo's deadline, a `YYYY-MM-DD` date. Clearing it
/// clears the due time too.
#[tauri::command]
pub async fn set_todo_due_date(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    due_date: Option<String>,
) -> Result<(), String> {
    let due_date = focus::validate::due_date(due_date.as_deref())?;
    sqlx::query(
        "UPDATE todos SET due_date = ?, due_time = CASE WHEN ? IS NULL THEN NULL ELSE due_time END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&due_date)
    .bind(&due_date)
    .bind(now())
    .bind(&id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

/// Flip `done`; `completed_at` follows it, and un-doing returns to `todo`.
#[tauri::command]
pub async fn toggle_todo(app: AppHandle, db: State<'_, Db>, id: String) -> Result<(), String> {
    let now = now();
    sqlx::query(
        "UPDATE todos SET done = 1 - done, \
         completed_at = CASE WHEN done = 0 THEN ? ELSE NULL END, \
         status = CASE WHEN done = 1 THEN 'todo' ELSE status END, \
         updated_at = ? WHERE id = ?",
    )
    .bind(&now)
    .bind(&now)
    .bind(&id)
    .execute(db.inner())
    .await
    .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn update_todo(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    text: String,
) -> Result<(), String> {
    let text = focus::validate::text(&text)?;
    sqlx::query("UPDATE todos SET text = ?, updated_at = ? WHERE id = ?")
        .bind(&text)
        .bind(now())
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

/// Partial update (spec §6.1). `patch` keys (snake_case): `text, project_id,
/// due_date, due_time, priority, estimate_min, estimate_source, status,
/// snoozed_until, remind_at, ai_plan, actual_min, done`; a missing key leaves
/// the field alone, `null` clears it. Returns the updated todo.
#[tauri::command]
pub async fn update_todo_fields(
    app: AppHandle,
    db: State<'_, Db>,
    id: String,
    patch: Map<String, Value>,
) -> Result<Todo, String> {
    let db = db.inner();
    let current = get_todo(db, &id).await?;
    let t = apply_patch(&current, &patch, &now())?;
    sqlx::query(
        "UPDATE todos SET text = ?, project_id = ?, due_date = ?, due_time = ?, priority = ?, \
         estimate_min = ?, estimate_source = ?, status = ?, snoozed_until = ?, remind_at = ?, \
         ai_plan = ?, actual_min = ?, done = ?, started_at = ?, completed_at = ?, updated_at = ? \
         WHERE id = ?",
    )
    .bind(&t.text)
    .bind(&t.project_id)
    .bind(&t.due_date)
    .bind(&t.due_time)
    .bind(t.priority)
    .bind(t.estimate_min)
    .bind(&t.estimate_source)
    .bind(&t.status)
    .bind(&t.snoozed_until)
    .bind(&t.remind_at)
    .bind(&t.ai_plan)
    .bind(t.actual_min)
    .bind(t.done)
    .bind(&t.started_at)
    .bind(&t.completed_at)
    .bind(&t.updated_at)
    .bind(&id)
    .execute(db)
    .await
    .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(t)
}

pub const PATCH_KEYS: [&str; 13] = [
    "text",
    "project_id",
    "due_date",
    "due_time",
    "priority",
    "estimate_min",
    "estimate_source",
    "status",
    "snoozed_until",
    "remind_at",
    "ai_plan",
    "actual_min",
    "done",
];

fn opt_str(key: &str, v: &Value) -> Result<Option<String>, String> {
    match v {
        Value::Null => Ok(None),
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(format!("{key} must be a string or null")),
    }
}

fn opt_int(key: &str, v: &Value) -> Result<Option<i64>, String> {
    match v {
        Value::Null => Ok(None),
        v => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| format!("{key} must be an integer or null")),
    }
}

/// `patch` applied to `cur` (pure; `now` is RFC3339), validated per BR-01.
pub fn apply_patch(cur: &Todo, patch: &Map<String, Value>, now: &str) -> Result<Todo, String> {
    use focus::validate as v;
    if let Some(k) = patch.keys().find(|k| !PATCH_KEYS.contains(&k.as_str())) {
        return Err(format!("unknown field: {k}"));
    }
    let mut t = cur.clone();
    for (key, val) in patch {
        match key.as_str() {
            "text" => {
                let s = opt_str(key, val)?.ok_or("text is required")?;
                t.text = v::text(&s)?;
            }
            "project_id" => {
                t.project_id = opt_str(key, val)?.filter(|s| !s.trim().is_empty());
            }
            "due_date" => {
                t.due_date = v::due_date(opt_str(key, val)?.as_deref())?;
                if t.due_date.is_none() && !patch.contains_key("due_time") {
                    t.due_time = None;
                }
            }
            "due_time" => t.due_time = v::due_time(opt_str(key, val)?.as_deref())?,
            "priority" => t.priority = v::priority(opt_int(key, val)?.unwrap_or(2))?,
            "estimate_min" => {
                t.estimate_min = opt_int(key, val)?.map(v::estimate).transpose()?;
                if !patch.contains_key("estimate_source") {
                    t.estimate_source = t.estimate_min.map(|_| "user".to_string());
                }
            }
            "estimate_source" => {
                t.estimate_source = opt_str(key, val)?;
                if let Some(s) = t.estimate_source.as_deref() {
                    if s != "user" && s != "ai" {
                        return Err("estimate_source must be user or ai".into());
                    }
                }
            }
            "status" => {
                t.status = v::status(opt_str(key, val)?.as_deref().unwrap_or("todo"))?;
            }
            "snoozed_until" => t.snoozed_until = v::instant(key, opt_str(key, val)?.as_deref())?,
            "remind_at" => t.remind_at = v::instant(key, opt_str(key, val)?.as_deref())?,
            "ai_plan" => t.ai_plan = opt_str(key, val)?.filter(|s| !s.trim().is_empty()),
            "actual_min" => t.actual_min = opt_int(key, val)?.map(v::actual).transpose()?,
            "done" => {
                t.done = val.as_bool().ok_or("done must be a boolean")?;
            }
            _ => unreachable!(),
        }
    }
    if t.due_time.is_some() && t.due_date.is_none() {
        return Err("due_time needs a due_date".into());
    }
    if t.done != cur.done {
        if t.done {
            t.completed_at = Some(now.to_string());
        } else {
            t.completed_at = None;
            if !patch.contains_key("status") {
                t.status = "todo".into();
            }
        }
    }
    if t.status == "doing" && t.started_at.is_none() {
        t.started_at = Some(now.to_string());
    }
    t.updated_at = Some(now.to_string());
    Ok(t)
}

#[tauri::command]
pub async fn delete_todo(app: AppHandle, db: State<'_, Db>, id: String) -> Result<(), String> {
    item_links::delete_for_items(db.inner(), ItemKind::Todo, &id)
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM todos WHERE id = ?")
        .bind(&id)
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

/// Bulk-delete todos by id in a single transaction, so a failure mid-way rolls
/// back the whole batch instead of leaving a partial deletion.
#[tauri::command]
pub async fn delete_todos(
    app: AppHandle,
    db: State<'_, Db>,
    ids: Vec<String>,
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for id in &ids {
        item_links::delete_for_items(&mut *tx, ItemKind::Todo, id)
            .await
            .map_err(|e| e.to_string())?;
        sqlx::query("DELETE FROM todos WHERE id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

#[tauri::command]
pub async fn clear_completed_todos(app: AppHandle, db: State<'_, Db>) -> Result<(), String> {
    sqlx::query("DELETE FROM todo_runs WHERE todo_id IN (SELECT id FROM todos WHERE done = 1)")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM todos WHERE done = 1")
        .execute(db.inner())
        .await
        .map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

/// Persist a new order. The frontend sends the full list of ids top-to-bottom;
/// each row's position becomes its index, so the ordering is stable regardless
/// of prior position values. Done in one transaction to avoid partial reorders.
#[tauri::command]
pub async fn reorder_todos(
    app: AppHandle,
    db: State<'_, Db>,
    ids: Vec<String>,
) -> Result<(), String> {
    let mut tx = db.inner().begin().await.map_err(|e| e.to_string())?;
    for (index, id) in ids.iter().enumerate() {
        sqlx::query("UPDATE todos SET position = ? WHERE id = ?")
            .bind(index as i64)
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
    }
    tx.commit().await.map_err(|e| e.to_string())?;
    focus::emit_todos_changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: &str = "2026-10-28T04:00:00+00:00";

    fn todo() -> Todo {
        Todo {
            id: "t1".into(),
            text: "Fix CSV".into(),
            done: false,
            position: 0,
            created_at: "2026-10-27T00:00:00+00:00".into(),
            project_id: Some("p1".into()),
            due_date: Some("2026-10-30".into()),
            due_time: Some("15:00".into()),
            priority: 2,
            estimate_min: None,
            estimate_source: None,
            status: "todo".into(),
            snoozed_until: None,
            remind_at: None,
            source_type: None,
            source_ref: None,
            source_url: None,
            started_at: None,
            completed_at: None,
            actual_min: None,
            ai_plan: None,
            ai_analyzed_at: None,
            updated_at: None,
            linked_run_count: 0,
        }
    }

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn absent_keys_are_untouched_and_null_clears() {
        let t = apply_patch(&todo(), &patch(json!({ "project_id": null })), NOW).unwrap();
        assert_eq!(t.project_id, None);
        assert_eq!(t.due_time.as_deref(), Some("15:00"));
        assert_eq!(t.updated_at.as_deref(), Some(NOW));
        // Clearing the date drops the time with it.
        let t = apply_patch(&todo(), &patch(json!({ "due_date": null })), NOW).unwrap();
        assert_eq!((t.due_date, t.due_time), (None, None));
        let t = apply_patch(&todo(), &patch(json!({ "priority": null })), NOW).unwrap();
        assert_eq!(t.priority, 2);
    }

    #[test]
    fn br01_validation() {
        let bad = |p: Value| apply_patch(&todo(), &patch(p), NOW).unwrap_err();
        assert!(bad(json!({ "text": "  " })).contains("text"));
        assert!(bad(json!({ "text": null })).contains("text"));
        assert!(bad(json!({ "priority": 4 })).contains("priority"));
        assert!(bad(json!({ "estimate_min": 4 })).contains("estimate"));
        assert!(bad(json!({ "estimate_min": 2401 })).contains("estimate"));
        assert!(bad(json!({ "status": "done" })).contains("status"));
        assert!(bad(json!({ "due_time": "25:00" })).contains("due_time"));
        assert!(bad(json!({ "due_time": "9:00" })).contains("due_time"));
        assert!(bad(json!({ "due_date": null, "due_time": "09:00" })).contains("due_date"));
        assert!(bad(json!({ "due_date": "30/10/2026" })).contains("due date"));
        assert!(bad(json!({ "snoozed_until": "tomorrow" })).contains("snoozed_until"));
        assert!(bad(json!({ "estimate_source": "guess" })).contains("estimate_source"));
        assert!(bad(json!({ "done": "yes" })).contains("done"));
        assert!(bad(json!({ "colour": "red" })).contains("unknown field"));
        // Long text is kept whole.
        let long = "x".repeat(800);
        assert_eq!(apply_patch(&todo(), &patch(json!({ "text": long })), NOW).unwrap().text.len(), 800);
    }

    #[test]
    fn estimate_source_follows_estimate() {
        let t = apply_patch(&todo(), &patch(json!({ "estimate_min": 90 })), NOW).unwrap();
        assert_eq!((t.estimate_min, t.estimate_source.as_deref()), (Some(90), Some("user")));
        let t = apply_patch(&t, &patch(json!({ "estimate_min": 120, "estimate_source": "ai" })), NOW).unwrap();
        assert_eq!(t.estimate_source.as_deref(), Some("ai"));
        let t = apply_patch(&t, &patch(json!({ "estimate_min": null })), NOW).unwrap();
        assert_eq!((t.estimate_min, t.estimate_source), (None, None));
    }

    #[test]
    fn status_and_done_timestamps() {
        let t = apply_patch(&todo(), &patch(json!({ "status": "doing" })), NOW).unwrap();
        assert_eq!(t.started_at.as_deref(), Some(NOW));
        // started_at is kept on a later start.
        let t2 = apply_patch(&t, &patch(json!({ "status": "doing" })), "2026-10-29T00:00:00Z").unwrap();
        assert_eq!(t2.started_at.as_deref(), Some(NOW));
        let done = apply_patch(&t, &patch(json!({ "done": true, "actual_min": 45 })), NOW).unwrap();
        assert!(done.done);
        assert_eq!((done.completed_at.as_deref(), done.actual_min), (Some(NOW), Some(45)));
        assert_eq!(done.status, "doing");
        // Marking done again keeps the first completion time.
        let again = apply_patch(&done, &patch(json!({ "done": true })), "2026-10-29T00:00:00Z").unwrap();
        assert_eq!(again.completed_at.as_deref(), Some(NOW));
        let undone = apply_patch(&done, &patch(json!({ "done": false })), NOW).unwrap();
        assert_eq!((undone.done, undone.completed_at, undone.status.as_str()), (false, None, "todo"));
        let snoozed = apply_patch(
            &todo(),
            &patch(json!({ "snoozed_until": "2026-10-28T14:00:00+07:00", "status": "waiting" })),
            NOW,
        )
        .unwrap();
        assert_eq!(snoozed.snoozed_until.as_deref(), Some("2026-10-28T14:00:00+07:00"));
        assert_eq!(snoozed.status, "waiting");
    }
}
