//! Notes / todos ↔ sessions (runs), the `note_runs` / `todo_runs` tables
//! (migration 0044) — the sibling of `slack_threads::run_links`: link
//! (idempotent, the first source wins; an item without a project takes the
//! run's), unlink, the sessions of an item, the items of a session, and the
//! clean-ups on run / project / item delete. Readers join `runs`, so a link
//! whose run is gone is never listed.

use crate::db::Db;
use crate::slack_threads::run_links::LinkRunResult;
use crate::slack_threads::CodedError;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use tauri::{AppHandle, Emitter};

/// Broadcast after any write to notes / todos (same event the windows emit);
/// payload `{ kind }`, `kind` omitted = both kinds.
pub const ITEM_CHANGED_EVENT: &str = "item:changed";

/// Where a link came from.
pub const SOURCES: [&str; 4] = ["manual", "picker", "created_from_item", "captured"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Note,
    Todo,
}

impl ItemKind {
    pub const ALL: [ItemKind; 2] = [ItemKind::Note, ItemKind::Todo];

    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Note => "note",
            ItemKind::Todo => "todo",
        }
    }

    /// The items table.
    fn items(self) -> &'static str {
        match self {
            ItemKind::Note => "notes",
            ItemKind::Todo => "todos",
        }
    }

    /// The links table.
    fn links(self) -> &'static str {
        match self {
            ItemKind::Note => "note_runs",
            ItemKind::Todo => "todo_runs",
        }
    }

    /// The item column of the links table.
    fn col(self) -> &'static str {
        match self {
            ItemKind::Note => "note_id",
            ItemKind::Todo => "todo_id",
        }
    }

    fn label(self) -> &'static str {
        match self {
            ItemKind::Note => "note",
            ItemKind::Todo => "todo",
        }
    }
}

/// `COUNT` of an item's links whose run still exists; `alias` is the items
/// table alias of the enclosing query.
pub fn linked_count_sql(kind: ItemKind, alias: &str) -> String {
    format!(
        "(SELECT COUNT(*) FROM {links} l JOIN runs r ON r.id = l.run_id \
         WHERE l.{col} = {alias}.id) AS linked_run_count",
        links = kind.links(),
        col = kind.col(),
    )
}

/// A session linked to a note / todo (same fields as a Slack thread's).
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ItemRunLink {
    pub item_id: String,
    pub run_id: String,
    /// The run's current project.
    pub project_id: String,
    /// `manual` | `picker` | `created_from_item` | `captured`
    pub source: String,
    pub linked_at: String,
    pub title: Option<String>,
    pub run_type: String,
    pub ref_number: Option<i64>,
    pub status: String,
    pub engine: String,
    pub last_activity_at: Option<String>,
}

/// A note / todo linked to a session.
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunLinkedItem {
    pub kind: ItemKind,
    pub id: String,
    /// A note's own title ('' when it has none); '' for a todo.
    pub title: String,
    /// The body (note content / todo text).
    pub text: String,
    pub done: bool,
    pub project_id: Option<String>,
    pub linked_at: String,
}

