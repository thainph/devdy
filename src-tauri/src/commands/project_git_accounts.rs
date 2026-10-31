//! Per-project enabled GitHub / GitLab / AWS / Azure accounts (toggled on/off
//! like servers).
//!
//! `project_{github,gitlab,aws,azure}_accounts` hold the enabled set;
//! `projects.{github,gitlab,aws,azure}_account_id` hold the default, which is
//! always kept inside the enabled set: enabling the first account makes it the
//! default, disabling (or deleting) the default promotes another enabled one.
//! Ids only — PATs and cloud credentials stay in the Keychain / CLI config.

use crate::db::Db;
use tauri::{AppHandle, Manager, State};

/// Tables and columns of one provider.
#[derive(Clone, Copy)]
pub struct Provider {
    link_table: &'static str,
    account_table: &'static str,
    default_column: &'static str,
}

pub const GITHUB: Provider = Provider {
    link_table: "project_github_accounts",
    account_table: "github_accounts",
    default_column: "github_account_id",
};

pub const GITLAB: Provider = Provider {
    link_table: "project_gitlab_accounts",
    account_table: "gitlab_accounts",
    default_column: "gitlab_account_id",
};

pub const AWS: Provider = Provider {
    link_table: "project_aws_accounts",
    account_table: "aws_accounts",
    default_column: "aws_account_id",
};

pub const AZURE: Provider = Provider {
    link_table: "project_azure_accounts",
    account_table: "azure_accounts",
    default_column: "azure_account_id",
};

fn provider(name: &str) -> Result<Provider, String> {
    match name {
        "github" => Ok(GITHUB),
        "gitlab" => Ok(GITLAB),
        other => Err(format!("unknown git provider: {other}")),
    }
}

fn cloud_provider(name: &str) -> Result<Provider, String> {
    match name {
        "aws" => Ok(AWS),
        "azure" => Ok(AZURE),
        other => Err(format!("unknown cloud provider: {other}")),
    }
}

/// Enable / disable one account for a project, keeping the default inside the
/// enabled set.
pub async fn set_enabled(
    db: &Db,
    p: Provider,
    project_id: &str,
    account_id: &str,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        enable(db, p, project_id, account_id).await?;
    } else {
        let sql = format!(
            "DELETE FROM {} WHERE project_id = ? AND account_id = ?",
            p.link_table
        );
        sqlx::query(&sql)
            .bind(project_id)
            .bind(account_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
        // The default must stay enabled: drop it here, `repair_defaults` picks
        // another one.
        let sql = format!(
            "UPDATE projects SET {col} = NULL WHERE id = ? AND {col} = ?",
            col = p.default_column
        );
        sqlx::query(&sql)
            .bind(project_id)
            .bind(account_id)
            .execute(db)
            .await
            .map_err(|e| e.to_string())?;
    }
    repair_defaults(db, p, Some(project_id)).await
}

/// Add one account to the project's enabled set (no default repair). Returns
/// whether it was newly enabled.
pub async fn enable(
    db: &Db,
    p: Provider,
    project_id: &str,
    account_id: &str,
) -> Result<bool, String> {
    let sql = format!(
        "INSERT OR IGNORE INTO {} (project_id, account_id, created_at) VALUES (?, ?, ?)",
        p.link_table
    );
    let res = sqlx::query(&sql)
        .bind(project_id)
        .bind(account_id)
        .bind(chrono::Utc::now().to_rfc3339())
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(res.rows_affected() > 0)
}

