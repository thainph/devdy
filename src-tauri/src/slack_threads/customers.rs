//! Slack users marked by hand as customers (`slack_customers`, Work Dashboard)
//! and the per-thread "who spoke last" facts the dashboard classifies threads
//! with (waiting on me / waiting on the customer — see
//! `src/lib/workDashboard/slack.ts`). Plain `&Db` so it is testable against an
//! in-memory DB.

use super::mrkdwn;
use super::store::{internal, load_names};
use super::CodedError;
use crate::db::Db;
use serde::Serialize;
use sqlx::Row;
use std::collections::HashMap;
use std::path::Path;

const PREVIEW_MAX_CHARS: usize = 140;
const MAX_NOTE_CHARS: usize = 500;

/// Emitted after every customer write; the Slack window, Settings and the
/// dashboard reload their list.
pub const CUSTOMERS_CHANGED_EVENT: &str = "slack-customers://changed";

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackCustomer {
    pub team_id: String,
    pub user_id: String,
    pub project_id: Option<String>,
    pub note: String,
    pub created_at: String,
    /// From the `slack_users` cache; falls back to the user id.
    pub display_name: String,
    pub real_name: Option<String>,
    /// Absolute path of the cached avatar, if downloaded.
    pub avatar_path: Option<String>,
    /// Workspace name (`slack_threads.workspace`) of any thread of the team.
    pub workspace: Option<String>,
}

/// A human (non-bot) message of a thread, as the dashboard needs it.
#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackActivityMessage {
    pub user_id: String,
    pub display_name: String,
    pub ts: String,
    /// ISO time from `ts`.
    pub posted_at: Option<String>,
    pub is_customer: bool,
    /// First line, plain text, Vietnamese when translated.
    pub preview: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SlackThreadActivity {
    pub thread_id: String,
    /// `ts` the user last viewed; None = never opened (then every message is unread).
    pub last_seen_ts: Option<String>,
    /// Latest human message (bots and deleted messages skipped).
    pub last_human: Option<SlackActivityMessage>,
    /// Latest message from a marked customer.
    pub last_customer: Option<SlackActivityMessage>,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn validate_key(team_id: &str, user_id: &str) -> Result<(), CodedError> {
    let ok =
        |s: &str| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric());
    if ok(team_id) && ok(user_id) {
        Ok(())
    } else {
        Err(CodedError::new(
            "invalid_user",
            "team_id and user_id must be Slack ids",
        ))
    }
}

/// Every customer, newest first, with the cached Slack profile.
pub async fn list(db: &Db, app_data: &Path) -> Result<Vec<SlackCustomer>, CodedError> {
    let rows = sqlx::query(
        "SELECT c.team_id, c.user_id, c.project_id, c.note, c.created_at, \
                u.display_name, u.real_name, u.avatar_path, \
                (SELECT t.workspace FROM slack_threads t \
                   WHERE t.team_id = c.team_id AND t.workspace IS NOT NULL LIMIT 1) AS workspace \
         FROM slack_customers c \
         LEFT JOIN slack_users u ON u.team_id = c.team_id AND u.user_id = c.user_id \
         ORDER BY c.created_at DESC, c.user_id ASC",
    )
    .fetch_all(db)
    .await
    .map_err(internal)?;
    Ok(rows
        .iter()
        .map(|row| {
            let user_id: String = row.get("user_id");
            let display: Option<String> = row.get("display_name");
            let real: Option<String> = row.get("real_name");
            let avatar: Option<String> = row.get("avatar_path");
            SlackCustomer {
                team_id: row.get("team_id"),
                display_name: display
                    .filter(|s| !s.is_empty())
                    .or_else(|| real.clone().filter(|s| !s.is_empty()))
                    .unwrap_or_else(|| user_id.clone()),
                user_id,
                project_id: row.get("project_id"),
                note: row.get("note"),
                created_at: row.get("created_at"),
                real_name: real,
                avatar_path: avatar.map(|r| app_data.join(r).to_string_lossy().to_string()),
                workspace: row.get("workspace"),
            }
        })
        .collect())
}