fn internal(e: impl std::fmt::Display) -> CodedError {
    CodedError::new("internal", e.to_string())
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Emit `item:changed` (`kind` None = both kinds).
pub fn emit_changed(app: &AppHandle, kind: Option<ItemKind>) {
    #[derive(Clone, Serialize)]
    struct Payload {
        #[serde(skip_serializing_if = "Option::is_none")]
        kind: Option<ItemKind>,
    }
    let _ = app.emit(ITEM_CHANGED_EVENT, Payload { kind });
}

/// Link an item to a run. Re-linking is a no-op (the first source and time
/// are kept). An item without a project takes the run's.
pub async fn link(
    db: &Db,
    kind: ItemKind,
    item_id: &str,
    run_id: &str,
    source: &str,
) -> Result<LinkRunResult, CodedError> {
    if !SOURCES.contains(&source) {
        return Err(CodedError::new(
            "invalid_source",
            format!("unknown link source: {source}"),
        ));
    }
    let item: Option<Option<String>> = sqlx::query_scalar(&format!(
        "SELECT project_id FROM {} WHERE id = ?",
        kind.items()
    ))
    .bind(item_id)
    .fetch_optional(db)
    .await
    .map_err(internal)?;
    let item_project = item.ok_or_else(|| {
        CodedError::new(
            "not_found",
            format!("{} not found: {item_id}", kind.label()),
        )
    })?;
    let run_project: String = sqlx::query_scalar("SELECT project_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(db)
        .await
        .map_err(internal)?
        .ok_or_else(|| CodedError::new("run_not_found", format!("session not found: {run_id}")))?;
    let ts = now();
    let mut tx = db.begin().await.map_err(internal)?;
    let created = sqlx::query(&format!(
        "INSERT OR IGNORE INTO {} ({}, run_id, project_id, source, linked_at) \
         VALUES (?, ?, ?, ?, ?)",
        kind.links(),
        kind.col()
    ))
    .bind(item_id)
    .bind(run_id)
    .bind(&run_project)
    .bind(source)
    .bind(&ts)
    .execute(&mut *tx)
    .await
    .map_err(internal)?
    .rows_affected()
        > 0;
    let project_set = if item_project.is_none() {
        // Notes track their last edit; todos have no updated_at.
        let q = match kind {
            ItemKind::Note => sqlx::query(
                "UPDATE notes SET project_id = ?, updated_at = ? WHERE id = ? AND project_id IS NULL",
            )
            .bind(&run_project)
            .bind(&ts),
            ItemKind::Todo => {
                sqlx::query("UPDATE todos SET project_id = ? WHERE id = ? AND project_id IS NULL")
                    .bind(&run_project)
            }
        };
        q.bind(item_id)
            .execute(&mut *tx)
            .await
            .map_err(internal)?
            .rows_affected()
            > 0
    } else {
        false
    };
    tx.commit().await.map_err(internal)?;
    Ok(LinkRunResult {
        created,
        project_set,
    })
}

/// Remove one link; `false` when there was none.
pub async fn unlink(
    db: &Db,
    kind: ItemKind,
    item_id: &str,
    run_id: &str,
) -> Result<bool, CodedError> {
    let r = sqlx::query(&format!(
        "DELETE FROM {} WHERE {} = ? AND run_id = ?",
        kind.links(),
        kind.col()
    ))
    .bind(item_id)
    .bind(run_id)
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(r.rows_affected() > 0)
}

/// Sessions linked to an item whose run still exists, most recently active
/// first.
pub async fn runs_of_item(
    db: &Db,
    kind: ItemKind,
    item_id: &str,
) -> Result<Vec<ItemRunLink>, CodedError> {
    let rows = sqlx::query(&format!(
        "SELECT l.{col} AS item_id, l.run_id, l.source, l.linked_at, r.project_id, r.title, \
         r.type, r.ref_number, r.status, r.engine, \
         COALESCE(r.last_activity_at, r.finished_at, r.started_at, r.created_at) AS activity \
         FROM {links} l JOIN runs r ON r.id = l.run_id \
         WHERE l.{col} = ? \
         ORDER BY activity DESC, l.linked_at DESC",
        col = kind.col(),
        links = kind.links(),
    ))
    .bind(item_id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    Ok(rows
        .iter()
        .map(|row| ItemRunLink {
            item_id: row.get("item_id"),
            run_id: row.get("run_id"),
            project_id: row.get("project_id"),
            source: row.get("source"),
            linked_at: row.get("linked_at"),
            title: row.get("title"),
            run_type: row.get("type"),
            ref_number: row.get("ref_number"),
            status: row.get("status"),
            engine: row.get("engine"),
            last_activity_at: row.get("activity"),
        })
        .collect())
}

/// Notes then todos linked to a run, each most recently linked first.
pub async fn items_of_run(db: &Db, run_id: &str) -> Result<Vec<RunLinkedItem>, CodedError> {
    let notes = sqlx::query(
        "SELECT n.id, n.title, n.content AS text, 0 AS done, n.project_id, l.linked_at \
         FROM note_runs l JOIN notes n ON n.id = l.note_id \
         WHERE l.run_id = ? ORDER BY l.linked_at DESC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let todos = sqlx::query(
        "SELECT t.id, '' AS title, t.text, t.done, t.project_id, l.linked_at \
         FROM todo_runs l JOIN todos t ON t.id = l.todo_id \
         WHERE l.run_id = ? ORDER BY l.linked_at DESC",
    )
    .bind(run_id)
    .fetch_all(db)
    .await
    .map_err(internal)?;
    let item = |kind: ItemKind, row: &sqlx::sqlite::SqliteRow| RunLinkedItem {
        kind,
        id: row.get("id"),
        title: row.get("title"),
        text: row.get("text"),
        done: row.get::<i64, _>("done") != 0,
        project_id: row.get("project_id"),
        linked_at: row.get("linked_at"),
    };
    Ok(notes
        .iter()
        .map(|r| item(ItemKind::Note, r))
        .chain(todos.iter().map(|r| item(ItemKind::Todo, r)))
        .collect())
}

/// Drop every note / todo link of a deleted run; returns how many went.
pub async fn delete_for_run(db: &Db, run_id: &str) -> Result<u64, sqlx::Error> {
    let mut n = 0;
    for kind in ItemKind::ALL {
        n += sqlx::query(&format!("DELETE FROM {} WHERE run_id = ?", kind.links()))
            .bind(run_id)
            .execute(db)
            .await?
            .rows_affected();
    }
    Ok(n)
}

/// Drop every note / todo link of a project's runs (call BEFORE the project
/// row goes: its runs cascade away with it). Returns how many went.
pub async fn delete_for_project(db: &Db, project_id: &str) -> Result<u64, sqlx::Error> {
    let mut n = 0;
    for kind in ItemKind::ALL {
        n += sqlx::query(&format!(
            "DELETE FROM {} WHERE project_id = ?1 \
             OR run_id IN (SELECT id FROM runs WHERE project_id = ?1)",
            kind.links()
        ))
        .bind(project_id)
        .execute(db)
        .await?
        .rows_affected();
    }
    Ok(n)
}

/// Drop the links of deleted items (explicit: a connection without
/// `foreign_keys` would not cascade).
pub async fn delete_for_items<'e, E>(exec: E, kind: ItemKind, id: &str) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>,
{
    sqlx::query(&format!(
        "DELETE FROM {} WHERE {} = ?",
        kind.links(),
        kind.col()
    ))
    .bind(id)
    .execute(exec)
    .await?;
    Ok(())
}

/// Turn an item's capture run (`add_note` / `add_todo` with a `run_id`) into a
/// `captured` link. Best-effort: an unknown run is skipped. True = linked.
pub async fn link_captured(db: &Db, kind: ItemKind, item_id: &str, run_id: Option<&str>) -> bool {
    let Some(run_id) = run_id.filter(|r| !r.is_empty()) else {
        return false;
    };
    match link(db, kind, item_id, run_id, "captured").await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!(event = "item_capture_link_failed", kind = kind.as_str(), error = %e);
            false
        }
    }
}

/// An item's current project (after a `captured` link may have set it).
pub async fn project_of(db: &Db, kind: ItemKind, item_id: &str) -> Option<String> {
    sqlx::query_scalar(&format!(
        "SELECT project_id FROM {} WHERE id = ?",
        kind.items()
    ))
    .bind(item_id)
    .fetch_optional(db)
    .await
    .ok()
    .flatten()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack_threads::tests::memory_db;

    async fn insert_run(db: &Db, id: &str, project_id: &str, activity: &str) {
        sqlx::query(
            "INSERT INTO runs (id, project_id, type, status, engine, created_at, title, \
             last_activity_at) VALUES (?, ?, 'session', 'done', 'claude', ?, ?, ?)",
        )
        .bind(id)
        .bind(project_id)
        .bind(activity)
        .bind(format!("Run {id}"))
        .bind(activity)
        .execute(db)
        .await
        .unwrap();
    }

    async fn insert_note(db: &Db, id: &str, project_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO notes (id, title, content, project_id, position, created_at, updated_at) \
             VALUES (?, 'T', 'body', ?, 0, '2026-01-01', '2026-01-01')",
        )
        .bind(id)
        .bind(project_id)
        .execute(db)
        .await
        .unwrap();
    }

    async fn insert_todo(db: &Db, id: &str, project_id: Option<&str>) {
        sqlx::query(
            "INSERT INTO todos (id, text, done, position, created_at, project_id) \
             VALUES (?, 'do it', 0, 0, '2026-01-01', ?)",
        )
        .bind(id)
        .bind(project_id)
        .execute(db)
        .await
        .unwrap();
    }

    async fn count(db: &Db, table: &str) -> i64 {
        sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
            .fetch_one(db)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn links_are_idempotent_and_adopt_the_run_project() {
        let db = memory_db().await;
        insert_run(&db, "r1", "p2", "2026-02-01T00:00:00Z").await;
        insert_run(&db, "r2", "p2", "2026-03-01T00:00:00Z").await;
        insert_note(&db, "n1", None).await;
        insert_todo(&db, "t1", Some("p1")).await;

        let r = link(&db, ItemKind::Note, "n1", "r1", "picker")
            .await
            .unwrap();
        assert!(r.created && r.project_set);
        let project: Option<String> =
            sqlx::query_scalar("SELECT project_id FROM notes WHERE id = 'n1'")
                .fetch_one(&db)
                .await
                .unwrap();
        assert_eq!(project.as_deref(), Some("p2"));
        assert_eq!(
            project_of(&db, ItemKind::Note, "n1").await.as_deref(),
            Some("p2")
        );
        assert_eq!(project_of(&db, ItemKind::Todo, "nope").await, None);
        // Re-link: no-op, the first source is kept.
        let r = link(&db, ItemKind::Note, "n1", "r1", "manual")
            .await
            .unwrap();
        assert!(!r.created && !r.project_set);
        link(&db, ItemKind::Note, "n1", "r2", "created_from_item")
            .await
            .unwrap();
        let links = runs_of_item(&db, ItemKind::Note, "n1").await.unwrap();
        let got: Vec<(&str, &str)> = links
            .iter()
            .map(|l| (l.run_id.as_str(), l.source.as_str()))
            .collect();
        assert_eq!(got, [("r2", "created_from_item"), ("r1", "picker")]);
        assert_eq!(links[1].title.as_deref(), Some("Run r1"));

        // A todo that has a project keeps it.
        let r = link(&db, ItemKind::Todo, "t1", "r1", "manual")
            .await
            .unwrap();
        assert!(r.created && !r.project_set);
        let items = items_of_run(&db, "r1").await.unwrap();
        let kinds: Vec<(ItemKind, &str)> = items.iter().map(|i| (i.kind, i.id.as_str())).collect();
        assert_eq!(kinds, [(ItemKind::Note, "n1"), (ItemKind::Todo, "t1")]);
        assert_eq!(items[1].text, "do it");

        // The list count joins runs.
        let n: i64 = sqlx::query_scalar(&format!(
            "SELECT {} FROM notes n WHERE n.id = 'n1'",
            linked_count_sql(ItemKind::Note, "n")
        ))
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(n, 2);

        // Errors.
        let e = link(&db, ItemKind::Note, "n1", "r1", "bogus")
            .await
            .unwrap_err();
        assert_eq!(e.code, "invalid_source");
        let e = link(&db, ItemKind::Todo, "t1", "nope", "manual")
            .await
            .unwrap_err();
        assert_eq!(e.code, "run_not_found");
        let e = link(&db, ItemKind::Todo, "nope", "r1", "manual")
            .await
            .unwrap_err();
        assert_eq!(e.code, "not_found");
    }

    #[tokio::test]
    async fn links_are_cleaned_up_and_hide_missing_runs() {
        let db = memory_db().await;
        insert_run(&db, "r1", "p1", "2026-02-01T00:00:00Z").await;
        insert_run(&db, "r2", "p1", "2026-02-02T00:00:00Z").await;
        insert_run(&db, "r3", "p2", "2026-02-03T00:00:00Z").await;
        insert_note(&db, "n1", Some("p1")).await;
        insert_todo(&db, "t1", Some("p1")).await;
        for r in ["r1", "r2", "r3"] {
            link(&db, ItemKind::Note, "n1", r, "manual").await.unwrap();
            link(&db, ItemKind::Todo, "t1", r, "manual").await.unwrap();
        }

        // A run removed behind our back: hidden.
        sqlx::query("DELETE FROM runs WHERE id = 'r1'")
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(
            runs_of_item(&db, ItemKind::Todo, "t1").await.unwrap().len(),
            2
        );

        // The run-delete path drops the links of both kinds.
        let registry = crate::runs::RunRegistry::default();
        crate::commands::runs::delete_run_inner(&db, &registry, "r2")
            .await
            .unwrap();
        assert_eq!(delete_for_run(&db, "r1").await.unwrap(), 2);
        assert_eq!(count(&db, "note_runs").await, 1);
        assert_eq!(count(&db, "todo_runs").await, 1);

        // Project removal (r3 is p2's).
        assert_eq!(delete_for_project(&db, "p2").await.unwrap(), 2);
        assert_eq!(
            count(&db, "note_runs").await + count(&db, "todo_runs").await,
            0
        );

        // Unlink + item delete.
        insert_run(&db, "r4", "p1", "2026-02-04T00:00:00Z").await;
        link(&db, ItemKind::Note, "n1", "r4", "manual")
            .await
            .unwrap();
        link(&db, ItemKind::Todo, "t1", "r4", "manual")
            .await
            .unwrap();
        assert!(unlink(&db, ItemKind::Note, "n1", "r4").await.unwrap());
        assert!(!unlink(&db, ItemKind::Note, "n1", "r4").await.unwrap());
        delete_for_items(&db, ItemKind::Todo, "t1").await.unwrap();
        assert_eq!(
            count(&db, "note_runs").await + count(&db, "todo_runs").await,
            0
        );
    }

    #[tokio::test]
    async fn migration_0044_turns_capture_backlinks_into_links() {
        use sqlx::migrate::Migrator;
        use sqlx::sqlite::SqlitePoolOptions;
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let full = sqlx::migrate!("./migrations");
        let upto = Migrator {
            migrations: std::borrow::Cow::Owned(
                full.migrations
                    .iter()
                    .filter(|m| m.version <= 43)
                    .cloned()
                    .collect(),
            ),
            ignore_missing: false,
            locking: full.locking,
            no_tx: full.no_tx,
        };
        upto.run(&pool).await.unwrap();
        sqlx::query("INSERT INTO projects (id, name, path, created_at) VALUES ('p1', 'p', '/tmp/p', '2026')")
            .execute(&pool)
            .await
            .unwrap();
        insert_run(&pool, "r1", "p1", "2026-02-01T00:00:00Z").await;
        sqlx::query(
            "INSERT INTO notes (id, title, content, project_id, run_id, position, created_at, updated_at) \
             VALUES ('n1', 't', 'c', 'p1', 'r1', 0, '2026-01-05', '2026-01-05'), \
                    ('n2', 't', 'c', 'p1', 'gone', 1, '2026-01-05', '2026-01-05')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO todos (id, text, done, position, created_at, project_id, run_id) \
             VALUES ('t1', 'x', 0, 0, '2026-01-06', 'p1', 'r1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        full.run(&pool).await.unwrap();
        let notes: Vec<(String, String, String, String)> =
            sqlx::query_as("SELECT note_id, run_id, source, linked_at FROM note_runs")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            notes,
            [(
                "n1".to_string(),
                "r1".to_string(),
                "captured".to_string(),
                "2026-01-05".to_string()
            )]
        );
        let todos = runs_of_item(&pool, ItemKind::Todo, "t1").await.unwrap();
        assert_eq!(todos.len(), 1);
        assert_eq!(todos[0].source, "captured");
        assert_eq!(todos[0].project_id, "p1");
    }
}