/// Make `account_id` the project's default (enabling it), or clear the default
/// with `None`.
pub async fn set_default(
    db: &Db,
    p: Provider,
    project_id: &str,
    account_id: Option<&str>,
) -> Result<(), String> {
    if let Some(id) = account_id {
        set_enabled(db, p, project_id, id, true).await?;
    }
    let sql = format!("UPDATE projects SET {} = ? WHERE id = ?", p.default_column);
    sqlx::query(&sql)
        .bind(account_id)
        .bind(project_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Projects (one, or all with `None`) whose default is unset or no longer
/// enabled get their first enabled account (by label) as the default — or NULL
/// when none is enabled.
pub async fn repair_defaults(db: &Db, p: Provider, project_id: Option<&str>) -> Result<(), String> {
    let sql = format!(
        "UPDATE projects SET {col} = ( \
             SELECT l.account_id FROM {link} l JOIN {acct} a ON a.id = l.account_id \
             WHERE l.project_id = projects.id ORDER BY a.label COLLATE NOCASE, a.id LIMIT 1) \
         WHERE (?1 IS NULL OR id = ?1) \
           AND ({col} IS NULL OR {col} NOT IN ( \
               SELECT account_id FROM {link} WHERE project_id = projects.id))",
        col = p.default_column,
        link = p.link_table,
        acct = p.account_table,
    );
    sqlx::query(&sql)
        .bind(project_id)
        .execute(db)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Toggle one GitHub (`provider = "github"`) or GitLab (`"gitlab"`) account for
/// a project.
#[tauri::command]
pub async fn set_project_git_account_enabled(
    db: State<'_, Db>,
    project_id: String,
    provider: String,
    account_id: String,
    enabled: bool,
) -> Result<(), String> {
    set_enabled(
        db.inner(),
        self::provider(&provider)?,
        &project_id,
        &account_id,
        enabled,
    )
    .await
}

/// Toggle one AWS (`provider = "aws"`) or Azure (`"azure"`) account for a
/// project.
#[tauri::command]
pub async fn set_project_cloud_account_enabled(
    app: AppHandle,
    db: State<'_, Db>,
    project_id: String,
    provider: String,
    account_id: String,
    enabled: bool,
) -> Result<(), String> {
    let p = cloud_provider(&provider)?;
    set_enabled(db.inner(), p, &project_id, &account_id, enabled).await?;
    // The default may have changed: the AWS sandbox pins its region.
    if p.default_column == AWS.default_column {
        if let Ok(app_data_dir) = app.path().app_data_dir() {
            crate::aws_sandbox::write_project_aws_config(db.inner(), &app_data_dir, &project_id)
                .await;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::Row;

    async fn mem_db() -> Db {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        for sql in [
            "CREATE TABLE projects (id TEXT PRIMARY KEY, github_account_id TEXT, gitlab_account_id TEXT)",
            "CREATE TABLE github_accounts (id TEXT PRIMARY KEY, label TEXT)",
            "CREATE TABLE gitlab_accounts (id TEXT PRIMARY KEY, label TEXT)",
            "CREATE TABLE project_github_accounts (project_id TEXT, account_id TEXT, created_at TEXT, PRIMARY KEY (project_id, account_id))",
            "CREATE TABLE project_gitlab_accounts (project_id TEXT, account_id TEXT, created_at TEXT, PRIMARY KEY (project_id, account_id))",
            "INSERT INTO projects (id) VALUES ('p1')",
            "INSERT INTO github_accounts (id, label) VALUES ('a', 'Work'), ('b', 'Alt'), ('c', 'Zed')",
        ] {
            sqlx::query(sql).execute(&pool).await.unwrap();
        }
        pool
    }

    async fn enabled(db: &Db) -> Vec<String> {
        sqlx::query("SELECT account_id FROM project_github_accounts WHERE project_id = 'p1' ORDER BY account_id")
            .fetch_all(db)
            .await
            .unwrap()
            .iter()
            .map(|r| r.get("account_id"))
            .collect()
    }

    async fn default_of(db: &Db) -> Option<String> {
        sqlx::query("SELECT github_account_id FROM projects WHERE id = 'p1'")
            .fetch_one(db)
            .await
            .unwrap()
            .get("github_account_id")
    }

    #[tokio::test]
    async fn first_enabled_account_becomes_default() {
        let db = mem_db().await;
        set_enabled(&db, GITHUB, "p1", "a", true).await.unwrap();
        assert_eq!(default_of(&db).await.as_deref(), Some("a"));
        // Enabling more keeps the existing default.
        set_enabled(&db, GITHUB, "p1", "b", true).await.unwrap();
        assert_eq!(default_of(&db).await.as_deref(), Some("a"));
        assert_eq!(enabled(&db).await, vec!["a", "b"]);
    }

    #[tokio::test]
    async fn disabling_default_promotes_next_by_label() {
        let db = mem_db().await;
        for id in ["a", "b", "c"] {
            set_enabled(&db, GITHUB, "p1", id, true).await.unwrap();
        }
        set_default(&db, GITHUB, "p1", Some("c")).await.unwrap();
        set_enabled(&db, GITHUB, "p1", "c", false).await.unwrap();
        // "Alt" sorts first.
        assert_eq!(default_of(&db).await.as_deref(), Some("b"));
        set_enabled(&db, GITHUB, "p1", "b", false).await.unwrap();
        set_enabled(&db, GITHUB, "p1", "a", false).await.unwrap();
        assert_eq!(default_of(&db).await, None);
    }

    #[tokio::test]
    async fn set_default_enables_the_account() {
        let db = mem_db().await;
        set_default(&db, GITHUB, "p1", Some("c")).await.unwrap();
        assert_eq!(default_of(&db).await.as_deref(), Some("c"));
        assert_eq!(enabled(&db).await, vec!["c"]);
    }
}