/// Mark a Slack user as a customer (or update its project / note).
pub async fn set(
    db: &Db,
    team_id: &str,
    user_id: &str,
    project_id: Option<&str>,
    note: Option<&str>,
) -> Result<(), CodedError> {
    validate_key(team_id, user_id)?;
    if let Some(pid) = project_id {
        super::store::check_project(db, pid).await?;
    }
    let note = note.map(str::trim).unwrap_or_default();
    if note.chars().count() > MAX_NOTE_CHARS {
        return Err(CodedError::new(
            "invalid_note",
            format!("note exceeds {MAX_NOTE_CHARS} characters"),
        ));
    }
    sqlx::query(
        "INSERT INTO slack_customers (team_id, user_id, project_id, note, created_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(team_id, user_id) DO UPDATE SET \
           project_id = excluded.project_id, note = excluded.note",
    )
    .bind(team_id)
    .bind(user_id)
    .bind(project_id)
    .bind(note)
    .bind(now())
    .execute(db)
    .await
    .map_err(internal)?;
    Ok(())
}

/// Unmark a customer. Returns whether it was marked.
pub async fn remove(db: &Db, team_id: &str, user_id: &str) -> Result<bool, CodedError> {
    let r = sqlx::query("DELETE FROM slack_customers WHERE team_id = ? AND user_id = ?")
        .bind(team_id)
        .bind(user_id)
        .execute(db)
        .await
        .map_err(internal)?;
    Ok(r.rows_affected() > 0)
}

/// Per thread (optionally of one project): the latest human message and the
/// latest customer message. Bot messages (`bot_id`, or a user cached as a bot)
/// and deleted ones are skipped — a bot reply never answers a customer.
pub async fn thread_activity(
    db: &Db,
    project_id: Option<&str>,
) -> Result<Vec<SlackThreadActivity>, CodedError> {
    // Up to two rows per thread: the latest customer and the latest
    // non-customer human message (rn = 1 per partition).
    let mut sql = String::from(
        "SELECT t.id AS thread_id, t.last_seen_ts, t.team_id, x.ts, x.user_id, x.text, x.is_customer, x.seq \
         FROM slack_threads t \
         LEFT JOIN ( \
           SELECT m.thread_id, m.ts, m.seq, m.user_id, COALESCE(m.text_vi, m.text_raw) AS text, \
                  (c.user_id IS NOT NULL) AS is_customer, \
                  ROW_NUMBER() OVER ( \
                    PARTITION BY m.thread_id, (c.user_id IS NOT NULL) ORDER BY m.seq DESC \
                  ) AS rn \
           FROM slack_messages m \
           JOIN slack_threads tt ON tt.id = m.thread_id \
           LEFT JOIN slack_users u ON u.team_id = tt.team_id AND u.user_id = m.user_id \
           LEFT JOIN slack_customers c ON c.team_id = tt.team_id AND c.user_id = m.user_id \
           WHERE m.user_id IS NOT NULL AND m.bot_id IS NULL AND COALESCE(u.is_bot, 0) = 0 \
             AND COALESCE(m.subtype, '') NOT IN ('tombstone', 'bot_message') \
         ) x ON x.thread_id = t.id AND x.rn = 1 \
         WHERE 1 = 1",
    );
    if project_id.is_some() {
        sql.push_str(" AND t.project_id = ?");
    }
    sql.push_str(" ORDER BY t.id, x.seq ASC");
    let mut q = sqlx::query(&sql);
    if let Some(pid) = project_id {
        q = q.bind(pid);
    }
    let rows = q.fetch_all(db).await.map_err(internal)?;
    let names = load_names(db).await.map_err(internal)?;

    let mut out: Vec<SlackThreadActivity> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for row in &rows {
        let thread_id: String = row.get("thread_id");
        let i = *index.entry(thread_id.clone()).or_insert_with(|| {
            out.push(SlackThreadActivity {
                thread_id: thread_id.clone(),
                last_seen_ts: row.get("last_seen_ts"),
                last_human: None,
                last_customer: None,
            });
            out.len() - 1
        });
        let (Some(ts), Some(user_id)) = (
            row.get::<Option<String>, _>("ts"),
            row.get::<Option<String>, _>("user_id"),
        ) else {
            continue;
        };
        let team: Option<String> = row.get("team_id");
        let team_names = team.as_deref().and_then(|t| names.get(t));
        let text: Option<String> = row.get("text");
        let preview = match team_names {
            Some(n) => mrkdwn::first_line(
                &mrkdwn::to_plain(text.as_deref().unwrap_or_default(), n, mrkdwn::Links::Label),
                PREVIEW_MAX_CHARS,
            ),
            None => mrkdwn::first_line(
                &mrkdwn::to_plain(
                    text.as_deref().unwrap_or_default(),
                    &mrkdwn::Names::default(),
                    mrkdwn::Links::Label,
                ),
                PREVIEW_MAX_CHARS,
            ),
        };
        let is_customer = row.get::<i64, _>("is_customer") != 0;
        let msg = SlackActivityMessage {
            display_name: team_names
                .and_then(|n| n.users.get(&user_id).cloned())
                .unwrap_or_else(|| user_id.clone()),
            posted_at: mrkdwn::ts_to_iso(&ts),
            user_id,
            ts,
            is_customer,
            preview,
        };
        // Rows come in `seq` order, so the last one seen is the latest human message.
        let a = &mut out[i];
        if is_customer {
            a.last_customer = Some(msg.clone());
        }
        a.last_human = Some(msg);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::slack_threads::tests::memory_db;

    async fn thread(db: &Db, id: &str, project: Option<&str>, last_seen: Option<&str>) {
        sqlx::query(
            "INSERT INTO slack_threads (id, project_id, created_at, updated_at, team_id, workspace, \
               channel_id, thread_ts, last_seen_ts) \
             VALUES (?, ?, '2026-01-01', '2026-01-01', 'T1', 'Acme', 'C1', ?, ?)",
        )
        .bind(id)
        .bind(project)
        .bind(format!("1700000000.{id}"))
        .bind(last_seen)
        .execute(db)
        .await
        .unwrap();
    }

    async fn message(
        db: &Db,
        thread: &str,
        seq: i64,
        user: Option<&str>,
        bot: Option<&str>,
        text: &str,
    ) {
        sqlx::query(
            "INSERT INTO slack_messages (thread_id, ts, seq, user_id, bot_id, text_raw, src_hash) \
             VALUES (?, ?, ?, ?, ?, ?, 'h')",
        )
        .bind(thread)
        .bind(format!("17000000{seq:02}.000100"))
        .bind(seq)
        .bind(user)
        .bind(bot)
        .bind(text)
        .execute(db)
        .await
        .unwrap();
    }

    async fn user(db: &Db, id: &str, name: &str, bot: bool) {
        sqlx::query(
            "INSERT INTO slack_users (team_id, user_id, display_name, is_bot, fetched_at) \
             VALUES ('T1', ?, ?, ?, '2026-01-01')",
        )
        .bind(id)
        .bind(name)
        .bind(bot as i64)
        .execute(db)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn customers_crud_round_trip() {
        let db = memory_db().await;
        let app = Path::new("/data");
        user(&db, "U1", "Kenji", false).await;
        set(&db, "T1", "U1", Some("p1"), Some("  PM at Acme "))
            .await
            .unwrap();
        set(&db, "T1", "U2", None, None).await.unwrap();

        let list1 = list(&db, app).await.unwrap();
        assert_eq!(list1.len(), 2);
        let kenji = list1.iter().find(|c| c.user_id == "U1").unwrap();
        assert_eq!(kenji.display_name, "Kenji");
        assert_eq!(kenji.note, "PM at Acme");
        assert_eq!(kenji.project_id.as_deref(), Some("p1"));
        // Unknown profile falls back to the id.
        assert_eq!(
            list1
                .iter()
                .find(|c| c.user_id == "U2")
                .unwrap()
                .display_name,
            "U2"
        );

        // Upsert keeps one row and updates project / note.
        set(&db, "T1", "U1", None, Some("")).await.unwrap();
        let list2 = list(&db, app).await.unwrap();
        assert_eq!(list2.len(), 2);
        let kenji = list2.iter().find(|c| c.user_id == "U1").unwrap();
        assert_eq!(kenji.project_id, None);
        assert_eq!(kenji.note, "");

        assert!(remove(&db, "T1", "U1").await.unwrap());
        assert!(!remove(&db, "T1", "U1").await.unwrap());
        assert_eq!(list(&db, app).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn set_rejects_bad_input() {
        let db = memory_db().await;
        assert_eq!(
            set(&db, "", "U1", None, None).await.unwrap_err().code,
            "invalid_user"
        );
        assert_eq!(
            set(&db, "T1", "U 1", None, None).await.unwrap_err().code,
            "invalid_user"
        );
        assert_eq!(
            set(&db, "T1", "U1", Some("nope"), None)
                .await
                .unwrap_err()
                .code,
            "invalid_project"
        );
        let long = "x".repeat(MAX_NOTE_CHARS + 1);
        assert_eq!(
            set(&db, "T1", "U1", None, Some(&long))
                .await
                .unwrap_err()
                .code,
            "invalid_note"
        );
    }

    #[tokio::test]
    async fn removing_a_project_keeps_the_customer() {
        let db = memory_db().await;
        set(&db, "T1", "U1", Some("p2"), None).await.unwrap();
        sqlx::query("DELETE FROM projects WHERE id = 'p2'")
            .execute(&db)
            .await
            .unwrap();
        let rows = list(&db, Path::new("/data")).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].project_id, None);
    }

    #[tokio::test]
    async fn activity_reports_last_human_and_last_customer() {
        let db = memory_db().await;
        user(&db, "UC", "Customer", false).await;
        user(&db, "UME", "Me", false).await;
        user(&db, "UBOT", "Deploy bot", true).await;
        set(&db, "T1", "UC", None, None).await.unwrap();

        // a: customer spoke last (a bot after them does not count).
        thread(&db, "a", Some("p1"), None).await;
        message(&db, "a", 0, Some("UME"), None, "Hello").await;
        message(&db, "a", 1, Some("UC"), None, "<@UME> please fix login").await;
        message(&db, "a", 2, Some("UBOT"), None, "deployed").await;
        message(&db, "a", 3, None, Some("B1"), "ci green").await;
        // b: I answered after the customer.
        thread(&db, "b", Some("p2"), Some("1700000001.000100")).await;
        message(&db, "b", 0, Some("UC"), None, "Question").await;
        message(&db, "b", 1, Some("UME"), None, "Answer").await;
        // c: no customer at all; d: no messages yet.
        thread(&db, "c", Some("p1"), None).await;
        message(&db, "c", 0, Some("UME"), None, "Note to self").await;
        thread(&db, "d", Some("p1"), None).await;

        let all = thread_activity(&db, None).await.unwrap();
        let by = |id: &str| all.iter().find(|a| a.thread_id == id).unwrap().clone();
        assert_eq!(all.len(), 4);

        let a = by("a");
        let last = a.last_human.unwrap();
        assert_eq!(last.user_id, "UC");
        assert!(last.is_customer);
        assert_eq!(last.display_name, "Customer");
        assert_eq!(last.preview, "@Me please fix login");
        assert_eq!(a.last_customer.unwrap().user_id, "UC");
        assert_eq!(a.last_seen_ts, None);

        let b = by("b");
        assert_eq!(b.last_human.unwrap().user_id, "UME");
        assert_eq!(b.last_customer.unwrap().preview, "Question");
        assert_eq!(b.last_seen_ts.as_deref(), Some("1700000001.000100"));

        let c = by("c");
        assert!(!c.last_human.unwrap().is_customer);
        assert!(c.last_customer.is_none());

        let d = by("d");
        assert!(d.last_human.is_none() && d.last_customer.is_none());

        let p1 = thread_activity(&db, Some("p1")).await.unwrap();
        assert_eq!(p1.len(), 3);
        assert!(p1.iter().all(|a| a.thread_id != "b"));
    }
}
